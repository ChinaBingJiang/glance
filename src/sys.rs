//! The machine's own state, read and put where Windows keeps it: the display
//! modes a screen will wear, the growth the desktop is drawn at, the warmth
//! the night light lends it, and which apps may leave a notice. Each is heard
//! from the GDI and the registry that hold it, and written back the way the
//! system's own panel does, so a change taken here and one taken there are the
//! same change. Where the setting is kept in a form the registry calls cannot
//! reach — the per-app notices — PowerShell is asked quietly, as the start-up
//! list is.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LPARAM, POINTL, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CDS_NORESET, CDS_RESET, CDS_SET_PRIMARY, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW,
    CreateDCW, DEVMODEW, DEVMODE_DISPLAY_ORIENTATION, DeleteDC, DISPLAY_DEVICEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, DM_DISPLAYORIENTATION, DM_PELSWIDTH, DM_PELSHEIGHT, DM_POSITION,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW, EnumDisplaySettingsW, GetDC,
    GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromRect,
    ReleaseDC, EnumDisplayMonitors,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD, RegCloseKey, RegCreateKeyExW, RegGetValueW,
    RegSetValueExW,
};
use windows::Win32::UI::ColorSystem::{GetICMProfileW, SetDeviceGammaRamp};
use windows::Win32::UI::Shell::{
    SHEmptyRecycleBinW, SHQueryRecycleBinW, SHQUERYRBINFO, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
};
use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};

/// A mode a display can be told to wear: its size, and how often it is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mode {
    pub wide: u32,
    pub tall: u32,
    pub hz: u32,
}

/// A device name as the wide calls want it: UTF-16 with its end marked.
fn named(device: &str) -> Vec<u16> {
    let mut name: Vec<u16> = device.encode_utf16().collect();
    name.push(0);
    name
}

/// The tail of a wide string, up to its first zero, trimmed.
fn wide(name: &[u16]) -> String {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end]).trim().to_string()
}

/// The GDI name of the screen standing at this rectangle (`\\.\DISPLAY1`);
/// empty where no screen is. The name is what the modes are asked of, and what a
/// change is put to.
pub fn device_of(monitor: &RECT) -> String {
    unsafe {
        let at = MonitorFromRect(monitor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(at, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return String::new();
        }
        wide(&info.szDevice)
    }
}

/// The resolutions a display offers, each kept once at the fastest refresh said
/// for it, largest first — the list a panel drops down.
pub fn modes(device: &str) -> Vec<Mode> {
    let mut found = Vec::new();
    if device.is_empty() {
        return found;
    }
    let name = named(device);
    let mut index = 0u32;
    loop {
        let mut dm = DEVMODEW::default();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        // (A driver that will not end its list is given a head of it.)
        if index >= 512
            || !(unsafe {
                EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(index), &mut dm)
            })
            .as_bool()
        {
            break;
        }
        found.push(Mode { wide: dm.dmPelsWidth, tall: dm.dmPelsHeight, hz: dm.dmDisplayFrequency });
        index += 1;
    }
    folded(found)
}

/// Each resolution but once, at the fastest rate seen of it, largest first.
fn folded(all: Vec<Mode>) -> Vec<Mode> {
    let mut kept: Vec<Mode> = Vec::new();
    for mode in all {
        match kept.iter_mut().find(|held| held.wide == mode.wide && held.tall == mode.tall) {
            Some(held) if mode.hz > held.hz => held.hz = mode.hz,
            Some(_) => {}
            None => kept.push(mode),
        }
    }
    kept.sort_by(|a, b| (b.wide * b.tall).cmp(&(a.wide * a.tall)));
    kept
}

/// The mode a display wears now.
pub fn current(device: &str) -> Option<Mode> {
    if device.is_empty() {
        return None;
    }
    let name = named(device);
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
        .as_bool()
        .then(|| Mode { wide: dm.dmPelsWidth, tall: dm.dmPelsHeight, hz: dm.dmDisplayFrequency })
}

/// Tells a display to wear a mode, and keeps the change in the registry as
/// Windows' own panel does: true when it took. A mode the driver will not draw
/// is refused, and the display is left as it was.
pub fn set_mode(device: &str, mode: Mode) -> bool {
    if device.is_empty() {
        return false;
    }
    let name = named(device);
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    dm.dmPelsWidth = mode.wide;
    dm.dmPelsHeight = mode.tall;
    dm.dmDisplayFrequency = mode.hz;
    let took = unsafe {
        ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm as *const _), None, CDS_UPDATEREGISTRY, None)
    };
    took == DISP_CHANGE_SUCCESSFUL
}

