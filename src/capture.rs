//! Passive capture metadata on a dedicated MTA worker. The UI thread only
//! reads the last completed snapshot; this module never opens a media stream.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{Interface, HRESULT, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use hotmic::{
    native_microphone_is_visible, should_retry_native_camera, CaptureSample, NativeCaptureCache,
    NativeCaptureState, NativeCaptureUpdate,
};

use crate::capture_mf::CameraReader;

const SAMPLE_WINDOW: Duration = Duration::from_millis(500);
const SHUTDOWN_WAIT: Duration = Duration::from_millis(300);

pub struct CaptureMonitor {
    latest: Arc<Mutex<NativeCaptureCache>>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl CaptureMonitor {
    pub fn start() -> Self {
        let latest = Arc::new(Mutex::new(NativeCaptureCache::default()));
        let (stop, receive) = mpsc::channel();
        let publish = latest.clone();
        let worker = thread::Builder::new()
            .name("hotmic-capture".into())
            .spawn(move || run_worker(receive, publish))
            .ok();
        Self {
            latest,
            stop,
            worker,
        }
    }

    pub fn snapshot(&self) -> NativeCaptureState {
        self.latest
            .lock()
            .map(|cache| cache.current(Instant::now()))
            .unwrap_or_default()
    }
}

impl Drop for CaptureMonitor {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            // A failed system provider must not trap the tray's Exit command.
            // The detached fallback owns its state and touches no UI objects.
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn run_worker(stop: Receiver<()>, latest: Arc<Mutex<NativeCaptureCache>>) {
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        return;
    }
    let _apartment = ComApartment;
    // Camera support is optional. Core Audio remains usable when MF is absent.
    let mut camera = None;
    let mut last_camera_attempt = None;
    loop {
        if should_retry_native_camera(camera.is_some(), last_camera_attempt, Instant::now()) {
            camera = CameraReader::new().ok();
            // Back off from completion, so a slow failed initializer does not
            // immediately retry and starve otherwise-usable audio observation.
            last_camera_attempt = Some(Instant::now());
        }
        // Use the earliest observation bound, not publication time. A slow
        // COM call must not make an old camera reading look freshly observed.
        let camera_observed_at = Instant::now();
        let window = camera
            .as_ref()
            .and_then(|reader| reader.begin_sample().ok());
        let window_started = Instant::now();
        // Audio is sampled while the camera observer is listening, not after
        // an extra half-second wait. Each source has its own freshness bound.
        let mic_observed_at = Instant::now();
        let microphone = read_microphone();
        if let Ok(mut current) = latest.lock() {
            current.update(
                NativeCaptureUpdate {
                    camera: None,
                    microphone,
                },
                mic_observed_at,
            );
        }
        match stop.recv_timeout(SAMPLE_WINDOW.saturating_sub(window_started.elapsed())) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => break,
        }
        let camera_reading = window.and_then(|window| window.finish());
        // No cross-process/COM work is performed while this lock is held.
        if let Ok(mut current) = latest.lock() {
            current.update(
                NativeCaptureUpdate {
                    camera: camera_reading,
                    microphone: None,
                },
                camera_observed_at,
            );
        }
    }
}

fn read_microphone() -> Option<bool> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) }.ok()?;
    let devices = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }.ok()?;
    let count = unsafe { devices.GetCount() }.ok()?;
    let readings = CaptureSample::default();
    // A successful empty endpoint/session enumeration is a known negative.
    readings.observe(Some(false));
    for index in 0..count {
        let Ok(device) = (unsafe { devices.Item(index) }) else {
            readings.observe(None);
            continue;
        };
        // This is a metadata interface, not an IAudioClient/capture stream.
        let Ok(manager) =
            (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_INPROC_SERVER, None) })
        else {
            readings.observe(None);
            continue;
        };
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
            readings.observe(None);
            continue;
        };
        let Ok(count) = (unsafe { sessions.GetCount() }) else {
            readings.observe(None);
            continue;
        };
        for item in 0..count {
            let Ok(session) = (unsafe { sessions.GetSession(item) }) else {
                readings.observe(None);
                continue;
            };
            let Ok(state) = (unsafe { session.GetState() }) else {
                readings.observe(None);
                continue;
            };
            if state != AudioSessionStateActive {
                continue;
            }
            readings.observe(Some(active_session_is_visible(&session)));
            if readings.is_active() {
                return Some(true);
            }
        }
    }
    readings.observation()
}

fn active_session_is_visible(session: &IAudioSessionControl) -> bool {
    let Ok(owner) = session.cast::<IAudioSessionControl2>() else {
        return true;
    };
    let mut process_id = 0;
    // The generated Result<u32> wrapper loses the successful
    // AUDCLNT_S_NO_SINGLE_PROCESS status. Only exact S_OK proves one owner.
    let status = unsafe {
        (Interface::vtable(&owner).GetProcessId)(Interface::as_raw(&owner), &mut process_id)
    };
    let unique_owner = status == HRESULT(0) && process_id != 0;
    let image = if unique_owner {
        process_image(process_id)
    } else {
        None
    };
    native_microphone_is_visible(true, image.as_deref(), unique_owner)
}

fn process_image(process_id: u32) -> Option<String> {
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    // Paths are used only for classification and are never logged or retained.
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}
