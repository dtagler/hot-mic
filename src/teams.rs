//! Microsoft Teams mute-state client.
//!
//! Connects to the (local-only) Teams Local API at `ws://127.0.0.1:8124` and
//! reads `meetingUpdate` push messages so we know whether the user has muted
//! themselves *inside* Microsoft Teams. The OS doesn't expose this — Teams
//! drops captured samples in user space without ever calling the WASAPI mute
//! API. Teams builds that no longer expose the Local API fall back to the
//! read-only UI Automation detector in `teams_ui`.
//!
//! ## Single-thread integration
//! `WSAAsyncSelect` posts socket events as `WM_TEAMS_SOCKET` window messages
//! that arrive on the existing message-loop thread. No tokio, no extra
//! threads, no channels.
//!
//! ## Minimal client surface
//! This client deliberately sends exactly one type of action: a single
//! `{"action":"pair"}` request, sent once per connection only when Teams
//! advertises `canPair:true` and we have no stored token. That's what
//! triggers the Allow banner inside Teams during first-run pairing. We never
//! send `toggle-mute`, `leave-call`, `toggle-video`, or any other action.
//! Other outgoing payloads are the HTTP upgrade request, pong frames in
//! response to server pings, and a single close frame on shutdown. The token
//! Teams gives us grants WRITE access to those actions; this discipline plus
//! DPAPI wrapping is the mitigation for token misuse.
//!
//! ## Degradation
//! The Local API is preferred after it supplies a complete meeting state.
//! Otherwise `muted_now()` asks the UI Automation fallback while Teams holds
//! the mic. If neither source is readable, it returns `false`, keeping the
//! border on rather than hiding a real capture indicator.

use std::env;
use std::ffi::c_void;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::SystemTime;

use windows::Win32::Foundation::*;
use windows::Win32::Networking::WinSock::*;
use windows::Win32::Security::Cryptography::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use hotmic::{
    base64_encode, parse_can_pair, parse_meeting_update, redact_secrets, resolve_teams_muted,
    ws_build_close_frame, ws_build_text_frame, ws_handshake_request, ws_parse_frame, WsFrameParse,
};

use crate::teams_ui::MuteDetector;

/// Posted to `msg_hwnd` by `WSAAsyncSelect` whenever something happens on the
/// Teams socket. lParam encodes the FD_* event in the low word and the
/// winsock error in the high word.
pub const WM_TEAMS_SOCKET: u32 = WM_USER + 2;

/// Posted to `msg_hwnd` by a `SetTimer` fire when it's time to retry the
/// WebSocket connection after a backoff.
pub const RECONNECT_TIMER: usize = 3;

const TEAMS_PORT: u16 = 8124;
const MAX_RECV_BUFFER: usize = 64 * 1024;
const MAX_BACKOFF_MS: u32 = 30_000;
const INITIAL_BACKOFF_MS: u32 = 1_000;
const APP_NAME: &str = "HotMic";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const MANUFACTURER: &str = "HotMic";
const DEVICE: &str = "PC";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// No socket open. We're either between connection attempts (waiting on
    /// the reconnect timer) or have not yet started.
    Idle,
    /// `connect()` returned `WSAEWOULDBLOCK`. Waiting for `FD_CONNECT`.
    Connecting,
    /// TCP up, HTTP upgrade request sent. Waiting for `101 Switching
    /// Protocols` and the empty CRLF terminator.
    Upgrading,
    /// HTTP upgrade complete. Receiving WebSocket frames.
    Open,
}

