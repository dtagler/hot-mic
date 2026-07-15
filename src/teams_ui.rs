use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;

use hotmic::{combine_teams_mic_button_states, parse_teams_mic_button_name};

const TEAMS_WINDOW_CLASS: PCWSTR = w!("TeamsWebView");
const MICROPHONE_BUTTON_ID: &str = "microphone-button";

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct MuteDetector {
    automation: IUIAutomation,
    microphone_condition: IUIAutomationCondition,
    // Must remain after the COM interfaces so they are released before the
    // apartment's Drop calls CoUninitialize.
    _apartment: ComApartment,
}

impl MuteDetector {
    pub fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        let apartment = ComApartment;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let microphone_id = VARIANT::from(MICROPHONE_BUTTON_ID);
        let microphone_condition = unsafe {
            automation.CreatePropertyCondition(UIA_AutomationIdPropertyId, &microphone_id)?
        };

        Ok(Self {
            automation,
            microphone_condition,
            _apartment: apartment,
        })
    }

    pub fn muted_now(&self) -> Option<bool> {
        let mut states = Vec::with_capacity(2);
        let mut previous = None;

        while let Ok(hwnd) =
            unsafe { FindWindowExW(None, previous, TEAMS_WINDOW_CLASS, PCWSTR::null()) }
        {
            previous = Some(hwnd);
            let window = match unsafe { self.automation.ElementFromHandle(hwnd) } {
                Ok(window) => window,
                Err(_) => {
                    states.push(None);
                    continue;
                }
            };
            let button = match unsafe {
                window.FindFirst(TreeScope_Descendants, &self.microphone_condition)
            } {
                Ok(button) => button,
                // FindFirst reports S_OK with a null element when this window has
                // no mic button. windows-rs represents that result as an empty
                // error, so the window is irrelevant rather than unreadable.
                Err(error) if error.code().is_ok() => continue,
                Err(_) => {
                    states.push(None);
                    continue;
                }
            };
            let name = match unsafe { button.CurrentName() } {
                Ok(name) => name,
                Err(_) => {
                    states.push(None);
                    continue;
                }
            };
            states.push(parse_teams_mic_button_name(&name.to_string()));
        }

        combine_teams_mic_button_states(&states)
    }
}
