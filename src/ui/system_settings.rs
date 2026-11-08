//! The system settings' shared helpers: the OS-level reads and writes that the
//! old settings window uses for its own pages (registry, clock, timezone, theme).
//! The Mac-styled window that used to live here is gone — the old window
//! (`super::settings_window`) is now the sole host for every OS category. These
//! free functions remain available as `super::system_settings::X`.

use std::mem::size_of;

use windows::core::{w, GUID, HSTRING, PWSTR, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex, PowerSetActiveScheme, PowerWriteACValueIndex, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_BINARY, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::{GetProductInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW, OS_PRODUCT_TYPE};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::UI::Shell::{SHLoadIndirectString, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    ANIMATIONINFO, HWND_BROADCAST, SMTO_ABORTIFHUNG, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
    SPI_GETANIMATION, SPI_SETANIMATION, SPI_SETDESKWALLPAPER, SW_HIDE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SendMessageTimeoutW, SystemParametersInfoW, WM_SETTINGCHANGE,
};

use super::text::Lang;

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

// ---- Theme: dark, transparent, animation, accent ----

/// Whether the system is in dark mode now.
pub(crate) fn system_dark() -> bool {
    reg_word(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    ) != Some(1)
}

/// Puts the system in dark or light mode; the desktop is told so every window follows.
pub(crate) fn set_system_dark(dark: bool) {
    let value = u32::from(!dark);
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            None,
            KEY_WRITE,
            &mut key,
        )
        .is_ok()
        {
            for name in ["AppsUseLightTheme", "SystemUsesLightTheme"] {
                let _ = RegSetValueExW(
                    key,
                    &HSTRING::from(name),
                    None,
                    REG_DWORD,
                    Some(&value.to_le_bytes()),
                );
            }
            let _ = RegCloseKey(key);
        }
        broadcast_settings("Theme");
    }
}

/// Whether transparency (frosted glass) is on.
pub(crate) fn system_transparent() -> bool {
    reg_word(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "EnableTransparency",
    ) == Some(1)
}

/// Turns transparency on or off.
pub(crate) fn set_system_transparent(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            None,
            KEY_WRITE,
            &mut key,
        )
        .is_ok()
        {
            let data = u32::from(on).to_le_bytes();
            let _ = RegSetValueExW(
                key,
                &HSTRING::from("EnableTransparency"),
                None,
                REG_DWORD,
                Some(&data),
            );
            let _ = RegCloseKey(key);
        }
    }
    broadcast_settings("Themes");
}

/// Whether system animations are on.
pub(crate) fn system_animation() -> bool {
    let mut info = ANIMATIONINFO {
        cbSize: size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: 0,
    };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETANIMATION,
            size_of::<ANIMATIONINFO>() as u32,
            Some(&mut info as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    info.iMinAnimate != 0
}

/// Turns system animations on or off.
pub(crate) fn set_system_animation(on: bool) {
    let info = ANIMATIONINFO {
        cbSize: size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: i32::from(on),
    };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETANIMATION,
            size_of::<ANIMATIONINFO>() as u32,
            Some(&info as *const _ as *mut _),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

/// Whether accent colour shows on title bars and borders.
pub(crate) fn color_prevalence() -> bool {
    reg_word(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\DWM",
        "ColorPrevalence",
    )
    .unwrap_or(0)
        != 0
}

/// Turns accent colour on borders on or off.
pub(crate) fn set_color_prevalence(on: bool) {
    reg_write(
        r"Software\Microsoft\Windows\DWM",
        "ColorPrevalence",
        u32::from(on),
    );
    reg_write(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "ColorPrevalence",
        u32::from(on),
    );
    broadcast_settings("Theme");
}

/// Sets the accent colour (0xRRGGBB) in the registry.
pub(crate) fn set_accent_colour(rgb: u32) {
    // Windows stores accent as 0x00BBGGRR in the DWM key.
    let packed = ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF);
    reg_write(r"Software\Microsoft\Windows\DWM", "AccentColor", packed);
    reg_write(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentColorMenu",
        packed,
    );
    broadcast_settings("ImmersiveColorSet");
}

// ---- Themes: the .theme files Windows can wear ----

/// The themes that can be put on: Windows' own files, then those made here. Each told by the name it
/// gives itself, and by the file that carries it; sorted where they are found.
pub(crate) fn themes() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for room in [
        std::env::var_os("SystemRoot").map(|root| std::path::PathBuf::from(root).join(r"resources\Themes")),
        std::env::var_os("LOCALAPPDATA").map(|here| std::path::PathBuf::from(here).join(r"Microsoft\Windows\Themes")),
    ]
    .into_iter()
    .flatten()
    {
        let Ok(listed) = std::fs::read_dir(&room) else {
            continue;
        };
        let mut here: Vec<(String, String)> = listed
            .flatten()
            .map(|one| one.path())
            .filter(|file| file.extension().is_some_and(|end| end == "theme"))
            .map(|file| (theme_name(&file), file.to_string_lossy().into_owned()))
            .filter(|(name, _)| !name.is_empty())
            .collect();
        here.sort();
        found.extend(here);
    }
    found
}

/// The themes to choose between, by their names, and which of them is worn now.
pub(crate) fn theme_choices() -> (Vec<String>, Option<usize>) {
    let listed = themes();
    let worn = theme_now();
    let chosen = listed.iter().position(|(_, path)| same_file(path, &worn));
    (listed.into_iter().map(|(name, _)| name).collect(), chosen)
}

/// The theme worn: the file `CurrentTheme` points at.
fn theme_now() -> String {
    reg_sz(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes",
        "CurrentTheme",
    )
}

/// Puts a theme on by its index in `themes()`.
pub(crate) fn wear_theme(index: usize) {
    let listed = themes();
    let Some((_, path)) = listed.get(index) else {
        return;
    };
    wear(path);
}

/// Puts a theme on by its file path.
pub(crate) fn wear(file: &str) {
    let word = HSTRING::from(file);
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &word, PCWSTR::null(), PCWSTR::null(), SW_HIDE);
    }
}

/// Whether two ways of naming one file name the same file.
fn same_file(one: &str, other: &str) -> bool {
    !one.is_empty() && leaned(one) == leaned(other)
}