pub struct Client {
    msg_hwnd: HWND,
    socket: SOCKET,
    state: State,
    recv_buf: Vec<u8>,
    /// Last `meetingState.isMuted` we observed from a `meetingUpdate`.
    is_muted: bool,
    /// Last `meetingState.isInMeeting` we observed from a `meetingUpdate`.
    in_meeting: bool,
    /// True after the Local API has supplied a complete meeting state on this
    /// connection. Until then, UI Automation remains the fallback source.
    meeting_state_seen: bool,
    backoff_ms: u32,
    /// True between scheduling a reconnect and the timer firing. Prevents
    /// `schedule_reconnect()` from re-arming the timer (and re-doubling the
    /// backoff) when multiple FD_* events arrive for one logical disconnect
    /// (e.g. recv==0 followed by FD_CLOSE).
    reconnect_pending: bool,
    token: Option<String>,
    /// Set the moment a successful `connect()` is initiated; used to know
    /// whether `WSAStartup` needs cleaning up at shutdown.
    wsa_started: bool,
    /// Buffer for assembling a fragmented WebSocket text message. Empty when
    /// no fragmented message is in progress.
    fragment_buf: Vec<u8>,
    /// Opcode of the in-progress fragmented message (typically 0x1 for text).
    /// `None` when no fragmented message is in progress.
    fragment_opcode: Option<u8>,
    /// True once we've sent a `pair` action on the current connection. Reset
    /// in `close_socket`. Prevents spamming `pair` requests every time Teams
    /// re-broadcasts `meetingPermissions` with `canPair:true`.
    pair_sent: bool,
    /// Read-only fallback for Teams builds that no longer expose the Local API.
    ui_mute: Option<MuteDetector>,
}

impl Client {
    pub fn new(msg_hwnd: HWND) -> Self {
        let token = load_token();
        Self {
            msg_hwnd,
            socket: INVALID_SOCKET,
            state: State::Idle,
            recv_buf: Vec::with_capacity(4096),
            is_muted: false,
            in_meeting: false,
            meeting_state_seen: false,
            backoff_ms: INITIAL_BACKOFF_MS,
            reconnect_pending: false,
            token,
            wsa_started: false,
            fragment_buf: Vec::new(),
            fragment_opcode: None,
            pair_sent: false,
            ui_mute: MuteDetector::new().ok(),
        }
    }

    /// Resolve the current Teams mute state. A complete Local API state wins;
    /// otherwise the read-only UI Automation detector supplies the fallback
    /// while Teams holds the mic. Unknown fails safe to `false`.
    pub fn muted_now(&self, teams_active: bool) -> bool {
        let local_api_muted = (self.state == State::Open && self.meeting_state_seen)
            .then_some(self.in_meeting && self.is_muted);
        resolve_teams_muted(local_api_muted, teams_active, || {
            self.ui_mute.as_ref().and_then(MuteDetector::muted_now)
        })
    }

    /// Kick off the first connect attempt. Safe to call exactly once at
    /// startup. Subsequent reconnects go through `on_reconnect_timer()`.
    pub fn start(&mut self) {
        log_debug(&format!(
            "start: token_present={} ui_fallback_available={}",
            self.token.is_some(),
            self.ui_mute.is_some()
        ));
        self.ensure_wsa_started();
        self.try_connect();
    }

    /// Tear down the socket and Winsock. Call once before app exit.
    pub fn shutdown(&mut self) {
        self.close_socket(true);
        if self.wsa_started {
            unsafe {
                let _ = WSACleanup();
            }
            self.wsa_started = false;
        }
    }

    /// Called by `msg_wnd_proc` when `WM_TEAMS_SOCKET` arrives. Reads the
    /// FD_* event out of `lparam` and dispatches.
    pub fn on_socket_event(&mut self, _wparam: WPARAM, lparam: LPARAM) {
        let event = (lparam.0 as u32) & 0xFFFF;
        let err = ((lparam.0 as u32) >> 16) & 0xFFFF;
        log_debug(&format!(
            "socket_event: event=0x{:X} err={} state={:?}",
            event, err, self.state
        ));
        if err != 0 && event != FD_WRITE {
            log_debug("  -> error path, scheduling reconnect");
            self.schedule_reconnect();
            return;
        }
        match event {
            FD_CONNECT => self.on_connected(),
            FD_READ => self.on_read(),
            FD_CLOSE => {
                log_debug("  -> FD_CLOSE, scheduling reconnect");
                self.schedule_reconnect();
            }
            _ => {}
        }
    }

