//! The system settings' shared helpers: the OS-level reads and writes that the
//! old settings window uses for its own pages (registry, clock, timezone). The
//! Mac-styled window that used to live here is gone — the old window
//! (`super::settings_window`) is now the sole host for every OS category. These
//! free functions remain available as `super::system_settings::X`.

use windows::core::HSTRING;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
    RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE, SendMessageTimeoutW,
};

// ---- Registry foundation ----

/// A string read from the registry; empty where it is not there.
pub(crate) fn reg_sz(root: HKEY, key: &str, value: &str) -> String {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut data = [0u16; 512];
    let mut size = data.len() as u32 * 2;
    let read = unsafe {
        RegGetValueW(
            root,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return String::new();
    }
    let length = data.iter().position(|&c| c == 0).unwrap_or(data.len());
    String::from_utf16_lossy(&data[..length])
}

/// A word read from the registry; `None` where it is not there.
pub(crate) fn reg_word(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let (mut data, mut size) = (0u32, 4u32);
    let read = unsafe {
        RegGetValueW(
            root,
            &key,
            &value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut _ as *mut _),
            Some(&mut size),
        )
    };
    read.is_ok().then_some(data)
}

/// A word written to the registry, under the user: the key made if it is not there.
pub(crate) fn reg_write(key: &str, value: &str, data: u32) {
    unsafe {
        let mut hkey = HKEY::default();
        let open = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if open.is_ok() {
            let _ = RegSetValueExW(
                hkey,
                &HSTRING::from(value),
                None,
                REG_DWORD,
                Some(&data.to_le_bytes()),
            );
            let _ = RegCloseKey(hkey);
        }
    }
}

/// A string written to the registry, under the user.
pub(crate) fn reg_write_sz(key: &str, value: &str, data: &str) {
    let wide: Vec<u8> = data
        .encode_utf16()
        .chain([0])
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    unsafe {
        let mut hkey = HKEY::default();
        let open = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if open.is_ok() {
            let _ = RegSetValueExW(hkey, &HSTRING::from(value), None, REG_SZ, Some(&wide));
            let _ = RegCloseKey(hkey);
        }
    }
}

/// A word sent round the desktop so that every window follows the setting just written.
pub(crate) fn broadcast_settings(section: &str) {
    let word: Vec<u16> = section.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(word.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            200,
            None,
        );
    }
}

// ---- Timezone ----

/// The timezone Windows is in now, by its own identifier (what `tzutil` takes).
pub(crate) fn current_zone() -> String {
    reg_sz(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation",
        "TimeZoneKeyName",
    )
}

// ---- Clock ----

pub(crate) fn clock_24() -> bool {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International", "sTimeFormat").contains("HH")
}

pub(crate) fn clock_seconds() -> bool {
    reg_word(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        "ShowSecondsInSystemClock",
    )
    .unwrap_or(0)
        != 0
}

pub(crate) fn toggle_seconds() {
    let path = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
    reg_write(path, "ShowSecondsInSystemClock", u32::from(!clock_seconds()));
    broadcast_settings("SubSystems");
}

pub(crate) fn toggle_clock24() {
    let now = clock_24();
    reg_write_sz(
        r"Control Panel\International",
        "sTimeFormat",
        if now { "h:mm:ss tt" } else { "HH:mm:ss" },
    );
    broadcast_settings("intl");
}
