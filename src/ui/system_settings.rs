//! What Windows is asked, rather than what it is shown: the reads behind the about-this-PC card (see
//! `super::about`) — the machine's own name and parts, the screens it has, whether it stands on battery, and the
//! shade its theme wears. Registry words are read here and nowhere written: what the card says, it only says.

use windows::core::{HSTRING, w};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use super::canvas::{Canvas, Color, Fill, Point};
use super::gfx::Frame;
use super::settings_window::fill;
use super::text::Lang;
use crate::metrics::reg_string;

// ---- Registry foundation ----

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

// ---- The shade Windows wears ----

pub(crate) fn system_dark() -> bool {
    reg_word(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    ) != Some(1)
}

// ---- Battery ----

pub(crate) fn battery_status() -> Option<(u8, bool, bool)> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    if status.BatteryFlag & 128 != 0 || status.BatteryLifePercent == 255 {
        return None;
    }
    Some((
        status.BatteryLifePercent,
        status.BatteryFlag & 8 != 0,
        status.ACLineStatus == 1,
    ))
}

// ---- About: the machine's facts ----

const GB: u64 = 1024 * 1024 * 1024;

fn gigabytes(bytes: u64) -> String {
    let gb = (bytes as f64 / GB as f64 * 10.0).round() / 10.0;
    if gb.fract() == 0.0 {
        format!("{} GB", gb as u64)
    } else {
        format!("{gb:.1} GB")
    }
}

fn said(first: String, second: String) -> String {
    if first.is_empty() {
        second
    } else {
        first
    }
}

pub(crate) fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// A screen, as the pane shows it.
#[derive(Clone)]
struct Shown {
    size: String,
    scale: String,
    primary: bool,
}

/// The screens as they are now, each with the scale it was made for.
fn read_screens() -> Vec<Shown> {
    crate::screens::all()
        .iter()
        .map(|screen| {
            let (wide, tall) = (
                screen.monitor.right - screen.monitor.left,
                screen.monitor.bottom - screen.monitor.top,
            );
            let dpi = unsafe {
                let monitor = MonitorFromRect(&screen.monitor, MONITOR_DEFAULTTONEAREST);
                let (mut dpi, mut vertical) = (96u32, 96u32);
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut vertical);
                dpi
            };
            Shown {
                size: format!("{wide} × {tall}"),
                scale: format!("{}%", dpi as u32 * 100 / 96),
                primary: screen.primary,
            }
        })
        .collect()
}

/// What this PC is made of, as the pane reads it.
struct About {
    machine: String,
    os: String,
    cpu: String,
    threads: usize,
    memory: String,
    gpus: Vec<String>,
    screen: String,
    serial: String,
    bios: String,
}