/// A path normalised for comparison: lowercase, forward slashes.
fn leaned(way: &str) -> String {
    way.replace('\\', "/").to_lowercase()
}

/// The name a theme file gives itself: the `DisplayName` under `[Theme]`.
pub(crate) fn theme_name(file: &std::path::Path) -> String {
    let Ok(bytes) = std::fs::read(file) else {
        return String::new();
    };
    let written = if bytes.starts_with(&[0xff, 0xfe]) {
        String::from_utf16_lossy(
            &bytes[2..]
                .chunks_exact(2)
                .map(|two| u16::from_le_bytes([two[0], two[1]]))
                .collect::<Vec<u16>>(),
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let mut in_theme = false;
    for line in written.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_theme = line == "[Theme]";
            continue;
        }
        if !in_theme {
            continue;
        }
        let Some(said) = line.strip_prefix("DisplayName=").map(|rest| rest.trim_matches('"')) else {
            continue;
        };
        if said.starts_with('@') {
            if let Some(word) = from_dll(said) {
                return word;
            }
        } else if !said.is_empty() {
            return said.to_string();
        }
    }
    file.file_stem()
        .map_or(String::new(), |stem| stem.to_string_lossy().into_owned())
}

/// The wallpaper path a theme file declares: `Wallpaper` under `[WallPaper]` — or, as Windows
/// 11 writes its own themes, under `[Control Panel\Desktop]` — with environment variables
/// expanded. None where the theme has no wallpaper or the file cannot be read.
pub(crate) fn theme_wallpaper(file: &str) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    let written = if bytes.starts_with(&[0xff, 0xfe]) {
        String::from_utf16_lossy(
            &bytes[2..]
                .chunks_exact(2)
                .map(|two| u16::from_le_bytes([two[0], two[1]]))
                .collect::<Vec<u16>>(),
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let mut in_wallpaper = false;
    for line in written.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // (Only the desktop's own section: `[Control Panel\Colors]` keeps a `Wallpaper` colour too.)
            in_wallpaper = line.eq_ignore_ascii_case("[wallpaper]") || line.eq_ignore_ascii_case("[control panel\\desktop]");
            continue;
        }
        if !in_wallpaper {
            continue;
        }
        if let Some(raw) = line.strip_prefix("Wallpaper=").or_else(|| line.strip_prefix("wallpaper=")).map(|r| r.trim_matches('"')) {
            if raw.is_empty() {
                return None;
            }
            return Some(expand_vars(raw));
        }
    }
    None
}

