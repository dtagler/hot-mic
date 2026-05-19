#![cfg_attr(not(test), windows_subsystem = "windows")]

mod autostart;
mod detect;
mod overlay;
mod teams;

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use detect::{DeviceState, Watcher};
use hotmic::{
    should_cancel_pending_off, should_commit_pending_off_timer, should_defer_off_transition,
    should_start_debounce, should_update_tray_icon, tray_appearance, visible_devices, ICON_BOTH,
    ICON_CAM, ICON_IDLE, ICON_MIC,
};
use overlay::Overlay;
use teams::{Client as TeamsClient, RECONNECT_TIMER, WM_TEAMS_SOCKET, WM_TEAMS_STATE_CHANGED};

const TRAY_CALLBACK: u32 = WM_USER + 1;
const BACKSTOP_TIMER: usize = 1;
const DEBOUNCE_TIMER: usize = 2;
const TRAY_ID: u32 = 1;
const DEBOUNCE_DURATION_MS: u32 = 150;
const TRAY_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// `TaskbarCreated` message id, returned by `RegisterWindowMessageW` and broadcast
/// by Explorer when the shell restarts. We re-add our tray icon when we see it,
/// otherwise the icon stays gone until the user relaunches.
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

const ID_ENABLED: u32 = 100;
const ID_AUTOSTART: u32 = 101;
const ID_EXIT: u32 = 103;

struct App {
    msg_hwnd: HWND,
    icons: IconCache,
    overlay: Overlay,
    watcher: Watcher,
    enabled: bool,
    last_state: DeviceState,
    pending_off: bool,
    debounce_due: Option<Instant>,
    /// Latest known Teams in-app mute state, fed by the Teams Local API
    /// WebSocket client. Defaults to `false` whenever the WS isn't connected
    /// (first run, classic Teams, IT-disabled API, connection drop) so the
    /// detection rule degrades cleanly to the original registry-only behavior.
    teams_muted_now: bool,
    /// Last visible (cam, mic) the overlay was asked to paint. Used to make
    /// the debounce decision based on what the user actually saw, not on the
    /// raw registry state — a flip in `teams_muted_now` alone (registry
    /// unchanged) must still arm the off-debounce, otherwise a quick
    /// mute/unmute toggle would visibly flicker the border off.
    last_visible: (bool, bool),
    last_tray_icon: u16,
    last_tray_tip: &'static str,
    last_tray_refresh: Instant,
    teams: TeamsClient,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let mutex_name = w!("Local\\HotMic-Singleton");
    // We intentionally don't free the handle: the OS releases it when the process
    // exits, which is exactly when we want the singleton lock to drop. `HANDLE` in
    // the windows crate has no Drop impl, so binding to `_h` doesn't close it.
    //
    // If CreateMutexW itself fails (essentially impossible for a Local\ name —
    // only out-of-memory could do it), we degrade gracefully and launch without
    // singleton protection rather than refusing to start.
    let mutex = unsafe { CreateMutexW(None, true, mutex_name) };
    if let Ok(_h) = mutex {
        let last = unsafe { GetLastError() };
        if last == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
    }

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };
    let msg_hwnd = create_message_window(hinstance)?;
    let overlay = Overlay::new(hinstance)?;
    let watcher = Watcher::new()?;
    let icons = IconCache::new(hinstance)?;

    add_tray_icon(msg_hwnd, icons.get(ICON_IDLE), "HotMic")?;

    let app = App {
        msg_hwnd,
        icons,
        overlay,
        watcher,
        enabled: true,
        last_state: DeviceState::default(),
        pending_off: false,
        debounce_due: None,
        teams_muted_now: false,
        last_visible: (false, false),
        last_tray_icon: ICON_IDLE,
        last_tray_tip: "HotMic",
        last_tray_refresh: Instant::now(),
        teams: TeamsClient::new(msg_hwnd),
    };

    APP.with(|cell| *cell.borrow_mut() = Some(app));

    // Cache the TaskbarCreated message id so the WndProc can re-add the tray icon
    // if Explorer restarts (the broadcast is sent to all top-level windows).
    let _ = TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    unsafe {
        // 500 ms backstop poll. Registry-change notifications usually fire faster,
        // but CapabilityAccessManager sometimes delays the write itself, so this
        // catches the gap without being noticeably slow.
        let _ = SetTimer(Some(msg_hwnd), BACKSTOP_TIMER, 500, None);
    }

    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            // Kick off the Teams WebSocket. Posts WM_TEAMS_SOCKET messages back
            // to msg_hwnd as the connection progresses; degrades silently if
            // Teams isn't running or the API is disabled.
            app.teams.start();
            let s = app.watcher.scan();
            apply_scanned_state(app, s);
        }
    });

    run_message_loop(msg_hwnd);

    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            app.teams.shutdown();
            remove_tray_icon(app.msg_hwnd);
        }
        *cell.borrow_mut() = None;
    });

    Ok(())
}