/// A word read from under the user, where one is kept.
fn word_at(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut data = 0u32;
    let mut size = 4u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
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

/// How far the desktop is grown: the DPI Windows applied at the last log-in,
/// told back as a share of the plain hundred.
pub fn scale() -> u32 {
    step_of(word_at(r"Control Panel\Desktop\WindowMetrics", "AppliedDPI").unwrap_or(96))
}

/// The offered growth nearest a figure — the steps lean to the one a figure sits
/// between.
fn step_of(dpi: u32) -> u32 {
    [100u32, 125, 150, 175, 200]
        .into_iter()
        .min_by_key(|step| (*step as i64 - dpi as i64 * 100 / 96).abs())
        .unwrap_or(100)
}

/// The warmth the night light lends the screen: none, or three depths of amber.
/// Kept in the running program only — the gamma is worn while the display draws,
/// and a mode change or a reboot washes it off by itself.
static NIGHT: AtomicU8 = AtomicU8::new(0);

/// How warm the screen is worn now, as this running Glance put it on.
pub fn night() -> u32 {
    NIGHT.load(Ordering::Relaxed) as u32
}

/// The gamma a depth asks for: the reds left whole while the greens and blues
/// are drawn down, which is the amber of a screen at night.
fn night_ramp(level: u32) -> [[u16; 3]; 128] {
    let (green, blue) = match level {
        1 => (0.92, 0.80),
        2 => (0.86, 0.68),
        _ if level >= 3 => (0.80, 0.56),
        _ => (1.0, 1.0),
    };
    let mut ramp = [[0u16; 3]; 128];
    for (at, row) in ramp.iter_mut().enumerate() {
        let step = (at as u32 * 65535 / 127) as u16;
        row[0] = step;
        row[1] = (step as f32 * green).min(65535.0) as u16;
        row[2] = (step as f32 * blue).min(65535.0) as u16;
    }
    ramp
}

/// The warmth put on the display's own drawing, told back whether it took. The
/// plain ramp (none) is what a fresh login wears, so the way off is to write the
/// straight line back.
pub fn set_night(level: u32) -> bool {
    let level = level.min(3);
    let ramp = night_ramp(level);
    let screen = unsafe { GetDC(None) };
    let gave = unsafe { SetDeviceGammaRamp(screen, &ramp as *const _ as *const core::ffi::c_void) };
    unsafe {
        let _ = ReleaseDC(None, screen);
    }
    let took = gave.as_bool();
    if took {
        NIGHT.store(level as u8, Ordering::Relaxed);
    }
    took
}

/// A screen of the desktop as it lies: the GDI name it is known by, where it
/// stands, and whether things start on it.
struct Watch {
    name: String,
    left: i32,
    top: i32,
    primary: bool,
}

/// Every screen Windows is drawing on, heard at once off the monitor list.
fn watchtowers() -> Vec<Watch> {
    unsafe extern "system" fn hear(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> BOOL {
        let found = unsafe { &mut *(found.0 as *mut Vec<Watch>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            let at = info.monitorInfo.rcMonitor;
            found.push(Watch {
                name: wide(&info.szDevice),
                left: at.left,
                top: at.top,
                // MONITORINFOF_PRIMARY, which the crate does not name.
                primary: info.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL::from(true)
    }
    let mut found = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(hear), LPARAM(&mut found as *mut _ as isize));
    }
    found
}

/// A screen's own current mode retold with a new place on the desktop. The flags
/// say whether the change lands at once or waits staged for a flush.
fn placed(device: &str, x: i32, y: i32, flags: CDS_TYPE) -> bool {
    let name = named(device);
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    if !(unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }).as_bool() {
        return false;
    }
    dm.dmFields = dm.dmFields | DM_POSITION;
    dm.Anonymous1.Anonymous2.dmPosition = POINTL { x, y };
    unsafe { ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm as *const _), None, flags, None) == DISP_CHANGE_SUCCESSFUL }
}

/// Crown another screen as the one things start on. The desktop is shifted whole
/// — every screen keeps the place it had against the others, and the chosen one
/// is carried to the origin wearing `CDS_SET_PRIMARY` — staged, flushed, and read
/// back: true only when the origin now lies on that screen.
pub fn set_primary(device: &str) -> bool {
    let board = watchtowers();
    let Some(chosen) = board.iter().find(|one| one.name == device) else {
        return false;
    };
    if chosen.primary {
        return true;
    }
    for one in &board {
        if one.name == device {
            let _ = placed(&one.name, 0, 0, CDS_UPDATEREGISTRY | CDS_SET_PRIMARY | CDS_NORESET);
        } else {
            let _ = placed(&one.name, one.left - chosen.left, one.top - chosen.top, CDS_UPDATEREGISTRY | CDS_NORESET);
        }
    }
    unsafe {
        let _ = ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_RESET, None);
    }
    watchtowers().into_iter().find(|one| one.primary).is_some_and(|one| one.name == device)
}

/// The name the monitor itself reports — a model, as "BenQ PD2725U" — where the
/// display device behind the output says one; the GDI device name otherwise.
pub fn friendly(device: &str) -> String {
    if device.is_empty() {
        return String::new();
    }
    let name = named(device);
    let mut dd = DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    if !(unsafe { EnumDisplayDevicesW(PCWSTR(name.as_ptr()), 0, &mut dd, 0) }).as_bool() {
        return device.to_string();
    }
    let said = wide(&dd.DeviceString);
    if said.is_empty() {
        device.to_string()
    } else {
        said
    }
}