/// Expands `%VAR%` tokens in a path using the environment.
fn expand_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if let Some(val) = std::env::var_os(name) {
                out.push_str(&val.to_string_lossy());
            }
            rest = &after[end + 1..];
        } else {
            out.push('%');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// A name Windows keeps in a dll: `@path,-id` resolved.
fn from_dll(said: &str) -> Option<String> {
    let source = HSTRING::from(said);
    let mut buffer = [0u16; 256];
    unsafe { SHLoadIndirectString(&source, &mut buffer, None) }.ok()?;
    let ended = buffer.iter().position(|&letter| letter == 0).unwrap_or(buffer.len());
    let word = String::from_utf16_lossy(&buffer[..ended]);
    (!word.is_empty()).then_some(word)
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

/// How far the zone sits from UTC as it stands now, in the form the clock itself
/// writes it ("UTC+8"); nothing where the machine keeps no bias to read.
pub(crate) fn zone_shift() -> Option<String> {
    let bias = reg_word(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation", "ActiveTimeBias")? as i32;
    // (The bias counts the minutes behind UTC: the offset is the other way about.)
    let ahead = -bias;
    let (sign, off) = if ahead < 0 { ('-', -ahead) } else { ('+', ahead) };
    let (hours, mins) = (off / 60, off % 60);
    Some(if mins == 0 {
        format!("UTC{sign}{hours}")
    } else {
        format!("UTC{sign}{hours}:{mins:02}")
    })
}

/// Whether the clock is on summer time this minute.
pub(crate) fn summer_time() -> bool {
    reg_word(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation", "DaylightTimeActive").unwrap_or(0) != 0
}

/// The clock as the machine reads it, to the second ("2026-11-04 15:04:05").
pub(crate) fn wall_clock() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}  {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
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

// ---- Binary registry helpers (taskbar's StuckRects3) ----

/// A byte run read from the registry; empty where it is not there.
fn reg_bytes(root: HKEY, key: &str, value: &str) -> Vec<u8> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut size = 0u32;
    if unsafe { RegGetValueW(root, &key, &value, RRF_RT_REG_BINARY, None, None, Some(&mut size)) }.is_err() {
        return Vec::new();
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { RegGetValueW(root, &key, &value, RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr() as *mut _), Some(&mut size)) }.is_err() {
        return Vec::new();
    }
    data.truncate(size as usize);
    data
}

/// A byte run written to the registry, under the user.
fn reg_write_bytes(key: &str, value: &str, data: &[u8]) {
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut hkey, None).is_ok() {
            let _ = RegSetValueExW(hkey, &HSTRING::from(value), None, REG_BINARY, Some(data));
            let _ = RegCloseKey(hkey);
        }
    }
}

/// A multi-string read from the registry, split at its nulls into its parts.
pub(crate) fn reg_multi(root: HKEY, key: &str, value: &str) -> Vec<String> {
    use windows::Win32::System::Registry::RRF_RT_REG_MULTI_SZ;
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut units = [0u16; 1024];
    let mut size = (units.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(
            root,
            &key,
            &value,
            RRF_RT_REG_MULTI_SZ,
            None,
            Some(units.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return Vec::new();
    }
    let n = (size as usize / 2).min(units.len());
    units[..n]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf16_lossy(s))
        .collect()
}

// ---- Power & battery ----

/// The three schemes Windows names, by their GUID and their words in both tongues.
const PLANS: [(GUID, &str, &str); 3] = [
    (GUID::from_u128(0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c), "最佳性能", "Best performance"),
    (GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df5e), "平衡", "Balanced"),
    (GUID::from_u128(0x961cc777_2547_4f9b_8174_4d86181b8cd7), "节能", "Power saver"),
];

/// The active plan's place in `PLANS` (None when a scheme Windows does not name).
fn active_plan() -> Option<usize> {
    let mut p: *mut GUID = std::ptr::null_mut();
    let got = unsafe { PowerGetActiveScheme(None, &mut p as *mut _ as *mut _) };
    if got.is_err() || p.is_null() {
        return None;
    }
    let worn = unsafe { *p };
    PLANS.iter().position(|(g, ..)| *g == worn)
}

/// The plans to choose between, and which is on now.
pub(crate) fn plan_choices(zh: bool) -> (Vec<String>, Option<usize>) {
    (PLANS.iter().map(|(_, c, e)| if zh { (*c).to_string() } else { (*e).to_string() }).collect(), active_plan())
}

/// Puts a plan on, by its place in `PLANS`.
pub(crate) fn set_plan(index: usize) {
    if let Some((g, ..)) = PLANS.get(index) {
        let _ = unsafe { PowerSetActiveScheme(None, Some(g as *const GUID)) };
        broadcast_settings("power");
    }
}

/// The sleep sub-group and its idle key.
const SUB_SLEEP: GUID = GUID::from_u128(0x238c9fa8_0aad_4b51_97fc_47a08b115a6c);
const STANDBYIDLE: GUID = GUID::from_u128(0x29f6c1db_86da_48c5_9fdb_f2b67b1f44da);

/// The active scheme's GUID, for the calls that must name it.
fn active_scheme() -> Option<GUID> {
    let mut p: *mut GUID = std::ptr::null_mut();
    if unsafe { PowerGetActiveScheme(None, &mut p as *mut _ as *mut _) }.is_ok() && !p.is_null() {
        Some(unsafe { *p })
    } else {
        None
    }
}

/// A power setting's value in seconds, for the active plan (AC while on a cable).
fn read_power_seconds(sub: GUID, key: GUID) -> Option<u32> {
    let mut value = 0u32;
    let got = unsafe { PowerReadACValueIndex(None, None, Some(&sub), Some(&key), &mut value) };
    got.is_ok().then_some(value)
}

/// Writes a power setting's seconds to the active plan (AC).
fn write_power_seconds(sub: GUID, key: GUID, seconds: u32) {
    if let Some(scheme) = active_scheme() {
        let _ = unsafe { PowerWriteACValueIndex(None, &scheme, Some(&sub), Some(&key), seconds) };
    }
    broadcast_settings("power");
}

/// The screen's turn-off delay, in minutes, read from the console's own key.
pub(crate) fn screen_timeout() -> u32 {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\Desktop", "ScreenSaveTimeOut").parse().unwrap_or(0) / 60
}

/// Sets the screen's turn-off delay, in minutes (0: never).
pub(crate) fn set_screen_timeout(minutes: u32) {
    reg_write_sz(r"Control Panel\Desktop", "ScreenSaveTimeOut", &(minutes * 60).to_string());
    reg_write_sz(r"Control Panel\Desktop", "ScreenSaverIsSecure", if minutes == 0 { "0" } else { "1" });
    broadcast_settings("Desktop");
}

/// The machine's sleep delay, in minutes, from the plan.
pub(crate) fn sleep_timeout() -> u32 {
    read_power_seconds(SUB_SLEEP, STANDBYIDLE).unwrap_or(0) / 60
}

/// Sets the machine's sleep delay, in minutes (0: never).
pub(crate) fn set_sleep_timeout(minutes: u32) {
    write_power_seconds(SUB_SLEEP, STANDBYIDLE, minutes * 60);
}

/// The battery, in per cent, and whether it is charging; None with no battery.
pub(crate) fn battery() -> Option<(u32, bool)> {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() || status.BatteryLifePercent == 255 {
        return None;
    }
    let charging = status.ACLineStatus == 1 && (status.BatteryFlag & 8 != 0 || status.BatteryFlag & 128 == 0 && status.BatteryLifePercent >= 100);
    Some((status.BatteryLifePercent as u32, charging))
}

/// The battery's health, in per cent, from its driver.
pub(crate) fn battery_health() -> Option<u32> {
    crate::battery::read().health.map(|h| h.round() as u32)
}

// ---- Keyboard & mouse ----

/// A key's hold before it repeats, Windows' 0-3 scale.
pub(crate) fn key_delay() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Control Panel\Keyboard", "KeyboardDelay").unwrap_or(1)
}

pub(crate) fn set_key_delay(value: u32) {
    reg_write(r"Control Panel\Keyboard", "KeyboardDelay", value.clamp(0, 3));
    broadcast_settings("Keyboard");
}

/// How fast a key repeats once held, Windows' 0-31 scale.
pub(crate) fn key_rate() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Control Panel\Keyboard", "KeyboardSpeed").unwrap_or(31).min(31)
}

pub(crate) fn set_key_rate(value: u32) {
    reg_write(r"Control Panel\Keyboard", "KeyboardSpeed", value.clamp(0, 31));
    broadcast_settings("Keyboard");
}

/// The pointer's speed, 0-20.
pub(crate) fn mouse_speed() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Control Panel\Mouse", "MouseSensitivity").unwrap_or(10).min(20)
}

pub(crate) fn set_mouse_speed(value: u32) {
    let value = value.clamp(0, 20);
    reg_write(r"Control Panel\Mouse", "MouseSensitivity", value);
    reg_write(r"Control Panel\Mouse", "MouseSpeed", u32::from(value > 6));
    broadcast_settings("Mouse");
}

/// The wheel's lines at a notch; -1 (as 0xFFFF) scrolls a page.
pub(crate) fn scroll_lines() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Control Panel\Desktop", "WheelScrollLines").unwrap_or(3)
}

pub(crate) fn set_scroll_lines(value: u32) {
    reg_write(r"Control Panel\Desktop", "WheelScrollLines", value);
    broadcast_settings("MouseScreen");
}

/// The pointer's scheme, by the name it gives itself.
pub(crate) fn cursor_scheme() -> String {
    let name = reg_sz(HKEY_CURRENT_USER, r"Control Panel\Cursors", "Scheme Name");
    if name.is_empty() {
        String::from("(default)")
    } else {
        name
    }
}

// ---- Accessibility ----

