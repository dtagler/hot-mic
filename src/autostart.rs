use windows::core::*;
use windows::Win32::System::Registry::*;

use hotmic::{wide_bytes, wide_chars as wide};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "HotMic";

pub fn is_enabled() -> bool {
    unsafe {
        let mut hkey = HKEY::default();
        let path = wide(RUN_KEY);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return false;
        }
        let name = wide(VALUE_NAME);
        let mut size: u32 = 0;
        let r = RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        );
        let _ = RegCloseKey(hkey);
        r.is_ok()
    }
}

pub fn set_enabled(on: bool) -> Result<()> {
    unsafe {
        let mut hkey = HKEY::default();
        let path = wide(RUN_KEY);
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_WRITE,
            &mut hkey,
        )
        .ok()?;
        let name = wide(VALUE_NAME);
        let result = if on {
            let exe = current_exe_quoted();
            let bytes = wide_bytes(&exe);
            RegSetValueExW(hkey, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(&bytes))
        } else {
            RegDeleteValueW(hkey, PCWSTR(name.as_ptr()))
        };
        let _ = RegCloseKey(hkey);
        result.ok()
    }
}

fn current_exe_quoted() -> String {
    let p = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_default();
    format!("\"{}\"", p)
}