fn run_message_loop(_msg_hwnd: HWND) {
    loop {
        let events: Vec<HANDLE> = APP.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|a| a.watcher.events.clone())
                .unwrap_or_default()
        });

        let count = events.len() as u32;
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&events),
                INFINITE,
                QS_ALLINPUT,
                MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS(0),
            )
        };

        if wait.0 < count {
            // Registry event signaled. Re-arm first, then rescan.
            APP.with(|cell| {
                if let Some(app) = cell.borrow_mut().as_mut() {
                    app.watcher.arm_all();
                    let s = app.watcher.scan();
                    apply_scanned_state(app, s);
                }
            });
        } else if wait.0 == count {
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
                if msg.message == WM_QUIT {
                    return;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        } else {
            // WAIT_FAILED or similar; bail out.
            unsafe {
                PostQuitMessage(0);
            }
        }
    }
}

fn apply_state(app: &mut App, new_state: DeviceState, bypass_debounce: bool) {
    let (cam_visible, mic_visible) = visible_devices(
        new_state.cam,
        new_state.mic_non_teams,
        new_state.mic_teams,
        app.teams_muted_now,
    );

    if !app.enabled {
        if app.pending_off {
            unsafe {
                let _ = KillTimer(Some(app.msg_hwnd), DEBOUNCE_TIMER);
            }
            app.pending_off = false;
            app.debounce_due = None;
        }
        app.overlay.force_hide();
        app.last_state = new_state;
        // `last_visible` is "what the overlay was asked to paint." When
        // disabled we asked to paint nothing, so record (false, false). This
        // keeps the debounce decision honest when the user toggles enabled
        // back on later.
        app.last_visible = (false, false);
        let (icon, tip) = tray_appearance(false, cam_visible, mic_visible);
        update_app_tray_icon(app, icon, tip);
        return;
    }

    // Debounce against what was actually painted, not against the raw registry
    // state. A flip in `teams_muted_now` alone (registry unchanged) still
    // counts as an active→idle transition and must be debounced, otherwise a
    // quick mute/unmute toggle would visibly flicker the border off.
    //
    // `bypass_debounce` is set when this call is the DEBOUNCE_TIMER firing.
    // The timer firing IS the commit point of the deferred off-transition,
    // so we must skip the debounce check or we re-arm forever (the timer
    // would call apply_state, `last_visible` would still say "border on,"
    // and we'd re-enter the same active→idle branch indefinitely — the
    // border would never actually turn off).
    let (last_cam_visible, last_mic_visible) = app.last_visible;
    let was_active = last_cam_visible || last_mic_visible;
    let now_active = cam_visible || mic_visible;

    let needs_off_debounce = !bypass_debounce && should_start_debounce(was_active, now_active);
    let debounce_timer_armed = if needs_off_debounce {
        let now = Instant::now();
        let timer = unsafe {
            // 150 ms off-debounce: just enough to ride through the brief stop/start
            // that some apps do during device negotiation, without feeling laggy.
            SetTimer(
                Some(app.msg_hwnd),
                DEBOUNCE_TIMER,
                DEBOUNCE_DURATION_MS,
                None,
            )
        };
        if timer != 0 {
            app.debounce_due = Some(now + Duration::from_millis(u64::from(DEBOUNCE_DURATION_MS)));
            true
        } else {
            false
        }
    } else {
        false
    };

    if should_defer_off_transition(
        was_active,
        now_active,
        bypass_debounce,
        debounce_timer_armed,
    ) {
        app.pending_off = true;
        app.last_state = new_state;
        return;
    }

    // If the debounce timer cannot be armed under resource pressure, fall
    // through and commit the off-transition immediately rather than
    // suppressing future applies.

    app.pending_off = false;
    app.debounce_due = None;
    app.last_state = new_state;
    app.last_visible = (cam_visible, mic_visible);
    app.overlay.set_colors(cam_visible, mic_visible);

    let (icon, tip) = tray_appearance(true, cam_visible, mic_visible);
    update_app_tray_icon(app, icon, tip);
}

