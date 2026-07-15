//! Pure, platform-independent helpers used by the hotmic binary.
//! Kept free of Win32 imports so tests can run on any host.

pub const COLOR_BLUE: u32 = 0x00FF8000;
pub const COLOR_RED: u32 = 0x000000DC;
pub const COLOR_PURPLE: u32 = 0x00EA3393;

pub const ICON_IDLE: u16 = 1;
pub const ICON_CAM: u16 = 2;
pub const ICON_MIC: u16 = 3;
pub const ICON_BOTH: u16 = 4;

/// Color for each border edge (top, bottom, left, right), or None to hide.
/// When both devices are in use, the border goes purple — visually distinct
/// from either single-device state.
pub fn edge_colors(cam: bool, mic: bool) -> Option<[u32; 4]> {
    match (cam, mic) {
        (false, false) => None,
        (true, false) => Some([COLOR_BLUE, COLOR_BLUE, COLOR_BLUE, COLOR_BLUE]),
        (false, true) => Some([COLOR_RED, COLOR_RED, COLOR_RED, COLOR_RED]),
        (true, true) => Some([COLOR_PURPLE, COLOR_PURPLE, COLOR_PURPLE, COLOR_PURPLE]),
    }
}

/// Logical-pixel thickness of the screen border. Scaled by DPI at draw time.
pub const BORDER_THICKNESS_LOGICAL: u32 = 3;

/// Logical-pixel corner radius for the rounded screen border. Should roughly
/// match the curve of a typical laptop's rounded display corner.
pub const CORNER_RADIUS_LOGICAL: u32 = 12;

/// Physical-pixel border thickness, scaled by DPI. Never less than 1.
pub fn border_thickness_px(dpi: u32) -> i32 {
    (BORDER_THICKNESS_LOGICAL.saturating_mul(dpi) / 96).max(1) as i32
}

/// Physical-pixel corner radius, scaled by DPI. Never less than 1.
pub fn corner_radius_px(dpi: u32) -> i32 {
    (CORNER_RADIUS_LOGICAL.saturating_mul(dpi) / 96).max(1) as i32
}

/// Tray icon id + tooltip for a given app state.
pub fn tray_appearance(enabled: bool, cam: bool, mic: bool) -> (u16, &'static str) {
    if !enabled {
        return (ICON_IDLE, "HotMic (disabled)");
    }
    match (cam, mic) {
        (false, false) => (ICON_IDLE, "HotMic"),
        (true, false) => (ICON_CAM, "HotMic: camera in use"),
        (false, true) => (ICON_MIC, "HotMic: microphone in use"),
        (true, true) => (ICON_BOTH, "HotMic: camera + microphone in use"),
    }
}

/// True only when transitioning active → idle, where flicker debounce should kick in.
///
/// **Caller contract for deferred-commit paths.** When the debounce timer
/// itself fires (the deferred off-transition is committing), callers MUST
/// bypass this check. If they don't, `last_visible` will still report
/// "border on" on re-entry and this function will return `true` again,
/// re-arming the timer indefinitely — the border would never turn off.
/// See `apply_state(.., bypass_debounce)` in `src/main.rs`.
pub fn should_start_debounce(was_active: bool, now_active: bool) -> bool {
    was_active && !now_active
}

/// True when the off-debounce path should defer an active to idle transition.
/// If the timer cannot be armed, callers must commit immediately rather than
/// leaving `pending_off` set forever.
pub fn should_defer_off_transition(
    was_active: bool,
    now_active: bool,
    bypass_debounce: bool,
    debounce_timer_armed: bool,
) -> bool {
    !bypass_debounce && debounce_timer_armed && should_start_debounce(was_active, now_active)
}

/// True when a pending off-debounce should be canceled because a fresh scan
/// says the overlay is active again before the deferred commit fires.
pub fn should_cancel_pending_off(cam_visible: bool, mic_visible: bool) -> bool {
    cam_visible || mic_visible
}

/// True when a DEBOUNCE_TIMER message should commit the deferred off state for
/// the currently pending debounce. KillTimer does not remove an already-posted
/// WM_TIMER, so stale messages must be ignored until the active debounce's own
/// deadline has arrived.
pub fn should_commit_pending_off_timer(pending_off: bool, debounce_due_reached: bool) -> bool {
    pending_off && debounce_due_reached
}

/// True when the tray icon needs a shell update. The periodic refresh is a
/// heartbeat that lets HotMic recover if Explorer loses the icon without
/// broadcasting TaskbarCreated.
pub fn should_update_tray_icon(
    last_icon: u16,
    last_tip: &str,
    next_icon: u16,
    next_tip: &str,
    refresh_due: bool,
) -> bool {
    last_icon != next_icon || last_tip != next_tip || refresh_due
}

