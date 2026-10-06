# Changelog

## Unreleased

### Fixed

- Restored camera and microphone indication for joined Teams desktop meetings when Windows consent-store records remain at old, stopped timestamps.
- Added independent, read-only polling of `hangup-button`, `microphone-button`, and `video-button` in eligible `TeamsWebView` windows. An enabled hangup control establishes a meeting; previews and hidden WebViews do not establish fallback activity.
- Kept Local API precedence for complete meeting/mute state and preserved other apps' registry-reported activity.
- Preserved successful control readings when another control fails. Unconfirmed read failures remain uncertain across windows instead of incorrectly allowing microphone suppression.
- Added 34 library regression tests for stale registry data, device combinations, API precedence, meeting lifecycle, preview rejection, partial-read failures, and conservative mute handling without a confirmed hangup control.

### Cause And Scope

During diagnosis, Teams reported a live microphone and an enabled camera while both registry views still contained stopped usage records from earlier sessions. HotMic's message loop and border window were responsive.

The previous fallback only resolved mute after the registry reported Teams microphone activity. It could neither establish microphone use independently nor recover camera activity. The new meeting-state path removes that dependency.

The reporting mismatch was observed, but no particular Teams or Windows update was established as its cause. The fallback is specific to compatible Teams desktop meeting controls; it is not a replacement for global device monitoring.

### Validation On October 5, 2026

- The initial implementation gate passed all 145 library tests: the original 112 plus 33 new regressions.
- `cargo fmt --check`, library clippy, Windows-binary clippy with `-D warnings`, and the Windows x86-64 release build passed. The existing non-snake-case name warning in a token-redaction test remains.
- A native read-only Windows probe exercised the detector in the idle state. The rebuilt executable was installed locally and its message loop and overlay window responded after restart.
- Standalone native idle-probe samples took 96 to 175 ms per scan on the tested host. This is an observation, not a latency guarantee or an in-call benchmark; in-call scan time was not measured.
- The user confirmed the red (microphone), blue (camera), and purple (both) states in a live Teams call after the update.

Both-devices-off behavior, meeting-end transitions, minimized meetings, pre-join previews, and overlap with another capturing app remain manual regression checks. Their logic is covered by automated tests where applicable, but the live color confirmation above does not establish that every manual scenario was exercised.

See the [Windows smoke-test checklist](README.md#windows-smoke-test) and [detector limitations](README.md#known-limitations).

### Final Pre-Commit Coverage

Final review added one further regression test covering a muted microphone label with either a missing or disabled hangup control while the registry reports Teams microphone activity. It pins the intended conservative red indication. The suite now contains 146 library tests: the original 112 plus 34 added by this fix.

Fresh validation of that final tree passed formatting, both clippy targets, all 146 library tests, and the Windows release build.

These notes describe the unreleased fix in this checkout. They do not announce a new numbered release or imply that an older tagged release asset includes it.