    /// Called by the message loop when the reconnect `SetTimer` fires.
    pub fn on_reconnect_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.msg_hwnd), RECONNECT_TIMER);
        }
        self.reconnect_pending = false;
        if self.state == State::Idle {
            self.try_connect();
        }
    }

    // -----------------------------------------------------------------------

    fn ensure_wsa_started(&mut self) {
        if self.wsa_started {
            return;
        }
        let mut data = WSADATA::default();
        let r = unsafe { WSAStartup(0x0202u16, &mut data) };
        if r == 0 {
            self.wsa_started = true;
        }
    }

    fn try_connect(&mut self) {
        // Retry WSAStartup on every connect attempt: if Winsock briefly fails
        // to initialize at app start, we still recover on the next reconnect
        // tick instead of being stuck dead forever.
        self.ensure_wsa_started();
        if !self.wsa_started {
            log_debug("try_connect: WSAStartup not ready, rescheduling");
            self.schedule_reconnect();
            return;
        }
        log_debug("try_connect: opening socket");

        let s = unsafe { socket(AF_INET.0.into(), SOCK_STREAM, 0) };
        let s = match s {
            Ok(s) if s != INVALID_SOCKET => s,
            _ => {
                log_debug(&format!("  -> socket() failed, err={}", unsafe {
                    WSAGetLastError().0
                }));
                self.schedule_reconnect();
                return;
            }
        };
        self.socket = s;
        self.recv_buf.clear();
        self.state = State::Connecting;

        // Route socket events as window messages on the existing message-loop
        // thread. This is the entire reason we don't need an extra thread or
        // a tokio runtime.
        let r = unsafe {
            WSAAsyncSelect(
                self.socket,
                self.msg_hwnd,
                WM_TEAMS_SOCKET,
                (FD_CONNECT | FD_READ | FD_WRITE | FD_CLOSE) as i32,
            )
        };
        if r == SOCKET_ERROR {
            self.close_socket(false);
            self.schedule_reconnect();
            return;
        }

        let addr = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port: TEAMS_PORT.to_be(),
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: 0x0100_007Fu32, // 127.0.0.1 in network byte order
                },
            },
            ..Default::default()
        };

        let r = unsafe {
            connect(
                self.socket,
                &addr as *const _ as *const SOCKADDR,
                std::mem::size_of::<SOCKADDR_IN>() as i32,
            )
        };
        if r == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            // WSAEWOULDBLOCK is the expected case for an async socket: the
            // FD_CONNECT message will tell us when it's actually connected.
            if err != WSAEWOULDBLOCK {
                log_debug(&format!("  -> connect() failed sync, err={}", err.0));
                self.close_socket(false);
                self.schedule_reconnect();
            } else {
                log_debug("  -> connect() returned WSAEWOULDBLOCK (expected)");
            }
        } else {
            log_debug("  -> connect() returned 0 (immediate)");
        }
    }

    fn on_connected(&mut self) {
        // Send the HTTP upgrade. Teams accepts a small set of identity query
        // parameters and a token if we have one from a prior pair.
        let key_bytes = generate_key();
        let key = base64_encode(&key_bytes);
        let host = format!("localhost:{TEAMS_PORT}");
        let path = build_pair_path(self.token.as_deref());
        log_debug(&format!(
            "on_connected: sending upgrade for path={}",
            redact_secrets(&path)
        ));
        let req = ws_handshake_request(&host, &path, &key);

        if !self.send_all(req.as_bytes()) {
            log_debug("  -> send_all(handshake) failed");
            self.close_socket(false);
            self.schedule_reconnect();
            return;
        }
        log_debug(&format!("  -> upgrade sent ({} bytes)", req.len()));
        self.state = State::Upgrading;
    }

    fn on_read(&mut self) {
        // Drain everything currently available so we don't get stuck if FD_READ
        // is edge-triggered for a particular byte boundary.
        loop {
            let before = self.recv_buf.len();
            if before >= MAX_RECV_BUFFER {
                // Almost certainly a protocol bug or runaway message; cycle
                // the connection rather than grow without bound.
                self.close_socket(false);
                self.schedule_reconnect();
                return;
            }
            let mut tmp = [0u8; 4096];
            let n = unsafe { recv(self.socket, &mut tmp, SEND_RECV_FLAGS(0)) };
            if n > 0 {
                log_debug(&format!(
                    "recv: {} bytes (state={:?}, buf_total={})",
                    n,
                    self.state,
                    self.recv_buf.len() + n as usize
                ));
                self.recv_buf.extend_from_slice(&tmp[..n as usize]);
            } else if n == 0 {
                // Peer closed.
                log_debug("recv: 0 (peer closed)");
                self.schedule_reconnect();
                return;
            } else {
                let err = unsafe { WSAGetLastError() };
                if err == WSAEWOULDBLOCK {
                    break;
                }
                log_debug(&format!("recv: error {}", err.0));
                self.schedule_reconnect();
                return;
            }
        }

        if self.state == State::Upgrading && !self.try_finish_upgrade() {
            return;
        }
        if self.state == State::Open {
            self.drain_frames();
        }
    }

    fn try_finish_upgrade(&mut self) -> bool {
        // RFC 7230: HTTP headers end at the first CRLF CRLF.
        let term = b"\r\n\r\n";
        let Some(pos) = find_subslice(&self.recv_buf, term) else {
            return false;
        };
        let headers = &self.recv_buf[..pos];
        let head_str = std::str::from_utf8(headers).unwrap_or("");
        log_debug(&format!(
            "upgrade response head ({} bytes):\n{}",
            pos, head_str
        ));
        // Loose check: must be HTTP/1.1 101.
        let first_line = head_str.lines().next().unwrap_or("");
        if !first_line.starts_with("HTTP/1.1 101") {
            log_debug(&format!(
                "  -> upgrade rejected (first_line={:?})",
                first_line
            ));
            self.close_socket(false);
            self.schedule_reconnect();
            return false;
        }
        // Drop the HTTP head from the buffer; what's left (if any) is the
        // start of the WebSocket stream.
        self.recv_buf.drain(..pos + term.len());
        self.state = State::Open;
        log_debug("  -> upgrade OK, state=Open");
        // Successful handshake — reset backoff so a future drop gets the
        // fast initial retry again.
        self.backoff_ms = INITIAL_BACKOFF_MS;
        true
    }

    fn drain_frames(&mut self) {
        loop {
            let parse = ws_parse_frame(&self.recv_buf);
            match parse {
                WsFrameParse::Need(_) => return,
                WsFrameParse::Error => {
                    self.close_socket(false);
                    self.schedule_reconnect();
                    return;
                }
                WsFrameParse::Frame {
                    fin,
                    opcode,
                    payload,
                    consumed,
                } => {
                    self.recv_buf.drain(..consumed);
                    self.handle_frame(fin, opcode, &payload);
                }
            }
        }
    }

    fn handle_frame(&mut self, fin: bool, opcode: u8, payload: &[u8]) {
        match opcode {
            0x0 => {
                // Continuation of a previously-fragmented data frame.
                let Some(start_op) = self.fragment_opcode else {
                    // Continuation without a prior fragmented frame: protocol
                    // error — cycle the connection.
                    self.schedule_reconnect();
                    return;
                };
                if self.fragment_buf.len() + payload.len() > MAX_RECV_BUFFER {
                    self.schedule_reconnect();
                    return;
                }
                self.fragment_buf.extend_from_slice(payload);
                if fin {
                    let assembled = std::mem::take(&mut self.fragment_buf);
                    self.fragment_opcode = None;
                    self.dispatch_data(start_op, &assembled);
                }
            }
            0x1 | 0x2 => {
                if fin {
                    self.dispatch_data(opcode, payload);
                } else {
                    // Start of a fragmented data message.
                    self.fragment_buf.clear();
                    self.fragment_buf.extend_from_slice(payload);
                    self.fragment_opcode = Some(opcode);
                }
            }
            0x9 => {
                // Ping → reply with pong carrying the same payload.
                let mask = generate_mask();
                let mut out = Vec::with_capacity(payload.len() + 14);
                // Build a pong manually: FIN=1, opcode=0xA, masked.
                out.push(0x80 | 0x0A);
                if payload.len() < 126 {
                    out.push(0x80 | payload.len() as u8);
                } else if payload.len() <= u16::MAX as usize {
                    out.push(0x80 | 126);
                    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                } else {
                    out.push(0x80 | 127);
                    out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
                }
                out.extend_from_slice(&mask);
                for (i, b) in payload.iter().enumerate() {
                    out.push(b ^ mask[i & 3]);
                }
                let _ = self.send_all(&out);
            }
            0x8 => {
                // Close. Acknowledge and tear down.
                let _ = self.send_all(&ws_build_close_frame(generate_mask()));
                self.schedule_reconnect();
            }
            _ => {
                // Pong (0xA) and any unknown opcode — nothing to do.
            }
        }
    }

    fn dispatch_data(&mut self, opcode: u8, payload: &[u8]) {
        if opcode != 0x1 {
            // Binary frame: Teams Local API doesn't send these.
            return;
        }
        if let Ok(text) = std::str::from_utf8(payload) {
            self.handle_text(text);
        }
    }

    fn handle_text(&mut self, text: &str) {
        // Teams sends two relevant message shapes:
        //   {"tokenRefresh":"<uuid>"}                  — store this for next pair
        //   {"meetingUpdate":{"meetingState":...,...}} — current call state
        let preview: String = text.chars().take(400).collect();
        log_debug(&format!(
            "text frame ({} bytes): {}",
            text.len(),
            redact_secrets(&preview)
        ));
        if let Some(tok) = extract_token_refresh(text) {
            log_debug("  -> tokenRefresh received");
            self.token = Some(tok.clone());
            let saved = save_token(&tok);
            log_debug(&format!("  -> save_token: {}", saved));
        }
        let m = parse_meeting_update(text);
        log_debug(&format!(
            "  -> parsed: in_meeting={:?} is_muted={:?}",
            m.in_meeting, m.is_muted
        ));
        if let Some(b) = m.is_muted {
            self.is_muted = b;
        }
        if let Some(b) = m.in_meeting {
            self.in_meeting = b;
            // Leaving a meeting clears the mute flag automatically: a fresh
            // meetingState will reassert isMuted on the next call.
            if !b {
                self.is_muted = false;
            }
        }
        if m.in_meeting.is_some() && m.is_muted.is_some() {
            self.meeting_state_seen = true;
        }
        log_debug(&format!(
            "  -> client state: in_meeting={} is_muted={}",
            self.in_meeting, self.is_muted
        ));
        // (Re-)pairing: when Teams advertises canPair:true, it's explicitly
        // inviting us to pair. On a fresh client this triggers the Allow
        // toast; on a client with a stale/revoked token (user removed us in
        // Teams Privacy settings, reinstalled Teams, signed in as a different
        // account, etc.) Teams sends the same signal, and we need to discard
        // the stored token and pair from scratch — otherwise we stay
        // unauthorized forever and the user has no signal to manually delete
        // the token file. Without this action, modern Teams (2.0/MSIX) never
        // pushes meetingState, so we never see isMuted.
        if !self.pair_sent && parse_can_pair(text) == Some(true) {
            if self.token.is_some() {
                log_debug("  -> canPair:true with token in hand; treating as stale, clearing");
                self.token = None;
                delete_token();
            }
            self.send_pair_action();
        }
        // Nudge the message loop to recompute the border. This posts a
        // backstop-timer-equivalent message rather than reaching into App's
        // borrow_mut() from inside our own borrow_mut().
        unsafe {
            let _ = PostMessageW(
                Some(self.msg_hwnd),
                WM_TEAMS_STATE_CHANGED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    fn send_pair_action(&mut self) {
        // Single fixed payload — we never send any other action. The token
        // Teams returns nominally grants write access, but we deliberately
        // expose no API to use it.
        const PAIR_JSON: &[u8] = br#"{"action":"pair","parameters":{},"requestId":1}"#;
        let mask = generate_mask();
        let frame = ws_build_text_frame(PAIR_JSON, mask);
        log_debug("  -> sending pair action");
        if self.send_all(&frame) {
            self.pair_sent = true;
            log_debug("  -> pair action sent");
        } else {
            // Pair is the single most consequential send in the client (no
            // pair → no Allow toast → no `meetingState` ever). A 47-byte
            // loopback write that fails means the socket is broken; don't
            // wait for another `canPair:true` push that may never come —
            // cycle the connection and let the next session retry.
            log_debug("  -> pair action send failed; cycling connection");
            self.schedule_reconnect();
        }
    }

    fn send_all(&self, bytes: &[u8]) -> bool {
        let mut sent = 0usize;
        while sent < bytes.len() {
            let r = unsafe { send(self.socket, &bytes[sent..], SEND_RECV_FLAGS(0)) };
            if r == SOCKET_ERROR {
                let err = unsafe { WSAGetLastError() };
                if err == WSAEWOULDBLOCK {
                    // Would block — yield to the message loop. For the small
                    // payloads we ever send (HTTP upgrade ~300 bytes, pong/close
                    // <16 bytes), this is exceedingly rare on loopback. Treat
                    // as fatal for this connection rather than block.
                    return false;
                }
                return false;
            }
            sent += r as usize;
        }
        true
    }

    fn close_socket(&mut self, graceful: bool) {
        if self.socket != INVALID_SOCKET {
            if graceful && self.state == State::Open {
                let _ = self.send_all(&ws_build_close_frame(generate_mask()));
            }
            unsafe {
                let _ = closesocket(self.socket);
            }
            self.socket = INVALID_SOCKET;
        }
        self.state = State::Idle;
        self.recv_buf.clear();
        // Discard any partial fragmented message; the next connection starts
        // clean.
        self.fragment_buf.clear();
        self.fragment_opcode = None;
        // Lose the live mute signal as soon as the socket goes down so the
        // detection rule degrades immediately.
        self.is_muted = false;
        self.in_meeting = false;
        self.meeting_state_seen = false;
        // The pair handshake is per-connection: each new socket needs to
        // re-send the action if Teams asks (canPair:true and no token).
        self.pair_sent = false;
    }

    fn schedule_reconnect(&mut self) {
        self.close_socket(false);
        // Idempotent: if a reconnect timer is already armed for this logical
        // disconnect, don't re-arm or double the backoff. Multiple FD_*
        // events (recv==0, recv error, FD_CLOSE) commonly fire for one drop.
        if self.reconnect_pending {
            return;
        }
        let delay = self.backoff_ms;
        self.backoff_ms = (self.backoff_ms.saturating_mul(2)).min(MAX_BACKOFF_MS);
        self.reconnect_pending = true;
        log_debug(&format!(
            "schedule_reconnect: delay={}ms, next_backoff={}ms",
            delay, self.backoff_ms
        ));
        unsafe {
            let _ = SetTimer(Some(self.msg_hwnd), RECONNECT_TIMER, delay, None);
        }
    }
}

/// Posted to `msg_hwnd` by `Client` when the Teams mute state may have
/// changed. The message loop responds by re-running `apply_state` against the
/// current registry scan + the new Teams state.
pub const WM_TEAMS_STATE_CHANGED: u32 = WM_USER + 3;

fn build_pair_path(token: Option<&str>) -> String {
    let token_qs = match token {
        Some(t) if !t.is_empty() => format!("&token={t}"),
        _ => String::new(),
    };
    format!(
        "/?protocol-version=2.0.0&manufacturer={MANUFACTURER}&device={DEVICE}&app={APP_NAME}&app-version={APP_VERSION}{token_qs}"
    )
}

fn extract_token_refresh(json: &str) -> Option<String> {
    let key = "\"tokenRefresh\"";
    let idx = json.find(key)?;
    let after = &json[idx + key.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn generate_key() -> [u8; 16] {
    let mut buf = [0u8; 16];
    let r = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if r.is_err() {
        // BCryptGenRandom shouldn't fail on a healthy system, but if it does,
        // a deterministic placeholder is still valid framing — Teams doesn't
        // verify Sec-WebSocket-Key cryptographically and per-process freshness
        // is good enough for our threat model.
        for (i, b) in buf.iter_mut().enumerate() {
            *b = i as u8;
        }
    }
    buf
}

fn generate_mask() -> [u8; 4] {
    let mut buf = [0u8; 4];
    let _ = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    buf
}

// ---------------------------------------------------------------------------
// Diagnostic log at %LOCALAPPDATA%\HotMic\teams-debug.log
//
// Off by default to keep production runs from touching disk. Enable for
// troubleshooting by setting the `HOTMIC_TEAMS_DEBUG` environment variable
// to any non-empty value before launching `hotmic.exe`.
// ---------------------------------------------------------------------------

fn log_debug(msg: &str) {
    if env::var_os("HOTMIC_TEAMS_DEBUG").is_none() {
        return;
    }
    let Some(path) = log_path() else { return };
    // Cap at ~256 KB by truncating when oversized: small enough to keep the
    // log human-skimmable, large enough to capture an entire repro session.
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = fs::write(&path, b"-- log truncated --\n");
        }
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{now:>14}] {msg}");
    }
}

fn log_path() -> Option<PathBuf> {
    let local = env::var("LOCALAPPDATA").ok()?;
    let dir = PathBuf::from(local).join("HotMic");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join("teams-debug.log"))
}

