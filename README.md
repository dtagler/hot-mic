<p align="center">
  <img src="assets/logo.svg" alt="HotMic" width="120" height="120"/>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/title-dark.png">
    <img alt="HotMic" src="assets/title-light.png" width="544">
  </picture>
</p>

<p align="center"><em>Windows tray app that paints a thin colored border around your primary monitor whenever your camera or microphone is in use. Detects Microsoft Teams mute state in real time.</em></p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-0078D6?logo=windows11&logoColor=white" alt="Platform"/>
  <img src="https://img.shields.io/badge/Microsoft%20Teams-supported-6264A7?logo=microsoftteams&logoColor=white" alt="Microsoft Teams"/>
  <img src="https://img.shields.io/badge/arch-x64%20%E2%80%A2%20ARM64%20(via%20x64%20emulation)-8957E5?logo=windows&logoColor=white" alt="Architecture"/>
  <img src="https://img.shields.io/badge/built%20with-Rust-CE422B?logo=rust&logoColor=white" alt="Language"/>
  <img src="https://img.shields.io/badge/-Docker-2496ED?logo=docker&logoColor=white" alt="Docker"/>
  <img src="https://img.shields.io/badge/tests-83%20passing-brightgreen" alt="Tests"/>
  <img src="https://img.shields.io/badge/license-MIT-brightgreen" alt="License"/>
</p>

---

<p align="center">
  <img src="assets/screenshots/cam.png"  alt="Camera in use"        width="32%"/>
  <img src="assets/screenshots/mic.png"  alt="Microphone in use"    width="32%"/>
  <img src="assets/screenshots/both.png" alt="Camera + mic in use"  width="32%"/>
</p>

**Blue** for camera, **red** for mic, **purple** for both. The border fades smoothly on every transition, hugs your laptop's rounded screen corners, and is click-through. The tray icon snaps to the matching state without an animation.

## Why

Modern apps light up the webcam or pick up audio from the mic without making it obvious. Windows 11 shows a tiny privacy indicator in the system tray, but it's easy to miss. HotMic gives you a peripheral-vision signal you can't ignore.

## Install