/// Whether high contrast is on.
pub(crate) fn high_contrast() -> bool {
    reg_word(HKEY_CURRENT_USER, r"Control Panel\Accessibility\HighContrast", "Flags").unwrap_or(0) & 0x1 != 0
}

/// Turns high contrast on or off, and tells the desktop.
pub(crate) fn set_high_contrast(on: bool) {
    let flags = if on { 0x3f } else { 0 };
    reg_write(r"Control Panel\Accessibility\HighContrast", "Flags", flags);
    reg_write(r"Control Panel\Accessibility\HighContrast", "HighContrastOn", u32::from(on));
    broadcast_settings("Theme");
}

/// The text scale, in per cent (100 as drawn).
pub(crate) fn text_scale() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Accessibility", "TextScaleFactor").unwrap_or(100)
}

/// Sets the text scale, in per cent; the log-off to take it fully.
pub(crate) fn set_text_scale(percent: u32) {
    reg_write(r"Software\Microsoft\Accessibility", "TextScaleFactor", percent.clamp(100, 225));
    broadcast_settings("Windows");
}

/// The pointer's size, Windows' 1-15 scale.
pub(crate) fn cursor_size() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Accessibility", "CursorSize").unwrap_or(1).clamp(1, 15)
}

/// Sets the pointer's size, Windows' 1-15 scale.
pub(crate) fn set_cursor_size(size: u32) {
    reg_write(r"Software\Microsoft\Accessibility", "CursorSize", size.clamp(1, 15));
    broadcast_settings("Windows");
}

// ---- Focus / Do not disturb ----

const DND_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";

/// Whether Do Not Disturb is on (notifications hushed).
pub(crate) fn dnd_on() -> bool {
    reg_word(HKEY_CURRENT_USER, DND_KEY, "NOC_GLOBAL_SETTING_TOASTS_ENABLED") == Some(0)
}

/// Turns Do Not Disturb on or off.
pub(crate) fn set_dnd(on: bool) {
    reg_write(DND_KEY, "NOC_GLOBAL_SETTING_TOASTS_ENABLED", u32::from(!on));
    reg_write(DND_KEY, "NOC_GLOBAL_SETTING_SUPPRESS_ADD_TO_QUEUE", u32::from(on));
    broadcast_settings("Dnd");
}

/// Whether notices may still show above the lock screen.
pub(crate) fn lock_toast() -> bool {
    reg_word(HKEY_CURRENT_USER, DND_KEY, "NOC_GLOBAL_SETTING_ALLOW_TOASTS_ABOVE_LOCK").unwrap_or(1) != 0
}

/// Lets or holds notices above the lock screen.
pub(crate) fn set_lock_toast(on: bool) {
    reg_write(DND_KEY, "NOC_GLOBAL_SETTING_ALLOW_TOASTS_ABOVE_LOCK", u32::from(on));
    broadcast_settings("Dnd");
}

// ---- Gaming ----

const GAME_DVR: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";
const GAME_BAR: &str = r"Software\Microsoft\GameBar";

/// Whether a Game DVR flag is set; off where it is not there.
pub(crate) fn game_flag(name: &str) -> bool {
    reg_word(HKEY_CURRENT_USER, GAME_DVR, name).unwrap_or(0) != 0
}

/// Flips a Game DVR flag.
pub(crate) fn toggle_game_flag(name: &str) {
    reg_write(GAME_DVR, name, u32::from(!game_flag(name)));
    broadcast_settings("Environment");
}

/// Whether Game Mode may take over automatically; on where it is not there.
pub(crate) fn game_mode() -> bool {
    reg_word(HKEY_CURRENT_USER, GAME_BAR, "AllowAutoGameMode").unwrap_or(1) != 0
}

/// Lets or holds Game Mode from taking over automatically.
pub(crate) fn set_game_mode(on: bool) {
    reg_write(GAME_BAR, "AllowAutoGameMode", u32::from(on));
    broadcast_settings("Environment");
}

// ---- Windows Update ----

const WIN_VER: &str = r"Software\Microsoft\Windows NT\CurrentVersion";

/// The running Windows' real version — major, minor and build — read from
/// `ntdll`'s `RtlGetVersion`. Unlike the old `GetVersionEx`, it is not capped
/// by an application manifest, so it tells the build the system truly is.
/// Zeros where the call fails.
fn os_version() -> (u32, u32, u32) {
    let mut v = OSVERSIONINFOW { dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    if unsafe { RtlGetVersion(&mut v) }.is_ok() {
        (v.dwMajorVersion, v.dwMinorVersion, v.dwBuildNumber)
    } else {
        (0, 0, 0)
    }
}

/// The edition `GetProductInfo` reports, by its product-type number; empty for
/// one not named here. This is the licensed edition the system carries, read
/// from the OS itself rather than the name the registry happens to keep.
fn product_edition() -> &'static str {
    let (major, minor, _) = os_version();
    let mut kind = OS_PRODUCT_TYPE(0);
    if !unsafe { GetProductInfo(major, minor, 0, 0, &mut kind) }.as_bool() {
        return "";
    }
    match kind.0 {
        1 | 28 | 71 => "Ultimate",
        2 | 3 | 5 | 26 | 67 | 68 | 98 | 99 | 100 | 101 => "Home",
        6 | 7 | 48 | 49 | 69 | 103 => "Pro",
        161 | 162 => "Pro Workstation",
        4 | 27 | 70 | 84 | 125 | 126 | 129 | 130 => "Enterprise",
        121 | 122 => "Education",
        123 | 131 => "IoT Core",
        188..=191 => "IoT Enterprise",
        8 | 12 | 13 | 24 | 25 | 33 | 36 => "Server",
        _ => "",
    }
}

/// The installed Windows' edition, e.g. "Windows 11 Pro" — worked out from the
/// system's own APIs: the build number tells whether it is 10 or 11 (a build
/// of 22000 and up is 11, whatever the registry's product name once claimed),
/// and `GetProductInfo` tells the edition.
pub(crate) fn os_product() -> String {
    let (_, _, build) = os_version();
    let family = if build >= 22000 {
        "Windows 11"
    } else if build > 0 {
        "Windows 10"
    } else {
        "Windows"
    };
    let edition = product_edition();
    if edition.is_empty() {
        family.to_string()
    } else {
        format!("{family} {edition}")
    }
}

