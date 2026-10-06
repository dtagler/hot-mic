//! Teams meeting activity that can recover from stale consent-store records.
//! No Win32 calls live here; the UI reader supplies short-lived observations.

use crate::{combine_teams_mic_button_states, resolve_teams_muted, visible_devices};

/// A fresh read of confirmed meeting controls. An unknown microphone label
/// cannot prove mute; camera activity requires an explicit camera-off action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TeamsUiState {
    pub in_meeting: bool,
    pub mic_muted: Option<bool>,
    pub camera_on: bool,
}

/// Resolved Teams observations, not a guarantee of hardware use or transmission.
/// A complete Local API state wins for meeting/mute; camera remains UI-derived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TeamsActivity {
    pub in_meeting: bool,
    pub muted: bool,
    pub camera_on: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamsControl {
    Hangup(bool),
    Microphone(Option<bool>),
    Camera(Option<bool>),
}

/// Keep positive evidence from this poll if a duplicate control fails to read.
/// An unidentified failure may be another microphone, so it blocks mute
/// suppression but does not erase an already confirmed meeting or camera.
/// Without confirmed hangup, preserve that failure as an unreadable window
/// rather than declaring it irrelevant to other windows' mute decisions.
pub fn teams_window_state_from_controls(controls: &[Option<TeamsControl>]) -> Option<TeamsUiState> {
    let mut has_hangup = false;
    let mut unreadable_control = false;
    let mut mic_buttons = Vec::with_capacity(1);
    let mut camera_buttons = Vec::with_capacity(1);
    for control in controls {
        match control {
            Some(TeamsControl::Hangup(enabled)) => has_hangup |= enabled,
            Some(TeamsControl::Microphone(muted)) => mic_buttons.push(*muted),
            Some(TeamsControl::Camera(on)) => camera_buttons.push(*on),
            None => {
                unreadable_control = true;
                mic_buttons.push(None);
            }
        }
    }
    if !has_hangup && unreadable_control {
        None
    } else {
        Some(teams_window_state(
            has_hangup,
            &mic_buttons,
            &camera_buttons,
        ))
    }
}

/// Like the microphone button, the video button names its next action.
pub fn parse_teams_camera_button_name(name: &str) -> Option<bool> {
    let lower = name.to_ascii_lowercase();
    let mut words = lower.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("turn"), Some("camera"), Some("off"))
        | (Some("turn"), Some("off"), Some("camera"))
        | (Some("stop"), Some("video"), _) => Some(true),
        (Some("turn"), Some("camera"), Some("on"))
        | (Some("turn"), Some("on"), Some("camera"))
        | (Some("start"), Some("video"), _) => Some(false),
        _ => None,
    }
}

/// An enabled hangup control distinguishes a joined call from a pre-join preview.
/// Duplicate controls fail safe toward an active device.
pub fn teams_window_state(
    has_hangup: bool,
    mic_buttons: &[Option<bool>],
    camera_buttons: &[Option<bool>],
) -> TeamsUiState {
    if !has_hangup {
        return TeamsUiState::default();
    }
    TeamsUiState {
        in_meeting: true,
        mic_muted: combine_teams_mic_button_states(mic_buttons),
        camera_on: camera_buttons.contains(&Some(true)),
    }
}

/// Successfully read non-meeting windows are irrelevant. Unreadable windows
/// contribute uncertainty, preventing unsafe mute suppression in another window
/// without establishing a meeting by themselves.
pub fn combine_teams_window_states(states: &[Option<TeamsUiState>]) -> TeamsUiState {
    let mut combined = TeamsUiState::default();
    let mut mic_states = Vec::with_capacity(states.len());
    for state in states {
        match state {
            Some(state) if state.in_meeting => {
                combined.in_meeting = true;
                combined.camera_on |= state.camera_on;
                mic_states.push(state.mic_muted);
            }
            None => mic_states.push(None),
            _ => {}
        }
    }
    combined.mic_muted = combine_teams_mic_button_states(&mic_states);
    combined
}