/// The refresh rates a display offers at one size, fastest first — the list its
/// rate drop-down holds. The whole mode list is walked, the sizes that are not
/// this one dropped.
pub fn refreshes(device: &str, wide: u32, tall: u32) -> Vec<u32> {
    let mut found = Vec::new();
    if device.is_empty() {
        return found;
    }
    let name = named(device);
    let mut index = 0u32;
    loop {
        let mut dm = DEVMODEW::default();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        if index >= 512
            || !(unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(index), &mut dm) })
                .as_bool()
        {
            break;
        }
        if dm.dmPelsWidth == wide && dm.dmPelsHeight == tall && !found.contains(&dm.dmDisplayFrequency) {
            found.push(dm.dmDisplayFrequency);
        }
        index += 1;
    }
    found.sort_unstable_by(|a, b| b.cmp(a));
    found
}

/// Which way the display is turned: none, a quarter right, upside down, or a quarter left.
pub fn orientation(device: &str) -> u32 {
    if device.is_empty() {
        return 0;
    }
    let name = named(device);
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    if !(unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }).as_bool() {
        return 0;
    }
    // (A union field, so read within `unsafe`; the turn is kept whole in it.)
    unsafe { dm.Anonymous1.Anonymous2.dmDisplayOrientation.0 }
}

/// Turns a display, keeping the size and rate it wears. A driver that will not
/// draw it so is refused, and the display is left as it was: true when it took.
pub fn set_orientation(device: &str, orient: u32) -> bool {
    if device.is_empty() {
        return false;
    }
    let name = named(device);
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    if !(unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }).as_bool() {
        return false;
    }
    dm.dmFields = DM_DISPLAYORIENTATION;
    dm.Anonymous1.Anonymous2.dmDisplayOrientation = DEVMODE_DISPLAY_ORIENTATION(orient);
    let took = unsafe { ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm as *const _), None, CDS_UPDATEREGISTRY, None) };
    took == DISP_CHANGE_SUCCESSFUL
}

/// The colour profile the display is associated with, told by its file's name;
/// empty where none is read. Heard off the display's own device context, which
/// is taken and let go here.
pub fn color_profile(device: &str) -> String {
    if device.is_empty() {
        return String::new();
    }
    let name = named(device);
    let screen = unsafe { CreateDCW(PCWSTR::null(), PCWSTR(name.as_ptr()), PCWSTR::null(), None) };
    if screen.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    let got = unsafe { GetICMProfileW(screen, &mut len, Some(PWSTR(buf.as_mut_ptr()))) };
    unsafe {
        let _ = DeleteDC(screen);
    }
    if !got.as_bool() {
        return String::new();
    }
    let path = wide(&buf);
    std::path::Path::new(&path)
        .file_stem()
        .map(|said| said.to_string_lossy().into_owned())
        .unwrap_or(path)
}

// ---- Notices ----

/// An app Windows keeps a notification setting for: the friendly name it is shown by, the id
/// (AUMID) the setting is kept under, and whether its banners are let now.
#[derive(Clone, PartialEq)]
pub struct NotifyApp {
    pub display: String,
    pub aumid: String,
    pub enabled: bool,
}