/// The release id and build joined, e.g. "23H2 · 22631"; either may be empty.
/// The feature-update label (24H2 and the like) lives only in the registry, so
/// it is read there; the build number comes from `RtlGetVersion`.
pub(crate) fn os_release() -> (String, String) {
    let (_, _, build) = os_version();
    (reg_sz(HKEY_LOCAL_MACHINE, WIN_VER, "DisplayVersion"), if build > 0 { build.to_string() } else { String::new() })
}

/// The day this Windows was installed, as "YYYY-MM-DD"; empty where it is not there.
pub(crate) fn os_install_date() -> String {
    let Some(secs) = reg_word(HKEY_LOCAL_MACHINE, WIN_VER, "InstallDate") else {
        return String::new();
    };
    let (y, m, d) = civil_from_days(secs as i64 / 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How Windows Update is set to fetch fixes: the `AUOptions` the machine keeps,
/// where it keeps one. None is the Windows default (managed of its own accord).
pub(crate) fn update_auto_option() -> Option<u32> {
    reg_word(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update", "AUOptions")
        .or_else(|| reg_word(HKEY_LOCAL_MACHINE, r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU", "AUOptions"))
}

/// The day Windows Update last looked for fixes, as "YYYY-MM-DD"; empty where it
/// has not, or does not tell. The value reads "2024-01-01:00:00:00" in UTC.
pub(crate) fn update_last_check() -> String {
    reg_sz(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\Results\Detect",
        "LastSuccessTime",
    )
    .split(':')
    .next()
    .unwrap_or("")
    .to_string()
}

/// Every drive mounted with a letter, with the room it has taken and the room it
/// holds. Both asked of the volume's root, which answers at once: nothing is walked,
/// so the drawn frame may wait on it. A letter that will not answer is left out.
pub(crate) fn drive_space() -> Vec<(String, u64, u64)> {
    let mounted = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|letter| mounted & (1 << letter) != 0)
        .filter_map(|letter| {
            let name = format!("{}:", (b'A' + letter) as char);
            let root: Vec<u16> = format!("{name}\\").encode_utf16().chain([0]).collect();
            let (mut total, mut free) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut total), Some(&mut free)) }.ok()?;
            (total > 0).then(|| (name, total - free, total))
        })
        .collect()
}

/// A physical disk, the letters mounted on it, and the room they hold. The storage page groups by
/// this, so one drive split over several letters is told once rather than as loose volumes.
pub(crate) struct Disk {
    pub number: u32,
    pub model: String,
    pub size: u64,
    pub volumes: Vec<(String, u64, u64)>,
}

/// The physical disk a mounted letter sits on, asked of the volume's own handle opened for its
/// properties only (no data touched): the disk number the volume reports spanning, which meets the
/// `DeviceNumber` that `crate::drives::models` keys by, so a letter and its disk share one number.
/// A volume striped over several disks is grouped by its first. A letter that will not answer is `None`.
fn disk_of_letter(letter: &str) -> Option<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING};
    use windows::Win32::System::Ioctl::VOLUME_DISK_EXTENTS;
    use windows::Win32::System::IO::DeviceIoControl;
    let path: Vec<u16> = format!(r"\\.\{letter}:").encode_utf16().chain([0]).collect();
    unsafe {
        let handle = CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None).ok()?;
        let mut extents = VOLUME_DISK_EXTENTS::default();
        let asked = DeviceIoControl(handle, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, 0, Some(&mut extents as *mut _ as *mut _), size_of::<VOLUME_DISK_EXTENTS>() as u32, None, None);
        let _ = CloseHandle(handle);
        (asked.is_ok() && extents.NumberOfDiskExtents > 0).then(|| extents.Extents[0].DiskNumber)
    }
}

/// Every physical disk that carries a mounted letter, each with the letters on it and the room they
/// hold: the model read from `crate::drives::models`, the size the sum of its volumes. The room of
/// each letter asked of the volume's root (which answers at once, nothing walked), its disk from
/// the property handle. A letter whose disk cannot be read is kept under a number of its own, so
/// nothing mounted goes missing.
pub(crate) fn disks() -> Vec<Disk> {
    let models = crate::drives::models();
    let mut out: Vec<Disk> = Vec::new();
    let mut lost = 0x8000_0000u32;
    for (name, used, total) in drive_space() {
        let letter = name.trim_end_matches(':');
        let number = match disk_of_letter(letter) {
            Some(n) => n,
            None => {
                let n = lost;
                lost += 1;
                n
            }
        };
        if let Some(disk) = out.iter_mut().find(|d| d.number == number) {
            disk.size += total;
            disk.volumes.push((name, used, total));
            continue;
        }
        let model = models.iter().find(|(n, _)| *n == number).map(|(_, m)| m.clone()).unwrap_or_default();
        out.push(Disk { number, model, size: total, volumes: vec![(name, used, total)] });
    }
    out.sort_by_key(|d| d.number);
    out
}

/// A date from a count of days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---- Desktop & taskbar ----

const STUCK_RECTS: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";

/// One byte of StuckRects3: its 12th tells the edge the taskbar stands on, its 16th carries the
/// state bits (auto-hide among them) — the two must not be read or written as one.
fn taskbar_byte(at: usize) -> u8 {
    reg_bytes(HKEY_CURRENT_USER, STUCK_RECTS, "Settings").get(at).copied().unwrap_or(0)
}

/// Sets one byte of StuckRects3 and restarts nothing: a refresh tells Explorer.
fn put_taskbar_byte(at: usize, value: u8) {
    let mut data = reg_bytes(HKEY_CURRENT_USER, STUCK_RECTS, "Settings");
    if data.len() <= at {
        return;
    }
    data[at] = value;
    reg_write_bytes(STUCK_RECTS, "Settings", &data);
    broadcast_settings("");
}

/// Whether the taskbar hides until the pointer comes: the state byte's second bit, kept apart
/// from the edge byte the position lives in.
pub(crate) fn taskbar_autohide() -> bool {
    taskbar_byte(16) & 2 != 0
}

pub(crate) fn set_taskbar_autohide(on: bool) {
    let was = taskbar_byte(16);
    put_taskbar_byte(16, if on { was | 2 } else { was & !2 });
}

