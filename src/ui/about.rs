//! The about-this-PC card: a small, borderless, topmost window of its own — the
//! machine drawn as it is, the name it goes by, what its parts are, and a way to
//! Windows' own page. It lives on a thread of its own (see `run`), made and drawn
//! with the panel's graphics device, and goes when it is closed. The tray's
//! 关于本机 opens it (see `crate::show_about`).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::{HSTRING, w};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetWindowRect, IsWindow, LoadCursorW, MsgWaitForMultipleObjects,
    PeekMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, HTCAPTION, HTCLIENT, IDC_ARROW,
    MSG, PM_REMOVE, QS_ALLINPUT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_NCHITTEST, WM_PAINT, WM_QUIT, WM_SIZE, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::canvas::{Align, Canvas, Color, Family, Fill, Font};
use super::gfx::{self, Frame, Surface};
use super::icons::Icon;
use super::settings_window::{fill, text_centred};
use super::system_settings;
use super::text::Lang;
use super::theme;

/// The card's width in DIPs: an upright panel, wide enough that a long reading (a network adapter's name, a
/// disk's) mostly keeps to one line rather than breaking about every word. Its height is not fixed — a
/// reading that still wraps over several lines grows the card to hold them, so nothing is cut off (see `layout`).
const CARD_W: f32 = 420.0;
/// The least it stands to: the card when every reading fits a single line.
const CARD_MIN_H: f32 = 470.0;
/// Its sides, the room the header takes, and the width a reading's label is given.
const PAD: f32 = 22.0;
const TOP: f32 = 188.0;
const LABEL_W: f32 = 118.0;
/// The height a wrapped line takes, and the room left between readings — kept wide
/// enough that the readings breathe rather than press together.
const LINE: f32 = 19.0;
const GAP: f32 = 12.0;
/// The red dot's hit box, in DIPs from the card's corner.
const CLOSE: (f32, f32, f32, f32) = (14.0, 14.0, 20.0, 20.0);
/// The way on's own width, height, the room it is kept under the last reading, and
/// the room it and the line under it are kept above the card's foot. Its x is set
/// once the card's width is known, its y once its height is.
const MORE_W: f32 = 150.0;
const MORE_H: f32 = 32.0;
const MORE_ABOVE: f32 = 28.0;
const FOOT: f32 = 96.0;

/// The card while it is up (its hwnd, or 0).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// A thread being started, so a second click does not make two cards.
static OPENING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The window asked to be drawn again (shown, sized, a DPI change): the loop
    /// reads and clears it. Set true at first, so the card is drawn as it comes up.
    static REDRAW: Cell<bool> = const { Cell::new(true) };
    /// The card's height in DIPs, set as it is made and read back on a DPI change.
    static HEIGHT: Cell<f32> = const { Cell::new(CARD_MIN_H) };
    /// The way on's box in DIPs, set as the card is drawn and read back to hit-test
    /// it. Held here because where it sits depends on how the readings wrapped.
    static MORE: Cell<(f32, f32, f32, f32)> = const { Cell::new((CARD_W / 2.0 - 75.0, 346.0, 150.0, 32.0)) };
}

/// Brings the card up from any thread: raised if it is open, else made on a thread
/// of its own, centred on the monitor the pointer is on. `place` is taken for the
/// call's shape (the tray's icon place) but the card centres itself regardless.
pub fn open(place: Option<RECT>) {
    let _ = place;
    let held = WINDOW.load(Ordering::Acquire);
    if held != 0 {
        let hwnd = HWND(held as *mut _);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    if OPENING.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = std::thread::Builder::new().name("about".into()).spawn(run).is_ok();
    if !started {
        OPENING.store(false, Ordering::Release);
    }
}

/// The card's thread: the device taken, the window made and shown, then drawn
/// whenever it is asked to, between the messages that come, till it is closed.
fn run() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let left = || {
        WINDOW.store(0, Ordering::Release);
        OPENING.store(false, Ordering::Release);
    };
    let Ok(gfx) = gfx::at_pointer() else {
        left();
        return;
    };
    let Some((work, scale)) = crate::panel::work_area_at_cursor() else {
        left();
        return;
    };
    let scale = scale as f32;
    let (lang, dark) = read_look();
    // The card's height depends on how its readings wrap, so settle it before the
    // window is made and keep it for the draw and a later DPI change to read back.
    let card_h = layout(&gfx, lang).height;
    HEIGHT.with(|h| h.set(card_h));

    let instance = unsafe { GetModuleHandleW(None) };
    let Ok(instance) = instance else {
        left();
        return;
    };
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(procedure),
        hInstance: instance.into(),
        lpszClassName: w!("GlanceAbout"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassExW(&class);
    }
    let (width, height) = ((CARD_W * scale) as i32, (card_h * scale) as i32);
    let x = work.left + (work.right - work.left - width) / 2;
    let y = work.top + (work.bottom - work.top - height) / 2;
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
            w!("GlanceAbout"),
            w!("Glance"),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let Ok(hwnd) = created else {
        left();
        return;
    };
    let mut surface = match Surface::new(&gfx, hwnd) {
        Ok(surface) => surface,
        Err(error) => {
            gfx::failed("about", &gfx, Some(&error));
            left();
            return;
        }
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    WINDOW.store(hwnd.0 as isize, Ordering::Release);
    OPENING.store(false, Ordering::Release);

    let mut msg = MSG::default();
    let mut first = true;
    loop {
        let mut quit = false;
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                quit = true;
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if quit || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            break;
        }
        if first || REDRAW.with(|redraw| redraw.replace(false)) {
            let mut rect = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut rect);
            }
            let size = ((rect.right - rect.left).max(1) as u32, (rect.bottom - rect.top).max(1) as u32);
            let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
            let _ = surface.draw(&gfx, size, scale, |frame| paint(frame, lang, dark, card_h));
            first = false;
        }
        unsafe {
            MsgWaitForMultipleObjects(None, false, u32::MAX, QS_ALLINPUT);
        }
    }
    left();
}