/// A program run with its console kept out of sight; what it printed, where it ran.
fn run(program: &str, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(program).args(args).creation_flags(0x0800_0000).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A program run quietly; whether it ended clean.
fn ok(program: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(program)
        .args(args)
        .creation_flags(0x0800_0000)
        .status()
        .map(|code| code.success())
        .unwrap_or(false)
}

/// The apps that have left a per-app notification setting behind, read from the subkeys under
/// `…\Notifications\Settings`: each subkey is one app, its `Enabled` word a banner let (or, where
/// it is `0`, held); a subkey without the word is let, as Windows counts it. The ids are turned into
/// friendly names through `Get-StartApps`, matched by package family; an id it cannot name is shown
/// cleaned of its `!` suffix and publisher hash. The throwaway ids the old balloon icons leave are
/// left out, as Windows' own page leaves them out. Read through PowerShell, as the start-up list is.
pub fn notify_apps() -> Vec<NotifyApp> {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
                  $fam=@{}; \
                  Get-StartApps | ForEach-Object { if($_.AppID){ $f=($_.AppID -split '!')[0]; if(-not $fam.ContainsKey($f)){ $fam[$f]=$_.Name } } }; \
                  Get-ChildItem 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings' -ErrorAction SilentlyContinue | ForEach-Object { \
                    $id=$_.PSChildName; if($id -like 'NOC_*' -or $id -like 'NotifyIconGeneratedAumid_*'){ return }; \
                    $k=Get-Item $_.PSPath; $on=$true; \
                    if(($k.GetValueNames()) -contains 'Enabled'){ if([int]($k.GetValue('Enabled')) -eq 0){ $on=$false } }; \
                    $f=($id -split '!')[0]; if($fam.ContainsKey($f)){ $n=$fam[$f] } else { $n=($f -replace '_[A-Za-z0-9]{13}$','') }; \
                    '{0}|{1}|{2}' -f $on,$id,$n}";
    let said = match run("powershell.exe", &["-NoProfile", "-Command", script]) {
        Some(said) => said,
        None => return Vec::new(),
    };
    let mut found = Vec::new();
    for line in said.lines() {
        let bits = line.splitn(3, '|').collect::<Vec<_>>();
        if bits.len() < 3 || bits[1].is_empty() {
            continue;
        }
        found.push(NotifyApp {
            display: bits[2].trim().to_string(),
            aumid: bits[1].trim().to_string(),
            enabled: bits[0].trim() == "True",
        });
    }
    found.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    found
}

/// Lets or holds one app's banners, written the `Enabled` word Windows reads under its id. The id is
/// doubled into a quoted literal so a stray quote cannot break the script.
fn notify_set(aumid: &str, on: bool) -> bool {
    let quoted = aumid.replace('\'', "''");
    let script = format!(
        "New-ItemProperty -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings\\{quoted}' -Name 'Enabled' -PropertyType DWord -Value {} -Force | Out-Null",
        u32::from(on)
    );
    ok("powershell.exe", &["-NoProfile", "-Command", &script])
}

/// Whether notices are let at all now, as the master switch Windows keeps; let where it says nothing.
pub fn notify_master() -> bool {
    word_at(r"Software\Microsoft\Windows\CurrentVersion\PushNotifications", "ToastEnabled").unwrap_or(1) != 0
}

/// Puts the master switch, through PowerShell, as the per-app setting is.
fn notify_master_set(on: bool) -> bool {
    let script = format!(
        "New-ItemProperty -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications' -Name 'ToastEnabled' -PropertyType DWord -Value {} -Force | Out-Null",
        u32::from(on)
    );
    ok("powershell.exe", &["-NoProfile", "-Command", &script])
}

static NOTIFY: Mutex<Option<Vec<NotifyApp>>> = Mutex::new(None);
static NOTIFY_BUSY: AtomicBool = AtomicBool::new(false);
static NOTIFY_MS: AtomicU64 = AtomicU64::new(0);

fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The apps as last read; a fresh read taken off-thread when they are stale — reading them runs
/// PowerShell, which the drawn frame must not wait on — and the window woken when it lands.
pub fn notify_snapshot() -> Option<Vec<NotifyApp>> {
    let last = NOTIFY_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 30000 {
        if !NOTIFY_BUSY.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let apps = notify_apps();
                *NOTIFY.lock().unwrap() = Some(apps);
                NOTIFY_MS.store(millis(), Ordering::Relaxed);
                NOTIFY_BUSY.store(false, Ordering::SeqCst);
                crate::ui::settings_window::follow_settings();
            });
        }
    }
    NOTIFY.lock().unwrap().clone()
}

/// Lets or holds an app's banners, the write done on a thread of its own so PowerShell is not waited
/// on, and the list read again when it lands.
pub fn notify_apply(aumid: String, on: bool) {
    std::thread::spawn(move || {
        notify_set(&aumid, on);
        NOTIFY_MS.store(0, Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}

/// Puts the master switch, the write done off-thread and the window woken when it lands.
pub fn notify_master_apply(on: bool) {
    std::thread::spawn(move || {
        notify_master_set(on);
        NOTIFY_MS.store(0, Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}

// ---- Storage ----

/// What the storage page shows at a glance: the room the temporary files take, and the recycle
/// bin's size and count together across every drive.
#[derive(Clone, PartialEq)]
pub struct StorageSnapshot {
    pub temp: u64,
    pub recycle_bytes: u64,
    pub recycle_count: u64,
}

/// The user's and the system's temporary folders.
fn temp_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(temp) = std::env::var_os("TEMP") {
        dirs.push(std::path::PathBuf::from(temp));
    }
    if let Some(windir) = std::env::var_os("SystemRoot") {
        dirs.push(std::path::Path::new(&windir).join("Temp"));
    }
    dirs
}

/// The bytes under a folder, its subfolders taken one by one off a stack so a deep or looping tree
/// cannot fill the call stack. A file that cannot be looked at is walked past.
fn measured(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(here) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&here) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// The room the temporary files take.
pub fn temp_bytes() -> u64 {
    temp_dirs().iter().map(|dir| measured(dir)).sum()
}

/// The temporary files taken away as far as they can be: what is locked by a running program is
/// left, and the room freed is told back.
pub fn clean_temp() -> u64 {
    let mut gone = 0u64;
    for dir in temp_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let went = if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if went.is_ok() {
                gone += meta.len();
            }
        }
    }
    gone
}

/// The recycle bin: the room it takes and the number of things in it, as the shell keeps them
/// across every drive together.
pub fn recycle() -> (u64, u64) {
    let mut bin = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    if unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut bin) }.is_err() {
        return (0, 0);
    }
    (bin.i64Size.max(0) as u64, bin.i64NumItems.max(0) as u64)
}

/// The recycle bin emptied, the whole way through, with no box and no sound of its own — the page
/// says what it did.
pub fn empty_recycle() -> bool {
    unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) }.is_ok()
}

/// The storage sense's own key, under the user.
const SENSE: &str = r"Software\Microsoft\Windows\CurrentVersion\StorageSense\Parameters\StoragePolicy";

/// Whether storage sense is on.
pub fn storage_sense() -> bool {
    word_at(SENSE, "01").unwrap_or(0) != 0
}

/// Storage sense written on or off, and the change sent round the desktop.
fn set_storage_sense(on: bool) {
    write_word(SENSE, "01", u32::from(on));
    broadcast("Environment");
}