fn read_about() -> About {
    let info = &crate::app().info;
    let bios_key = w!(r"HARDWARE\DESCRIPTION\System\BIOS");
    let machine_key = w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\OEMInformation");
    let stock = crate::smbios::system();
    let badge = reg_string(machine_key, w!("Model"));
    let named = reg_string(bios_key, w!("SystemProductName"));
    About {
        machine: said(
            said(badge, stock.product),
            if named.is_empty() {
                info.board.clone()
            } else {
                named
            },
        ),
        serial: stock.serial,
        bios: stock.bios,
        os: crate::diagnostics::windows(w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"))
            .strip_prefix("Windows: ")
            .unwrap_or("")
            .to_string(),
        cpu: info.cpu_name.clone(),
        threads: info.threads,
        memory: info
            .memory_modules
            .clone()
            .unwrap_or_else(|| gigabytes(info.mem_total)),
        gpus: info.gpus.iter().map(|gpu| gpu.name.clone()).collect(),
        screen: {
            let screens = read_screens();
            screens
                .iter()
                .find(|one| one.primary)
                .or_else(|| screens.first())
                .map(|one| format!("{} · {}", one.size, one.scale))
                .unwrap_or_default()
        },
    }
}

/// This PC, as the about page tells it: a label and a value to a row.
pub(crate) fn about_facts(lang: Lang) -> Vec<(String, String)> {
    let about = read_about();
    let none = lang.pick("固件没有登记", "Nothing stamped");
    let told = |said: &str| -> String {
        if said.is_empty() {
            none.to_string()
        } else {
            said.to_string()
        }
    };
    let host = computer_name();
    let head = if about.machine.is_empty() {
        host.clone()
    } else {
        about.machine.clone()
    };
    let gpus = about.gpus.join(" · ");
    // The further hardware the app already knows from the firmware and Windows: the
    // board's own model, each physical drive, and the adapter traffic leaves by.
    let info = &crate::app().info;
    let board = info.board.clone();
    let drives = info.drives.join(" · ");
    let nic = info.network_adapter.clone().unwrap_or_default();
    vec![
        (lang.pick("设备名称", "Device name").to_string(), host),
        (lang.pick("型号", "Model").to_string(), told(&head)),
        (lang.pick("系统", "System").to_string(), told(&about.os)),
        (
            lang.pick("处理器", "Processor").to_string(),
            format!("{} · {}", about.cpu, about.threads),
        ),
        (
            lang.pick("内存", "Memory").to_string(),
            about.memory.clone(),
        ),
        (lang.pick("主板", "Motherboard").to_string(), told(&board)),
        (lang.pick("显卡", "Graphics").to_string(), told(&gpus)),
        (lang.pick("硬盘", "Storage").to_string(), told(&drives)),
        (
            lang.pick("显示器", "Display").to_string(),
            told(&about.screen),
        ),
        (lang.pick("网卡", "Network").to_string(), told(&nic)),
        (
            lang.pick("序列号", "Serial number").to_string(),
            told(&about.serial),
        ),
        (
            lang.pick("BIOS 版本", "BIOS version").to_string(),
            told(&about.bios),
        ),
    ]
}

// ---- The machine drawn ----

/// The machine drawn: a laptop or a tower, in the card's own line, 96 across.
pub(crate) fn device_art(frame: &Frame, x: f32, y: f32, laptop: bool) {
    let k = 96.0 / 152.0;
    let (sx, sy) = (|vx: f32| x + vx * k, |vy: f32| y + vy * k);
    let (lid, void, edge, metal, foot) = (
        Color::hex(0x20242E, 0.95),
        Color::hex(0x04050A, 0.9),
        Color::hex(0xFFFFFF, 0.32),
        Color::hex(0xB4BCCC, 0.55),
        Color::hex(0x000000, 0.16),
    );
    if laptop {
        fill(frame, lid, sx(21.0), sy(6.0), 110.0 * k, 70.0 * k, 4.6 * k);
        frame.stroke_rounded(
            Fill::Solid(edge),
            sx(21.0),
            sy(6.0),
            110.0 * k,
            70.0 * k,
            4.6 * k,
            1.2 * k,
        );
        fill(frame, void, sx(24.4), sy(9.2), 103.2 * k, 63.6 * k, 2.4 * k);
        frame.fill_shape(
            &[
                Point {
                    x: sx(26.4),
                    y: sy(11.2),
                },
                Point {
                    x: sx(125.6),
                    y: sy(11.2),
                },
                Point {
                    x: sx(125.6),
                    y: sy(70.8),
                },
                Point {
                    x: sx(26.4),
                    y: sy(70.8),
                },
            ],
            Fill::Down {
                top: sy(11.2),
                from: Color::hex(0x8FB4FF, 0.60),
                bottom: sy(70.8),
                to: Color::hex(0xA86BFF, 0.34),
            },
        );
        fill(frame, void, sx(68.0), sy(11.2), 16.0 * k, 5.0 * k, 2.0 * k);
        frame.fill_shape(
            &[
                Point {
                    x: sx(12.0),
                    y: sy(78.0),
                },
                Point {
                    x: sx(140.0),
                    y: sy(78.0),
                },
                Point {
                    x: sx(148.0),
                    y: sy(100.0),
                },
                Point {
                    x: sx(4.0),
                    y: sy(100.0),
                },
            ],
            Fill::Solid(metal),
        );
        fill(frame, void, sx(54.0), sy(82.0), 44.0 * k, 14.0 * k, 2.0 * k);
    } else {
        fill(frame, lid, sx(48.0), sy(6.0), 56.0 * k, 94.0 * k, 4.0 * k);
        frame.stroke_rounded(
            Fill::Solid(edge),
            sx(48.0),
            sy(6.0),
            56.0 * k,
            94.0 * k,
            4.0 * k,
            1.2 * k,
        );
        fill(frame, void, sx(60.0), sy(18.0), 16.0 * k, 16.0 * k, 8.0 * k);
        fill(frame, metal, sx(52.0), sy(56.0), 48.0 * k, 4.0 * k, 2.0 * k);
        fill(frame, foot, sx(44.0), sy(100.0), 64.0 * k, 4.0 * k, 2.0 * k);
    }
}
