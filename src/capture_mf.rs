//! Optional, metadata-only Media Foundation camera observation. MF entry points
//! are resolved from System32 so absent media components cannot prevent startup.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::*;
use windows::Win32::Foundation::{FreeLibrary, E_FAIL, E_POINTER, HMODULE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use hotmic::CaptureSample;

type StartupFn = unsafe extern "system" fn(u32, u32) -> HRESULT;
type ShutdownFn = unsafe extern "system" fn() -> HRESULT;
type AttributesFn = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type SourcesFn = unsafe extern "system" fn(*mut c_void, *mut *mut *mut c_void, *mut u32) -> HRESULT;
type MonitorFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;

struct Module(HMODULE);

impl Module {
    fn load(name: PCWSTR) -> Result<Self> {
        unsafe { LoadLibraryExW(name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map(Self)
    }

    fn address(&self, name: PCSTR) -> Result<unsafe extern "system" fn() -> isize> {
        unsafe { GetProcAddress(self.0, name) }.ok_or_else(Error::from_thread)
    }

    fn keep_resident(self) -> usize {
        let handle = self.0 .0 as usize;
        std::mem::forget(self);
        handle
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

struct MediaApi {
    startup: StartupFn,
    shutdown: ShutdownFn,
    attributes: Option<AttributesFn>,
    sources: Option<SourcesFn>,
    monitor: MonitorFn,
    // Cached once, like static imports, but optional and System32-only.
    // Keep the DLLs resident until process exit: an OS callback may outlive a
    // slow shutdown. Failed initialization releases its partial Module guards.
    _modules: [usize; 3],
}

impl MediaApi {
    fn get() -> Result<&'static Self> {
        static API: OnceLock<MediaApi> = OnceLock::new();
        static LOAD: Mutex<()> = Mutex::new(());
        if let Some(api) = API.get() {
            return Ok(api);
        }
        // Cache successful resolution only. Failed loads release their module
        // guards and can recover on the worker's bounded initialization retry.
        let _load = LOAD.lock().map_err(|_| Error::from_hresult(E_FAIL))?;
        if let Some(api) = API.get() {
            return Ok(api);
        }
        let api = Self::load()?;
        Ok(API.get_or_init(|| api))
    }

    fn load() -> Result<Self> {
        let platform = Module::load(w!("mfplat.dll"))?;
        // Catalog enumeration matches the verified startup path, but it must
        // not prevent activity monitoring when only the observer is available.
        let foundation = Module::load(w!("mf.dll")).ok();
        let sensors = Module::load(w!("mfsensorgroup.dll"))?;
        // Signatures are the SDK ABI for the five named functions. Never
        // resolve a user-supplied DLL or symbol, or load from the working dir.
        let startup = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, StartupFn>(
                platform.address(s!("MFStartup"))?,
            )
        };
        let shutdown = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, ShutdownFn>(
                platform.address(s!("MFShutdown"))?,
            )
        };
        let attributes = platform
            .address(s!("MFCreateAttributes"))
            .ok()
            .map(|address| unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, AttributesFn>(address)
            });
        let sources = foundation
            .as_ref()
            .and_then(|module| module.address(s!("MFEnumDeviceSources")).ok())
            .map(|address| unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SourcesFn>(address)
            });
        let monitor = unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, MonitorFn>(
                sensors.address(s!("MFCreateSensorActivityMonitor"))?,
            )
        };
        Ok(Self {
            startup,
            shutdown,
            attributes,
            sources,
            monitor,
            _modules: [
                platform.keep_resident(),
                foundation.map(Module::keep_resident).unwrap_or_default(),
                sensors.keep_resident(),
            ],
        })
    }
}

pub struct CameraReader {
    api: &'static MediaApi,
}

