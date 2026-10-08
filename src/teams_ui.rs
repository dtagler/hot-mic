//! Read only known meeting controls in desktop Teams and trusted browser pages.
//! Browser documents must have a recognized Teams HTTPS origin before their
//! controls are queried. No control is invoked or meeting content returned.

use windows::core::*;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, IsWindowVisible,
};

use hotmic::{
    combine_teams_window_states, is_teams_browser_process, is_teams_web_url,
    parse_teams_camera_button_name, parse_teams_mic_button_name, teams_window_state_from_controls,
    top_level_browser_documents, TeamsControl, TeamsUiSnapshot, TeamsUiState,
};

const TEAMS_WINDOW_CLASS: PCWSTR = w!("TeamsWebView");
const BROWSER_WINDOW_CLASS: PCWSTR = w!("Chrome_WidgetWin_1");
const MICROPHONE_BUTTON_ID: &str = "microphone-button";
const VIDEO_BUTTON_ID: &str = "video-button";
const HANGUP_BUTTON_ID: &str = "hangup-button";

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct MeetingDetector {
    automation: IUIAutomation,
    controls_condition: IUIAutomationCondition,
    all_elements_condition: IUIAutomationCondition,
    // Must remain after the COM interfaces so they are released before the
    // apartment's Drop calls CoUninitialize.
    _apartment: ComApartment,
}

impl MeetingDetector {
    pub fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let apartment = ComApartment;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let condition = |id: &str| unsafe {
            automation.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(id))
        };
        let microphone = condition(MICROPHONE_BUTTON_ID)?;
        let video = condition(VIDEO_BUTTON_ID)?;
        let hangup = condition(HANGUP_BUTTON_ID)?;
        let controls_condition = unsafe {
            let devices = automation.CreateOrCondition(&microphone, &video)?;
            automation.CreateOrCondition(&devices, &hangup)?
        };
        let all_elements_condition = unsafe { automation.CreateTrueCondition()? };

        Ok(Self {
            automation,
            controls_condition,
            all_elements_condition,
            _apartment: apartment,
        })
    }

    /// Search matching windows for known control IDs, not desktop descendants
    /// or meeting content. Each poll replaces the prior observation rather than
    /// retaining device activity after the meeting controls disappear.
    pub fn scan(&self) -> TeamsUiSnapshot {
        let mut desktop = Vec::with_capacity(2);
        let mut browser = Vec::with_capacity(2);

        for (class, is_browser) in [(TEAMS_WINDOW_CLASS, false), (BROWSER_WINDOW_CLASS, true)] {
            let mut previous = None;
            while let Ok(hwnd) = unsafe { FindWindowExW(None, previous, class, PCWSTR::null()) } {
                previous = Some(hwnd);
                // Hidden windows can retain stale controls. Minimized windows
                // still have WS_VISIBLE and must continue to be monitored.
                if !unsafe { IsWindowVisible(hwnd).as_bool() }
                    || (is_browser && !is_browser_window(hwnd))
                {
                    continue;
                }
                let window = match unsafe { self.automation.ElementFromHandle(hwnd) } {
                    Ok(window) => window,
                    Err(_) => {
                        // Without a verified Teams document, an unrelated
                        // browser failure says nothing about Teams mute.
                        if !is_browser {
                            desktop.push(None);
                        }
                        continue;
                    }
                };
                if is_browser {
                    self.read_browser_window(&window, &mut browser);
                } else {
                    desktop.push(self.read_window(&window).ok().flatten());
                }
            }
        }

        TeamsUiSnapshot {
            desktop: combine_teams_window_states(&desktop),
            browser: combine_teams_window_states(&browser),
        }
    }

    fn read_browser_window(
        &self,
        window: &IUIAutomationElement,
        states: &mut Vec<Option<TeamsUiState>>,
    ) {
        // Each query is explicitly scoped to a parent's immediate children.
        // A document-only TreeWalker would flatten away the browser window,
        // letting "next sibling" escape into another application's documents.
        let documents = top_level_browser_documents(
            window.clone(),
            |parent| self.read_children(parent),
            |node| {
                unsafe { node.CurrentControlType() }
                    .ok()
                    .map(|kind| kind == UIA_DocumentControlTypeId)
            },
        );
        for page in documents {
            let is_teams = unsafe {
                page.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .and_then(|pattern| pattern.CurrentValue())
            }
            .is_ok_and(|url| is_teams_web_url(&url.to_string()));
            if is_teams {
                states.push(self.read_window(&page).ok().flatten());
            }
        }
    }

    fn read_children(&self, parent: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
        let Ok(children) =
            (unsafe { parent.FindAll(TreeScope_Children, &self.all_elements_condition) })
        else {
            return Vec::new();
        };
        let Ok(length) = (unsafe { children.Length() }) else {
            return Vec::new();
        };
        (0..length)
            .filter_map(|index| unsafe { children.GetElement(index) }.ok())
            .collect()
    }

    fn read_window(&self, window: &IUIAutomationElement) -> Result<Option<TeamsUiState>> {
        let controls =
            match unsafe { window.FindAll(TreeScope_Descendants, &self.controls_condition) } {
                Ok(controls) => controls,
                // windows-rs can represent S_OK with a null result as an empty
                // error. No controls is different from a provider read failure.
                Err(error) if error.code().is_ok() => {
                    return Ok(Some(TeamsUiState::default()));
                }
                Err(error) => return Err(error),
            };
        let mut observations = Vec::new();
        for index in 0..unsafe { controls.Length()? } {
            // A stale duplicate must not discard controls already read from
            // this window. Preserve the uncertainty for the pure reducer.
            observations.push(
                unsafe { controls.GetElement(index) }
                    .and_then(|button| Self::read_control(&button))
                    .ok()
                    .flatten(),
            );
        }
        Ok(teams_window_state_from_controls(&observations))
    }

    fn read_control(button: &IUIAutomationElement) -> Result<Option<TeamsControl>> {
        let id = unsafe { button.CurrentAutomationId()? }.to_string();
        Ok(match id.as_str() {
            HANGUP_BUTTON_ID => Some(TeamsControl::Hangup(
                unsafe { button.CurrentIsEnabled()? }.as_bool(),
            )),
            MICROPHONE_BUTTON_ID => {
                let muted = unsafe { button.CurrentName() }
                    .ok()
                    .and_then(|name| parse_teams_mic_button_name(&name.to_string()));
                Some(TeamsControl::Microphone(muted))
            }
            VIDEO_BUTTON_ID => {
                let camera_on = unsafe { button.CurrentName() }
                    .ok()
                    .and_then(|name| parse_teams_camera_button_name(&name.to_string()));
                Some(TeamsControl::Camera(camera_on))
            }
            _ => None,
        })
    }
}

/// Read process identity with minimum query rights. Do not inspect command
/// lines or keep process handles alive across polls.
fn is_browser_window(hwnd: HWND) -> bool {
    let mut process_id = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    if process_id == 0 {
        return false;
    }
    let Ok(process) =
        (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) })
    else {
        return false;
    };
    let mut image_path = vec![0u16; 32768];
    let mut length = image_path.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(image_path.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    result.is_ok()
        && is_teams_browser_process(&String::from_utf16_lossy(&image_path[..length as usize]))
}