/// The tongue and the shade the card wears, read as the rest of Glance reads them.
fn read_look() -> (Lang, bool) {
    (crate::update::language(), system_settings::system_dark())
}

/// The card's window: the whole of it drags (a caption), except the red dot and the
/// button, which take clicks; the dot and Esc close it, the button opens the
/// settings' own about page.
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => {
            // lParam here is in screen pixels; the card's body drags, its dot and
            // button take clicks.
            let (x, y) = where_clicked(hwnd, lparam, true);
            let more = MORE.with(|m| m.get());
            if in_box(x, y, CLOSE) || in_box(x, y, more) {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_LBUTTONDOWN => {
            // lParam here is already in client pixels, not screen: taken whole.
            let (x, y) = where_clicked(hwnd, lparam, false);
            let more = MORE.with(|m| m.get());
            if in_box(x, y, more) {
                way_on();
                let _ = DestroyWindow(hwnd);
            } else if in_box(x, y, CLOSE) {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let scale = (wparam.0 & 0xFFFF) as f32 / 96.0;
            let card_h = HEIGHT.with(|h| h.get());
            let _ = SetWindowPos(hwnd, None, 0, 0, (CARD_W * scale) as i32, (card_h * scale) as i32, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE);
            REDRAW.with(|redraw| redraw.set(true));
            LRESULT(0)
        }
        WM_SIZE => {
            REDRAW.with(|redraw| redraw.set(true));
            LRESULT(0)
        }
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            REDRAW.with(|redraw| redraw.set(true));
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// Where the pointer is over the card, in DIPs from its top-left. `lparam` holds
/// screen pixels for a hit-test (`from_screen`) and client pixels for a click.
fn where_clicked(hwnd: HWND, lparam: LPARAM, from_screen: bool) -> (f32, f32) {
    let (mut x, mut y) = (lparam.0 as i16 as i32, ((lparam.0 >> 16) as i16) as i32);
    if from_screen {
        let mut whole = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut whole);
        }
        x -= whole.left;
        y -= whole.top;
    }
    let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    (x as f32 / scale, y as f32 / scale)
}

/// Whether `(x, y)` falls in `box` (its top-left and size, in DIPs).
fn in_box(x: f32, y: f32, (bx, by, bw, bh): (f32, f32, f32, f32)) -> bool {
    x >= bx && x < bx + bw && y >= by && y < by + bh
}

/// `text` in `font` broken to lines no wider than `max`: on spaces where it can,
/// and character by character when a single word (a long serial, say) will not fit.
fn wrap(gfx: &gfx::Gfx, text: &str, font: Font, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let flush = |line: &mut String, lines: &mut Vec<String>| {
        if !line.is_empty() {
            lines.push(std::mem::take(line));
        }
    };
    for word in text.split_whitespace() {
        // A word too long for the column on its own is packed a character at a time.
        if gfx.measure(word, font) > max {
            flush(&mut line, &mut lines);
            let mut chunk = String::new();
            for ch in word.chars() {
                if gfx.measure(&format!("{chunk}{ch}"), font) <= max || chunk.is_empty() {
                    chunk.push(ch);
                } else {
                    lines.push(std::mem::take(&mut chunk));
                    chunk.push(ch);
                }
            }
            line = chunk;
            continue;
        }
        let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if gfx.measure(&next, font) <= max {
            line = next;
        } else {
            flush(&mut line, &mut lines);
            line = word.to_string();
        }
    }
    flush(&mut line, &mut lines);
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// A reading as it is laid out: its label, the value broken to lines, and the centre
/// of the first line down the card.
struct Reading {
    label: String,
    lines: Vec<String>,
    top: f32,
}

/// The card's own measure: the readings laid out for its width, where its way on
/// sits, and the height the whole needs. Drawn and sized from here, so the two never
/// fall out of step.
struct Layout {
    readings: Vec<Reading>,
    more: (f32, f32, f32, f32),
    height: f32,
}

fn layout(gfx: &gfx::Gfx, lang: Lang) -> Layout {
    // The device's own name stands at the card's head already, so its reading is left out below.
    let facts = system_settings::about_facts(lang == Lang::Zh);
    let small = Font::new(Family::Segoe, 12.5, 400.0);
    let value_x = PAD + LABEL_W;
    let value_w = CARD_W - PAD - value_x;
    let mut cy = TOP;
    let mut readings = Vec::new();
    for (label, value) in facts.iter().skip(1) {
        let lines = wrap(gfx, value, small, value_w);
        readings.push(Reading { label: label.clone(), lines, top: cy });
        let last = readings.last().unwrap();
        cy = last.top + last.lines.len() as f32 * LINE + GAP;
    }
    let last = readings.last().map(|r| r.top + (r.lines.len() - 1) as f32 * LINE);
    let height = last.map_or(CARD_MIN_H, |bottom| (bottom + FOOT).max(CARD_MIN_H));
    let more = (CARD_W / 2.0 - MORE_W / 2.0, height - FOOT + MORE_ABOVE, MORE_W, MORE_H);
    Layout { readings, more, height }
}

/// Draws the card: its glass, the machine, its name, the readings, the way on, and
/// the line under it all.
fn paint(frame: &Frame, lang: Lang, dark: bool, card_h: f32) {
    let layout = layout(&frame.gfx, lang);
    MORE.with(|m| m.set(layout.more));
    let accent = theme::accent(dark);
    let (bg, text, dim, edge, pill) = if dark {
        (Color::hex(0x2B_2B_30, 1.0), Color::hex(0xF2_F2_F5, 1.0), Color::hex(0x9A_9A_A4, 1.0), Color::hex(0xFF_FF_FF, 0.12), Color::hex(0x3A_3A_41, 1.0))
    } else {
        (Color::hex(0xF5_F5_F7, 1.0), Color::hex(0x1C_1C_20, 1.0), Color::hex(0x6E_6E_78, 1.0), Color::hex(0x00_00_00, 0.10), Color::hex(0xFF_FF_FF, 1.0))
    };
    // Its glass, and the hairline about it.
    fill(frame, bg, 0.0, 0.0, CARD_W, card_h, 16.0);
    frame.stroke_rounded(Fill::Solid(edge), 0.5, 0.5, CARD_W - 1.0, card_h - 1.0, 16.0, 1.0);
    // The one light, top-left: the red dot that closes the card.
    fill(frame, Color::hex(0xFF_5F_57, 1.0), 16.0, 16.0, 12.0, 12.0, 6.0);
    // The machine itself, and the name it goes by under it.
    frame.icon(Icon::AppWindow, super::canvas::Point { x: CARD_W / 2.0, y: 70.0 }, 54.0, dim);
    let centred = |s: &str, font: Font, color: Color, cy: f32| {
        let m = frame.gfx.measure(s, font).min(CARD_W - 2.0 * PAD);
        text_centred(frame, s, font, color, (CARD_W - m) / 2.0, cy, m, Align::Start);
    };
    centred(&system_settings::computer_name(), Font::new(Family::SegoeDisplay, 17.0, 600.0), text, 134.0);
    centred(lang.pick("本机", "This PC"), Font::new(Family::Segoe, 12.0, 400.0), dim, 156.0);
    // The readings: a label in its own column at the left, its value in the column
    // that is left. A value too long for that column is wrapped over several lines,
    // each kept under the first, and the label held level with the line it starts on.
    let small = Font::new(Family::Segoe, 12.5, 400.0);
    let value_x = PAD + LABEL_W;
    let value_w = CARD_W - PAD - value_x;
    for reading in &layout.readings {
        text_centred(frame, &reading.label, small, dim, PAD, reading.top, LABEL_W, Align::Start);
        for (k, line) in reading.lines.iter().enumerate() {
            text_centred(frame, line, small, text, value_x, reading.top + k as f32 * LINE, value_w, Align::End);
        }
    }
    // The way on, to Windows' own page, and the line under it all.
    let more = layout.more;
    fill(frame, pill, more.0, more.1, more.2, more.3, 8.0);
    frame.stroke_rounded(Fill::Solid(accent), more.0 + 0.5, more.1 + 0.5, more.2 - 1.0, more.3 - 1.0, 8.0, 1.0);
    centred(lang.pick("更多信息…", "More…"), Font::new(Family::Segoe, 13.0, 500.0), text, more.1 + more.3 / 2.0);
    centred("© 1983–2026 Glance", Font::new(Family::Segoe, 11.0, 400.0), dim, card_h - 22.0);
}

/// The way on: Windows' own page about this PC, which says what the card says and
/// a good deal more beside.
fn way_on() {
    let page = HSTRING::from("ms-settings:about");
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &page, None, None, SW_SHOW);
    }
}
