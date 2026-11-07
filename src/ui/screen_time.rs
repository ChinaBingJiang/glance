//! Screen time: how long each program has held the front of the desktop.
//! A thread asks every few seconds which window is foreground, whose process it
//! runs, and gives that span to the day it fell in; the tally is kept in a small
//! table on disk, twenty-one days of it, so it stands across runs. Nothing here
//! is a link to Windows — the figures are Glance's own, read from the desktop.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// A day's sums: each program's name against the seconds it was fore
type Day = BTreeMap<String, u64>;

/// The whole kept history, oldest day first, keyed `YYYY-MM-DD`.
static HISTORY: RwLock<BTreeMap<String, Day>> = RwLock::new(BTreeMap::new());

/// Whether the watch has been started once.
static ONCE: std::sync::Once = std::sync::Once::new();

/// The file the table lives in.
fn file() -> PathBuf {
    crate::settings::config_dir().join("screen-time.tsv")
}

/// Today, as Windows' own local clock counts it.
fn today() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}

/// A day's key shifted back `n` days, by the civil calendar Windows keeps.
fn day_before(n: u16) -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let mut serial = to_serial(t.wYear as i32, t.wMonth as u32, t.wDay as u32) - n as i64;
    let (y, m, d) = from_serial(&mut serial);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Days since 0000-03-01 (Howard Hinnant's civil algorithm, enough to add/subtract).
fn to_serial(y: i32, m: u32, d: u32) -> i64 {
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 1) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m as i64) + 2) / 5 + d as i64 - 1;
    let doe = yoe as i64 * 365 + yoe as i64 / 4 - yoe as i64 / 100 + doy;
    era as i64 * 146097 + doe - 719468
}

/// The inverse: a civil date from a day count since 1970.
fn from_serial(days: &mut i64) -> (i32, u32, u32) {
    let z = *days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// The program holding the foreground window, by its file's stem; empty where none.
fn foreground_app() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut pid = 0u32;
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    process_name(pid)
}

/// A process's executable name, from its id.
fn process_name(pid: u32) -> String {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut buffer = [0u16; 512];
    let mut size = buffer.len() as u32;
    let got = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0), PWSTR(buffer.as_mut_ptr()), &mut size) };
    unsafe { let _ = CloseHandle(handle); };
    if got.is_err() || size == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    std::path::Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads the table from disk into memory.
fn load() {
    let Ok(text) = std::fs::read_to_string(file()) else {
        return;
    };
    let mut history = HISTORY.write().unwrap();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let (Some(day), Some(app), Some(secs)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let secs: u64 = secs.parse().unwrap_or(0);
        if secs > 0 {
            history.entry(day.to_string()).or_default().insert(app.to_string(), secs);
        }
    }
}

/// Writes the table back, dropping days older than three weeks.
fn save() {
    let mut history = HISTORY.write().unwrap();
    let cut = day_before(21);
    history.retain(|day, _| day.as_str() >= cut.as_str());
    let mut text = String::new();
    for (day, apps) in history.iter() {
        for (app, secs) in apps {
            text.push_str(&format!("{day}\t{app}\t{secs}\n"));
        }
    }
    let _ = std::fs::write(file(), text);
}

/// Adds `secs` to the given day's sum for `app`.
fn add(day: &str, app: &str, secs: u64) {
    HISTORY.write().unwrap().entry(day.to_string()).or_default().entry(app.to_string()).and_modify(|s| *s += secs).or_insert(secs);
}

/// A day's programs, longest first; empty where the day is unknown.
pub fn usage(day: &str) -> Vec<(String, u64)> {
    let history = HISTORY.read().unwrap();
    let mut listed: Vec<(String, u64)> = history.get(day).map(|apps| apps.clone().into_iter().collect()).unwrap_or_default();
    listed.sort_by(|a, b| b.1.cmp(&a.1));
    listed
}

/// The hours a day holds in total, as "3 时 24 分" or "3 h 24 min".
pub fn spoken(day: &str, zh: bool) -> String {
    let secs: u64 = usage(day).iter().map(|(_, s)| s).sum();
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if zh {
        format!("{h} 时 {m:02} 分")
    } else {
        format!("{h} h {m:02} min")
    }
}

/// The last seven days, each by its date and total seconds.
pub fn week() -> Vec<(String, u64)> {
    (0..7).rev().map(|back| {
        let day = day_before(back as u16);
        let total = usage(&day).iter().map(|(_, s)| s).sum();
        (day, total)
    }).collect()
}

/// Today's spoken total.
pub fn today_spoken(zh: bool) -> String {
    spoken(&today(), zh)
}

/// Yesterday's spoken total.
pub fn yesterday_spoken(zh: bool) -> String {
    spoken(&day_before(1), zh)
}

/// The program in front right now, by name.
pub fn foreground() -> String {
    foreground_app()
}

/// Starts the watch: reads the table once, then feeds it every ten seconds.
pub fn start() {
    ONCE.call_once(|| {
        load();
        std::thread::Builder::new()
            .name("glance: screen time".into())
            .spawn(|| {
                let mut since = 0u32;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    let app = foreground_app();
                    if !app.is_empty() {
                        add(&today(), &app, 10);
                    }
                    since += 1;
                    if since >= 6 {
                        since = 0;
                        save();
                    }
                }
            })
            .expect("a thread started");
    });
}