// ---------------------------------------------------------------------------
// DPAPI-protected token storage at %LOCALAPPDATA%\HotMic\teams.token
// ---------------------------------------------------------------------------

fn token_path() -> Option<PathBuf> {
    let local = env::var("LOCALAPPDATA").ok()?;
    let dir = PathBuf::from(local).join("HotMic");
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join("teams.token"))
}

pub fn load_token() -> Option<String> {
    let path = token_path()?;
    let blob = fs::read(&path).ok()?;
    if blob.is_empty() {
        return None;
    }

    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let r = unsafe {
        CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if r.is_err() || out_blob.pbData.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) };
    let token = String::from_utf8(bytes.to_vec()).ok();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out_blob.pbData as *mut c_void)));
    }
    token
}

pub fn save_token(token: &str) -> bool {
    let Some(path) = token_path() else {
        return false;
    };

    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: token.len() as u32,
        pbData: token.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let r = unsafe {
        CryptProtectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if r.is_err() || out_blob.pbData.is_null() {
        return false;
    }
    let encrypted =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) };

    // Write atomically: temp file + rename, so a crash mid-write leaves the
    // existing token intact rather than half-overwritten.
    let tmp = path.with_extension("token.tmp");
    let ok = (|| {
        let mut f = fs::File::create(&tmp).ok()?;
        f.write_all(encrypted).ok()?;
        f.sync_all().ok()?;
        fs::rename(&tmp, &path).ok()?;
        Some(())
    })()
    .is_some();

    unsafe {
        let _ = LocalFree(Some(HLOCAL(out_blob.pbData as *mut c_void)));
    }
    ok
}

