//! Platform-independent rules for passive native capture observations.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

const NATIVE_MAX_AGE: Duration = Duration::from_secs(3);

pub fn should_retry_native_camera(
    available: bool,
    last_attempt: Option<Instant>,
    now: Instant,
) -> bool {
    !available
        && last_attempt.is_none_or(|attempt| {
            now.checked_duration_since(attempt)
                .is_some_and(|age| age >= Duration::from_secs(5))
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeCaptureState {
    pub camera: bool,
    pub microphone: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeCaptureUpdate {
    pub camera: Option<bool>,
    pub microphone: Option<bool>,
}

impl From<NativeCaptureState> for NativeCaptureUpdate {
    fn from(state: NativeCaptureState) -> Self {
        Self {
            camera: Some(state.camera),
            microphone: Some(state.microphone),
        }
    }
}

#[derive(Default)]
pub struct NativeCaptureCache {
    camera: Option<(bool, Instant)>,
    microphone: Option<(bool, Instant)>,
}

impl NativeCaptureCache {
    pub fn update(&mut self, update: impl Into<NativeCaptureUpdate>, observed_at: Instant) {
        let update = update.into();
        if let Some(camera) = update.camera {
            self.camera = Some((camera, observed_at));
        }
        if let Some(microphone) = update.microphone {
            self.microphone = Some((microphone, observed_at));
        }
    }

    /// A hung provider must not latch a native positive indefinitely. Expiry
    /// only removes this source's contribution; registry/Teams stay independent.
    pub fn current(&self, now: Instant) -> NativeCaptureState {
        let current = |latest: Option<(bool, Instant)>| {
            latest
                .filter(|(_, observed_at)| {
                    now.checked_duration_since(*observed_at)
                        .is_some_and(|age| age < NATIVE_MAX_AGE)
                })
                .map(|(state, _)| state)
                .unwrap_or(false)
        };
        NativeCaptureState {
            camera: current(self.camera),
            microphone: current(self.microphone),
        }
    }
}

#[derive(Default)]
pub struct CaptureSample {
    flags: AtomicU8,
}

impl CaptureSample {
    /// Positive evidence wins for this sampling window. Each new window owns
    /// a fresh sample, so delayed callbacks cannot contaminate a later window.
    pub fn observe(&self, streaming: Option<bool>) {
        let flags = match streaming {
            Some(true) => 0b011, // A known reading and a positive.
            Some(false) => 0b001,
            None => 0b100, // An unreadable entry must not become fresh idle.
        };
        self.flags.fetch_or(flags, Ordering::Release);
    }

    pub fn is_active(&self) -> bool {
        self.observation() == Some(true)
    }

    pub fn observation(&self) -> Option<bool> {
        // Read one atomic word so concurrent callbacks cannot produce a torn
        // combination of "observed", "active", and "unknown" flags.
        let flags = self.flags.load(Ordering::Acquire);
        if flags & 0b010 != 0 {
            Some(true)
        } else if flags & 0b100 != 0 || flags & 0b001 == 0 {
            None
        } else {
            Some(false)
        }
    }
}

/// The existing Teams/UI paths own mute semantics for these engines. Their
/// raw capture sessions can remain open while muted. Unknown/shared ownership
/// cannot safely establish that exception. No registry signal is suppressed.
pub fn native_microphone_is_visible(
    active: bool,
    process_image: Option<&str>,
    unique_owner: bool,
) -> bool {
    if !active {
        return false;
    }
    if !unique_owner {
        return true;
    }
    let executable = process_image
        .and_then(|path| path.rsplit(['\\', '/']).next())
        .unwrap_or_default();
    ![
        "Teams.exe",
        "ms-teams.exe",
        "msedge.exe",
        "chrome.exe",
        "msedgewebview2.exe",
    ]
    .iter()
    .any(|handled| executable.eq_ignore_ascii_case(handled))
}

/// Native observations supplement registry activity; they never erase it.
pub fn supplement_registry_capture(
    camera: bool,
    non_teams_mic: bool,
    native: NativeCaptureState,
) -> (bool, bool) {
    (camera || native.camera, non_teams_mic || native.microphone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resolve_teams_activity, resolve_teams_activity_sources, teams_window_state,
        visible_devices_with_teams, TeamsActivity, TeamsUiSnapshot, TeamsUiState,
    };
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn camera_sample_starts_without_positive_evidence() {
        assert!(!CaptureSample::default().is_active());
    }

    #[test]
    fn camera_sample_latches_streaming() {
        let sample = CaptureSample::default();
        sample.observe(Some(true));
        assert!(sample.is_active());
    }

    #[test]
    fn inactive_or_unreadable_entries_do_not_erase_a_live_camera() {
        let sample = CaptureSample::default();
        sample.observe(Some(true));
        sample.observe(None);
        sample.observe(Some(false));
        assert!(sample.is_active());
    }

    #[test]
    fn inactive_and_unknown_observations_do_not_invent_activity() {
        let sample = CaptureSample::default();
        sample.observe(None);
        sample.observe(Some(false));
        assert!(!sample.is_active());
    }

    #[test]
    fn new_camera_window_does_not_retain_previous_activity() {
        let previous = CaptureSample::default();
        previous.observe(Some(true));
        assert!(previous.is_active());
        assert!(!CaptureSample::default().is_active());
    }

    #[test]
    fn late_previous_camera_callback_cannot_contaminate_new_window() {
        let previous = Arc::new(CaptureSample::default());
        let late_callback = previous.clone();
        let current = Arc::new(CaptureSample::default());
        std::thread::spawn(move || late_callback.observe(Some(true)))
            .join()
            .unwrap();
        assert!(previous.is_active());
        assert!(!current.is_active());
    }

    #[test]
    fn native_camera_recovers_blue_from_stale_registry() {
        let (cam, mic) = supplement_registry_capture(
            false,
            false,
            NativeCaptureState {
                camera: true,
                microphone: false,
            },
        );
        assert_eq!(
            visible_devices_with_teams(cam, mic, false, TeamsActivity::default()),
            (true, false)
        );
    }

    #[test]
    fn native_recorder_recovers_red_from_stale_registry() {
        let (cam, mic) = supplement_registry_capture(
            false,
            false,
            NativeCaptureState {
                camera: false,
                microphone: true,
            },
        );
        assert_eq!(
            visible_devices_with_teams(cam, mic, false, TeamsActivity::default()),
            (false, true)
        );
    }

    #[test]
    fn native_camera_and_recorder_recover_purple_from_stale_registry() {
        let (cam, mic) = supplement_registry_capture(
            false,
            false,
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
        );
        assert_eq!(
            visible_devices_with_teams(cam, mic, false, TeamsActivity::default()),
            (true, true)
        );
    }

    #[test]
    fn fresh_idle_native_sample_clears_old_native_activity() {
        let active = NativeCaptureState {
            camera: true,
            microphone: true,
        };
        assert_eq!(
            supplement_registry_capture(false, false, active),
            (true, true)
        );
        assert_eq!(
            supplement_registry_capture(false, false, NativeCaptureState::default()),
            (false, false)
        );
    }

    #[test]
    fn native_idle_never_clears_positive_registry_activity() {
        assert_eq!(
            supplement_registry_capture(true, true, NativeCaptureState::default()),
            (true, true)
        );
    }

    #[test]
    fn native_recorder_is_not_muted_by_desktop_teams() {
        let native = NativeCaptureState {
            camera: false,
            microphone: native_microphone_is_visible(true, Some(r"C:\Apps\SoundRec.exe"), true),
        };
        let (cam, mic) = supplement_registry_capture(false, false, native);
        let teams = resolve_teams_activity(Some((true, true)), TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(cam, mic, true, teams),
            (false, true)
        );
    }

    #[test]
    fn native_camera_survives_desktop_meeting_end() {
        let (cam, mic) = supplement_registry_capture(
            false,
            false,
            NativeCaptureState {
                camera: true,
                microphone: false,
            },
        );
        let teams = resolve_teams_activity(Some((false, true)), TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(cam, mic, false, teams),
            (true, false)
        );
    }

    #[test]
    fn handled_apps_keep_their_existing_mute_rules() {
        for path in [
            r"C:\Apps\Teams.exe",
            r"C:\Apps\ms-teams.exe",
            r"C:\Apps\msedge.exe",
            r"C:\Apps\chrome.exe",
            r"C:\Apps\msedgewebview2.exe",
            r"C:\Apps\CHROME.EXE",
        ] {
            assert!(
                !native_microphone_is_visible(true, Some(path), true),
                "{path}"
            );
        }
    }

    #[test]
    fn inactive_sessions_never_establish_microphone_use() {
        for image in [
            None,
            Some(r"C:\Apps\SoundRec.exe"),
            Some(r"C:\Apps\chrome.exe"),
        ] {
            for unique in [true, false] {
                assert!(!native_microphone_is_visible(false, image, unique));
            }
        }
    }

    #[test]
    fn ordinary_native_app_capture_is_reported() {
        for image in [
            r"C:\Apps\SoundRec.exe",
            r"C:\Apps\WindowsCamera.exe",
            r"C:\Apps\Audacity.exe",
        ] {
            assert!(
                native_microphone_is_visible(true, Some(image), true),
                "{image}"
            );
        }
    }

    #[test]
    fn failed_process_reads_do_not_hide_active_microphones() {
        assert!(native_microphone_is_visible(true, None, true));
        assert!(native_microphone_is_visible(true, Some(""), true));
    }

    #[test]
    fn shared_process_sessions_do_not_hide_other_capture() {
        assert!(native_microphone_is_visible(
            true,
            Some(r"C:\Apps\chrome.exe"),
            false
        ));
        assert!(native_microphone_is_visible(true, None, false));
    }

    #[test]
    fn owner_classification_uses_only_the_executable_basename() {
        assert!(native_microphone_is_visible(
            true,
            Some(r"C:\chrome.exe\unrelated.exe"),
            true
        ));
        assert!(native_microphone_is_visible(
            true,
            Some(r"C:\Apps\chrome.exe.backup"),
            true
        ));
    }

    #[test]
    fn browser_color_matrix_survives_an_open_browser_audio_session() {
        for (muted, camera, expected) in [
            (false, true, (true, true)),
            (true, true, (true, false)),
            (false, false, (false, true)),
            (true, false, (false, false)),
        ] {
            let native = NativeCaptureState {
                camera: false,
                microphone: native_microphone_is_visible(true, Some(r"C:\Apps\msedge.exe"), true),
            };
            let (cam, mic) = supplement_registry_capture(false, false, native);
            let teams = resolve_teams_activity_sources(
                Some((false, false)),
                TeamsUiSnapshot {
                    browser: teams_window_state(true, &[Some(muted)], &[Some(camera)]),
                    ..Default::default()
                },
            );
            assert_eq!(visible_devices_with_teams(cam, mic, false, teams), expected);
        }
    }

    #[test]
    fn native_cache_keeps_recent_observations() {
        let now = Instant::now();
        let active = NativeCaptureState {
            camera: true,
            microphone: true,
        };
        let mut cache = NativeCaptureCache::default();
        cache.update(active, now);
        assert_eq!(cache.current(now + Duration::from_millis(2999)), active);
    }

    #[test]
    fn native_cache_expires_stalled_provider_activity() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
            now,
        );
        assert_eq!(
            cache.current(now + Duration::from_secs(3)),
            NativeCaptureState::default()
        );
    }

    #[test]
    fn delayed_publication_cannot_make_old_observations_fresh() {
        let started = Instant::now();
        let published = started + Duration::from_secs(10);
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: false,
            },
            started,
        );
        assert_eq!(cache.current(published), NativeCaptureState::default());
    }

    #[test]
    fn expired_native_activity_starts_the_normal_off_debounce() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: false,
            },
            now,
        );
        let old = supplement_registry_capture(false, false, cache.current(now));
        let next =
            supplement_registry_capture(false, false, cache.current(now + Duration::from_secs(3)));
        assert_eq!(old, (true, false));
        assert_eq!(next, (false, false));
        assert!(crate::should_start_debounce(
            old.0 || old.1,
            next.0 || next.1
        ));
    }

    #[test]
    fn expired_native_activity_never_suppresses_registry_or_teams() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
            now,
        );
        let expired = cache.current(now + Duration::from_secs(3));
        assert_eq!(
            supplement_registry_capture(true, true, expired),
            (true, true)
        );
        let (cam, mic) = supplement_registry_capture(false, false, expired);
        let teams = resolve_teams_activity(Some((true, false)), TeamsUiState::default());
        assert_eq!(
            visible_devices_with_teams(cam, mic, false, teams),
            (false, true)
        );
    }

    #[test]
    fn fresh_idle_update_clears_activity_without_waiting_for_expiry() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
            now,
        );
        cache.update(
            NativeCaptureState::default(),
            now + Duration::from_millis(500),
        );
        assert_eq!(
            cache.current(now + Duration::from_millis(501)),
            NativeCaptureState::default()
        );
    }

    #[test]
    fn fresh_updates_resume_after_native_cache_expiry() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        let active = NativeCaptureState {
            camera: true,
            microphone: false,
        };
        cache.update(active, now);
        assert_eq!(
            cache.current(now + Duration::from_secs(3)),
            NativeCaptureState::default()
        );
        cache.update(active, now + Duration::from_secs(4));
        assert_eq!(cache.current(now + Duration::from_millis(4500)), active);
    }

    #[test]
    fn future_dated_native_observations_are_not_fresh() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
            now + Duration::from_secs(1),
        );
        assert_eq!(cache.current(now), NativeCaptureState::default());
    }

    #[test]
    fn empty_capture_sample_remains_unknown() {
        assert_eq!(CaptureSample::default().observation(), None);
    }

    #[test]
    fn unreadable_capture_sample_is_not_idle() {
        let sample = CaptureSample::default();
        sample.observe(None);
        assert_eq!(sample.observation(), None);
    }

    #[test]
    fn partial_failure_does_not_claim_all_devices_idle() {
        let sample = CaptureSample::default();
        sample.observe(Some(false));
        sample.observe(None);
        assert_eq!(sample.observation(), None);
    }

    #[test]
    fn positive_capture_wins_over_partial_failures() {
        let sample = CaptureSample::default();
        sample.observe(None);
        sample.observe(Some(true));
        assert_eq!(sample.observation(), Some(true));
    }

    #[test]
    fn known_idle_capture_is_reported() {
        let sample = CaptureSample::default();
        sample.observe(Some(false));
        assert_eq!(sample.observation(), Some(false));
    }

    #[test]
    fn error_update_preserves_recent_state_without_refreshing_expiry() {
        let now = Instant::now();
        let active = NativeCaptureState {
            camera: true,
            microphone: true,
        };
        let mut cache = NativeCaptureCache::default();
        cache.update(active, now);
        cache.update(NativeCaptureUpdate::default(), now + Duration::from_secs(1));
        assert_eq!(cache.current(now + Duration::from_millis(1100)), active);
        assert_eq!(
            cache.current(now + Duration::from_secs(3)),
            NativeCaptureState::default()
        );
    }

    #[test]
    fn camera_error_does_not_discard_fresh_microphone_activity() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: false,
            },
            now,
        );
        cache.update(
            NativeCaptureUpdate {
                camera: None,
                microphone: Some(true),
            },
            now + Duration::from_secs(1),
        );
        assert_eq!(
            cache.current(now + Duration::from_millis(1100)),
            NativeCaptureState {
                camera: true,
                microphone: true
            }
        );
        assert_eq!(
            cache.current(now + Duration::from_secs(3)),
            NativeCaptureState {
                camera: false,
                microphone: true
            }
        );
    }

    #[test]
    fn fresh_camera_does_not_renew_stale_microphone_activity() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: false,
                microphone: true,
            },
            now,
        );
        cache.update(
            NativeCaptureUpdate {
                camera: Some(true),
                microphone: None,
            },
            now + Duration::from_secs(2),
        );
        assert_eq!(
            cache.current(now + Duration::from_millis(2100)),
            NativeCaptureState {
                camera: true,
                microphone: true
            }
        );
        assert_eq!(
            cache.current(now + Duration::from_secs(3)),
            NativeCaptureState {
                camera: true,
                microphone: false
            }
        );
    }

    #[test]
    fn fresh_idle_after_an_error_clears_without_waiting_for_expiry() {
        let now = Instant::now();
        let mut cache = NativeCaptureCache::default();
        cache.update(
            NativeCaptureState {
                camera: true,
                microphone: true,
            },
            now,
        );
        cache.update(
            NativeCaptureUpdate::default(),
            now + Duration::from_millis(500),
        );
        assert_eq!(
            cache.current(now + Duration::from_secs(1)),
            NativeCaptureState {
                camera: true,
                microphone: true
            }
        );
        cache.update(
            NativeCaptureUpdate {
                camera: Some(false),
                microphone: None,
            },
            now + Duration::from_millis(1100),
        );
        assert_eq!(
            cache.current(now + Duration::from_millis(1200)),
            NativeCaptureState {
                camera: false,
                microphone: true
            }
        );
    }

    #[test]
    fn native_camera_initialization_is_attempted_initially() {
        assert!(should_retry_native_camera(false, None, Instant::now()));
    }

    #[test]
    fn failed_native_camera_initialization_has_a_backoff() {
        let now = Instant::now();
        assert!(!should_retry_native_camera(
            false,
            Some(now),
            now + Duration::from_millis(4999)
        ));
    }

    #[test]
    fn failed_native_camera_initialization_can_recover() {
        let now = Instant::now();
        assert!(should_retry_native_camera(
            false,
            Some(now),
            now + Duration::from_secs(5)
        ));
    }

    #[test]
    fn available_native_camera_reader_is_not_reinitialized() {
        let now = Instant::now();
        assert!(!should_retry_native_camera(
            true,
            Some(now),
            now + Duration::from_secs(60)
        ));
    }

    #[test]
    fn future_retry_timestamp_does_not_cause_a_retry_loop() {
        let now = Instant::now();
        assert!(!should_retry_native_camera(
            false,
            Some(now + Duration::from_secs(1)),
            now
        ));
    }
}