1. Build with `.\build.ps1` (see [Build](#build)); this produces `dist\hotmic.exe` (~470 KB) in your clone.
2. Double-click `dist\hotmic.exe` to run. A microphone icon shows up in the system tray.
3. Optional: right-click the tray icon > **Start with Windows**. From then on it launches at logon from that same `dist\hotmic.exe` path.

Run it directly from the repo's `dist\` folder. The autostart entry records the absolute path you launched from, so don't move or rename the folder afterwards. If you do, re-toggle **Start with Windows** to refresh the path.

No installer, no service, no admin rights, no .NET / Visual C++ runtime. The exe is self-contained.

**First-launch SmartScreen warning.** The exe is unsigned, so the first time you run it Windows SmartScreen will say "Windows protected your PC". Click *More info* > *Run anyway*. To avoid this on managed corporate machines, you'd need a code-signing certificate; that's out of scope for this build.

## Uninstall

1. Right-click the tray icon > **Exit**. (If autostart is enabled, also right-click > uncheck **Start with Windows** first, so the next logon doesn't relaunch it.)
2. Delete `dist\hotmic.exe` (or just `git clean` / remove the repo).
3. If you skipped step 1's autostart-disable, also delete `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\HotMic` via `regedit` to prevent Windows from trying to launch the missing exe on next logon.
4. Optional: clear the Teams pairing state by deleting `%LOCALAPPDATA%\HotMic\` (contains `teams.token` and, if debug logging was on, `teams-debug.log`).

## Use

It's passive. As soon as something opens the camera or microphone, the screen border lights up and the tray icon changes color. When the device closes, the border disappears.

The tray menu (right-click) has:
- **Enabled**: toggles the border on/off without exiting the app
- **Start with Windows**: toggles the HKCU Run-key entry
- **Exit**: quits

## ![Microsoft Teams logo](assets/microsoft-teams.svg) Microsoft Teams Setup

If you use Teams, you want this. Otherwise HotMic will paint the red mic border for the entire duration of every Teams call: Teams keeps the WASAPI capture stream open while you're muted (so unmute is zero-latency), so Windows reports the mic as "in use" the whole time. HotMic fixes that by asking Teams directly for your real mute state via the **Teams Local API**, but that API is gated behind a per-user opt-in plus a one-time approval banner.

### Prerequisites

- **New Teams** (the MSIX build). Classic Teams (`Teams.exe` Squirrel install) and Teams Personal don't expose the API; HotMic falls back to registry-only behavior for those, meaning the red border will stay on for the whole call.
- **Third-party API not disabled by IT policy.** If your managed device blocks it, the toggle in Step 1 will be greyed out and there's nothing HotMic can do.

### Step 1: Enable the API In Teams

1. Open Teams.
2. Click the three-dot menu next to your profile picture (top right) > **Settings**.
3. Go to **Settings > Privacy**.
4. Find the section labeled something like *Manage API*, *Third-party app API*, or *Local API*. The exact wording has changed across Teams versions; look for any toggle that mentions "third-party" and "API".
5. Toggle it **on**.

### Step 2: Approve HotMic

1. Make sure HotMic is running (microphone icon visible in the system tray).
2. Start a Teams meeting. A **Meet now** session works for testing; you don't need to invite anyone.
3. Once you're in the meeting, Teams will show an **Allow** banner inside its own window asking whether HotMic can connect to its local API. Click **Allow**.

That's it. The red border now clears within ~150 ms whenever you mute in a Teams call, and comes back when you unmute. The Allow banner only appears once: HotMic stores Teams' token DPAPI-wrapped at `%LOCALAPPDATA%\HotMic\teams.token`, and subsequent runs reconnect silently with no UI.

### Re-Pairing

HotMic detects token rejection automatically and re-pairs without any manual action when:

- You removed HotMic from Teams' allowed-apps list.
- You signed into Teams as a different user.
- You reinstalled Teams.

In any of those cases, Teams resumes sending `canPair:true` the next time you're in a meeting, HotMic discards the stale token, and the Allow banner appears again. Click Allow once more.

To force a fresh pair manually: delete `%LOCALAPPDATA%\HotMic\teams.token`, restart HotMic, and start a Teams meeting to trigger the banner.

### If the Border Still Doesn't Clear On Mute

1. **Are you in a meeting?** Teams only emits mute updates while `isInMeeting=true`. Outside meetings the API is mostly idle by design.
2. **Did you click Allow?** Check Teams' Privacy settings: if HotMic isn't in the allowed-apps list, the pair banner was either missed or denied. Re-trigger it by starting a Meet-now while HotMic is running.
3. **Are you on new Teams?** Right-click the Teams icon in the system tray and check the version. If you're on Classic Teams (`Teams.exe`) or Teams Personal, the API isn't available and the red border will keep its registry-only behavior for the whole call.
4. **Capture a debug log.** Stop HotMic, then re-launch from a PowerShell window with `$env:HOTMIC_TEAMS_DEBUG = "1"; .\dist\hotmic.exe`. The Teams client will append protocol frames and state transitions to `%LOCALAPPDATA%\HotMic\teams-debug.log` (truncated at 256 KB). Tokens are auto-redacted, so the log is safe to share.

### What HotMic Does And Does Not Do With Teams

- **Reads** `isInMeeting` and `isMuted` from the `meetingUpdate` event stream over a loopback-only WebSocket at `127.0.0.1:8124`.
- **Sends** exactly one outgoing request, `{"action":"pair"}`, the very first time, plus pong frames in response to Teams' pings and a single close frame on shutdown.
- **Never sends** `toggle-mute`, `leave-call`, `toggle-video`, or any other write action. Although the token Teams hands us grants WRITE access, `src/teams.rs` exposes no public method that produces any of those payloads. See [Security Posture](#security-posture) for the full constraint list.

## Troubleshooting

**Border doesn't appear when I open the camera.** Check that the app in question goes through `CapabilityAccessManager`: open `regedit` and look under `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam` while the camera is active. If your app isn't listed there, it's using one of the legacy paths HotMic doesn't watch. Some older capture tools and a few games fall into this bucket.

**Red mic border stays on after I muted in Microsoft Teams.** See the [Microsoft Teams Setup](#microsoft-teams-setup) section above. Other VoIP apps (Zoom, Discord, Slack huddles, Meet-in-browser) don't expose an equivalent local API, so the border still false-positives on mute for those.

**Border appears in the wrong place / corners don't match my screen.** The corner radius is hardcoded to 12 logical pixels. If your laptop's display has a tighter or wider curve, edit `CORNER_RADIUS_LOGICAL` in `src/lib.rs` and rebuild.

**Border is too thin / too thick.** Edit `BORDER_THICKNESS_LOGICAL` (also `src/lib.rs`), default 3, and rebuild.

**Autostart is broken after I moved or rebuilt the repo somewhere else.** The Run-key records the absolute path you originally launched from. Right-click the tray icon > toggle **Start with Windows** off, then on again. That re-writes the Run-key with the current `dist\hotmic.exe` path.

## How It Detects Things

Windows writes per-app device-usage state to the registry under:

```
HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{webcam,microphone}
HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{webcam,microphone}
```

Each app gets a subkey with a `LastUsedTimeStop` value. While the app holds the device, the value is `0`; once it's released, it gets a real FILETIME. HotMic walks both hives, watches all four roots with `RegNotifyChangeKeyValue`, and reads the values when a change fires.

**What this catches**: Teams, Zoom, Chrome/Edge/Firefox, Slack, Discord, the Windows Camera app, and basically any modern UWP or Electron-based app that goes through the capability access manager.

**What it does not catch**: legacy apps using raw DirectShow or older WASAPI paths that skip the consent store entirely (some older capture tools, a few games). This is a known limitation. Detecting those paths would require hooking MMDevice and Media Foundation directly, which is materially more code without changing detection for any of the apps listed above.

### Microsoft Teams In-App Mute

The registry trick above has one well-known false positive: Microsoft Teams keeps the WASAPI capture stream open even when you mute yourself in a call (it discards the captured samples in user space so unmute is zero-latency). The OS therefore reports the mic as still "in use" while you're muted, and HotMic's red border would otherwise stay on for the entire call.

To fix this without losing detection accuracy, HotMic also speaks to the **Teams Local API** at `ws://127.0.0.1:8124`. New Teams (the MSIX build) exposes a per-user, opt-in WebSocket that pushes a `meetingUpdate` event every time `isInMeeting` or `isMuted` changes. HotMic reads only those two booleans. The detection rule becomes:

```
mic_border_on = non_teams_app_holds_mic
             || (teams_holds_mic && !teams_says_we_are_muted_in_a_call)
```

If Teams isn't running, the API is disabled, you have classic Teams, or you decline the Allow toast, the WebSocket simply isn't connected and the rule degrades to the original behavior (border on whenever Teams holds the mic). Per-app fusion means a non-Teams app holding the mic always keeps the border on, even if Teams is muted in parallel.

**First-run pairing.** On first launch, the WebSocket connects and Teams sends a `meetingPermissions` block. When Teams sets `canPair:true` (which only happens once you're in a meeting), HotMic sends a single `{"action":"pair"}` request. That's what triggers the Allow banner inside Teams. Click Allow and Teams returns a one-time token that HotMic stores DPAPI-wrapped at `%LOCALAPPDATA%\HotMic\teams.token`; subsequent connections present the token and skip the pairing step. If the token is ever rejected (you removed HotMic from Teams' allowed apps, signed in as a different account, reinstalled Teams, etc.), Teams will resume sending `canPair:true` and HotMic will automatically discard the stale token and pair again, with no manual cleanup required. To force a fresh pair anyway, delete the token file and restart HotMic while in a meeting.

## How It Draws the Border

A single full-screen layered window covers the primary monitor. The window uses color-key transparency (magenta is the key) so most of it is invisible; only the pixels actually painted in the border color show up. Layered alpha is also enabled so the whole border can be animated.

In `WM_PAINT` the border is drawn as one stroked `RoundRect` (single GDI call) in the current state color: blue for camera, red for mic, purple for both. The rounded corners follow your laptop's screen curve.

Corner radius is **12 logical pixels** by default. Change `CORNER_RADIUS_LOGICAL` in `src/lib.rs` if your screen's curve is different. Thickness is **3 logical pixels**, scaled by DPI at draw time.

The fade animation runs on a `WM_TIMER` ticking at 60 Hz; layered-window alpha steps from 0 to 255 (or back) in 8 ticks, so appear/disappear takes about 128 ms. The window is hidden once the disappear animation completes.

The window has `WS_EX_TRANSPARENT` and `WS_EX_NOACTIVATE`, so all input passes through it and it never steals focus. It reasserts `HWND_TOPMOST` in `WM_WINDOWPOSCHANGING` because Windows occasionally demotes topmost windows on virtual-desktop or UAC transitions.

## Build

All builds happen in Docker. Nothing is installed on the host beyond Docker itself.

```powershell
.\build.ps1
```

That script:
1. Builds the `hotmic-builder` image (Rust 1 + mingw-w64 cross-compiler, ~20 s first time, cached after)
2. Runs `cargo build --release --target x86_64-pc-windows-gnu` inside the container
3. Copies the resulting exe to `dist\hotmic.exe`

The first run pulls and builds the image. Subsequent runs reuse it and finish in seconds.

### Regenerating the Tray Icons

Source SVGs live in `assets\icons\tray-{idle,cam,mic,both}.svg`. The `.ico` files are derived. To regenerate after editing an SVG:

```powershell
docker run --rm -v "${PWD}:/work" -w /work debian:bookworm-slim bash -c "
  apt-get update -qq && apt-get install -y -qq --no-install-recommends librsvg2-bin imagemagick &&
  for n in idle cam mic both; do
    for s in 16 20 24 32 40 48 64; do
      rsvg-convert -w \$s -h \$s -b none assets/icons/tray-\${n}.svg -o /tmp/\${n}-\${s}.png
    done
    convert /tmp/\${n}-16.png /tmp/\${n}-20.png /tmp/\${n}-24.png /tmp/\${n}-32.png /tmp/\${n}-40.png /tmp/\${n}-48.png /tmp/\${n}-64.png assets/icons/tray-\${n}.ico
  done
"
```

Each `.ico` packs seven native resolutions so Windows can pick the best size for the active tray DPI (`LoadIconMetric(LIM_SMALL)` does the selection at runtime).

## Tests

Pure logic (color matrix, DPI scaling, registry value parsing, wide-string helpers, Teams identity matching, mute-fusion truth table, JSON scanner for `meetingUpdate`, base64, and the RFC 6455 framer/parser) lives in `src/lib.rs` and runs natively on the build container's host target (Linux ARM/x64). The Win32 surface (windowing, registry I/O, tray, sockets, DPAPI) can't be unit-tested without running on Windows; that's covered by your manual smoke test.

```powershell
docker run --rm -v "${PWD}:/work" -w /work hotmic-builder cargo test --lib
```

83 tests as of this writing. Format, clippy, tests, and build all run cleanly:

```powershell
docker run --rm -v "${PWD}:/work" -w /work hotmic-builder bash -c "
  cargo fmt --check &&
  cargo clippy --lib -- -D warnings &&
  cargo clippy --bin hotmic --target x86_64-pc-windows-gnu -- -D warnings &&
  cargo test --lib &&
  cargo build --release --target x86_64-pc-windows-gnu
"
```

## Project Layout

```
hotmic/
├── Cargo.toml              # windows = 0.62, embed-resource = 3
├── Cargo.lock
├── .cargo/config.toml      # static-link mingw runtime; -static-libgcc
├── Dockerfile              # rust:1-bookworm + mingw + clippy + rustfmt
├── build.ps1               # one-shot Docker build + copy to dist/
├── build.rs                # embeds app.rc resources into the exe
├── app.rc                  # RT_MANIFEST + 4 icon resources
├── app.manifest            # PerMonitorV2 DPI, asInvoker, common controls v6
├── src/
│   ├── main.rs             # tray, menu, message loop, autostart, mutex
│   ├── lib.rs              # pure helpers + unit tests
│   ├── overlay.rs          # full-screen layered window + GDI border drawing
│   ├── detect.rs           # registry walk + RegNotifyChangeKeyValue watcher
│   ├── teams.rs            # Teams Local API WebSocket client (pair + read)
│   └── autostart.rs        # HKCU Run-key read/write/delete
├── assets/
│   ├── logo.svg            # README logo
│   ├── title-{light,dark}.{svg,png}  # README title image
│   ├── screenshots/        # README header screenshots
│   └── icons/
│       ├── tray-{idle,cam,mic,both}.svg
│       └── tray-{idle,cam,mic,both}.ico
├── dist/
│   └── hotmic.exe          # shipped artifact
└── target/                 # cargo build cache (gitignored)
```

## Architecture

```
   ┌──────────────────────────────────────┐    ┌──────────────────────────────────┐
   │   Windows CapabilityAccessManager    │    │   Microsoft Teams (new Teams)    │
   │   HKCU + HKLM   webcam | microphone  │    │   Local API ws://127.0.0.1:8124  │
   └──────────────────┬───────────────────┘    └──────────────┬───────────────────┘
                      │ change events                         │ meetingUpdate frames
                      ▼                                       ▼
   ┌────────────────────────────────────┐   ┌────────────────────────────────────┐
   │ Watcher (src/detect.rs)            │   │ Teams client (src/teams.rs)        │
   │ 4× RegNotifyChangeKeyValue         │   │ WebSocket + DPAPI-wrapped token,   │
   │ + 500 ms backstop poll             │   │ posts WM_TEAMS_STATE_CHANGED       │
   └─────────────────┬──────────────────┘   └──────────────────┬─────────────────┘
                     │                                         │
                     └──────────────────┬──────────────────────┘
                                        ▼
   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ Message loop   MsgWaitForMultipleObjectsEx                                   │
   └──────────────────────────────────┬───────────────────────────────────────────┘
                                      ▼
   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ apply_state                                                                  │
   │   mic_should_show(non_teams, teams, teams_muted_now)                         │
   │   150 ms off-debounce on active → idle,  (cam, mic) → color                  │
   └─────────────┬────────────────────────────────────────────┬───────────────────┘
                 │ color + visibility                         │ icon + tooltip
                 ▼                                            ▼
   ┌──────────────────────────┐                    ┌──────────────────────────┐
   │ Overlay window           │                    │ Tray icon                │
   │ layered + click-through, │                    │ Shell_NotifyIcon         │
   │ GDI rounded rect,        │                    │                          │
   │ ~128 ms fade @ 60 Hz     │                    │                          │
   └────────────┬─────────────┘                    └─────────────┬────────────┘
                │ colored border                                 │ state icon
                ▼                                                ▼
       ┌──────────────────┐                            ┌──────────────────┐
       │ Primary monitor  │                            │ System tray      │
       └──────────────────┘                            └──────────────────┘
```

One process, one thread. The registry tells us when a device opens or closes; the Teams Local API tells us whether the user is muted inside a Teams call; the message loop fuses both signals into a colored border on the screen and a state-matching icon in the tray.

### Event-Driven, Not Polling

The watcher (`src/detect.rs`) opens four registry keys (HKCU + HKLM × webcam + microphone) and creates a manual-reset event per key. It arms `RegNotifyChangeKeyValue` on each with subtree-recursive watching and `REG_NOTIFY_THREAD_AGNOSTIC`. The main message loop uses `MsgWaitForMultipleObjectsEx` to block on the four events and the message queue simultaneously, so between events the message-loop thread parks in the kernel and reacts within milliseconds when a registry value changes.

A 500 ms backstop `WM_TIMER` covers the rare case where `CapabilityAccessManager` writes don't trigger a `RegNotifyChangeKeyValue` callback (sometimes the kernel coalesces deeply-nested changes). A 150 ms off-debounce prevents the border from flickering during the brief stop/start that some apps do while negotiating device formats.

### Teams Local API Client

The Teams client (`src/teams.rs`) runs on the same thread as the watcher. It opens a single non-blocking TCP socket to `127.0.0.1:8124` and uses `WSAAsyncSelect` so the socket's `FD_READ`/`FD_WRITE`/`FD_CLOSE` notifications post `WM_TEAMS_SOCKET` messages back to the message loop. The WebSocket upgrade, pairing handshake, frame parsing, ping/pong, and reconnect/backoff all run inside the same `MsgWaitForMultipleObjectsEx` pump as the registry watcher. Every received text frame that updates `isInMeeting` or `isMuted` posts a `WM_TEAMS_STATE_CHANGED`, which re-runs `apply_state` immediately rather than waiting for the next 500 ms backstop tick.

The first time you run HotMic with Teams open, the client sends a single `{"action":"pair"}` request to trigger Teams' Allow banner. After you click **Allow**, Teams replies with a permanent token; HotMic encrypts it with `CryptProtectData(CRYPTPROTECT_UI_FORBIDDEN)` and stores it at `%LOCALAPPDATA%\HotMic\teams.token`. Subsequent runs reuse the wrapped token with no UI. If Teams ever returns "token invalid" (after a Teams reinstall or a manual sign-out), the client deletes the stored token and re-pairs from scratch.

### Single Instance

The app calls `CreateMutexW` on `Local\HotMic-Singleton` at startup. If the mutex already exists, the second instance exits immediately. This prevents a slow logon from spawning two tray icons.

### Autostart

Toggling **Start with Windows** writes the current exe path (as `"\"...\""`) to `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\HotMic`. HKCU means no admin elevation. The Run-key is per-user, so the autostart only applies to the account that toggled it.

If you move `hotmic.exe`, the Run-key still points at the old path and autostart will fail silently. Re-toggle the menu item to refresh it.

## Configuration

There's no settings file. The two values you might want to tune are constants in `src/lib.rs`:

| Constant | Default | What it controls |
|---|---|---|
| `BORDER_THICKNESS_LOGICAL` | 3 | Border line thickness in logical pixels (scales with DPI) |
| `CORNER_RADIUS_LOGICAL` | 12 | Corner curve radius. Match your laptop's screen rounding |

Change either, rebuild via `.\build.ps1`, relaunch.

## Security Posture

- **No elevation.** `asInvoker` in the manifest. Runs entirely as the current user.
- **Loopback networking only.** One TCP connection to `127.0.0.1:8124` for the Teams Local API (see "Microsoft Teams in-app mute" above). The socket is opened only after the Watcher starts and is closed on exit. No outbound traffic ever leaves `127.0.0.1`.
- **DPAPI-wrapped Teams token.** When Teams pairs with us, it sends a one-time token granting access to its local API. We store it at `%LOCALAPPDATA%\HotMic\teams.token`, encrypted with `CryptProtectData(CRYPTPROTECT_UI_FORBIDDEN)` so only the same Windows user on the same machine can decrypt it. `%LOCALAPPDATA%` already has user-only ACLs by default.
- **Minimal Teams client surface.** Although the token Teams gives us also grants WRITE access (toggle mute, leave call, toggle video, etc.), `src/teams.rs` exposes no public method that sends any action other than a single one-time `{"action":"pair"}` request used to trigger Teams' Allow banner during initial pairing. After that, the only outgoing payloads are pong frames in response to pings and a single close frame on shutdown. We never send `toggle-mute`, `leave-call`, `toggle-video`, or any other write action.
- **HKCU/HKLM read-only for detection.** Only opens the two registry trees with `KEY_READ | KEY_NOTIFY`. The only registry writes are to the optional autostart Run-key, which is HKCU and per-user.
- **No input capture.** The overlay window is `WS_EX_TRANSPARENT` + `WS_EX_NOACTIVATE`, so all keyboard/mouse input passes through it untouched.
- **No external execution.** The app never launches subprocesses or shells out.

It's a passive indicator. It can't itself prevent the camera or mic from being opened.

## Known Limitations

- **Primary monitor only.** Multi-monitor support would mean tracking each monitor's bounds and DPI separately and managing multiple overlay windows. Out of scope for the current design.
- **Legacy DirectShow / older WASAPI apps not detected.** See "How it detects things" above.
- **Mic-mute false positive on non-Teams VoIP.** Zoom, Discord, Slack huddles, Meet-in-browser, and other apps that keep their capture stream open while muted will still show the red border. The Teams Local API fixes this for new Teams only; no equivalent local API exists for the others.
- **Classic Teams and Teams Personal don't expose the API.** The mic-mute fix only kicks in for new Teams (the `MSTeams_8wekyb3d8bbwe` MSIX). Classic Teams (`Teams.exe` Squirrel install) and Teams Personal degrade to the original behavior.
- **IT-disabled API.** A managed device can disable the third-party API via Teams admin policy; the toggle under **Settings > Privacy** is greyed out in that case. HotMic falls back to registry-only behavior automatically.
- **Full-screen exclusive apps cover the border.** Acceptable: those apps aren't really compatible with any topmost indicator.
- **Move or rename the repo: autostart breaks.** The Run-key records the absolute path to `dist\hotmic.exe`. Re-toggle the menu item to fix.

## License

MIT. See [LICENSE](LICENSE).