/// A word written under the user, the key made if it is not there.
fn write_word(key: &str, value: &str, data: u32) {
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let _ = RegSetValueExW(hkey, &HSTRING::from(value), None, REG_DWORD, Some(&data.to_le_bytes()));
            let _ = RegCloseKey(hkey);
        }
    }
}

/// A word sent round the desktop so the shell follows what was just written.
fn broadcast(section: &str) {
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

static STORAGE: Mutex<Option<StorageSnapshot>> = Mutex::new(None);
static STORAGE_BUSY: AtomicBool = AtomicBool::new(false);
static STORAGE_MS: AtomicU64 = AtomicU64::new(0);

/// The storage figures as last read; a fresh read taken off-thread when they are stale — counting
/// the temporary files walks the disk, which the drawn frame must not wait on — and the window
/// woken when it lands.
pub fn storage_snapshot() -> Option<StorageSnapshot> {
    let last = STORAGE_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 30000 {
        if !STORAGE_BUSY.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let (recycle_bytes, recycle_count) = recycle();
                let shot = StorageSnapshot { temp: temp_bytes(), recycle_bytes, recycle_count };
                *STORAGE.lock().unwrap() = Some(shot);
                STORAGE_MS.store(millis(), Ordering::Relaxed);
                STORAGE_BUSY.store(false, Ordering::SeqCst);
                crate::ui::settings_window::follow_settings();
            });
        }
    }
    STORAGE.lock().unwrap().clone()
}