/// The taskbar's edge: 0 left, 1 top, 2 right, 3 bottom.
pub(crate) fn taskbar_position() -> u32 {
    taskbar_byte(12) as u32
}

pub(crate) fn set_taskbar_position(edge: u32) {
    put_taskbar_byte(12, edge.clamp(0, 3) as u8);
}

/// Whether the taskbar's icons are small.
pub(crate) fn taskbar_small() -> bool {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarSmallIcons").unwrap_or(0) != 0
}

pub(crate) fn set_taskbar_small(on: bool) {
    reg_write(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarSmallIcons", u32::from(on));
    broadcast_settings("TaskbarSmallIcons");
}

/// How taskbar buttons combine: 0 always, 1 when full, 2 never.
pub(crate) fn combine_icons() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarGlomLevel").unwrap_or(0)
}

pub(crate) fn set_combine_icons(value: u32) {
    reg_write(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarGlomLevel", value.clamp(0, 2));
    broadcast_settings("TaskbarGlomLevel");
}

/// The taskbar's size: Windows' TaskbarSi number, 0 small, 1 medium, 2 large.
pub(crate) fn taskbar_size() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarSi").unwrap_or(1)
}

pub(crate) fn set_taskbar_size(value: u32) {
    reg_write(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarSi", value.clamp(0, 2));
    broadcast_settings("TaskbarSi");
}

/// Where the taskbar's icons stand: Windows' TaskbarAl number, 0 against the start's corner, 1 in
/// the middle of the bar (Windows 11's own setting; 1 is what Explorer keeps when it was never told).
pub(crate) fn taskbar_align() -> u32 {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl").unwrap_or(1)
}

pub(crate) fn set_taskbar_align(value: u32) {
    reg_write(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl", value.clamp(0, 1));
    broadcast_settings("TaskbarAl");
}

// ---- Wallpaper ----

/// The wallpapers Windows ships, and those the themes use.
pub(crate) fn wallpapers() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(std::path::Path::new(&root).join("Web\\Wallpaper"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(std::path::Path::new(&local).join(r"Microsoft\Windows\Themes"));
    }
    for dir in dirs {
        collect_wallpapers(&dir, &mut found, 0);
    }
    found.sort();
    found.dedup_by(|a, b| a.1 == b.1);
    found
}

/// Gathers picture files under `dir`, up to a depth, by their stem and full path.
fn collect_wallpapers(dir: &std::path::Path, out: &mut Vec<(String, String)>, depth: u32) {
    if depth > 2 {
        return;
    }
    let Ok(listed) = std::fs::read_dir(dir) else { return };
    for entry in listed.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_wallpapers(&path, out, depth + 1);
        } else if path.extension().is_some_and(|end| matches!(end.to_str(), Some("jpg") | Some("jpeg") | Some("png") | Some("bmp"))) {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push((name, path.to_string_lossy().into_owned()));
        }
    }
}

/// The wallpaper now on, by its file.
pub(crate) fn current_wallpaper() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\Desktop", "WallPaper")
}

/// Puts a wallpaper file on the desktop.
pub(crate) fn set_wallpaper(path: &str) {
    let word: Vec<u16> = path.encode_utf16().chain([0]).collect();
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(word.as_ptr() as *mut _), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
    }
}

/// How the wallpaper fills the screen: Windows' WallpaperStyle number.
pub(crate) fn wallpaper_style() -> u32 {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\Desktop", "WallpaperStyle").parse().unwrap_or(10)
}

/// Sets how the wallpaper fills the screen.
pub(crate) fn set_wallpaper_style(style: u32) {
    reg_write_sz(r"Control Panel\Desktop", "WallpaperStyle", &style.to_string());
    reg_write_sz(r"Control Panel\Desktop", "TileWallpaper", if style == 0 { "1" } else { "0" });
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, None, SPIF_UPDATEINIFILE | SPIF_SENDCHANGE);
    }
}

// ---- Account ----

/// The signed-in user's name.
pub(crate) fn user_name() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

/// The account's full name where Windows keeps it, else the sign-in name.
pub(crate) fn display_name() -> String {
    let full = reg_sz(HKEY_CURRENT_USER, r"Volatile Environment", "DisplayName");
    if full.is_empty() {
        user_name()
    } else {
        full
    }
}

/// The machine's name.
pub(crate) fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// Whether the lock screen shows when the PC wakes.
pub(crate) fn lock_screen_on() -> bool {
    reg_word(HKEY_CURRENT_USER, r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon", "LockWorkStationTimeout").unwrap_or(0) != 0
        || reg_sz(HKEY_CURRENT_USER, r"Control Panel\Desktop", "ScreenSaverIsSecure") == "1"
}

/// Turns the lock screen on waking on or off.
pub(crate) fn set_lock_screen(on: bool) {
    reg_write_sz(r"Control Panel\Desktop", "ScreenSaverIsSecure", if on { "1" } else { "0" });
    broadcast_settings("Desktop");
}

// ---- Control centre ----

/// The quick-setting buttons shown, by the name each carries.
pub(crate) fn quick_buttons() -> Vec<String> {
    let key = r"Software\Microsoft\Windows\CurrentVersion\Control Panel\Quick Settings\PrimaryApplications";
    let mut names = Vec::new();
    let mut hkey = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, KEY_READ, &mut hkey) }.is_ok() {
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let mut class = [0u16; 64];
            let mut clen = 0u32;
            let got = unsafe {
                windows::Win32::System::Registry::RegEnumKeyExW(
                    hkey,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    Some(PWSTR(class.as_mut_ptr())),
                    Some(&mut clen),
                    None,
                )
            };
            if got.is_err() {
                break;
            }
            let ended = name.iter().position(|&c| c == 0).unwrap_or(len as usize);
            names.push(String::from_utf16_lossy(&name[..ended]));
            index += 1;
            if index > 32 {
                break;
            }
        }
        let _ = unsafe { RegCloseKey(hkey) };
    }
    names.sort();
    names
}

// ---- About this PC ----

