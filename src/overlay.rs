use std::ffi::c_void;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use hotmic::{
    border_thickness_px, corner_radius_px, overlay_visibility_plan, COLOR_BLUE, COLOR_PURPLE,
    COLOR_RED,
};

const CLASS_NAME: PCWSTR = w!("HotMicOverlay");

/// Magenta color-key. Pixels of this exact value are masked out by the
/// layered-window compositor, giving us per-pixel transparency without
/// needing UpdateLayeredWindow. Chosen because we never paint magenta.
const TRANSPARENT_KEY: u32 = 0x00FF00FF;

/// WM_TIMER id used for the fade animation.
const FADE_TIMER: usize = 1;

/// Tick interval in ms. 16 ms ≈ 60 Hz, indistinguishable from continuous.
const FADE_TICK_MS: u32 = 16;

/// Alpha delta per tick. 255 ÷ 32 = ~8 ticks to fully fade; 8 × 16 ms ≈ 128 ms.
const FADE_STEP: i16 = 32;

#[derive(Default)]
struct OverlayState {
    // Colors currently being painted. Stay put during fade-out so the border
    // keeps its color all the way down to alpha 0 instead of vanishing mid-fade.
    paint_cam: bool,
    paint_mic: bool,
    current_alpha: u8,
    target_alpha: u8,
    // Whether the OS-level window is shown (ShowWindow has been called).
    // Lives in shared state so the fade timer can flip it to false after a
    // completed fade-out, and the next set_colors knows to re-show.
    shown: bool,
}

pub struct Overlay {
    hinstance: HINSTANCE,
    hwnd: HWND,
    // Boxed so the heap address stays stable for the WndProc to read via GWLP_USERDATA.
    state: Box<OverlayState>,
}

impl Overlay {
    pub fn new(hinstance: HINSTANCE) -> Result<Self> {
        register_class(hinstance)?;
        let hwnd = create_overlay_window(hinstance)?;
        let state = Box::new(OverlayState::default());
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state.as_ref() as *const _ as isize);
        }
        let mut o = Self {
            hinstance,
            hwnd,
            state,
        };
        o.reposition();
        Ok(o)
    }

    pub fn set_colors(&mut self, cam: bool, mic: bool) {
        let show = cam || mic;
        self.state.target_alpha = if show { 255 } else { 0 };

        if show {
            let plan = overlay_visibility_plan(show, self.state.shown, is_dwm_cloaked(self.hwnd));
            if plan.recreate {
                self.recreate_on_current_desktop();
            }

            // Going to a visible state: snap paint colors to the new state
            // immediately (so a cam→both transition swaps color crisply).
            self.state.paint_cam = cam;
            self.state.paint_mic = mic;

            if plan.show_window {
                // Window is hidden (either first-ever or after a completed
                // fade-out). Start invisible and fade up.
                self.state.current_alpha = 0;
                unsafe {
                    let _ = SetLayeredWindowAttributes(
                        self.hwnd,
                        COLORREF(TRANSPARENT_KEY),
                        0,
                        LWA_COLORKEY | LWA_ALPHA,
                    );
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                self.state.shown = true;
            }

            if plan.repair {
                self.repair_visible_window();
            }
        }
        // For show=false: keep paint_cam/paint_mic as they were, so the
        // existing border color persists while the fade-out runs.

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
            if SetTimer(Some(self.hwnd), FADE_TIMER, FADE_TICK_MS, None) == 0 {
                self.snap_to_target_alpha();
            }
        }
    }

    pub fn force_hide(&mut self) {
        self.state.target_alpha = 0;
        self.state.current_alpha = 0;
        self.state.paint_cam = false;
        self.state.paint_mic = false;
        self.state.shown = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), FADE_TIMER);
            let _ = SetLayeredWindowAttributes(
                self.hwnd,
                COLORREF(TRANSPARENT_KEY),
                0,
                LWA_COLORKEY | LWA_ALPHA,
            );
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn reposition(&mut self) {
        let (left, top, width, height, _dpi) = primary_monitor_bounds();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    fn repair_visible_window(&mut self) {
        let (left, top, width, height, _dpi) = primary_monitor_bounds();
        unsafe {
            let _ = SetLayeredWindowAttributes(
                self.hwnd,
                COLORREF(TRANSPARENT_KEY),
                self.state.current_alpha,
                LWA_COLORKEY | LWA_ALPHA,
            );
            // Teams call/share windows, virtual-desktop transitions, and display
            // topology changes can leave an otherwise active overlay hidden,
            // behind another topmost window, or at stale bounds. The 500 ms
            // backstop tick calls this while active, so those states self-heal.
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
            );
        }
    }

    fn recreate_on_current_desktop(&mut self) {
        let Ok(new_hwnd) = create_overlay_window(self.hinstance) else {
            return;
        };
        unsafe {
            SetWindowLongPtrW(
                new_hwnd,
                GWLP_USERDATA,
                self.state.as_ref() as *const _ as isize,
            );
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
        self.hwnd = new_hwnd;
        self.state.shown = false;
        self.state.current_alpha = 0;
        self.reposition();
    }

    fn snap_to_target_alpha(&mut self) {
        self.state.current_alpha = self.state.target_alpha;
        unsafe {
            let _ = SetLayeredWindowAttributes(
                self.hwnd,
                COLORREF(TRANSPARENT_KEY),
                self.state.current_alpha,
                LWA_COLORKEY | LWA_ALPHA,
            );
            if self.state.current_alpha == 0 {
                finish_fade_out(self.hwnd, &mut self.state);
            }
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Destroy the window before `state` (the Box behind the GWLP_USERDATA pointer)
        // is freed, so a late WM_PAINT can't dereference a dangling pointer.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn primary_monitor_bounds() -> (i32, i32, i32, i32, u32) {
    // Primary-monitor only by design. Multi-monitor support would track per-display
    // bounds + DPI and manage one Overlay per monitor; see README "Known limitations".
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut info);
        let r = info.rcMonitor;
        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        (r.left, r.top, r.right - r.left, r.bottom - r.top, dpi_x)
    }
}