/// The temporary files cleared off-thread, the figures read again when it lands.
pub fn storage_clean() {
    std::thread::spawn(|| {
        clean_temp();
        STORAGE_MS.store(0, Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}

/// The recycle bin emptied off-thread, the figures read again when it lands.
pub fn storage_empty() {
    std::thread::spawn(|| {
        empty_recycle();
        STORAGE_MS.store(0, Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}

/// Storage sense put on or off, the write done off-thread and the window woken when it lands.
pub fn storage_sense_apply(on: bool) {
    std::thread::spawn(move || {
        set_storage_sense(on);
        crate::ui::settings_window::follow_settings();
    });
}

// ---- Consent ----

/// An app that has left a per-app consent mark for one capability (the camera, the microphone, the
/// whereabout): the friendly name, the keys its marks are kept under (one plain program can leave
/// several, across its versioned folders), whether it is a packaged app rather than a plain
/// program, and whether it is let now (no mark under it an outright `Deny`).
#[derive(Clone, PartialEq)]
pub struct ConsentApp {
    pub display: String,
    pub keys: Vec<String>,
    pub packaged: bool,
    pub allowed: bool,
}

/// The apps that have asked for `capability` and left a mark behind, read from the subkeys under
/// `…\CapabilityAccessManager\ConsentStore\<capability>`: a packaged app is a subkey named by its
/// package family, a plain program one under `NonPackaged\` named by its path with `\` turned to
/// `#`. Each carries a `Value` of Allow, Deny, Prompt or none; only `Deny` holds it back. The
/// families are named through `Get-StartApps`; a plain program by its file's own description.
/// Programs sharing a name are folded into one row, held back if any of their marks is. Read
/// through PowerShell, as the other lists are.
fn consent_apps_raw(capability: &str) -> Vec<ConsentApp> {
    let script = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
         $fam=@{{}}; \
         Get-StartApps | ForEach-Object {{ if($_.AppID){{ $f=($_.AppID -split '!')[0]; if(-not $fam.ContainsKey($f)){{ $fam[$f]=$_.Name }} }} }}; \
         $p='HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\{capability}'; \
         Get-ChildItem $p -ErrorAction SilentlyContinue | ForEach-Object {{ \
           $n=$_.PSChildName; \
           if($n -eq 'NonPackaged'){{ Get-ChildItem $_.PSPath -ErrorAction SilentlyContinue | ForEach-Object {{ \
             $k=$_.PSChildName; $v=[string](Get-ItemProperty -LiteralPath $_.PSPath -Name Value -ErrorAction SilentlyContinue).Value; \
             $pp=$k.Replace('#','\\'); \
             $d=(Get-Item -LiteralPath $pp -ErrorAction SilentlyContinue).VersionInfo.FileDescription; \
             if(-not $d){{ $d=(($k -split '#')[-1] -replace '\\.exe$','') }}; \
             'NP|{{0}}|{{1}}|{{2}}' -f $v,$k,$d }} }} \
           else {{ \
             $v=[string](Get-ItemProperty -LiteralPath $_.PSPath -Name Value -ErrorAction SilentlyContinue).Value; \
             if($fam.ContainsKey($n)){{ $d=$fam[$n] }} else {{ $d=($n -replace '_[A-Za-z0-9]{{13}}$','') }}; \
             'UP|{{0}}|{{1}}|{{2}}' -f $v,$n,$d }} }}"
    );
    let said = match run("powershell.exe", &["-NoProfile", "-Command", &script]) {
        Some(said) => said,
        None => return Vec::new(),
    };
    // Folded by the name shown: one program across its versioned folders is one row, held back if
    // any of its marks is a `Deny`.
    let mut found: Vec<ConsentApp> = Vec::new();
    for line in said.lines() {
        let bits = line.splitn(4, '|').collect::<Vec<_>>();
        if bits.len() < 4 || bits[2].is_empty() {
            continue;
        }
        let packaged = bits[0].trim() == "UP";
        let allowed = bits[1].trim() != "Deny";
        let display = bits[3].trim().to_string();
        let key = bits[2].trim().to_string();
        if let Some(same) = found.iter_mut().find(|app| app.packaged == packaged && app.display == display) {
            same.keys.push(key);
            same.allowed &= allowed;
        } else {
            found.push(ConsentApp { display, keys: vec![key], packaged, allowed });
        }
    }
    found.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    found
}

/// Lets or holds one app's use of `capability`, written the `Value` Windows reads under each of its
/// keys. Each key is doubled into a quoted literal so a stray quote cannot break the script.
fn consent_app_set(capability: &str, keys: &[String], packaged: bool, allow: bool) -> bool {
    keys.iter().all(|key| {
        let quoted = key.replace('\'', "''");
        let leaf = if packaged {
            format!(r"HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{capability}\{quoted}")
        } else {
            format!(r"HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{capability}\NonPackaged\{quoted}")
        };
        let script = format!(
            "New-ItemProperty -LiteralPath '{leaf}' -Name 'Value' -PropertyType String -Value '{}' -Force | Out-Null",
            if allow { "Allow" } else { "Deny" }
        );
        ok("powershell.exe", &["-NoProfile", "-Command", &script])
    })
}

/// One capability's apps as last read, kept with the moment they landed and whether a read is in
/// flight, so the privacy page can show several capabilities' apps at once — the camera, the
/// microphone, the whereabout — without the drawn frame ever waiting on PowerShell.
struct ConsentHeld {
    capability: String,
    apps: Vec<ConsentApp>,
    landed: u64,
    busy: bool,
}

static CONSENT: Mutex<Vec<ConsentHeld>> = Mutex::new(Vec::new());

/// The apps for `capability` as last read; a fresh read taken off-thread when they have aged out
/// and none is already in flight — reading them runs PowerShell, which the drawn frame must not
/// wait on — and the window woken when they land. None while nothing has landed for this capability.
pub fn consent_snapshot(capability: &str) -> Option<Vec<ConsentApp>> {
    const AGE: u64 = 15000;
    let mut need = false;
    {
        let mut held = CONSENT.lock().unwrap();
        let now = millis();
        match held.iter().position(|h| h.capability == capability) {
            Some(i) => {
                let h = &mut held[i];
                if !h.busy && now.saturating_sub(h.landed) > AGE {
                    h.busy = true;
                    need = true;
                }
            }
            None => {
                held.push(ConsentHeld { capability: capability.to_string(), apps: Vec::new(), landed: 0, busy: true });
                need = true;
            }
        }
    }
    if need {
        let cap = capability.to_string();
        std::thread::spawn(move || {
            let apps = consent_apps_raw(&cap);
            let now = millis();
            let mut held = CONSENT.lock().unwrap();
            if let Some(h) = held.iter_mut().find(|h| h.capability == cap) {
                h.apps = apps;
                h.landed = now;
                h.busy = false;
            }
            drop(held);
            crate::ui::settings_window::follow_settings();
        });
    }
    let held = CONSENT.lock().unwrap();
    // Only a read that has actually landed is shown; one freshly claimed but not yet filled stays
    // blank, so the page says it is reading rather than showing an empty list it has not confirmed.
    held.iter().find(|h| h.capability == capability && h.landed != 0).map(|h| h.apps.clone())
}

/// Lets or holds one app's use of a capability, the write done on a thread of its own, the list
/// read again when it lands.
pub fn consent_apply(capability: String, keys: Vec<String>, packaged: bool, allow: bool) {
    std::thread::spawn(move || {
        consent_app_set(&capability, &keys, packaged, allow);
        // The apps read again now the mark is written, so the page shows the change at once.
        let apps = consent_apps_raw(&capability);
        let now = millis();
        let mut held = CONSENT.lock().unwrap();
        if let Some(h) = held.iter_mut().find(|h| h.capability == capability) {
            h.apps = apps;
            h.landed = now;
            h.busy = false;
        }
        drop(held);
        crate::ui::settings_window::follow_settings();
    });
}

// ---- Installed updates ----

/// An update the system says it has put on: the day it went in, its number, and the kind of
/// fix it is.
#[derive(Clone, PartialEq)]
pub struct Patch {
    pub when: String,
    pub id: String,
    pub what: String,
}

static PATCHES: Mutex<Option<Vec<Patch>>> = Mutex::new(None);
static PATCHES_BUSY: AtomicBool = AtomicBool::new(false);
static PATCHES_MS: AtomicU64 = AtomicU64::new(0);

/// The updates installed, newest first, as last read; a read taken off-thread the first time
/// they are asked for — the list lives in WMI's store and is slow to answer, which the drawn
/// frame must not wait on — and the window woken when it lands. None while nothing has landed
/// yet, which the page says rather than leaving blank.
pub fn patches_snapshot() -> Option<Vec<Patch>> {
    if PATCHES_MS.load(Ordering::Relaxed) == 0 && !PATCHES_BUSY.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            *PATCHES.lock().unwrap() = Some(patches());
            PATCHES_MS.store(millis(), Ordering::Relaxed);
            PATCHES_BUSY.store(false, Ordering::SeqCst);
            crate::ui::settings_window::follow_settings();
        });
    }
    PATCHES.lock().unwrap().clone()
}

/// The updates installed, newest first, read through Windows' own `Get-CimInstance`, for the
/// list is kept in WMI's store rather than in one registry key. Asking takes a few seconds off
/// the machine, so it is asked in the background; saying nothing is honest when it fails.
fn patches() -> Vec<Patch> {
    let query = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
                 Get-CimInstance Win32_QuickFixEngineering | Sort-Object InstalledOn -Descending | \
                 ForEach-Object { '{0}|{1}|{2}' -f ($_.InstalledOn -as [datetime]).ToString('yyyy-MM-dd'), $_.HotFixID, $_.Description }";
    let said = match run("powershell.exe", &["-NoProfile", "-Command", query]) {
        Some(said) => said,
        None => return Vec::new(),
    };
    // Three fields to a line; a line that says less than that is dropped rather than half-shown.
    let mut found = Vec::new();
    for line in said.lines() {
        let bits = line.splitn(3, '|').collect::<Vec<_>>();
        if bits.len() < 3 || bits[1].is_empty() {
            continue;
        }
        found.push(Patch {
            when: bits[0].trim().to_string(),
            id: bits[1].trim().to_string(),
            what: bits[2].trim().to_string(),
        });
    }
    found
}

// ---- Login items ----

/// An app kept for start-up: the name the registry holds it under, the name Windows
/// shows for it, the command it runs, whether it is set under the whole machine or
/// just this user, and whether it is let now.
#[derive(Clone, PartialEq)]
pub struct Startup {
    pub name: String,
    pub display: String,
    pub command: String,
    pub machine: bool,
    pub enabled: bool,
}

static STARTUPS: Mutex<Option<Vec<Startup>>> = Mutex::new(None);
static STARTUPS_BUSY: AtomicBool = AtomicBool::new(false);
static STARTUPS_MS: AtomicU64 = AtomicU64::new(0);

/// The apps kept for start-up as last read; a fresh read taken off-thread the first
/// time they are asked for — the pair spans two hives, the marks are binary, and the
/// name lives in a file's version resource, so PowerShell is asked quietly, which the
/// drawn frame must not wait on — and the window woken when it lands. None while
/// nothing has landed yet.
pub fn startup_snapshot() -> Option<Vec<Startup>> {
    if STARTUPS_MS.load(Ordering::Relaxed) == 0 && !STARTUPS_BUSY.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            *STARTUPS.lock().unwrap() = Some(startup_list());
            STARTUPS_MS.store(millis(), Ordering::Relaxed);
            STARTUPS_BUSY.store(false, Ordering::SeqCst);
            crate::ui::settings_window::follow_settings();
        });
    }
    STARTUPS.lock().unwrap().clone()
}

