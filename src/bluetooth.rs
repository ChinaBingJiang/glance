//! The Bluetooth devices Windows knows. The radios are counted with the
//! Bluetooth API, and the devices — those remembered, and those near enough to
//! be asked — are walked with the enumeration the old Bluetooth control-panel
//! snap-in used. A pairing is the shell's to be trusted, so the row that asks
//! for one opens Windows' own Bluetooth page; everything shown here is read
//! straight from the stack, not from a link.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Devices::Bluetooth::{
    BluetoothFindDeviceClose, BluetoothFindFirstDevice, BluetoothFindFirstRadio, BluetoothFindNextDevice, BluetoothFindNextRadio, BluetoothFindRadioClose,
    BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS, BLUETOOTH_FIND_RADIO_PARAMS,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// One device a radio can see, or has met.
#[derive(Clone)]
pub struct Device {
    /// Its name as it gives it.
    pub name: String,
    /// Its address, dotted as Windows shows it.
    pub addr: String,
    /// Whether this PC has paired with it before.
    pub remembered: bool,
    /// Whether it is in touch right now.
    pub connected: bool,
}

/// What the radios and their devices say all at once.
#[derive(Clone)]
pub struct Snapshot {
    /// How many radios the machine has.
    pub radios: usize,
    /// Devices paired before.
    pub paired: Vec<Device>,
    /// Devices found by an inquiry just now.
    pub nearby: Vec<Device>,
}

/// A wide fixed array turned to a String at its first NUL.
fn wide(text: &[u16]) -> String {
    let ended = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..ended])
}

/// A device's address, big-endian as Windows prints it.
fn addr(info: &BLUETOOTH_DEVICE_INFO) -> String {
    let b = unsafe { info.Address.Anonymous.rgBytes };
    format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", b[5], b[4], b[3], b[2], b[1], b[0])
}

/// Turns a device info block into a `Device`.
fn of(info: &BLUETOOTH_DEVICE_INFO) -> Device {
    Device {
        name: wide(&info.szName),
        addr: addr(info),
        remembered: info.fRemembered.0 != 0,
        connected: info.fConnected.0 != 0,
    }
}

/// The radio handles the machine has, each to be closed after use.
fn radios() -> Vec<HANDLE> {
    let params = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
    let mut out = Vec::new();
    let mut radio = HANDLE::default();
    let Ok(find) = (unsafe { BluetoothFindFirstRadio(&params, &mut radio) }) else {
        return out;
    };
    out.push(radio);
    loop {
        match unsafe { BluetoothFindNextRadio(find, &mut radio) } {
            Ok(()) => out.push(radio),
            Err(_) => break,
        }
    }
    let _ = unsafe { BluetoothFindRadioClose(find) };
    out
}

/// Whether the radio is up: an adapter answers the stack only while it is enabled,
/// so a radio heard of now means Bluetooth is on.
pub fn on() -> bool {
    !radios().is_empty()
}