fn apply_scanned_state(app: &mut App, new_state: DeviceState) {
    app.teams_muted_now = app.teams.muted_now();

    if app.pending_off {
        let (cam_visible, mic_visible) = visible_devices(
            new_state.cam,
            new_state.mic_non_teams,
            new_state.mic_teams,
            app.teams_muted_now,
        );
        if should_cancel_pending_off(cam_visible, mic_visible) {
            unsafe {
                let _ = KillTimer(Some(app.msg_hwnd), DEBOUNCE_TIMER);
            }
            app.pending_off = false;
            app.debounce_due = None;
            apply_state(app, new_state, false);
        }
        return;
    }

    apply_state(app, new_state, false);
}

fn current_tray_appearance(app: &App) -> (u16, &'static str) {
    let (cam_visible, mic_visible) = visible_devices(
        app.last_state.cam,
        app.last_state.mic_non_teams,
        app.last_state.mic_teams,
        app.teams_muted_now,
    );
    tray_appearance(app.enabled, cam_visible, mic_visible)
}

fn update_app_tray_icon(app: &mut App, icon: u16, tip: &'static str) {
    let refresh_due = app.last_tray_refresh.elapsed() >= TRAY_REFRESH_INTERVAL;
    if !should_update_tray_icon(
        app.last_tray_icon,
        app.last_tray_tip,
        icon,
        tip,
        refresh_due,
    ) {
        return;
    }

    if !update_tray_icon(app.msg_hwnd, app.icons.get(icon), tip) {
        let _ = add_tray_icon(app.msg_hwnd, app.icons.get(icon), tip);
    }

    app.last_tray_icon = icon;
    app.last_tray_tip = tip;
    app.last_tray_refresh = Instant::now();
}

fn create_message_window(hinstance: HINSTANCE) -> Result<HWND> {
    unsafe {
        let class_name = w!("HotMicMsg");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(msg_wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread());
        }
        // Hidden top-level window (no parent). We previously used HWND_MESSAGE,
        // but message-only windows can't be foregrounded — so SetForegroundWindow
        // before TrackPopupMenu silently failed and the popup rendered as a blank
        // white box. They also don't receive RegisterWindowMessageW broadcasts
        // like TaskbarCreated, which we rely on to re-add the tray icon after an
        // Explorer restart.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name,
            w!("HotMic"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        Ok(hwnd)
    }
}

extern "system" fn msg_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            TRAY_CALLBACK => {
                // We don't call NIM_SETVERSION, so the default NOTIFYICON_VERSION (0)
                // applies and the mouse event id is packed into the low word of lParam.
                let event = (lparam.0 as u32) & 0xFFFF;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 as u32) & 0xFFFF;
                handle_menu(hwnd, id);
                LRESULT(0)
            }
            WM_TIMER => {
                let id = wparam.0;
                if id == BACKSTOP_TIMER {
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            let s = app.watcher.scan();
                            apply_scanned_state(app, s);
                        }
                    });
                } else if id == DEBOUNCE_TIMER {
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            let debounce_due_reached =
                                app.debounce_due.is_some_and(|due| Instant::now() >= due);
                            if should_commit_pending_off_timer(
                                app.pending_off,
                                debounce_due_reached,
                            ) {
                                let _ = KillTimer(Some(hwnd), DEBOUNCE_TIMER);
                                app.pending_off = false;
                                app.debounce_due = None;
                                let s = app.watcher.scan();
                                app.teams_muted_now = app.teams.muted_now();
                                // bypass_debounce=true: the timer firing IS the
                                // commit of the deferred off-transition. Without
                                // this, apply_state would see was_active=true (from
                                // last_visible) → now_active=false (still off) →
                                // re-arm the same 150 ms timer forever, and the
                                // border would never turn off.
                                apply_state(app, s, true);
                            }
                        }
                    });
                } else if id == RECONNECT_TIMER {
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            app.teams.on_reconnect_timer();
                        }
                    });
                }
                LRESULT(0)
            }
            m if m == WM_TEAMS_SOCKET => {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        app.teams.on_socket_event(wparam, lparam);
                    }
                });
                LRESULT(0)
            }
            m if m == WM_TEAMS_STATE_CHANGED => {
                // Teams reported a new isMuted/isInMeeting; recompute the
                // border immediately rather than waiting for the next 500 ms
                // backstop tick.
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let s = app.watcher.scan();
                        apply_scanned_state(app, s);
                    }
                });
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        app.overlay.reposition();
                    }
                });
                LRESULT(0)
            }
            m if Some(m) == TASKBAR_CREATED.get().copied() => {
                // Explorer restarted — our tray icon was lost. Re-add it.
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        let (icon, tip) = current_tray_appearance(app);
                        if add_tray_icon(app.msg_hwnd, app.icons.get(icon), tip).is_ok() {
                            app.last_tray_icon = icon;
                            app.last_tray_tip = tip;
                            app.last_tray_refresh = Instant::now();
                        }
                    }
                });
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let (enabled, autostart) = APP.with(|cell| {
            let b = cell.borrow();
            let app = b.as_ref();
            (
                app.map(|a| a.enabled).unwrap_or(true),
                autostart::is_enabled(),
            )
        });

        let check = |on: bool| if on { MF_CHECKED.0 } else { MF_UNCHECKED.0 };
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(MF_STRING.0 | check(enabled)),
            ID_ENABLED as usize,
            w!("Enabled"),
        );
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(MF_STRING.0 | check(autostart)),
            ID_AUTOSTART as usize,
            w!("Start with Windows"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT as usize, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        // Documented MSDN workaround: post a benign message so the menu dismisses
        // cleanly when the user clicks outside it.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn handle_menu(hwnd: HWND, id: u32) {
    match id {
        ID_ENABLED => {
            APP.with(|cell| {
                if let Some(app) = cell.borrow_mut().as_mut() {
                    app.enabled = !app.enabled;
                    let s = app.watcher.scan();
                    app.teams_muted_now = app.teams.muted_now();
                    apply_state(app, s, false);
                }
            });
        }
        ID_AUTOSTART => {
            let now = autostart::is_enabled();
            let _ = autostart::set_enabled(!now);
        }
        ID_EXIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
}

fn add_tray_icon(hwnd: HWND, hicon: HICON, tip: &str) -> Result<()> {
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: hicon,
            ..Default::default()
        };
        set_tip(&mut data, tip);

        // Shell_NotifyIconW can transiently fail during shell startup or restart
        // with ERROR_TIMEOUT. Retry a few times with a short backoff before giving up.
        for attempt in 0..3 {
            if Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                return Ok(());
            }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        Err(Error::from_thread())
    }
}