/// Lets or holds one start-up app, the list read again when the write lands. Where a
/// machine's own entry is refused the administrator's leave the write simply does not
/// land, and the old state comes back rather than a lie.
pub fn startup_apply(machine: bool, name: String, on: bool) {
    std::thread::spawn(move || {
        startup_set(machine, &name, on);
        let read = startup_list();
        *STARTUPS.lock().unwrap() = Some(read);
        STARTUPS_MS.store(millis(), Ordering::Relaxed);
        crate::ui::settings_window::follow_settings();
    });
}

/// The apps kept for start-up, read as the shell keeps them: the entries under both
/// `Run` keys, each paired with its `StartupApproved` mark — a value whose first byte
/// is `3` is one the shell is holding back. Each is named as Windows names it on its
/// own login-items page: the friendly `FileDescription` read from the program the
/// command runs, falling back to the registry's own name.
fn startup_list() -> Vec<Startup> {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
                  function d($cmd){ $p=''; \
                    if($cmd -match '^\"([^\"]+)\"'){ $p=$matches[1] } \
                    elseif($cmd -match '^(\\S+\\.exe)'){ $p=$matches[1] } \
                    else { $p=($cmd -split ' ')[0] }; \
                    $r=''; if($p -and (Test-Path -LiteralPath $p -ErrorAction SilentlyContinue)){ \
                      $r=(Get-Item -LiteralPath $p).VersionInfo.FileDescription }; if(-not $r){$r=''}; $r }; \
                  function g($run,$app,$mach){ \
                    $k=Get-Item -LiteralPath $run -ErrorAction SilentlyContinue; if(-not $k){return}; \
                    $a=Get-Item -LiteralPath $app -ErrorAction SilentlyContinue; \
                    $k.GetValueNames() | ForEach-Object { $n=$_; if($n){ \
                      $cmd=[string]($k.GetValue($n)); \
                      $on=$true; if($a){$b=$a.GetValue($n); if($b -and $b.Length -ge 1 -and $b[0] -eq 3){$on=$false}}; \
                      '{0}|{1}|{2}|{3}|{4}' -f $on,$mach,$n,(d $cmd),$cmd}}}; \
                  g 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run' '0'; \
                  g 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run' 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run' '1'";
    let said = match run("powershell.exe", &["-NoProfile", "-Command", script]) {
        Some(said) => said,
        None => return Vec::new(),
    };
    // Five fields to a line; a line that says less than that is dropped rather than half-shown.
    let mut found = Vec::new();
    for line in said.lines() {
        let bits = line.splitn(5, '|').collect::<Vec<_>>();
        if bits.len() < 5 || bits[2].is_empty() {
            continue;
        }
        let name = bits[2].trim().to_string();
        // The friendly name where the file gave one, the registry's own name where it
        // did not — so a row is never left without a title.
        let display = {
            let said = bits[3].trim();
            if said.is_empty() {
                name.clone()
            } else {
                said.to_string()
            }
        };
        found.push(Startup {
            name,
            display,
            command: bits[4].trim().to_string(),
            machine: bits[1].trim() == "1",
            enabled: bits[0].trim() == "True",
        });
    }
    found
}

