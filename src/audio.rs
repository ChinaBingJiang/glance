//! The sound devices this PC has, and how loud each is. They are enumerated as
//! Windows keeps them — the ones sound plays through, and the ones it is
//! recorded from — each read for the name it gives itself, its master volume,
//! and whether it is muted; and put back through the very endpoint volume the
//! system's own mixer uses. Nothing here blocks the drawn frame: the list is
//! taken off the render thread, and a change is written on a thread of its own.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::HSTRING;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eCapture, eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

/// One sound device: which way it goes (true where sound plays through it), the
/// name it gives itself, the id Windows knows it by, whether it is the default
/// for its kind, and how loud and muted it stands.
#[derive(Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub render: bool,
    pub default: bool,
    pub volume: u32,
    pub muted: bool,
}

/// What every device says all at once, for a frame to show.
#[derive(Clone, PartialEq)]
pub struct Snapshot {
    pub devices: Vec<Device>,
}

/// A device's own name, as it is kept in its property store; its id where none.
fn name_of(device: &IMMDevice) -> String {
    unsafe {
        let fallback = || device.GetId().ok().and_then(|id| id.to_string().ok()).unwrap_or_default();
        let Some(store) = device.OpenPropertyStore(STGM_READ).ok() else { return fallback() };
        let Ok(value) = store.GetValue(&PKEY_Device_FriendlyName) else { return fallback() };
        let Ok(text) = PropVariantToStringAlloc(&value) else { return fallback() };
        let said = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        if said.is_empty() { fallback() } else { said }
    }
}

/// The volume and mute of a device, where it answers; none where it will not.
fn read_volume(device: &IMMDevice) -> Option<(u32, bool)> {
    unsafe {
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let scalar = volume.GetMasterVolumeLevelScalar().ok()?;
        let muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
        Some(((scalar * 100.0).round().clamp(0.0, 100.0) as u32, muted))
    }
}

/// Every device, both ways round, each with its name, its state, and whether it
/// is the default for its kind.
pub fn read() -> Snapshot {
    let mut devices = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            for render in [true, false] {
                let flow = if render { eRender } else { eCapture };
                let default = enumerator
                    .GetDefaultAudioEndpoint(flow, eConsole)
                    .ok()
                    .and_then(|d| d.GetId().ok())
                    .and_then(|id| id.to_string().ok());
                let Ok(list) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
                for index in 0..list.GetCount().unwrap_or(0) {
                    let Ok(device) = list.Item(index) else { continue };
                    let id = device.GetId().ok().and_then(|id| id.to_string().ok()).unwrap_or_default();
                    if id.is_empty() {
                        continue;
                    }
                    let name = name_of(&device);
                    let (volume, muted) = read_volume(&device).unwrap_or((0, false));
                    devices.push(Device { id, name, render, default: false, volume, muted });
                    if let Some(want) = &default {
                        if let Some(last) = devices.last_mut() {
                            last.default = &last.id == want;
                        }
                    }
                }
            }
        }
        CoUninitialize();
    }
    // Playing first, the default of each kind above the rest, then by name.
    devices.sort_by(|a, b| b.render.cmp(&a.render).then(b.default.cmp(&a.default)).then(a.name.cmp(&b.name)));
    Snapshot { devices }
}

static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The devices as last read; a fresh read is taken off-thread when they are stale, and the window woken when it lands.
pub fn snapshot() -> Option<Snapshot> {
    let last = LAST_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 8000 {
        if !BUSY.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let shot = read();
                *CACHE.lock().unwrap() = Some(shot);
                LAST_MS.store(millis(), Ordering::Relaxed);
                BUSY.store(false, Ordering::SeqCst);
                crate::ui::settings_window::follow_settings();
            });
        }
    }
    CACHE.lock().unwrap().clone()
}

/// Puts a device's volume, and whether it is muted, as asked; the write is done
/// on a thread of its own so the window's COM is not touched, and the list read
/// again when it is.
pub fn set(id: &str, volume: Option<u32>, muted: Option<bool>) {
    let id = id.to_string();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                if let Ok(device) = enumerator.GetDevice(&HSTRING::from(id.as_str())) {
                    let endpoint: Option<IAudioEndpointVolume> = device.Activate(CLSCTX_ALL, None).ok();
                    if let Some(endpoint) = endpoint {
                        if let Some(percent) = volume {
                            let _ = endpoint.SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null());
                        }
                        if let Some(muted) = muted {
                            let _ = endpoint.SetMute(muted, std::ptr::null());
                        }
                    }
                }
            }
            CoUninitialize();
        }
        LAST_MS.store(0, Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}