/// The machine's own facts: name, value; the names localised by `zh`. Gathered
/// as macOS lays out its 关于本机: the machine named over its maker, model,
/// system, chip, memory, graphics, disk, serial and the day Windows came.
pub(crate) fn about_facts(zh: bool) -> Vec<(String, String)> {
    let label = |c: &str, e: &str| if zh { c.to_string() } else { e.to_string() };
    let mut facts = Vec::new();
    facts.push((label("设备名称", "Device name"), computer_name()));
    // The maker, model and serial, from the firmware's own inventory (SMBIOS).
    let (maker, model, serial) = crate::smbios::system();
    if !maker.is_empty() {
        facts.push((label("制造商", "Manufacturer"), maker));
    }
    if !model.is_empty() {
        facts.push((label("机型", "Model"), model));
    }
    // Windows edition, its version and build — the name worked out from the
    // system's own APIs, not the string the registry keeps.
    let display = reg_sz(HKEY_LOCAL_MACHINE, WIN_VER, "DisplayVersion");
    let (_, _, build) = os_version();
    facts.push((label("操作系统", "Operating system"), os_product()));
    if !display.is_empty() {
        facts.push((label("版本", "Version"), display));
    }
    if build > 0 {
        facts.push((label("内部版本", "OS build"), build.to_string()));
    }
    // Processor, from the hardware hive.
    let cpu = reg_sz(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString");
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    let unit = if zh { "核" } else { "cores" };
    if !cpu.is_empty() {
        facts.push((label("处理器", "Processor"), format!("{} ({cores} {unit})", cpu.trim())));
    }
    // Installed memory.
    let mut mem = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
        facts.push((label("机载内存", "Installed RAM"), format!("{:.0} GB", mem.ullTotalPhys as f64 / (1024.0 * 1024.0 * 1024.0))));
    }
    // The graphics, and the disk Windows started from.
    let gpu = graphics();
    if !gpu.is_empty() {
        facts.push((label("显卡", "Graphics"), gpu));
    }
    let disk = startup_disk();
    if !disk.is_empty() {
        facts.push((label("启动磁盘", "Startup disk"), disk));
    }
    facts.push((label("系统类型", "System type"), if cfg!(target_pointer_width = "64") { label("64 位操作系统，基于 x64 的处理器", "64-bit operating system, x64-based processor") } else { label("32 位操作系统", "32-bit operating system") }));
    if !serial.is_empty() {
        facts.push((label("序列号", "Serial number"), serial));
    }
    // The day Windows came to this machine.
    let install = os_install_date();
    if !install.is_empty() {
        facts.push((label("安装于", "Installed on"), install));
    }
    facts
}

/// The names of a key's subkeys, sorted; empty where the key is not there.
fn reg_subkeys(root: HKEY, key: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut hkey = HKEY::default();
    if unsafe { RegOpenKeyExW(root, &HSTRING::from(key), None, KEY_READ, &mut hkey) }.is_ok() {
        let mut index = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let got = unsafe { windows::Win32::System::Registry::RegEnumKeyExW(hkey, index, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
            if got.is_err() {
                break;
            }
            let ended = name.iter().position(|&c| c == 0).unwrap_or(len as usize);
            names.push(String::from_utf16_lossy(&name[..ended]));
            index += 1;
            if index > 512 {
                break;
            }
        }
        let _ = unsafe { RegCloseKey(hkey) };
    }
    names.sort();
    names
}

/// The machine's graphics, as its drivers name them; the discrete one where
/// there is more than a basic adapter. Empty where the class key tells nothing.
pub(crate) fn graphics() -> String {
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut names: Vec<String> = Vec::new();
    for slot in reg_subkeys(HKEY_LOCAL_MACHINE, class) {
        // Only the four-digit numbered keys hold a live adapter.
        if slot.len() != 4 || !slot.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let said = reg_sz(HKEY_LOCAL_MACHINE, &format!("{class}\\{slot}"), "DriverDesc");
        // Keep the real adapters; let go the basic and the virtual ones (a remote
        // or a made-up display), which are no part of the machine's graphics.
        let low = said.to_lowercase();
        if said.is_empty() || ["basic", "基础", "remote", "idd", "virtual", "display driver"].iter().any(|skip| low.contains(skip)) {
            continue;
        }
        if !names.contains(&said) {
            names.push(said);
        }
    }
    names.join(" + ")
}

/// The disk Windows started from: its label (or its letter where it has none),
/// the room it holds and the room used. Empty where it cannot be read.
pub(crate) fn startup_disk() -> String {
    let root = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let path = format!("{root}\\");
    let wide = HSTRING::from(&path);
    let (mut free, mut total) = (0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(&wide, Some(&mut free), Some(&mut total), None) }.is_err() {
        return String::new();
    }
    // Its label, where it has one.
    let mut name = [0u16; 64];
    let _ = unsafe { GetVolumeInformationW(&wide, Some(&mut name), None, None, None, None) };
    let ended = name.iter().position(|&c| c == 0).unwrap_or(0);
    let named = String::from_utf16_lossy(&name[..ended]);
    let gb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let said = if named.is_empty() { root } else { named };
    format!("{said} · {:.0} / {:.0} GB", gb(total) - gb(free), gb(total))
}

// ---- Language & region ----

/// The regional format's name, as Windows keeps it ("zh-CN").
pub(crate) fn locale_name() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International", "LocaleName")
}

/// The country or region's two-letter code the machine is set to ("CN").
pub(crate) fn geo_name() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International\Geo", "Name")
}

/// The short date pattern in use ("yyyy/M/d").
pub(crate) fn short_date() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International", "sShortDate")
}

/// The long date pattern in use ("yyyy'年'M'月'd'日'"), which carries the weekday too.
pub(crate) fn long_date() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International", "sLongDate")
}

/// The time pattern in use ("H:mm:ss").
pub(crate) fn time_format() -> String {
    reg_sz(HKEY_CURRENT_USER, r"Control Panel\International", "sTimeFormat")
}

/// The languages the display may be worn in, most wanted first; empty where the
/// machine keeps only the one it was set up with.
pub(crate) fn preferred_languages() -> Vec<String> {
    reg_multi(
        HKEY_CURRENT_USER,
        r"Control Panel\International\User Profile",
        "PreferredLanguages",
    )
}