/// Whether to let one start-up app run, written the way the shell reads it back: the
/// `StartupApproved` mark set to twelve bytes leading `2` to let it, `3` to hold it.
/// The name is doubled into a quoted literal so a stray quote cannot break the script.
fn startup_set(machine: bool, name: &str, on: bool) -> bool {
    let hive = if machine {
        r"HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
    } else {
        r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
    };
    let head = u32::from(!on) + 2;
    let quoted = name.replace('\'', "''");
    let script = format!(
        "$b=[byte[]]({head},0,0,0,0,0,0,0,0,0,0,0); \
         New-ItemProperty -LiteralPath '{hive}' -Name '{quoted}' -Value $b -PropertyType Binary -Force | Out-Null"
    );
    ok("powershell.exe", &["-NoProfile", "-Command", &script])
}

// ---- Services ----

/// A service the machine keeps: the name it goes by, the name Windows shows for
/// it, whether it is running now, and whether it is left to start of its own accord.
#[derive(Clone, PartialEq)]
pub struct Service {
    pub name: String,
    pub display: String,
    pub running: bool,
    pub auto: bool,
}

static SERVICES: Mutex<Option<Vec<Service>>> = Mutex::new(None);
static SERVICES_BUSY: AtomicBool = AtomicBool::new(false);
static SERVICES_MS: AtomicU64 = AtomicU64::new(0);

/// How long a service read is kept before it is taken again: long enough that
/// opening the page costs no fresh process, short enough that a service turned
/// elsewhere is followed.
const SERVICES_AGE: u64 = 20_000;

/// The services named, as last read; a fresh read taken off-thread when the last
/// is past its age — asking runs PowerShell, which the drawn frame must not wait
/// on — and the window woken when it lands. None while nothing has landed yet,
/// which the page says rather than leaving blank.
pub fn service_snapshot(names: &[&str]) -> Option<Vec<Service>> {
    let asked = names.join("|");
    let last = SERVICES_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > SERVICES_AGE {
        if !SERVICES_BUSY.swap(true, Ordering::SeqCst) {
            let asked = asked.clone();
            std::thread::spawn(move || {
                *SERVICES.lock().unwrap() = Some(service_list(&asked));
                SERVICES_MS.store(millis(), Ordering::Relaxed);
                SERVICES_BUSY.store(false, Ordering::SeqCst);
                crate::ui::settings_window::follow_settings();
            });
        }
    }
    let read = SERVICES.lock().unwrap().clone();
    // The cache serves one question only: the same names, in the same order.
    read.filter(|found| found.iter().map(|one| one.name.as_str()).collect::<Vec<_>>() == names)
}

/// The services named, read in one ask: those there are, each with the name
/// Windows shows it, whether it answers as running, and how it is left to start.
/// A name the machine does not know is simply left out.
fn service_list(asked: &str) -> Vec<Service> {
    let names = asked.split('|').map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(",");
    let script = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \n\
         Get-CimInstance Win32_Service -Filter 'Name IN ({names})' | ForEach-Object {{ \n\
           $r = if ($_.State -eq 'Running') {{ 1 }} else {{ 0 }} \n\
           $a = if ($_.StartMode -eq 'Auto') {{ 1 }} else {{ 0 }} \n\
           Write-Output ($_.Name + '|' + $_.DisplayName + '|' + $r + '|' + $a) \n\
         }}"
    );
    let said = match run("powershell.exe", &["-NoProfile", "-Command", &script]) {
        Some(said) => said,
        None => return Vec::new(),
    };
    // Four fields to a line; a line that says less than that is dropped rather than half-shown.
    let mut found = Vec::new();
    for line in said.lines() {
        let bits = line.splitn(4, '|').collect::<Vec<_>>();
        if bits.len() < 4 || bits[0].is_empty() {
            continue;
        }
        let display = bits[1].trim();
        found.push(Service {
            name: bits[0].trim().to_string(),
            display: if display.is_empty() { bits[0].trim().to_string() } else { display.to_string() },
            running: bits[2].trim() == "1",
            auto: bits[3].trim() == "1",
        });
    }
    found
}