impl CameraReader {
    pub fn new() -> Result<Self> {
        let api = MediaApi::get()?;
        unsafe { (api.startup)(MF_VERSION, MFSTARTUP_NOSOCKET).ok()? };
        let reader = Self { api };
        // Warm the metadata catalog, matching the live-verified probe startup.
        // Never activate any returned IMFActivate,
        // obtain a media source, or request a video sample.
        let _ = reader.enumerate_metadata();
        Ok(reader)
    }

    fn enumerate_metadata(&self) -> Result<()> {
        let Some((attributes_fn, sources_fn)) = self.api.attributes.zip(self.api.sources) else {
            return Ok(());
        };
        let mut raw = std::ptr::null_mut();
        unsafe { attributes_fn(&mut raw, 1).ok()? };
        if raw.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let attributes = unsafe { IMFAttributes::from_raw(raw) };
        unsafe {
            attributes.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;
        }
        let mut sources = std::ptr::null_mut();
        let mut count = 0;
        unsafe {
            sources_fn(attributes.as_raw(), &mut sources, &mut count).ok()?;
            if !sources.is_null() {
                for index in 0..count {
                    let source = sources.add(index as usize).read();
                    if !source.is_null() {
                        drop(IUnknown::from_raw(source));
                    }
                }
                CoTaskMemFree(Some(sources.cast()));
            }
        }
        Ok(())
    }

    pub fn begin_sample(&self) -> Result<CameraObservation<'_>> {
        let sample = Arc::new(CaptureSample::default());
        let callback: IMFSensorActivitiesReportCallback = SensorCallback {
            sample: sample.clone(),
        }
        .into();
        let mut raw = std::ptr::null_mut();
        unsafe { (self.api.monitor)(callback.as_raw(), &mut raw).ok()? };
        if raw.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let observation = CameraObservation {
            monitor: unsafe { IMFSensorActivityMonitor::from_raw(raw) },
            sample,
            stopped: false,
            _reader: self,
        };
        // Construct the shutdown guard before Start, including its error path.
        unsafe { observation.monitor.Start()? };
        Ok(observation)
    }
}

impl Drop for CameraReader {
    fn drop(&mut self) {
        let _ = unsafe { (self.api.shutdown)() };
    }
}

pub struct CameraObservation<'a> {
    monitor: IMFSensorActivityMonitor,
    sample: Arc<CaptureSample>,
    stopped: bool,
    _reader: &'a CameraReader,
}

impl CameraObservation<'_> {
    pub fn finish(mut self) -> Option<bool> {
        self.stop();
        self.sample.observation()
    }

    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            let _ = unsafe { self.monitor.Stop() };
            if let Ok(shutdown) = self.monitor.cast::<IMFShutdown>() {
                let _ = unsafe { shutdown.Shutdown() };
            }
        }
    }
}

impl Drop for CameraObservation<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[implement(IMFSensorActivitiesReportCallback)]
struct SensorCallback {
    sample: Arc<CaptureSample>,
}

impl IMFSensorActivitiesReportCallback_Impl for SensorCallback_Impl {
    fn OnActivitiesReport(&self, report: Ref<IMFSensorActivitiesReport>) -> Result<()> {
        let Some(report) = report.as_ref() else {
            self.sample.observe(None);
            return Ok(());
        };
        let Ok(count) = (unsafe { report.GetCount() }) else {
            self.sample.observe(None);
            return Ok(());
        };
        for index in 0..count {
            let Ok(sensor) = (unsafe { report.GetActivityReport(index) }) else {
                self.sample.observe(None);
                continue;
            };
            let Ok(processes) = (unsafe { sensor.GetProcessCount() }) else {
                self.sample.observe(None);
                continue;
            };
            if processes == 0 {
                self.sample.observe(Some(false));
            }
            for process in 0..processes {
                let streaming = unsafe {
                    sensor
                        .GetProcessActivity(process)
                        .and_then(|activity| activity.GetStreamingState())
                }
                .ok()
                .map(|active| active.as_bool());
                self.sample.observe(streaming);
            }
        }
        Ok(())
    }
}
