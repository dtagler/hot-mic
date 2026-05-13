use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;

use hotmic::{is_teams_subkey, parse_in_use, wide_chars as wide};

const WEBCAM_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";
const MIC_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DeviceState {
    pub cam: bool,
    /// True when at least one non-Teams app subkey holds the microphone.
    pub mic_non_teams: bool,
    /// True when at least one Teams subkey (`MSTeams_8wekyb3d8bbwe` or
    /// classic Squirrel path) holds the microphone, regardless of whether
    /// the user has muted in-app. The Teams Local API supplies the mute
    /// state separately.
    pub mic_teams: bool,
}

pub struct Watcher {
    keys: Vec<HKEY>,
    pub events: Vec<HANDLE>,
}

impl Watcher {
    pub fn new() -> Result<Self> {
        let mut keys = Vec::new();
        let mut events = Vec::new();

        for (root, path) in [
            (HKEY_CURRENT_USER, WEBCAM_PATH),
            (HKEY_CURRENT_USER, MIC_PATH),
            (HKEY_LOCAL_MACHINE, WEBCAM_PATH),
            (HKEY_LOCAL_MACHINE, MIC_PATH),
        ] {
            let mut hkey = HKEY::default();
            let wide = wide(path);
            let r = unsafe {
                RegOpenKeyExW(
                    root,
                    PCWSTR(wide.as_ptr()),
                    Some(0),
                    KEY_READ | KEY_NOTIFY,
                    &mut hkey,
                )
            };
            if r.is_err() {
                continue;
            }
            let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) };
            let Ok(event) = event else {
                unsafe {
                    let _ = RegCloseKey(hkey);
                }
                continue;
            };
            keys.push(hkey);
            events.push(event);
        }

        let w = Self { keys, events };
        w.arm_all();
        Ok(w)
    }

    pub fn arm_all(&self) {
        for (i, &hkey) in self.keys.iter().enumerate() {
            let _ = unsafe {
                RegNotifyChangeKeyValue(
                    hkey,
                    true,
                    REG_NOTIFY_CHANGE_LAST_SET
                        | REG_NOTIFY_CHANGE_NAME
                        | REG_NOTIFY_THREAD_AGNOSTIC,
                    Some(self.events[i]),
                    true,
                )
            };
            unsafe {
                let _ = ResetEvent(self.events[i]);
            }
        }
    }

    pub fn scan(&self) -> DeviceState {
        let cam = any_in_use(HKEY_CURRENT_USER, WEBCAM_PATH)
            || any_in_use(HKEY_LOCAL_MACHINE, WEBCAM_PATH);
        let mic_hkcu = scan_mic(HKEY_CURRENT_USER, MIC_PATH);
        let mic_hklm = scan_mic(HKEY_LOCAL_MACHINE, MIC_PATH);
        DeviceState {
            cam,
            mic_non_teams: mic_hkcu.0 || mic_hklm.0,
            mic_teams: mic_hkcu.1 || mic_hklm.1,
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        for &k in &self.keys {
            unsafe {
                let _ = RegCloseKey(k);
            }
        }
        for &e in &self.events {
            unsafe {
                let _ = CloseHandle(e);
            }
        }
    }
}

/// Webcam path: simple any-in-use answer (we don't split cam by app).
fn any_in_use(root: HKEY, path: &str) -> bool {
    let mut hkey = HKEY::default();
    let wide = wide(path);
    let r = unsafe { RegOpenKeyExW(root, PCWSTR(wide.as_ptr()), Some(0), KEY_READ, &mut hkey) };
    if r.is_err() {
        return false;
    }
    let result = walk(hkey);
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    result
}