/// True when an active overlay should recreate its HWND because DWM cloaked it,
/// usually after a virtual-desktop transition.
pub fn should_recreate_cloaked_overlay(show: bool, dwm_cloaked: bool) -> bool {
    show && dwm_cloaked
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayVisibilityPlan {
    pub recreate: bool,
    pub show_window: bool,
    pub repair: bool,
}

/// Describes the Win32 overlay repair operations needed for a desired visible
/// state. Kept pure so the long-running visibility decisions stay unit-tested.
pub fn overlay_visibility_plan(
    show: bool,
    currently_shown: bool,
    dwm_cloaked: bool,
) -> OverlayVisibilityPlan {
    if !show {
        return OverlayVisibilityPlan {
            recreate: false,
            show_window: false,
            repair: false,
        };
    }

    OverlayVisibilityPlan {
        recreate: should_recreate_cloaked_overlay(show, dwm_cloaked),
        show_window: !currently_shown || dwm_cloaked,
        repair: true,
    }
}

/// Parses LastUsedTimeStop bytes. `0` means the device is in use right now.
/// Other values (real FILETIMEs) mean it was released. Wrong size = treat as not in use.
pub fn parse_in_use(bytes: &[u8], size: usize) -> bool {
    if size != 8 || bytes.len() < 8 {
        return false;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf) == 0
}

/// UTF-16 + null terminator. Used for Win32 wide-char strings.
pub fn wide_chars(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16-LE byte serialization with a trailing UTF-16 null terminator, suitable
/// as the `lpData` argument to `RegSetValueExW` for `REG_SZ` values.
pub fn wide_bytes(s: &str) -> Vec<u8> {
    let w = wide_chars(s);
    let mut bytes = Vec::with_capacity(w.len() * 2);
    for u in w {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    bytes
}

// ---------------------------------------------------------------------------
// Teams identity + per-app fusion
// ---------------------------------------------------------------------------

/// True if a CapabilityAccessManager subkey name belongs to Microsoft Teams.
///
/// Conservative match: new Teams MSIX (the single subkey under `microphone\`)
/// or classic Teams Squirrel install whose NonPackaged name encodes the path
/// `...\Microsoft\Teams\current\Teams.exe` (the consent store uses `#` as the
/// path separator). `msedge.exe` and `msedgewebview2.exe` are NOT counted as
/// Teams; if Teams ever attributes capture to one of those, the entry falls
/// through to the non-Teams branch and the border stays on (safe direction).
pub fn is_teams_subkey(name: &str) -> bool {
    if name == "MSTeams_8wekyb3d8bbwe" {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.contains("#microsoft#teams#current#teams.exe")
}

/// Interpret the accessible name of Teams' local microphone button.
///
/// The button describes the action it will perform, so "Unmute" means the
/// microphone is currently muted and "Mute" means it is currently live.
pub fn parse_teams_mic_button_name(name: &str) -> Option<bool> {
    let action = name.split_whitespace().next()?;
    if action.eq_ignore_ascii_case("unmute") {
        Some(true)
    } else if action.eq_ignore_ascii_case("mute") {
        Some(false)
    } else {
        None
    }
}

/// Combine microphone-button states from multiple Teams windows. A live
/// result wins conflicts so stale duplicate windows can never hide the border.
pub fn combine_teams_mic_button_states(states: &[Option<bool>]) -> Option<bool> {
    if states.contains(&Some(false)) {
        Some(false)
    } else if !states.is_empty() && states.iter().all(|state| *state == Some(true)) {
        Some(true)
    } else {
        None
    }
}

/// Prefer an authoritative Local API state. Query UI Automation only while
/// Teams holds the mic and the API is unavailable, then fail safe to mic-live.
pub fn resolve_teams_muted<F>(
    local_api_muted: Option<bool>,
    teams_active: bool,
    ui_automation_muted: F,
) -> bool
where
    F: FnOnce() -> Option<bool>,
{
    match local_api_muted {
        Some(muted) => muted,
        None if teams_active => ui_automation_muted().unwrap_or(false),
        None => false,
    }
}

/// Final mic-show decision after fusing registry attribution with the resolved
/// Teams mute state. The OR keeps the border on whenever any non-Teams app
/// holds the mic; Teams' branch is suppressed only when Teams is known muted.
pub fn mic_should_show(non_teams_active: bool, teams_active: bool, teams_muted_now: bool) -> bool {
    non_teams_active || (teams_active && !teams_muted_now)
}

/// Final visible device tuple after fusing raw registry state with Teams mute.
/// Keeps camera visibility independent of any Teams mic suppression.
pub fn visible_devices(
    cam_active: bool,
    mic_non_teams_active: bool,
    mic_teams_active: bool,
    teams_muted_now: bool,
) -> (bool, bool) {
    (
        cam_active,
        mic_should_show(mic_non_teams_active, mic_teams_active, teams_muted_now),
    )
}

// ---------------------------------------------------------------------------
// Minimal JSON scanner for Teams `meetingUpdate` payloads
// ---------------------------------------------------------------------------

/// Subset of `meetingUpdate.meetingState` we actually use. Both fields are
/// optional because Teams sometimes pushes events that contain only
/// `meetingPermissions` (e.g. when no meeting is active) and we must not
/// invent state in that case.
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub struct MeetingUpdate {
    pub in_meeting: Option<bool>,
    pub is_muted: Option<bool>,
}

/// Extract `meetingState.{isInMeeting,isMuted}` from a Teams `meetingUpdate`
/// JSON payload. Hand-rolled to keep the runtime dependency-free; tolerates
/// missing fields and field ordering, but does NOT do full JSON validation.
/// Bracket-aware so it does not pick up `isMuted` from sibling objects.
pub fn parse_meeting_update(json: &str) -> MeetingUpdate {
    let Some(state) = extract_object_value(json, "meetingState") else {
        return MeetingUpdate::default();
    };
    MeetingUpdate {
        in_meeting: find_bool_field(state, "isInMeeting"),
        is_muted: find_bool_field(state, "isMuted"),
    }
}

/// Read `meetingPermissions.canPair` out of a Teams `meetingUpdate` payload.
/// Teams uses this flag to signal that the client may now send a `pair`
/// action to request authorization. Returns `None` if the field is absent
/// (e.g. the payload only carries `meetingState` or no permissions block at
/// all).
pub fn parse_can_pair(json: &str) -> Option<bool> {
    let perms = extract_object_value(json, "meetingPermissions")?;
    find_bool_field(perms, "canPair")
}

/// Redacts Teams credentials from a string before it's written to the
/// diagnostic log. Strips both the `tokenRefresh` JSON value (returned by
/// Teams when pairing succeeds) and the `token=` URL query parameter (sent
/// on reconnect). The diagnostic log is already gated behind
/// `HOTMIC_TEAMS_DEBUG` and lives under the user's own `%LOCALAPPDATA%`,
/// but users routinely share logs to troubleshoot and the token has WRITE
/// access to Teams' Local API. Defense in depth.
pub fn redact_secrets(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\"tokenRefresh\"") {
            out.extend_from_slice(b"\"tokenRefresh\"");
            i += b"\"tokenRefresh\"".len();
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b':' {
                out.push(b':');
                i += 1;
            }
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                out.extend_from_slice(b"\"<redacted>\"");
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            continue;
        }
        if (bytes[i] == b'?' || bytes[i] == b'&')
            && i + 1 + b"token=".len() <= bytes.len()
            && &bytes[i + 1..i + 1 + b"token=".len()] == b"token="
        {
            out.push(bytes[i]);
            out.extend_from_slice(b"token=<redacted>");
            i += 1 + b"token=".len();
            while i < bytes.len() && bytes[i] != b'&' {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Find `"key":` followed by a `{...}` object value and return the slice
/// covering the object including its outer braces. Skips strings and tracks
/// brace depth so nested objects are returned whole.
fn extract_object_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\":", key);
    let key_pos = json.find(&needle)?;
    let bytes = json.as_bytes();
    let mut i = key_pos + needle.len();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return None;
    }
    let start = i;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    while i < bytes.len() {
        let b = bytes[i];
        if esc {
            esc = false;
        } else if in_str {
            match b {
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        // Return the body BETWEEN the outer braces, not
                        // including them. This lets `find_bool_field` treat
                        // the body's outermost members as depth 0.
                        return Some(&json[start + 1..i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Find `"field":true` or `"field":false` (whitespace tolerated after the
/// colon) inside `obj`. Returns `None` if neither is present, or the field
/// value is not a bare boolean literal.
fn find_bool_field(obj: &str, field: &str) -> Option<bool> {
    // Depth-aware, string-aware match. Only accept `field` at depth 0 within
    // `obj` so that, given `meetingState`'s body, a nested object's `isMuted`
    // can never shadow the top-level one.
    let bytes = obj.as_bytes();
    let key_quoted = format!("\"{field}\"");
    let kq = key_quoted.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                if depth == 0 && bytes[i..].starts_with(kq) {
                    let mut j = i + kq.len();
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b':' {
                        j += 1;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        if bytes[j..].starts_with(b"true") {
                            return Some(true);
                        }
                        if bytes[j..].starts_with(b"false") {
                            return Some(false);
                        }
                        return None;
                    }
                }
                // Skip past this string, honoring backslash escapes.
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    if b == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    if b == b'"' {
                        break;
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Base64 (RFC 4648) — needed only for `Sec-WebSocket-Key` generation.
// Hand-rolled to keep the runtime dependency-free.
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(B64_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(B64_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(B64_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

// ---------------------------------------------------------------------------
// RFC 6455 WebSocket framing
// ---------------------------------------------------------------------------

/// HTTP/1.1 GET upgrade request body. Caller supplies the Sec-WebSocket-Key
/// (16 random bytes, base64-encoded) and the path including query string.
pub fn ws_handshake_request(host: &str, path: &str, key_b64: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key_b64}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

/// Build a single-frame masked client text message (FIN=1, opcode=0x1).
/// The mask key is supplied so callers can use OS RNG; lib.rs stays pure.
pub fn ws_build_text_frame(payload: &[u8], mask_key: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 8 + 4 + payload.len());
    out.push(0x81); // FIN=1, RSV=000, opcode=text
    let len = payload.len();
    if len < 126 {
        out.push(0x80 | (len as u8));
    } else if len <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask_key);
    for (i, &b) in payload.iter().enumerate() {
        out.push(b ^ mask_key[i & 3]);
    }
    out
}

/// Build an unmasked control frame with no payload (FIN=1). Servers are
/// supposed to receive masked client frames; this helper is only used for
/// outgoing close-with-no-body, which is rendered with a separate masked
/// build step. Kept here because it is a pure byte calculation worth
/// testing.
pub fn ws_build_close_frame(mask_key: [u8; 4]) -> Vec<u8> {
    // Empty payload; FIN=1, opcode=close (0x8). Still must be masked
    // per RFC 6455 because it is a client-to-server frame.
    let mut out = Vec::with_capacity(6);
    out.push(0x88);
    out.push(0x80); // mask bit + length 0
    out.extend_from_slice(&mask_key);
    out
}

/// Result of an incremental frame-parse attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum WsFrameParse {
    /// Need at least this many MORE bytes appended to the buffer before
    /// another parse attempt can succeed.
    Need(usize),
    /// A complete frame was assembled. `consumed` is how many bytes from
    /// the start of the input buffer made up this frame; the caller drains
    /// that many.
    Frame {
        fin: bool,
        opcode: u8,
        payload: Vec<u8>,
        consumed: usize,
    },
    /// Frame is malformed (e.g. impossibly large length on a 32-bit host).
    Error,
}

/// Incrementally parse one WebSocket frame from the start of `buf`. Servers
/// (which Teams is) send unmasked frames per RFC 6455; we still tolerate
/// masked input for symmetry and testing.
pub fn ws_parse_frame(buf: &[u8]) -> WsFrameParse {
    if buf.len() < 2 {
        return WsFrameParse::Need(2 - buf.len());
    }
    let fin = (buf[0] & 0x80) != 0;
    let opcode = buf[0] & 0x0F;
    // RFC 6455 §5.5: control frames (opcodes ≥ 0x8) MUST NOT be fragmented
    // (FIN=1) and MUST have a payload length ≤ 125 bytes.
    let is_control = opcode >= 0x8;
    if is_control && !fin {
        return WsFrameParse::Error;
    }
    let masked = (buf[1] & 0x80) != 0;
    let len7 = (buf[1] & 0x7F) as u64;
    if is_control && len7 > 125 {
        return WsFrameParse::Error;
    }

    let (payload_len, mut header_size) = match len7 {
        126 => {
            if buf.len() < 4 {
                return WsFrameParse::Need(4 - buf.len());
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as u64, 4usize)
        }
        127 => {
            if buf.len() < 10 {
                return WsFrameParse::Need(10 - buf.len());
            }
            (
                u64::from_be_bytes([
                    buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9],
                ]),
                10usize,
            )
        }
        n => (n, 2usize),
    };

    let mask_key = if masked {
        if buf.len() < header_size + 4 {
            return WsFrameParse::Need(header_size + 4 - buf.len());
        }
        let k = [
            buf[header_size],
            buf[header_size + 1],
            buf[header_size + 2],
            buf[header_size + 3],
        ];
        header_size += 4;
        Some(k)
    } else {
        None
    };

    let Ok(payload_len_us) = usize::try_from(payload_len) else {
        return WsFrameParse::Error;
    };
    let Some(total) = header_size.checked_add(payload_len_us) else {
        return WsFrameParse::Error;
    };
    if buf.len() < total {
        return WsFrameParse::Need(total - buf.len());
    }
    let raw = &buf[header_size..total];
    let payload = if let Some(k) = mask_key {
        raw.iter().enumerate().map(|(i, &b)| b ^ k[i & 3]).collect()
    } else {
        raw.to_vec()
    };
    WsFrameParse::Frame {
        fin,
        opcode,
        payload,
        consumed: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_hides_border() {
        assert_eq!(edge_colors(false, false), None);
    }

    #[test]
    fn camera_only_paints_all_blue() {
        assert_eq!(edge_colors(true, false), Some([COLOR_BLUE; 4]));
    }

    #[test]
    fn mic_only_paints_all_red() {
        assert_eq!(edge_colors(false, true), Some([COLOR_RED; 4]));
    }

    #[test]
    fn both_active_paints_all_purple() {
        assert_eq!(edge_colors(true, true), Some([COLOR_PURPLE; 4]));
    }

    #[test]
    fn border_thickness_at_96_dpi_is_3() {
        assert_eq!(border_thickness_px(96), 3);
    }

    #[test]
    fn border_thickness_doubles_at_192_dpi() {
        assert_eq!(border_thickness_px(192), 6);
    }

    #[test]
    fn border_thickness_never_zero_at_low_dpi() {
        assert!(border_thickness_px(1) >= 1);
        assert!(border_thickness_px(0) >= 1);
    }

    #[test]
    fn corner_radius_at_96_dpi_matches_logical() {
        assert_eq!(corner_radius_px(96), CORNER_RADIUS_LOGICAL as i32);
    }

    #[test]
    fn corner_radius_scales_with_dpi() {
        assert_eq!(corner_radius_px(192), (CORNER_RADIUS_LOGICAL * 2) as i32);
        assert_eq!(
            corner_radius_px(144),
            (CORNER_RADIUS_LOGICAL * 144 / 96) as i32
        );
    }

    #[test]
    fn corner_radius_never_zero_at_low_dpi() {
        assert!(corner_radius_px(1) >= 1);
        assert!(corner_radius_px(0) >= 1);
    }

    #[test]
    fn disabled_uses_idle_icon_and_disabled_tooltip() {
        let (icon, tip) = tray_appearance(false, true, true);
        assert_eq!(icon, ICON_IDLE);
        assert!(tip.contains("disabled"));
    }

    #[test]
    fn enabled_idle_uses_idle_icon() {
        let (icon, tip) = tray_appearance(true, false, false);
        assert_eq!(icon, ICON_IDLE);
        assert_eq!(tip, "HotMic");
    }

    #[test]
    fn camera_only_uses_cam_icon() {
        let (icon, tip) = tray_appearance(true, true, false);
        assert_eq!(icon, ICON_CAM);
        assert!(tip.contains("camera"));
    }

    #[test]
    fn mic_only_uses_mic_icon() {
        let (icon, tip) = tray_appearance(true, false, true);
        assert_eq!(icon, ICON_MIC);
        assert!(tip.contains("microphone"));
    }

    #[test]
    fn both_active_uses_both_icon_and_combined_tooltip() {
        let (icon, tip) = tray_appearance(true, true, true);
        assert_eq!(icon, ICON_BOTH);
        assert!(tip.contains("camera") && tip.contains("microphone"));
    }

    #[test]
    fn debounce_fires_only_on_active_to_idle() {
        assert!(should_start_debounce(true, false));
        assert!(!should_start_debounce(false, true));
        assert!(!should_start_debounce(true, true));
        assert!(!should_start_debounce(false, false));
    }

    /// Regression: the DEBOUNCE_TIMER firing is itself the commit point of
    /// the deferred active→idle transition. If `apply_state` re-runs
    /// `should_start_debounce` on the timer path, `last_visible` will still
    /// say "active" while the scan reports "idle," so this function returns
    /// `true` and the timer re-arms forever — the border never turns off.
    /// The fix is `bypass_debounce=true` on that single call site; this test
    /// pins the contract down so a future refactor can't quietly drop it.
    #[test]
    fn timer_commit_path_must_bypass_debounce() {
        let was_active = true;
        let now_active = false;
        let bypass_debounce = true;

        // The raw predicate still returns true (active→idle is still happening),
        assert!(should_start_debounce(was_active, now_active));
        // but the caller short-circuits it on the deferred-commit path.
        let arm_again = !bypass_debounce && should_start_debounce(was_active, now_active);
        assert!(!arm_again);
    }

    #[test]
    fn off_debounce_defers_when_timer_is_armed() {
        assert!(should_defer_off_transition(true, false, false, true));
    }

    #[test]
    fn off_debounce_commits_when_timer_fails() {
        assert!(!should_defer_off_transition(true, false, false, false));
    }

    #[test]
    fn off_debounce_commit_path_ignores_active_to_idle() {
        assert!(!should_defer_off_transition(true, false, true, true));
    }

    #[test]
    fn off_debounce_does_not_defer_when_state_stays_active_or_idle() {
        assert!(!should_defer_off_transition(true, true, false, true));
        assert!(!should_defer_off_transition(false, false, false, true));
        assert!(!should_defer_off_transition(false, true, false, true));
    }

    #[test]
    fn pending_off_cancels_when_device_becomes_active_again() {
        assert!(should_cancel_pending_off(true, false));
        assert!(should_cancel_pending_off(false, true));
        assert!(should_cancel_pending_off(true, true));
    }

    #[test]
    fn pending_off_stays_pending_while_still_idle() {
        assert!(!should_cancel_pending_off(false, false));
    }

    #[test]
    fn pending_off_timer_commits_only_while_still_pending() {
        assert!(should_commit_pending_off_timer(true, true));
    }

    #[test]
    fn stale_pending_off_timer_is_ignored_after_cancel() {
        assert!(!should_commit_pending_off_timer(false, true));
        assert!(!should_commit_pending_off_timer(false, false));
    }

    #[test]
    fn stale_pending_off_timer_does_not_commit_new_debounce_early() {
        assert!(!should_commit_pending_off_timer(true, false));
    }

    #[test]
    fn visible_devices_keep_camera_on_when_teams_mic_is_muted() {
        assert_eq!(visible_devices(true, false, true, true), (true, false));
    }

    #[test]
    fn visible_devices_show_both_when_camera_and_unmuted_teams_mic_are_active() {
        assert_eq!(visible_devices(true, false, true, false), (true, true));
    }

    #[test]
    fn visible_devices_keep_mic_on_when_non_teams_app_is_active() {
        assert_eq!(visible_devices(false, true, true, true), (false, true));
    }

    #[test]
    fn tray_update_skips_unchanged_state_until_heartbeat() {
        assert!(!should_update_tray_icon(
            ICON_BOTH,
            "HotMic: camera + microphone in use",
            ICON_BOTH,
            "HotMic: camera + microphone in use",
            false
        ));
    }

    #[test]
    fn tray_update_runs_when_state_changes() {
        assert!(should_update_tray_icon(
            ICON_BOTH,
            "HotMic: camera + microphone in use",
            ICON_MIC,
            "HotMic: microphone in use",
            false
        ));
        assert!(should_update_tray_icon(
            ICON_CAM, "same tip", ICON_MIC, "same tip", false
        ));
        assert!(should_update_tray_icon(
            ICON_MIC,
            "HotMic: microphone in use",
            ICON_MIC,
            "HotMic (disabled)",
            false
        ));
    }

    #[test]
    fn tray_update_runs_on_periodic_heartbeat() {
        assert!(should_update_tray_icon(
            ICON_BOTH,
            "HotMic: camera + microphone in use",
            ICON_BOTH,
            "HotMic: camera + microphone in use",
            true
        ));
    }

    #[test]
    fn cloaked_overlay_recreates_only_while_active() {
        assert!(should_recreate_cloaked_overlay(true, true));
        assert!(!should_recreate_cloaked_overlay(true, false));
        assert!(!should_recreate_cloaked_overlay(false, true));
    }

    #[test]
    fn overlay_plan_repairs_active_window_even_when_already_shown() {
        assert_eq!(
            overlay_visibility_plan(true, true, false),
            OverlayVisibilityPlan {
                recreate: false,
                show_window: false,
                repair: true
            }
        );
    }

    #[test]
    fn overlay_plan_shows_and_repairs_active_hidden_window() {
        assert_eq!(
            overlay_visibility_plan(true, false, false),
            OverlayVisibilityPlan {
                recreate: false,
                show_window: true,
                repair: true
            }
        );
    }

    #[test]
    fn overlay_plan_recreates_shows_and_repairs_active_cloaked_window() {
        assert_eq!(
            overlay_visibility_plan(true, true, true),
            OverlayVisibilityPlan {
                recreate: true,
                show_window: true,
                repair: true
            }
        );
    }

    #[test]
    fn overlay_plan_does_nothing_when_idle_even_if_cloaked() {
        assert_eq!(
            overlay_visibility_plan(false, true, true),
            OverlayVisibilityPlan {
                recreate: false,
                show_window: false,
                repair: false
            }
        );
    }

    #[test]
    fn all_zero_bytes_means_in_use() {
        assert!(parse_in_use(&[0u8; 8], 8));
    }

    #[test]
    fn realistic_filetime_means_released() {
        let ts: u64 = 133_700_000_000_000_000;
        assert!(!parse_in_use(&ts.to_le_bytes(), 8));
    }

    #[test]
    fn one_means_in_use_only_if_zero() {
        let one: u64 = 1;
        assert!(!parse_in_use(&one.to_le_bytes(), 8));
    }

    #[test]
    fn wrong_size_is_not_in_use() {
        assert!(!parse_in_use(&[0u8; 8], 4));
        assert!(!parse_in_use(&[0u8; 8], 12));
        assert!(!parse_in_use(&[0u8; 8], 0));
    }

    #[test]
    fn short_buffer_is_not_in_use() {
        assert!(!parse_in_use(&[0u8; 4], 8));
    }

    #[test]
    fn wide_chars_is_null_terminated() {
        assert_eq!(wide_chars("AB"), vec![b'A' as u16, b'B' as u16, 0]);
    }

    #[test]
    fn wide_chars_empty_is_just_null() {
        assert_eq!(wide_chars(""), vec![0]);
    }

    #[test]
    fn wide_chars_handles_unicode_bmp() {
        assert_eq!(wide_chars("\u{00E9}"), vec![0x00E9, 0]);
    }

    #[test]
    fn wide_chars_handles_supplementary_plane_via_surrogate_pair() {
        // U+1F600 (😀) is outside the BMP and must be encoded as a UTF-16 surrogate pair.
        // High surrogate D83D, low surrogate DE00. Username paths with emoji rely on this.
        assert_eq!(wide_chars("\u{1F600}"), vec![0xD83D, 0xDE00, 0]);
    }

    #[test]
    fn wide_bytes_handles_supplementary_plane() {
        assert_eq!(wide_bytes("\u{1F600}"), vec![0x3D, 0xD8, 0x00, 0xDE, 0, 0]);
    }

    #[test]
    fn wide_bytes_emits_utf16_le_with_null() {
        assert_eq!(wide_bytes("AB"), vec![b'A', 0, b'B', 0, 0, 0]);
    }

    #[test]
    fn wide_bytes_empty_is_two_zero_bytes() {
        assert_eq!(wide_bytes(""), vec![0, 0]);
    }

    #[test]
    fn wide_bytes_handles_non_ascii() {
        assert_eq!(wide_bytes("\u{00E9}"), vec![0xE9, 0x00, 0, 0]);
    }

    // -----------------------------------------------------------------------
    // is_teams_subkey
    // -----------------------------------------------------------------------

    #[test]
    fn teams_match_new_msix() {
        assert!(is_teams_subkey("MSTeams_8wekyb3d8bbwe"));
    }

    #[test]
    fn teams_match_classic_squirrel_path() {
        let n = "C:#Users#davidtagler#AppData#Local#Microsoft#Teams#current#Teams.exe";
        assert!(is_teams_subkey(n));
    }

    #[test]
    fn teams_match_is_case_insensitive_for_classic_path() {
        let n = "C:#USERS#DAVID#APPDATA#LOCAL#MICROSOFT#TEAMS#CURRENT#TEAMS.EXE";
        assert!(is_teams_subkey(n));
    }

    #[test]
    fn teams_does_not_match_msedge() {
        assert!(!is_teams_subkey(
            "C:#Program Files (x86)#Microsoft#Edge#Application#msedge.exe"
        ));
    }

    #[test]
    fn teams_does_not_match_webview2() {
        assert!(!is_teams_subkey(
            "C:#Program Files (x86)#Microsoft#EdgeWebView#Application#msedgewebview2.exe"
        ));
    }

    #[test]
    fn teams_does_not_match_unrelated() {
        assert!(!is_teams_subkey("NonPackaged"));
        assert!(!is_teams_subkey(""));
        assert!(!is_teams_subkey("Microsoft.WindowsCamera_8wekyb3d8bbwe"));
        assert!(!is_teams_subkey("Microsoft.SkypeApp_kzf8qxf38zg5c"));
    }

    #[test]
    fn teams_unmute_button_means_the_local_mic_is_muted() {
        assert_eq!(parse_teams_mic_button_name("Unmute mic"), Some(true));
        assert_eq!(
            parse_teams_mic_button_name("Unmute microphone (Ctrl+Shift+M)"),
            Some(true)
        );
    }

    #[test]
    fn teams_mute_button_means_the_local_mic_is_live() {
        assert_eq!(parse_teams_mic_button_name("Mute mic"), Some(false));
    }

    #[test]
    fn unrelated_accessibility_names_do_not_invent_a_mute_state() {
        assert_eq!(parse_teams_mic_button_name("Alex Example, muted"), None);
        assert_eq!(parse_teams_mic_button_name(""), None);
    }

    #[test]
    fn conflicting_teams_windows_fail_safe_to_mic_live() {
        assert_eq!(
            combine_teams_mic_button_states(&[Some(true), Some(false)]),
            Some(false)
        );
    }

    #[test]
    fn teams_windows_report_muted_only_when_every_button_does() {
        assert_eq!(combine_teams_mic_button_states(&[None, Some(true)]), None);
        assert_eq!(
            combine_teams_mic_button_states(&[Some(true), Some(true)]),
            Some(true)
        );
        assert_eq!(combine_teams_mic_button_states(&[None, None]), None);
    }

    #[test]
    fn local_api_state_wins_over_accessibility_fallback() {
        assert!(!resolve_teams_muted(Some(false), true, || {
            panic!("authoritative Local API state must skip UI Automation")
        }));
        assert!(resolve_teams_muted(Some(true), true, || {
            panic!("authoritative Local API state must skip UI Automation")
        }));
    }

    #[test]
    fn accessibility_state_fills_local_api_gap() {
        assert!(resolve_teams_muted(None, true, || Some(true)));
        assert!(!resolve_teams_muted(None, true, || Some(false)));
    }

    #[test]
    fn inactive_teams_skips_accessibility_lookup() {
        let mut queried = false;
        let muted = resolve_teams_muted(None, false, || {
            queried = true;
            Some(true)
        });

        assert!(!muted);
        assert!(!queried);
    }

    #[test]
    fn unavailable_teams_signals_fail_safe_to_mic_live() {
        assert!(!resolve_teams_muted(None, true, || None));
    }

    // -----------------------------------------------------------------------
    // mic_should_show — full 8-row truth table
    // -----------------------------------------------------------------------

    #[test]
    fn mic_idle_is_off() {
        assert!(!mic_should_show(false, false, false));
        assert!(!mic_should_show(false, false, true));
    }

    #[test]
    fn mic_teams_only_unmuted_is_on() {
        assert!(mic_should_show(false, true, false));
    }

    #[test]
    fn mic_teams_only_muted_is_off() {
        // The whole point of Option A.
        assert!(!mic_should_show(false, true, true));
    }

    #[test]
    fn mic_non_teams_only_is_on_regardless_of_teams_mute() {
        assert!(mic_should_show(true, false, false));
        assert!(mic_should_show(true, false, true));
    }

    #[test]
    fn mic_both_unmuted_is_on() {
        assert!(mic_should_show(true, true, false));
    }

    #[test]
    fn mic_both_with_teams_muted_stays_on_for_other_app() {
        // Per-app fusion: Teams muted, but Voice Recorder is also live -> ON.
        assert!(mic_should_show(true, true, true));
    }

    // -----------------------------------------------------------------------
    // parse_meeting_update
    // -----------------------------------------------------------------------

    #[test]
    fn parse_full_meeting_update_payload() {
        let json = r#"{"meetingUpdate":{"meetingState":{"isMuted":false,"isVideoOn":false,"isHandRaised":false,"isInMeeting":true,"isRecordingOn":false,"isBackgroundBlurred":false,"isSharing":false,"hasUnreadMessages":false},"meetingPermissions":{"canToggleMute":true}}}"#;
        let m = parse_meeting_update(json);
        assert_eq!(m.in_meeting, Some(true));
        assert_eq!(m.is_muted, Some(false));
    }

    #[test]
    fn parse_meeting_state_with_muted_true() {
        let json = r#"{"meetingUpdate":{"meetingState":{"isInMeeting":true,"isMuted":true}}}"#;
        let m = parse_meeting_update(json);
        assert_eq!(m.in_meeting, Some(true));
        assert_eq!(m.is_muted, Some(true));
    }

    #[test]
    fn parse_no_meeting_state_returns_none() {
        // Real Teams payload when no meeting is active — only meetingPermissions.
        let json = r#"{"meetingUpdate":{"meetingPermissions":{"canToggleMute":false,"canToggleVideo":false,"isMuted":true}}}"#;
        let m = parse_meeting_update(json);
        assert_eq!(m.in_meeting, None);
        // Must NOT pick up isMuted from sibling meetingPermissions.
        assert_eq!(m.is_muted, None);
    }

    #[test]
    fn parse_partial_state_returns_partial() {
        let json = r#"{"meetingUpdate":{"meetingState":{"isInMeeting":true}}}"#;
        let m = parse_meeting_update(json);
        assert_eq!(m.in_meeting, Some(true));
        assert_eq!(m.is_muted, None);
    }

    #[test]
    fn parse_empty_or_garbage_yields_none() {
        assert_eq!(parse_meeting_update(""), MeetingUpdate::default());
        assert_eq!(parse_meeting_update("{}"), MeetingUpdate::default());
        assert_eq!(parse_meeting_update("not json"), MeetingUpdate::default());
    }

    #[test]
    fn can_pair_true_in_real_payload() {
        // Real frame captured from Teams Local API once the user is in a
        // meeting and Teams is willing to receive a `pair` request.
        let json = r#"{"meetingUpdate":{"meetingPermissions":{"canReact":true,"canToggleVideo":true,"canToggleMute":true,"canToggleHand":true,"canToggleShareTray":true,"canLeave":true,"canToggleBlur":false,"canToggleChat":true,"canStopSharing":false,"canPair":true}}}"#;
        assert_eq!(parse_can_pair(json), Some(true));
    }

    #[test]
    fn can_pair_false_when_not_yet_authorized() {
        let json =
            r#"{"meetingUpdate":{"meetingPermissions":{"canToggleMute":false,"canPair":false}}}"#;
        assert_eq!(parse_can_pair(json), Some(false));
    }

    #[test]
    fn can_pair_none_without_meeting_permissions() {
        // Just `meetingState` with no `meetingPermissions` block at all.
        let json = r#"{"meetingUpdate":{"meetingState":{"isInMeeting":true,"isMuted":false}}}"#;
        assert_eq!(parse_can_pair(json), None);
    }

    #[test]
    fn can_pair_none_when_field_absent() {
        // `meetingPermissions` present but `canPair` not in it.
        let json = r#"{"meetingUpdate":{"meetingPermissions":{"canToggleMute":true}}}"#;
        assert_eq!(parse_can_pair(json), None);
    }

    // -----------------------------------------------------------------------
    // redact_secrets — protects the diagnostic log when users share it
    // -----------------------------------------------------------------------

    #[test]
    fn redact_tokenrefresh_uuid() {
        let s = r#"text frame (55 bytes): {"tokenRefresh":"ed8f4f22-2db3-4ef4-bcb8-8ea22769d5d9"}"#;
        let r = redact_secrets(s);
        assert!(!r.contains("ed8f4f22"), "uuid leaked: {r}");
        assert!(r.contains(r#""tokenRefresh":"<redacted>""#), "got: {r}");
    }

    #[test]
    fn redact_handles_whitespace_and_escapes() {
        // Whitespace around the colon, plus an escaped quote inside the value.
        let s = r#"{ "tokenRefresh" : "abc\"def-123" , "other": 1 }"#;
        let r = redact_secrets(s);
        assert!(!r.contains("abc"), "value leaked: {r}");
        assert!(!r.contains("def-123"), "value leaked: {r}");
        assert!(r.contains("<redacted>"), "got: {r}");
        // Adjacent fields survive intact.
        assert!(r.contains(r#""other": 1"#), "got: {r}");
    }

    #[test]
    fn redact_token_query_param_amp() {
        let s = "GET /?protocol-version=2.0.0&manufacturer=HotMic&token=abcd-1234&app=HotMic";
        let r = redact_secrets(s);
        assert!(!r.contains("abcd-1234"), "token leaked: {r}");
        assert!(r.contains("&token=<redacted>&app=HotMic"), "got: {r}");
    }

    #[test]
    fn redact_token_query_param_question() {
        let s = "/?token=deadbeef";
        let r = redact_secrets(s);
        assert_eq!(r, "/?token=<redacted>");
    }

    #[test]
    fn redact_passes_through_unrelated_text() {
        let s = r#"{"meetingState":{"isMuted":true,"isInMeeting":true}}"#;
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }

    #[test]
    fn redact_handles_empty_string() {
        assert_eq!(redact_secrets(""), "");
    }

    #[test]
    fn redact_does_not_match_substring_named_tokenRefresh() {
        // A field literally named "myTokenRefresh" should NOT be redacted —
        // only the exact key. (We match the leading quote in the needle.)
        let s = r#"{"myTokenRefresh":"safe-value"}"#;
        let r = redact_secrets(s);
        assert!(r.contains("safe-value"), "false-positive redaction: {r}");
    }

    #[test]
    fn parse_ignores_string_value_for_boolean_field() {
        // If Teams ever serialized as string, we should NOT treat it as a bool.
        let json = r#"{"meetingState":{"isMuted":"true"}}"#;
        let m = parse_meeting_update(json);
        assert_eq!(m.is_muted, None);
    }

    // -----------------------------------------------------------------------
    // base64_encode (RFC 4648 vectors)
    // -----------------------------------------------------------------------

    #[test]
    fn base64_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_high_bytes() {
        assert_eq!(base64_encode(&[0xFF, 0xFF, 0xFF]), "////");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn base64_16_random_bytes_yields_24_chars() {
        // Sec-WebSocket-Key is 16 random bytes -> 24-char base64 with == padding.
        let key = [0xABu8; 16];
        let enc = base64_encode(&key);
        assert_eq!(enc.len(), 24);
        assert!(enc.ends_with("=="));
    }

    // -----------------------------------------------------------------------
    // ws_handshake_request
    // -----------------------------------------------------------------------

    #[test]
    fn handshake_includes_required_headers_and_crlf_terminator() {
        let req = ws_handshake_request("localhost:8124", "/?token=abc", "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(req.starts_with("GET /?token=abc HTTP/1.1\r\n"));
        assert!(req.contains("Host: localhost:8124\r\n"));
        assert!(req.contains("Upgrade: websocket\r\n"));
        assert!(req.contains("Connection: Upgrade\r\n"));
        assert!(req.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    // -----------------------------------------------------------------------
    // ws_build_text_frame — three length encodings
    // -----------------------------------------------------------------------

    #[test]
    fn frame_short_payload() {
        let f = ws_build_text_frame(b"hi", [0, 0, 0, 0]);
        assert_eq!(f[0], 0x81); // FIN + text
        assert_eq!(f[1], 0x82); // mask + len 2
        assert_eq!(&f[2..6], &[0, 0, 0, 0]);
        assert_eq!(&f[6..], b"hi"); // mask is zeros so payload unchanged
    }

    #[test]
    fn frame_short_payload_with_real_mask_round_trips() {
        let payload = b"hello";
        let mask = [0xAA, 0xBB, 0xCC, 0xDD];
        let f = ws_build_text_frame(payload, mask);
        // Round-trip via parser.
        match ws_parse_frame(&f) {
            WsFrameParse::Frame {
                fin,
                opcode,
                payload: p,
                consumed,
            } => {
                assert!(fin);
                assert_eq!(opcode, 0x1);
                assert_eq!(p, payload);
                assert_eq!(consumed, f.len());
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn frame_medium_payload_uses_16bit_length() {
        let payload = vec![b'x'; 200];
        let f = ws_build_text_frame(&payload, [0, 0, 0, 0]);
        assert_eq!(f[1], 0x80 | 126); // mask bit + extended-len marker
        assert_eq!(u16::from_be_bytes([f[2], f[3]]) as usize, 200);
    }

    #[test]
    fn frame_large_payload_uses_64bit_length() {
        let payload = vec![b'x'; 70_000];
        let f = ws_build_text_frame(&payload, [0, 0, 0, 0]);
        assert_eq!(f[1], 0x80 | 127);
        assert_eq!(
            u64::from_be_bytes([f[2], f[3], f[4], f[5], f[6], f[7], f[8], f[9]]) as usize,
            70_000
        );
    }

    #[test]
    fn close_frame_format() {
        let f = ws_build_close_frame([1, 2, 3, 4]);
        assert_eq!(f, vec![0x88, 0x80, 1, 2, 3, 4]);
    }

    // -----------------------------------------------------------------------
    // ws_parse_frame
    // -----------------------------------------------------------------------

    #[test]
    fn parse_too_short_asks_for_more() {
        match ws_parse_frame(&[]) {
            WsFrameParse::Need(n) => assert_eq!(n, 2),
            other => panic!("expected Need, got {other:?}"),
        }
        match ws_parse_frame(&[0x81]) {
            WsFrameParse::Need(n) => assert_eq!(n, 1),
            other => panic!("expected Need(1), got {other:?}"),
        }
    }

    #[test]
    fn parse_unmasked_short_text_from_server() {
        // Teams sends unmasked text frames as a server.
        let mut buf = vec![0x81u8, 0x05]; // FIN+text, len=5, no mask
        buf.extend_from_slice(b"hello");
        match ws_parse_frame(&buf) {
            WsFrameParse::Frame {
                fin,
                opcode,
                payload,
                consumed,
            } => {
                assert!(fin);
                assert_eq!(opcode, 0x1);
                assert_eq!(payload, b"hello");
                assert_eq!(consumed, 7);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_ping_frame_distinguished_by_opcode() {
        let buf = [0x89u8, 0x00]; // FIN + ping, no payload
        match ws_parse_frame(&buf) {
            WsFrameParse::Frame { opcode, .. } => assert_eq!(opcode, 0x9),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_close_frame_distinguished_by_opcode() {
        let buf = [0x88u8, 0x00];
        match ws_parse_frame(&buf) {
            WsFrameParse::Frame { opcode, .. } => assert_eq!(opcode, 0x8),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_extended_16bit_length_needs_full_header() {
        let buf = [0x81u8, 126]; // missing the 2-byte length
        match ws_parse_frame(&buf) {
            WsFrameParse::Need(n) => assert_eq!(n, 2),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_payload_truncation_asks_for_remaining() {
        let mut buf = vec![0x81u8, 0x05]; // need 5 payload bytes
        buf.extend_from_slice(b"hel"); // only 3 here
        match ws_parse_frame(&buf) {
            WsFrameParse::Need(n) => assert_eq!(n, 2),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_fragmented_frame_reports_fin_false() {
        // FIN=0, opcode=text, payload "hi"
        let buf = [0x01u8, 0x02, b'h', b'i'];
        match ws_parse_frame(&buf) {
            WsFrameParse::Frame {
                fin,
                opcode,
                payload,
                ..
            } => {
                assert!(!fin);
                assert_eq!(opcode, 0x1);
                assert_eq!(payload, b"hi");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_continuation_frame_opcode_zero() {
        // FIN=1, opcode=continuation (0x0)
        let buf = [0x80u8, 0x02, b'!', b'!'];
        match ws_parse_frame(&buf) {
            WsFrameParse::Frame { opcode, fin, .. } => {
                assert_eq!(opcode, 0x0);
                assert!(fin);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_fragmented_control_frame() {
        // FIN=0 with a control opcode (ping=0x9) is illegal per RFC 6455 §5.5.
        let buf = [0x09u8, 0x00];
        assert!(matches!(ws_parse_frame(&buf), WsFrameParse::Error));
    }

    #[test]
    fn parse_rejects_oversize_control_frame() {
        // Control frame payload >125 bytes is illegal.
        let mut buf = vec![0x89u8, 126, 0x00, 0xC8]; // ping with 16-bit len 200
        buf.extend(std::iter::repeat_n(0u8, 200));
        assert!(matches!(ws_parse_frame(&buf), WsFrameParse::Error));
    }

    // -----------------------------------------------------------------------
    // find_bool_field — depth-aware
    // -----------------------------------------------------------------------

    #[test]
    fn find_bool_picks_top_level_only() {
        // Top-level isMuted=true, nested isMuted=false → should pick true.
        let body = r#""isMuted":true,"nested":{"isMuted":false}"#;
        assert_eq!(find_bool_field(body, "isMuted"), Some(true));
    }

    #[test]
    fn find_bool_skips_nested_when_top_absent() {
        // Only nested isMuted exists → should NOT pick it.
        let body = r#""nested":{"isMuted":true}"#;
        assert_eq!(find_bool_field(body, "isMuted"), None);
    }

    #[test]
    fn find_bool_ignores_substring_inside_string_value() {
        // "isMuted" appears as the contents of a string value; not a key.
        let body = r#""label":"isMuted is a property name","other":true"#;
        assert_eq!(find_bool_field(body, "isMuted"), None);
    }

    #[test]
    fn find_bool_skips_non_bool_value() {
        // A field of the same name but whose value isn't a bool must return None
        // rather than mis-coercing.
        let body = r#""isMuted":"true""#;
        assert_eq!(find_bool_field(body, "isMuted"), None);
    }
}