fn create_overlay_window(hinstance: HINSTANCE) -> Result<HWND> {
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("HotMic"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?
    };
    unsafe {
        // Start fully transparent (alpha = 0), with color-key masking magenta.
        // Both LWA_COLORKEY and LWA_ALPHA so alpha can be animated independently.
        let _ = SetLayeredWindowAttributes(
            hwnd,
            COLORREF(TRANSPARENT_KEY),
            0,
            LWA_COLORKEY | LWA_ALPHA,
        );
    }
    Ok(hwnd)
}

fn is_dwm_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let r = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut c_void,
            std::mem::size_of_val(&cloaked) as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

unsafe fn finish_fade_out(hwnd: HWND, state: &mut OverlayState) {
    // Fully faded out. Hide the window and clear paint state so the next
    // set_colors starts a clean fade-in.
    state.paint_cam = false;
    state.paint_mic = false;
    state.shown = false;
    let _ = ShowWindow(hwnd, SW_HIDE);
}

fn dpi_for(hwnd: HWND) -> u32 {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        dpi_x
    }
}

fn register_class(hinstance: HINSTANCE) -> Result<()> {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: CLASS_NAME,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread());
        }
        Ok(())
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                if hdc.is_invalid() {
                    return LRESULT(0);
                }

                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);

                let bg = CreateSolidBrush(COLORREF(TRANSPARENT_KEY));
                let _ = FillRect(hdc, &rect, bg);
                let _ = DeleteObject(bg.into());

                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const OverlayState;
                if !state_ptr.is_null() && rect.right > 0 && rect.bottom > 0 {
                    let state = &*state_ptr;
                    let dpi = dpi_for(hwnd);
                    let t = border_thickness_px(dpi);
                    let r = corner_radius_px(dpi);
                    if rect.right > 2 * r && rect.bottom > 2 * r {
                        draw_border(
                            hdc,
                            rect.right,
                            rect.bottom,
                            t,
                            r,
                            state.paint_cam,
                            state.paint_mic,
                        );
                    }
                }

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGING => {
                let wp = &mut *(lparam.0 as *mut WINDOWPOS);
                wp.hwndInsertAfter = HWND_TOPMOST;
                wp.flags &= !SWP_NOZORDER;
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == FADE_TIMER => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayState;
                if state_ptr.is_null() {
                    let _ = KillTimer(Some(hwnd), FADE_TIMER);
                    return LRESULT(0);
                }
                let state = &mut *state_ptr;
                let delta = state.target_alpha as i16 - state.current_alpha as i16;
                if delta == 0 {
                    let _ = KillTimer(Some(hwnd), FADE_TIMER);
                    if state.current_alpha == 0 {
                        finish_fade_out(hwnd, state);
                    }
                    return LRESULT(0);
                }
                let step = delta.clamp(-FADE_STEP, FADE_STEP);
                state.current_alpha = (state.current_alpha as i16 + step).clamp(0, 255) as u8;
                let _ = SetLayeredWindowAttributes(
                    hwnd,
                    COLORREF(TRANSPARENT_KEY),
                    state.current_alpha,
                    LWA_COLORKEY | LWA_ALPHA,
                );
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn draw_border(hdc: HDC, w: i32, h: i32, t: i32, r: i32, cam: bool, mic: bool) {
    let null_brush = GetStockObject(NULL_BRUSH);
    let _ = SelectObject(hdc, null_brush);
    let _ = SetArcDirection(hdc, AD_CLOCKWISE);

    match (cam, mic) {
        (false, false) => {}
        (true, false) => stroke_rounded_rect(hdc, w, h, t, r, COLOR_BLUE),
        (false, true) => stroke_rounded_rect(hdc, w, h, t, r, COLOR_RED),
        (true, true) => stroke_rounded_rect(hdc, w, h, t, r, COLOR_PURPLE),
    }
}

unsafe fn stroke_rounded_rect(hdc: HDC, w: i32, h: i32, t: i32, r: i32, color: u32) {
    let pen = CreatePen(PS_SOLID, t, COLORREF(color));
    let old = SelectObject(hdc, pen.into());
    let _ = RoundRect(hdc, 0, 0, w, h, 2 * r, 2 * r);
    SelectObject(hdc, old);
    let _ = DeleteObject(pen.into());
}
