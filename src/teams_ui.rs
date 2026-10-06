//! Read only the three known meeting controls in visible TeamsWebView windows.
//! An enabled hangup control establishes a joined call. Registry activity is
//! not a prerequisite, and no control is invoked or meeting content returned.

use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, IsWindowVisible};

use hotmic::{
    combine_teams_window_states, parse_teams_camera_button_name, parse_teams_mic_button_name,
    teams_window_state_from_controls, TeamsControl, TeamsUiState,
};

const TEAMS_WINDOW_CLASS: PCWSTR = w!("TeamsWebView");
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

        Ok(Self {
            automation,
            controls_condition,
            _apartment: apartment,
        })
    }

    /// Search matching windows for known control IDs, not desktop descendants
    /// or meeting content. Each poll replaces the prior observation rather than
    /// retaining device activity after the meeting controls disappear.
    pub fn scan(&self) -> TeamsUiState {
        let mut states = Vec::with_capacity(2);
        let mut previous = None;

        while let Ok(hwnd) =
            unsafe { FindWindowExW(None, previous, TEAMS_WINDOW_CLASS, PCWSTR::null()) }
        {
            previous = Some(hwnd);
            // Hidden WebViews can retain stale meeting controls. Minimized
            // windows still have WS_VISIBLE and must continue to be monitored.
            if !unsafe { IsWindowVisible(hwnd).as_bool() } {
                continue;
            }
            let window = match unsafe { self.automation.ElementFromHandle(hwnd) } {
                Ok(window) => window,
                Err(_) => {
                    states.push(None);
                    continue;
                }
            };
            states.push(self.read_window(&window).ok().flatten());
        }

        combine_teams_window_states(&states)
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