/// Whether the machine has a Bluetooth adapter at all, enabled or not: a present
/// device of the Bluetooth class that is not one of the `BTHENUM\` children.
pub fn adapter() -> bool {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, DIGCF_PRESENT, GUID_DEVCLASS_BLUETOOTH,
        SP_DEVINFO_DATA,
    };
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_BLUETOOTH), PCWSTR::null(), None, DIGCF_PRESENT) else {
            return false;
        };
        let mut found = false;
        for index in 0.. {
            let mut info = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut info).is_err() {
                break;
            }
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInstanceIdW(set, &info as *const _, None, Some(&mut need));
            let mut buf = vec![0u16; need as usize];
            if SetupDiGetDeviceInstanceIdW(set, &info as *const _, Some(&mut buf), None).is_ok() {
                let ended = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                if !String::from_utf16_lossy(&buf[..ended]).to_uppercase().starts_with("BTHENUM\\") {
                    found = true;
                    break;
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}

/// Turns the machine's Bluetooth adapter on or off the way Device Manager does: a
/// property change put to the class installer. Glance runs elevated, so the call is
/// let; an adapter that will not take it is left as it was (false). Only the adapter
/// itself is touched — the `BTHENUM\` children are the paired devices, not the radio.
pub fn set(on: bool) -> bool {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiCallClassInstaller, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
        SetupDiSetClassInstallParamsW, DICS_DISABLE, DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIGCF_PRESENT, GUID_DEVCLASS_BLUETOOTH,
        SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS,
    };
    // The instance id of the device at `info`, as the wide form it is kept in.
    let id = |set, info: &SP_DEVINFO_DATA| -> String {
        let mut need = 0u32;
        unsafe {
            let _ = SetupDiGetDeviceInstanceIdW(set, info as *const _, None, Some(&mut need));
            let mut buf = vec![0u16; need as usize];
            if SetupDiGetDeviceInstanceIdW(set, info as *const _, Some(&mut buf), None).is_ok() {
                let ended = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                String::from_utf16_lossy(&buf[..ended])
            } else {
                String::new()
            }
        }
    };
    let mut done = false;
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_BLUETOOTH), PCWSTR::null(), None, DIGCF_PRESENT) else {
            return false;
        };
        for index in 0.. {
            let mut info = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut info).is_err() {
                break;
            }
            if id(set, &info).to_uppercase().starts_with("BTHENUM\\") {
                continue;
            }
            let mut change = SP_PROPCHANGE_PARAMS {
                ClassInstallHeader: SP_CLASSINSTALL_HEADER { cbSize: std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32, InstallFunction: DIF_PROPERTYCHANGE },
                StateChange: if on { DICS_ENABLE } else { DICS_DISABLE },
                Scope: DICS_FLAG_GLOBAL,
                HwProfile: 0,
            };
            let sized = std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32;
            let params = &mut change.ClassInstallHeader as *mut _ as *const _;
            if SetupDiSetClassInstallParamsW(set, Some(&info as *const _), Some(params), sized).is_ok()
                && SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, Some(&info as *const _)).is_ok()
            {
                done = true;
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    done
}

/// Walks the devices `radio` reports: the remembered, and — while `inquiry` —
/// those answering a call. (An inquiry must be bound to one radio; NULL is refused.)
fn walk(radio: HANDLE, inquiry: bool) -> Vec<Device> {
    let mut out = Vec::new();
    let params = BLUETOOTH_DEVICE_SEARCH_PARAMS {
        dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
        fReturnAuthenticated: BOOL(1),
        fReturnRemembered: BOOL(1),
        fReturnUnknown: BOOL(if inquiry { 1 } else { 0 }),
        fReturnConnected: BOOL(1),
        fIssueInquiry: BOOL(inquiry as i32),
        cTimeoutMultiplier: if inquiry { 4 } else { 1 },
        hRadio: radio,
    };
    let mut info = BLUETOOTH_DEVICE_INFO { dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
    let Ok(find) = (unsafe { BluetoothFindFirstDevice(&params, &mut info) }) else {
        return out;
    };
    loop {
        let device = of(&info);
        if !device.name.is_empty() && !out.iter().any(|d: &Device| d.addr == device.addr) {
            out.push(device);
        }
        if unsafe { BluetoothFindNextDevice(find, &mut info) }.is_err() {
            break;
        }
    }
    let _ = unsafe { BluetoothFindDeviceClose(find) };
    out
}

/// Everything shown at once: radios, remembered devices, and a brief inquiry for those near.
/// (The inquiry blocks about five seconds; `read` is asked off the render thread.)
pub fn read() -> Snapshot {
    let handles = radios();
    let radios = handles.len();
    let mut paired = Vec::new();
    let mut nearby = Vec::new();
    if let Some(&first) = handles.first() {
        paired = walk(first, false);
        nearby = walk(first, true);
        nearby.retain(|d| !paired.iter().any(|p| p.addr == d.addr));
    }
    for h in handles {
        unsafe { let _ = CloseHandle(h); };
    }
    Snapshot { radios, paired, nearby }
}

// ---- Cached, off-thread reads for the settings window's preview ----

/// The last snapshot taken, and whether one is being taken now.
static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The last snapshot, at once — never blocking the frame. An inquiry takes several
/// seconds, so a background read is started only where the last is well past its age
/// (twenty seconds); the window is woken when it lands. `None` until the first is done.
pub fn snapshot() -> Option<Snapshot> {
    let last = LAST_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 20000 {
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