/// A country's name for its two-letter code, in the tongue asked for; the code
/// itself where it is not one of the few kept here.
pub(crate) fn country_name(code: &str, lang: Lang) -> String {
    let said = match code {
        "CN" => lang.pick("中国", "China"),
        "HK" => lang.pick("中国香港", "Hong Kong SAR"),
        "TW" => lang.pick("中国台北", "Taipei"),
        "MO" => lang.pick("中国澳门", "Macao SAR"),
        "US" => lang.pick("美国", "United States"),
        "JP" => lang.pick("日本", "Japan"),
        "KR" => lang.pick("韩国", "Korea"),
        "SG" => lang.pick("新加坡", "Singapore"),
        "GB" => lang.pick("英国", "United Kingdom"),
        "DE" => lang.pick("德国", "Germany"),
        "FR" => lang.pick("法国", "France"),
        _ => return code.to_string(),
    };
    said.to_string()
}

// ---- Sharing (the network and sharing centre) ----

/// The workgroup or domain the machine belongs to, where it belongs to one.
pub(crate) fn workgroup() -> String {
    let key = r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters";
    let group = reg_sz(HKEY_LOCAL_MACHINE, key, "Workgroup");
    if group.is_empty() {
        reg_sz(HKEY_LOCAL_MACHINE, key, "Domain")
    } else {
        group
    }
}

/// A folder the machine holds out to the network: the name it goes by, where it
/// stands on the disk, and whether it is one of the administrative shares the
/// system keeps to itself (`C$`, `ADMIN$`, the shares no person chose).
pub(crate) struct Share {
    pub name: String,
    pub path: String,
    pub special: bool,
}

/// The shares the file service keeps, read off its own configuration: the names
/// are the shares' and the values sit under them, so each is opened in turn. One
/// ask of a handful of keys, which the drawn frame may wait on.
pub(crate) fn share_list() -> Vec<Share> {
    let key = r"SYSTEM\CurrentControlSet\Services\LanmanServer\Shares";
    let mut root = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), None, KEY_READ, &mut root) }.is_err() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut named = [0u16; 128];
    for index in 0..64 {
        let mut length = named.len() as u32;
        let read = unsafe { RegEnumKeyExW(root, index, Some(PWSTR(named.as_mut_ptr())), &mut length, None, None, None, None) };
        if read.is_err() {
            break;
        }
        let name = String::from_utf16_lossy(&named[..length as usize]);
        if name.is_empty() {
            continue;
        }
        let under = format!(r"{key}\{name}");
        let path = reg_sz(HKEY_LOCAL_MACHINE, &under, "Path");
        let special = reg_word(HKEY_LOCAL_MACHINE, &under, "Special").unwrap_or(0) != 0;
        found.push(Share { name, path, special });
    }
    let _ = unsafe { RegCloseKey(root) };
    found
}

// ---- Nearby sharing (AirDrop and Handoff's Windows kin) ----

/// The key the sharing and handoff switches are kept under.
const CDP: &str = r"Software\Microsoft\Windows\CurrentVersion\CDP";

/// Turns nearby sharing on to this user's own devices, or off again; the shell is
/// told so the page follows what truly took.
pub(crate) fn set_nearby_sharing(on: bool) {
    reg_write(CDP, "NearShareChannelUserAuthLevel", if on { 2 } else { 0 });
    broadcast_settings("CDPChange");
}

/// Whom nearby sharing may reach: nothing while 0, anyone near it while 1, this
/// user's own devices while 2 — the level Windows itself keeps the switch at, and
/// what the hub reads the switch from.
pub(crate) fn nearby_share_level() -> u32 {
    reg_word(HKEY_CURRENT_USER, CDP, "NearShareChannelUserAuthLevel").unwrap_or(0)
}

// ---- Backup (file history) ----

/// Where file history keeps its own settings, if it has ever been turned on.
pub(crate) fn file_history_configured() -> bool {
    std::env::var("LOCALAPPDATA")
        .map(|base| std::path::Path::new(&base).join(r"Microsoft\Windows\File History"))
        .map(|path| path.is_dir())
        .unwrap_or(false)
}

/// Whether system restore — the points a bad change is turned back to — is switched
/// off where Windows keeps it; `None` where the key is not there at all, which is
/// where Windows leaves it until the protection is turned on by hand.
pub(crate) fn restore_disabled() -> Option<bool> {
    reg_word(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\SystemRestore", "DisableSR").map(|word| word != 0)
}

// ---- Recovery (startup disk) ----

/// Whether the machine boots by UEFI rather than the older firmware; `None` where
/// the firmware will not say.
pub(crate) fn firmware_uefi() -> Option<bool> {
    use windows::Win32::System::SystemInformation::{
        FirmwareTypeUefi, GetFirmwareType, FIRMWARE_TYPE,
    };
    let mut kind = FIRMWARE_TYPE(0);
    if unsafe { GetFirmwareType(&mut kind) }.is_ok() {
        Some(kind == FirmwareTypeUefi)
    } else {
        None
    }
}

/// Whether secure boot is worn at start; `None` where the machine does not keep
/// the state (no secure boot at all).
pub(crate) fn secure_boot() -> Option<bool> {
    reg_word(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\SecureBoot\State",
        "SecureBoot",
    )
    .map(|word| word != 0)
}

/// What Windows writes of itself when it stops: nothing while 0, a whole kernel
/// dump while 1, the kernel's own while 2, a small one while 3, an automatic
/// while 4; `None` where the machine keeps no word of it.
pub(crate) fn crash_dump() -> Option<u32> {
    reg_word(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\CrashControl", "CrashDumpEnabled")
}
/// Restarts into the recovery environment (true) or the firmware's own setup
/// (false), asking for the privilege the restart wants.
pub(crate) fn reboot_to(recovery: bool) {
    let params = if recovery { "/r /o /t 0" } else { "/r /fw /t 0" };
    unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            w!("shutdown.exe"),
            &HSTRING::from(params),
            PCWSTR::null(),
            SW_HIDE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The edition is worked out from the system's own APIs, not the registry:
    // a real build must come back, and the name must be a Windows one.
    #[test]
    fn the_edition_is_read_from_the_system_itself() {
        let (_, _, build) = os_version();
        assert!(build > 0, "RtlGetVersion should report a build");
        assert!(os_product().starts_with("Windows"), "{}", os_product());
    }
}