/// Removes the persisted token file. Best-effort: errors (missing file, ACL,
/// AV quarantine) are swallowed because the caller's recovery path doesn't
/// depend on the file actually being gone — only on `self.token` being `None`.
fn delete_token() -> bool {
    let Some(path) = token_path() else {
        return false;
    };
    fs::remove_file(&path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_path_without_token() {
        let p = build_pair_path(None);
        assert!(p.starts_with("/?protocol-version=2.0.0&manufacturer=HotMic"));
        assert!(!p.contains("token="));
    }

    #[test]
    fn pair_path_with_token() {
        let p = build_pair_path(Some("deadbeef-1234"));
        assert!(p.contains("&token=deadbeef-1234"));
    }

    #[test]
    fn pair_path_empty_token_omits_param() {
        let p = build_pair_path(Some(""));
        assert!(!p.contains("token="));
    }

    #[test]
    fn token_refresh_parsed_from_real_payload() {
        let s = r#"{"tokenRefresh":"abc-123-def","extra":"ignored"}"#;
        assert_eq!(extract_token_refresh(s).as_deref(), Some("abc-123-def"));
    }

    #[test]
    fn token_refresh_returns_none_when_absent() {
        let s = r#"{"meetingUpdate":{"meetingState":{"isMuted":true}}}"#;
        assert_eq!(extract_token_refresh(s), None);
    }

    #[test]
    fn token_refresh_handles_whitespace() {
        let s = r#"{"tokenRefresh"   :   "xyz"}"#;
        assert_eq!(extract_token_refresh(s).as_deref(), Some("xyz"));
    }

    #[test]
    fn find_subslice_finds_crlf_terminator() {
        let buf = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\nbody";
        let pos = find_subslice(buf, b"\r\n\r\n").unwrap();
        assert_eq!(&buf[pos + 4..], b"body");
    }

    #[test]
    fn find_subslice_returns_none_when_absent() {
        assert_eq!(find_subslice(b"no terminator here", b"\r\n\r\n"), None);
        assert_eq!(find_subslice(b"", b"\r\n"), None);
    }
}