fn update_tray_icon(hwnd: HWND, hicon: HICON, tip: &str) -> bool {
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_TIP,
            hIcon: hicon,
            ..Default::default()
        };
        set_tip(&mut data, tip);
        Shell_NotifyIconW(NIM_MODIFY, &data).as_bool()
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn load_icon(hinstance: HINSTANCE, id: u16) -> Result<HICON> {
    unsafe {
        // LoadIconMetric picks the .ico subimage that matches the current system
        // small-icon size for the active DPI, so high-DPI displays get the 32 px
        // or 48 px variant instead of an upscaled 16 px.
        LoadIconMetric(
            Some(hinstance),
            PCWSTR(id as usize as *const u16),
            LIM_SMALL,
        )
    }
}

/// Cache of the four tray HICONs (idle/cam/mic/both) loaded once at startup.
///
/// Originally the binary called `LoadIconMetric` on every `update_tray_icon`,
/// but per MSDN that API returns a non-shared icon handle that must be freed
/// with `DestroyIcon`. We never freed them, and the 500 ms `BACKSTOP_TIMER`
/// drives `apply_state` (and therefore `update_tray_icon`) unconditionally,
/// so we leaked roughly 2 USER objects per second. After ~83 minutes the
/// per-process USER quota of 10,000 was exhausted, at which point new
/// `CreatePopupMenu`/`AppendMenuW` calls failed silently — the right-click
/// tray menu would render blank — and `CreatePen`/`RoundRect` in the overlay
/// stopped drawing the border, while the previously-set tray icon stayed
/// stuck on its last color. Caching here loads exactly four HICONs for the
/// life of the process, eliminating both the leak and the per-tick reload
/// cost. The handles are intentionally not destroyed at process exit; the
/// OS reclaims USER objects on termination.
struct IconCache {
    handles: [HICON; 4],
}

impl IconCache {
    fn new(hinstance: HINSTANCE) -> Result<Self> {
        Ok(Self {
            handles: [
                load_icon(hinstance, ICON_IDLE)?,
                load_icon(hinstance, ICON_CAM)?,
                load_icon(hinstance, ICON_MIC)?,
                load_icon(hinstance, ICON_BOTH)?,
            ],
        })
    }

    fn get(&self, id: u16) -> HICON {
        // ICON_* are 1..=4 by convention; clamp on out-of-range input rather
        // than panicking from the WndProc.
        let idx = (id as usize).saturating_sub(1).min(3);
        self.handles[idx]
    }
}

fn set_tip(data: &mut NOTIFYICONDATAW, tip: &str) {
    let wide: Vec<u16> = tip.encode_utf16().collect();
    let n = wide.len().min(data.szTip.len() - 1);
    data.szTip[..n].copy_from_slice(&wide[..n]);
    data.szTip[n] = 0;
}
