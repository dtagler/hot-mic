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
pub fn should_start_debounce(was_active: bool, now_active: bool) -> bool {
    was_active && !now_active
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
}