/// Mic path: enumerate top-level app subkeys (and `NonPackaged` children),
/// classify each as Teams or non-Teams via `lib::is_teams_subkey`, and
/// return `(non_teams_active, teams_active)`. We stop scanning a branch as
/// soon as a single in-use leaf is found, but always cover both branches
/// so per-app fusion sees every active app.
fn scan_mic(root: HKEY, path: &str) -> (bool, bool) {
    let mut hkey = HKEY::default();
    let wide_path = wide(path);
    let r = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(wide_path.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if r.is_err() {
        return (false, false);
    }
    let mut non_teams = false;
    let mut teams = false;
    let mut i: u32 = 0;
    loop {
        let mut name = [0u16; 512];
        let mut name_len: u32 = name.len() as u32;
        let r = unsafe {
            RegEnumKeyExW(
                hkey,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(PWSTR::null()),
                None,
                None,
            )
        };
        if r.is_err() {
            if r == ERROR_MORE_DATA {
                i += 1;
                continue;
            }
            break;
        }
        let name_str = String::from_utf16_lossy(&name[..name_len as usize]);
        if name_str.eq_ignore_ascii_case("NonPackaged") {
            // Each child of NonPackaged is itself an app subkey (path-encoded).
            let mut nphk = HKEY::default();
            let r =
                unsafe { RegOpenKeyExW(hkey, PCWSTR(name.as_ptr()), Some(0), KEY_READ, &mut nphk) };
            if r.is_ok() {
                classify_children(nphk, &mut non_teams, &mut teams);
                unsafe {
                    let _ = RegCloseKey(nphk);
                }
            }
        } else {
            // Top-level packaged app subkey (e.g. MSTeams_8wekyb3d8bbwe).
            let mut sub = HKEY::default();
            let r =
                unsafe { RegOpenKeyExW(hkey, PCWSTR(name.as_ptr()), Some(0), KEY_READ, &mut sub) };
            if r.is_ok() {
                if walk(sub) {
                    if is_teams_subkey(&name_str) {
                        teams = true;
                    } else {
                        non_teams = true;
                    }
                }
                unsafe {
                    let _ = RegCloseKey(sub);
                }
            }
        }
        i += 1;
        if non_teams && teams {
            break;
        }
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    (non_teams, teams)
}

/// Enumerate one level of app subkeys under a parent (used for NonPackaged),
/// classify each by name, and recurse into the standard `walk` for per-app
/// in-use detection.
fn classify_children(parent: HKEY, non_teams: &mut bool, teams: &mut bool) {
    let mut i: u32 = 0;
    loop {
        let mut name = [0u16; 512];
        let mut name_len: u32 = name.len() as u32;
        let r = unsafe {
            RegEnumKeyExW(
                parent,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(PWSTR::null()),
                None,
                None,
            )
        };
        if r.is_err() {
            if r == ERROR_MORE_DATA {
                i += 1;
                continue;
            }
            break;
        }
        let name_str = String::from_utf16_lossy(&name[..name_len as usize]);
        let mut sub = HKEY::default();
        let r =
            unsafe { RegOpenKeyExW(parent, PCWSTR(name.as_ptr()), Some(0), KEY_READ, &mut sub) };
        if r.is_ok() {
            if walk(sub) {
                if is_teams_subkey(&name_str) {
                    *teams = true;
                } else {
                    *non_teams = true;
                }
            }
            unsafe {
                let _ = RegCloseKey(sub);
            }
        }
        i += 1;
        if *non_teams && *teams {
            break;
        }
    }
}

fn walk(hkey: HKEY) -> bool {
    if read_in_use(hkey) {
        return true;
    }
    let mut i: u32 = 0;
    loop {
        let mut name = [0u16; 512];
        let mut name_len: u32 = name.len() as u32;
        let r = unsafe {
            RegEnumKeyExW(
                hkey,
                i,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(PWSTR::null()),
                None,
                None,
            )
        };
        if r.is_err() {
            // ERROR_NO_MORE_ITEMS ends enumeration. ERROR_MORE_DATA means the key
            // name exceeded our 512-char buffer (Win32 caps key names at 255, so
            // this shouldn't happen in practice) — skip to the next index.
            if r == ERROR_MORE_DATA {
                i += 1;
                continue;
            }
            break;
        }
        let mut sub = HKEY::default();
        let r = unsafe { RegOpenKeyExW(hkey, PCWSTR(name.as_ptr()), Some(0), KEY_READ, &mut sub) };
        if r.is_ok() {
            let in_use = walk(sub);
            unsafe {
                let _ = RegCloseKey(sub);
            }
            if in_use {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn read_in_use(hkey: HKEY) -> bool {
    let name = wide("LastUsedTimeStop");
    let mut kind = REG_VALUE_TYPE::default();
    let mut data = [0u8; 8];
    let mut size: u32 = 8;
    let r = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(data.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return false;
    }
    parse_in_use(&data, size as usize)
}