/// The API tuple is (in_meeting, is_muted), present only after a complete state
/// on the current connection. A known meeting end overrides stale UI controls.
pub fn resolve_teams_activity(
    local_api_state: Option<(bool, bool)>,
    ui: TeamsUiState,
) -> TeamsActivity {
    let in_meeting = local_api_state.map_or(ui.in_meeting, |(in_meeting, _)| in_meeting);
    TeamsActivity {
        in_meeting,
        muted: resolve_teams_muted(
            local_api_state.map(|(in_meeting, muted)| in_meeting && muted),
            ui.in_meeting,
            || ui.mic_muted,
        ),
        camera_on: in_meeting && ui.in_meeting && ui.camera_on,
    }
}

/// Teams supplies independent positive activity when consent-store timestamps
/// are stale. Its mute/camera state must never suppress another app's capture.
pub fn visible_devices_with_teams(
    registry_camera: bool,
    non_teams_mic: bool,
    teams_mic: bool,
    teams: TeamsActivity,
) -> (bool, bool) {
    visible_devices(
        registry_camera || (teams.in_meeting && teams.camera_on),
        non_teams_mic,
        teams_mic || teams.in_meeting,
        teams.muted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_meeting() -> TeamsUiState {
        TeamsUiState {
            in_meeting: true,
            mic_muted: Some(false),
            camera_on: true,
        }
    }

    #[test]
    fn stale_registry_does_not_hide_live_camera_and_microphone() {
        let teams = resolve_teams_activity(None, live_meeting());
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (true, true)
        );
    }

    #[test]
    fn camera_off_action_means_camera_is_on() {
        assert_eq!(
            parse_teams_camera_button_name("Turn camera off (Ctrl+Shift+O)"),
            Some(true)
        );
        assert_eq!(
            parse_teams_camera_button_name("Turn off camera"),
            Some(true)
        );
        assert_eq!(parse_teams_camera_button_name("Stop video"), Some(true));
    }

    #[test]
    fn camera_on_action_means_camera_is_off() {
        assert_eq!(
            parse_teams_camera_button_name("Turn camera on (Ctrl+Shift+O)"),
            Some(false)
        );
        assert_eq!(
            parse_teams_camera_button_name("Turn on camera"),
            Some(false)
        );
        assert_eq!(parse_teams_camera_button_name("Start video"), Some(false));
    }

    #[test]
    fn camera_parser_tolerates_case_and_whitespace() {
        assert_eq!(
            parse_teams_camera_button_name("  TURN  CAMERA  OFF  "),
            Some(true)
        );
    }

    #[test]
    fn unknown_camera_labels_do_not_invent_activity() {
        for name in [
            "",
            "Camera settings",
            "Turn camera offline",
            "Video effects",
        ] {
            assert_eq!(parse_teams_camera_button_name(name), None);
        }
    }

    #[test]
    fn preview_controls_without_hangup_are_not_a_meeting() {
        assert_eq!(
            teams_window_state(false, &[Some(false)], &[Some(true)]),
            TeamsUiState::default()
        );
    }

    #[test]
    fn meeting_controls_supply_device_state() {
        assert_eq!(
            teams_window_state(true, &[Some(false)], &[Some(true)]),
            live_meeting()
        );
    }

    #[test]
    fn conflicting_buttons_in_one_meeting_fail_safe_to_live() {
        assert_eq!(
            teams_window_state(true, &[Some(true), Some(false)], &[Some(false), Some(true)]),
            live_meeting()
        );
    }

    #[test]
    fn unreadable_duplicate_does_not_erase_confirmed_live_controls() {
        let controls = [
            Some(TeamsControl::Hangup(true)),
            Some(TeamsControl::Microphone(Some(false))),
            Some(TeamsControl::Camera(Some(true))),
            None,
        ];
        assert_eq!(
            teams_window_state_from_controls(&controls),
            Some(live_meeting())
        );
    }

    #[test]
    fn unreadable_control_before_live_controls_also_preserves_activity() {
        let controls = [
            None,
            Some(TeamsControl::Hangup(true)),
            Some(TeamsControl::Microphone(Some(false))),
            Some(TeamsControl::Camera(Some(true))),
        ];
        assert_eq!(
            teams_window_state_from_controls(&controls),
            Some(live_meeting())
        );
    }

    #[test]
    fn unreadable_duplicate_prevents_unsafe_mute_suppression() {
        let controls = [
            Some(TeamsControl::Hangup(true)),
            Some(TeamsControl::Microphone(Some(true))),
            Some(TeamsControl::Camera(Some(true))),
            None,
        ];
        assert_eq!(
            teams_window_state_from_controls(&controls),
            Some(TeamsUiState {
                in_meeting: true,
                mic_muted: None,
                camera_on: true,
            })
        );
    }

    #[test]
    fn unreadable_hangup_does_not_promote_preview_device_controls() {
        let controls = [
            None,
            Some(TeamsControl::Microphone(Some(false))),
            Some(TeamsControl::Camera(Some(true))),
        ];
        assert_eq!(teams_window_state_from_controls(&controls), None);
    }

    #[test]
    fn unreadable_unconfirmed_window_blocks_another_windows_mute() {
        let muted = teams_window_state(true, &[Some(true)], &[Some(false)]);
        let unreadable = teams_window_state_from_controls(&[
            Some(TeamsControl::Microphone(Some(false))),
            Some(TeamsControl::Camera(Some(true))),
            None,
        ]);
        let combined = combine_teams_window_states(&[Some(muted), unreadable]);
        assert_eq!(combined.mic_muted, None);
        assert_eq!(
            visible_devices_with_teams(false, false, true, resolve_teams_activity(None, combined)),
            (false, true)
        );
    }

    #[test]
    fn unreadable_unconfirmed_window_alone_cannot_invent_a_meeting() {
        let unreadable = teams_window_state_from_controls(&[
            Some(TeamsControl::Microphone(Some(false))),
            Some(TeamsControl::Camera(Some(true))),
            None,
        ]);
        let combined = combine_teams_window_states(&[unreadable]);
        assert_eq!(combined, TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(false, false, false, resolve_teams_activity(None, combined)),
            (false, false)
        );
    }

    #[test]
    fn missing_controls_do_not_invent_camera_activity_or_mute() {
        assert_eq!(
            teams_window_state(true, &[], &[]),
            TeamsUiState {
                in_meeting: true,
                mic_muted: None,
                camera_on: false,
            }
        );
    }

    #[test]
    fn no_meeting_windows_clear_all_fallback_activity() {
        assert_eq!(combine_teams_window_states(&[]), TeamsUiState::default());
        assert_eq!(
            combine_teams_window_states(&[Some(TeamsUiState::default()), None]),
            TeamsUiState::default()
        );
    }

    #[test]
    fn unrelated_teams_windows_do_not_invalidate_a_muted_meeting() {
        let muted = teams_window_state(true, &[Some(true)], &[Some(false)]);
        assert_eq!(
            combine_teams_window_states(&[Some(muted), Some(TeamsUiState::default())]),
            muted
        );
    }

    #[test]
    fn unreadable_window_prevents_suppressing_a_possibly_live_microphone() {
        let muted = teams_window_state(true, &[Some(true)], &[Some(true)]);
        assert_eq!(
            combine_teams_window_states(&[Some(muted), None]),
            TeamsUiState {
                in_meeting: true,
                mic_muted: None,
                camera_on: true,
            }
        );
    }

    #[test]
    fn duplicate_meetings_keep_any_live_device_visible() {
        let muted = teams_window_state(true, &[Some(true)], &[Some(false)]);
        assert_eq!(
            combine_teams_window_states(&[Some(muted), Some(live_meeting())]),
            live_meeting()
        );
    }

    #[test]
    fn muted_meeting_with_camera_on_is_blue_despite_stale_registry() {
        let ui = teams_window_state(true, &[Some(true)], &[Some(true)]);
        assert_eq!(
            visible_devices_with_teams(false, false, false, resolve_teams_activity(None, ui)),
            (true, false)
        );
    }

    #[test]
    fn live_microphone_with_camera_off_is_red_despite_stale_registry() {
        let ui = teams_window_state(true, &[Some(false)], &[Some(false)]);
        assert_eq!(
            visible_devices_with_teams(false, false, false, resolve_teams_activity(None, ui)),
            (false, true)
        );
    }

    #[test]
    fn muted_meeting_with_camera_off_has_no_border() {
        let ui = teams_window_state(true, &[Some(true)], &[Some(false)]);
        assert_eq!(
            visible_devices_with_teams(false, false, false, resolve_teams_activity(None, ui)),
            (false, false)
        );
    }

    #[test]
    fn unknown_mic_label_in_confirmed_meeting_fails_safe_to_live() {
        let ui = teams_window_state(true, &[None], &[None]);
        assert_eq!(
            visible_devices_with_teams(false, false, false, resolve_teams_activity(None, ui)),
            (false, true)
        );
    }

    #[test]
    fn unknown_ui_does_not_invent_a_meeting_when_registry_is_idle() {
        let teams = resolve_teams_activity(None, TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (false, false)
        );
    }

    #[test]
    fn ending_a_meeting_drops_previous_fallback_activity() {
        let before = resolve_teams_activity(None, live_meeting());
        let after = resolve_teams_activity(None, combine_teams_window_states(&[]));
        assert_eq!(
            visible_devices_with_teams(false, false, false, before),
            (true, true)
        );
        assert_eq!(
            visible_devices_with_teams(false, false, false, after),
            (false, false)
        );
    }

    #[test]
    fn non_teams_microphone_survives_teams_mute() {
        let ui = teams_window_state(true, &[Some(true)], &[Some(false)]);
        let teams = resolve_teams_activity(None, ui);
        assert_eq!(
            visible_devices_with_teams(false, true, false, teams),
            (false, true)
        );
    }

    #[test]
    fn registry_camera_is_not_suppressed_by_teams_camera_off() {
        let ui = teams_window_state(true, &[Some(true)], &[Some(false)]);
        let teams = resolve_teams_activity(None, ui);
        assert_eq!(
            visible_devices_with_teams(true, false, false, teams),
            (true, false)
        );
    }

    #[test]
    fn unreadable_ui_preserves_registry_microphone_detection() {
        let teams = resolve_teams_activity(None, TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(false, false, true, teams),
            (false, true)
        );
    }

    #[test]
    fn muted_label_without_confirmed_hangup_keeps_registry_mic_visible() {
        for hangup in [None, Some(TeamsControl::Hangup(false))] {
            let mut controls = vec![Some(TeamsControl::Microphone(Some(true)))];
            if let Some(hangup) = hangup {
                controls.push(Some(hangup));
            }
            let window = teams_window_state_from_controls(&controls);
            assert_eq!(window, Some(TeamsUiState::default()));
            let ui = combine_teams_window_states(&[window]);
            let teams = resolve_teams_activity(None, ui);
            assert_eq!(
                visible_devices_with_teams(false, false, true, teams),
                (false, true)
            );
        }
    }

    #[test]
    fn authoritative_api_mute_wins_over_live_ui_microphone() {
        let teams = resolve_teams_activity(Some((true, true)), live_meeting());
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (true, false)
        );
    }

    #[test]
    fn authoritative_api_unmute_wins_over_muted_ui_microphone() {
        let ui = teams_window_state(true, &[Some(true)], &[Some(false)]);
        let teams = resolve_teams_activity(Some((true, false)), ui);
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (false, true)
        );
    }

    #[test]
    fn authoritative_api_meeting_end_overrides_stale_ui_controls() {
        let teams = resolve_teams_activity(Some((false, true)), live_meeting());
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (false, false)
        );
    }

    #[test]
    fn local_api_also_recovers_microphone_activity_without_registry_or_ui() {
        let teams = resolve_teams_activity(Some((true, false)), TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(false, false, false, teams),
            (false, true)
        );
    }

    #[test]
    fn fallback_resumes_when_local_api_becomes_unavailable() {
        let muted = resolve_teams_activity(Some((true, true)), live_meeting());
        let fallback = resolve_teams_activity(None, live_meeting());
        assert_eq!(
            visible_devices_with_teams(false, false, false, muted),
            (true, false)
        );
        assert_eq!(
            visible_devices_with_teams(false, false, false, fallback),
            (true, true)
        );
    }
}
