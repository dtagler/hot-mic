#![cfg_attr(not(test), windows_subsystem = "windows")]

mod autostart;
mod detect;
mod overlay;

use std::cell::RefCell;
use std::sync::OnceLock;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use detect::{DeviceState, Watcher};
use hotmic::{should_start_debounce, tray_appearance, ICON_IDLE};
use overlay::Overlay;

const TRAY_CALLBACK: u32 = WM_USER + 1;
const BACKSTOP_TIMER: usize = 1;
const DEBOUNCE_TIMER: usize = 2;
const TRAY_ID: u32 = 1;

/// `TaskbarCreated` message id, returned by `RegisterWindowMessageW` and broadcast
/// by Explorer when the shell restarts. We re-add our tray icon when we see it,
/// otherwise the icon stays gone until the user relaunches.
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

const ID_ENABLED: u32 = 100;
const ID_AUTOSTART: u32 = 101;
const ID_EXIT: u32 = 103;

struct App {
    msg_hwnd: HWND,
    hinstance: HINSTANCE,
    overlay: Overlay,
    watcher: Watcher,
    enabled: bool,
    last_state: DeviceState,
    pending_off: bool,
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

    let app = App {
        msg_hwnd,
        hinstance,
        overlay,
        watcher,
        enabled: true,
        last_state: DeviceState::default(),
        pending_off: false,
    };

    APP.with(|cell| *cell.borrow_mut() = Some(app));

    add_tray_icon(msg_hwnd, hinstance, ICON_IDLE)?;

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
            let s = app.watcher.scan();
            apply_state(app, s);
        }
    });

    run_message_loop(msg_hwnd);

    APP.with(|cell| {
        if let Some(app) = cell.borrow().as_ref() {
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
                    if !app.pending_off {
                        apply_state(app, s);
                    }
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

fn apply_state(app: &mut App, new_state: DeviceState) {
    if !app.enabled {
        app.overlay.force_hide();
        app.last_state = new_state;
        let (icon, tip) = tray_appearance(false, new_state.cam, new_state.mic);
        update_tray_icon(app.msg_hwnd, app.hinstance, icon, tip);
        return;
    }

    let was_active = app.last_state.cam || app.last_state.mic;
    let now_active = new_state.cam || new_state.mic;

    if should_start_debounce(was_active, now_active) {
        unsafe {
            // 150 ms off-debounce: just enough to ride through the brief stop/start
            // that some apps do during device negotiation, without feeling laggy.
            let _ = SetTimer(Some(app.msg_hwnd), DEBOUNCE_TIMER, 150, None);
        }
        app.pending_off = true;
        app.last_state = new_state;
        return;
    }

    app.pending_off = false;
    app.last_state = new_state;
    app.overlay.set_colors(new_state.cam, new_state.mic);

    let (icon, tip) = tray_appearance(true, new_state.cam, new_state.mic);
    update_tray_icon(app.msg_hwnd, app.hinstance, icon, tip);
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
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("HotMic"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
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
                            if !app.pending_off {
                                apply_state(app, s);
                            }
                        }
                    });
                } else if id == DEBOUNCE_TIMER {
                    let _ = KillTimer(Some(hwnd), DEBOUNCE_TIMER);
                    APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            app.pending_off = false;
                            let s = app.watcher.scan();
                            apply_state(app, s);
                        }
                    });
                }
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
                    if let Some(app) = cell.borrow().as_ref() {
                        let _ = add_tray_icon(app.msg_hwnd, app.hinstance, ICON_IDLE);
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
                    apply_state(app, s);
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

fn add_tray_icon(hwnd: HWND, hinstance: HINSTANCE, icon_id: u16) -> Result<()> {
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: load_icon(hinstance, icon_id)?,
            ..Default::default()
        };
        set_tip(&mut data, "HotMic");

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

fn update_tray_icon(hwnd: HWND, hinstance: HINSTANCE, icon_id: u16, tip: &str) {
    unsafe {
        if let Ok(hicon) = load_icon(hinstance, icon_id) {
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ID,
                uFlags: NIF_ICON | NIF_TIP,
                hIcon: hicon,
                ..Default::default()
            };
            set_tip(&mut data, tip);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
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

fn load_icon(
    hinstance: HINSTANCE,
    id: u16,
) -> Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
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

fn set_tip(data: &mut NOTIFYICONDATAW, tip: &str) {
    let wide: Vec<u16> = tip.encode_utf16().collect();
    let n = wide.len().min(data.szTip.len() - 1);
    data.szTip[..n].copy_from_slice(&wide[..n]);
    data.szTip[n] = 0;
}
