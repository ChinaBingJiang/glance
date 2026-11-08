//! The settings window: the choices on the left, and on the right the panel
//! as it will look, over the desktop the window was opened on, running live.
//!
//! The window lives on the panel's thread and draws with its device. Its rows
//! are laid out and drawn from the settings on every frame; what moves (a
//! choice's thumb, a switch's knob, rows making way for a dragged one) is
//! kept from frame to frame as transitions.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::f32::consts::E;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_PARAMETERS1, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::Input::Ime::{ImmAssociateContextEx, ImmGetVirtualKey, HIMC};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_PROCESSKEY, VK_LEFT, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, LoadCursorW, LoadImageW,
    IsWindowVisible, MessageBoxW, PostMessageW, RegisterClassExW, SetCursor, SetForegroundWindow, MB_ICONINFORMATION, MB_OK, WM_CLOSE,
    WM_KEYDOWN, WM_SYSKEYDOWN, WM_ACTIVATE, WA_INACTIVE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, WM_APP, WM_DISPLAYCHANGE, WM_QUIT, MSG, PM_REMOVE, QS_ALLINPUT,
    DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage,
    SetWindowPos, SetWindowTextW, ShowWindow, HICON, IDC_ARROW, IDC_HAND, IMAGE_ICON, LR_SHARED, MINMAXINFO,
    SM_CXICON, SM_CXSMICON, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER, WM_CAPTURECHANGED, WM_DESTROY,
    WM_DPICHANGED, WM_GETMINMAXINFO, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_SETCURSOR, WM_SIZE, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};
use windows::Win32::System::Threading::{CreateWaitableTimerExW, SetWaitableTimer, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS};
use windows_numerics::{Matrix3x2, Vector2};

use super::backdrop::Capture;
use super::canvas::{Align, Canvas, Color, Family, Font, Point};
use super::gfx::{self, rect, Frame, Gfx, Surface};
use super::icons::Icon;
use super::motion::{Easing, Transition};
use super::overlay::{Layout, ROWS};
use super::prefs::{self, LanguagePref, MusicGround, Prefs, ThemePref};
use super::arrange::{self, GAP};
use super::life::Lives;
use super::render::{self, PanelLayers};
use super::skins;
use super::text::Lang;
use super::theme::{self, Shadow, Skin, Theme};
use super::view::{self, Rect, Scene};
use super::wallpaper;
use crate::elevation;
use crate::metrics;
use crate::settings::{Anchor, DockEdge, Edge, OverFullscreen, OverlayShown, Part, Sensitivity, Settings, Shortcut, EDGES};

pub(crate) mod guide;
mod layout_page;
mod reads;
use layout_page::{Carry, Zone};
use crate::panel::SIZES;
use crate::update;

/// The window's size, and the least it can be resized to (DIPs).
const SIZE: (f32, f32) = (1300.0, 760.0);
const MIN_SIZE: (f32, f32) = (1080.0, 560.0);
/// The first Windows 11 build that lets a window ask for Mica.
pub(crate) const FIRST_MICA_BUILD: u32 = 22621;

/// The list of choices: how wide its cards are (the least, in the narrowest window, and the most), how much wider
/// they grow for each DIP the window is wider than its narrowest, and the room past them to the preview (its scroll
/// bar in it). See `Ui::cards`.
const CARDS: (f32, f32) = (400.0, 520.0);
const CARDS_GROW: f32 = 40.0 / 220.0;
const LIST_GAP: f32 = 48.0;
/// The pages' list, left of the page.
const NAV: f32 = 200.0;
const NAV_ITEM: f32 = 40.0;
/// The room the components' heading takes in the pages' list, and the gap that sets it (and the rule after its
/// pages) apart.
const NAV_HEADING: f32 = 28.0;
const NAV_GAP: f32 = 12.0;
const PAD_TOP: f32 = 28.0;
const PAD_SIDE: f32 = 36.0;
const PAD_BOTTOM: f32 = 48.0;
/// Rows: their least height, sides, the space between rows of a group, and
/// before a group's heading.
const ROW: f32 = 60.0;
const ROW_SIDE: f32 = 20.0;
/// How round a card's corners are.
const CARD_RADIUS: f32 = 8.0;
/// The size of what is said under a row's name (and of notes).
const HINT: f32 = 13.0;
const ROW_GAP: f32 = 8.0;
/// The line of its own a row of choices with a word on it takes below.
const CHOICE_LINE: f32 = 40.0;
/// The room the taskbar's edge is chosen in: four cards of the screen, each with its word under it.
const POSITION_CARDS: f32 = 100.0;
/// A slider's track, the room for its value, and its thumb's radius.
const SLIDER_WIDTH: f32 = 160.0;
const SLIDER_VALUE: f32 = 44.0;
const SLIDER_THUMB: f32 = crate::ui::thumb::FACE;
/// The narrowest a row's word may be beside its choices.
const MIN_WORDS: f32 = 140.0;
/// The page's name, at the top of the page.
const TITLE: f32 = 52.0;
/// What the overlay's groups' rows are told from the panel's modules by:
/// the start of their ids.
const OVERLAY: &str = "overlay:";
/// What the workbench's modules' rows are told from the readings' by: the start of their ids.
const WORKBENCH: &str = "workbench:";

/// A row inside a module's open card (its name alone: as tall as Windows' own settings make such a row), and how far
/// in from the card's edge its label starts: under the module's name, past the grip.
const ITEM_ROW: f32 = 52.0;
const ITEM_INSET: f32 = 12.0 + 20.0 + 12.0;
/// Room at the right of a module's row for its chevron, after its switch.
const CHEVRON_ROOM: f32 = 28.0;
/// How large a chevron is drawn.
const CHEVRON: f32 = 14.0;
/// An open picker's list: the height of one of its words, and the most a pane shows at once —
/// a longer list is kept within that and turned by the wheel (see `paint_hub_menu`).
const DROP_LINE: f32 = 34.0;
const DROP_CAP: f32 = 314.0;
/// The screens in the layout's editor: a screen's picture's corners; and the dashes of an edge the pointer passes on
/// from (the least they are long, and apart).
const MAP_RADIUS: f32 = 3.0;
const MAP_DASH: f32 = 4.0;
const MAP_GAP: f32 = 3.0;
const SKINS_ROW: f32 = 16.0 + 72.0 + 10.0 + 18.0 + 16.0;
/// Desktop shown beside the panel in the preview (DIPs of screen).
const PREVIEW_MARGIN: f32 = 160.0;
/// How often the music page's preview is drawn while something plays (as a chart's speed: a quarter pixel each
/// thirtieth of a second, see `draw`).
const MUSIC_PREVIEW_PACE: f32 = 7.5;
/// How far a wheel notch scrolls the choices, and how quickly they follow.
const WHEEL: f32 = 100.0;
const SCROLL_EASE: f32 = 0.06;
/// How the controls move: a thumb sliding to the chosen option, a switch's
/// knob, rows making way for a dragged one, and the dragged row settling.
const SLIDE: (Duration, Easing) = (Duration::from_millis(240), Easing(0.16, 1.0, 0.3, 1.0));
const FLIP: (Duration, Easing) = (Duration::from_millis(160), Easing(0.16, 1.0, 0.3, 1.0));
const GLIDE: (Duration, Easing) = (Duration::from_millis(200), Easing(0.16, 1.0, 0.3, 1.0));
const SETTLE: (Duration, Easing) = (Duration::from_millis(180), Easing(0.16, 1.0, 0.3, 1.0));
/// How the site's notice, opened at from its notification, is marked: tinted in the accent, the tint held a moment and
/// let go.
const FLASH: (Duration, Easing) = (Duration::from_millis(1300), Easing(0.7, 0.0, 0.9, 0.6));
/// How soon to try again when drawing failed for want of a device.
/// A frame that takes longer than this shows as a stutter: noted.
const SLOW_FRAME: Duration = Duration::from_millis(33);
const DEVICE_RETRY: Duration = Duration::from_millis(250);
/// How often the window, hidden (made for the guide, till it shows it), looks again at what it shows.
const HIDDEN_LOOKED_AT: Duration = Duration::from_millis(250);
/// The pen of the preview's charts runs this far behind the newest sample
/// beyond one interval, as the panel's does.
const PEN_LAG_MS: f64 = 100.0;

/// The accent colours offered on chips: RGB, Chinese name, English name.
const ACCENTS: [(u32, &str, &str); 8] = [
    (0x0078D4, "蓝", "Blue"),
    (0xF7630C, "橙", "Orange"),
    (0xE81123, "红", "Red"),
    (0x16C60C, "绿", "Green"),
    (0x8E44AD, "紫", "Purple"),
    (0xE3008C, "粉", "Pink"),
    (0x00B7C3, "青", "Teal"),
    (0xFFD200, "黄", "Yellow"),
];

/// How long the diagnostics' button says they were copied.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// The window, while it is open.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The window the settings pour into their place on while they do (see `pour_away`): in front in their stead, the panel
/// and the widgets giving way to it.
static POURING: AtomicIsize = AtomicIsize::new(0);
/// The place the window was asked from (see `show`), while it is open, pouring back into it included.
static ASKED_FROM: std::sync::Mutex<Option<RECT>> = std::sync::Mutex::new(None);

/// The place the settings, open, were asked from (physical px: the button they pour back into as they close), if they
/// were from one. Known before the window is, so what it was asked from shows it was from the window's first frame on.
pub fn asked_from() -> Option<RECT> {
    *ASKED_FROM.lock().unwrap()
}

/// The place the settings were asked from, `from`, has moved `to` (a widget moved with its button, or none, its button
/// gone): they pour back into it there. Another place asked from since is kept.
pub fn asked_from_moved(from: RECT, to: Option<RECT>) {
    let mut asked = ASKED_FROM.lock().unwrap();
    if *asked == Some(from) {
        *asked = to;
    }
}

/// The place the settings last poured into as they closed, and when they were in (see `bounce`).
static LANDED: std::sync::Mutex<Option<(RECT, Instant)>> = std::sync::Mutex::new(None);
/// How long the button the settings poured into bounces as they land in it.
pub const BOUNCE: Duration = Duration::from_millis(450);

/// How long till charts creeping at `speed` (physical pixels a second) have crept a quarter of a pixel, when what they
/// are on is drawn again at rest: with no chart, a quarter of a second.
pub fn creeping(speed: Option<f32>) -> Duration {
    speed.map_or(Duration::from_millis(250), |speed| Duration::from_secs_f32(0.25 / speed))
}

/// The button at `button` (physical px) as the settings pour into it: how large it is drawn now (1: as it is), while it
/// bounces (see `BOUNCE`), they having landed in it. Up at once, a little under, settling: a spring let go.
pub fn bounce(button: Option<RECT>) -> Option<f32> {
    let (place, at) = (*LANDED.lock().unwrap())?;
    let t = at.elapsed().as_secs_f32() / BOUNCE.as_secs_f32();
    (Some(place) == button && t < 1.0).then(|| grown(t))
}

/// How large a button bouncing is drawn `t` of the way through (0 to 1): from 1, quickly up to about a fifth larger,
/// a little under, and back to 1.
fn grown(t: f32) -> f32 {
    1.0 + 0.3 * (-6.0 * t).exp() * (2.5 * std::f32::consts::PI * t).sin()
}

#[cfg(test)]
mod placed_tests {
    use super::placed_clear_of;
    use windows::Win32::Foundation::RECT;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    /// The settings open in the middle of the screen, unless they would cover the button they were asked from: moved
    /// clear of it the least way that fits (as on the owner's wide screen, a widget's gear under the middle: moved
    /// left of it), in the middle still where no way fits.
    #[test]
    fn the_settings_open_clear_of_their_button() {
        // A 3440 × 1440 screen at 150%, the window 1932 × 1131 (as measured).
        let work = rect(-745, -1440, 2695, -48);
        let size = (1932, 1131);
        let middle = (-745 + (3440 - 1932) / 2, -1440 + (1392 - 1131) / 2);
        assert_eq!(placed_clear_of(work, size, None, 18), middle);
        assert_eq!(placed_clear_of(work, size, Some(rect(2600, -100, 2627, -73)), 18), middle);
        let gear = rect(1431, -963, 1458, -936);
        let (x, y) = placed_clear_of(work, size, Some(gear), 18);
        assert_eq!((x + 1932, y), (1431 - 18, middle.1));
        assert!(x >= work.left);
        // Right of it where that is the shorter way.
        let (x, _) = placed_clear_of(work, size, Some(rect(100, -963, 127, -936)), 18);
        assert_eq!(x, 127 + 18);
        // A screen it covers whichever way: in the middle.
        let small = rect(0, 0, 2000, 1200);
        assert_eq!(placed_clear_of(small, size, Some(rect(990, 590, 1010, 610)), 18), ((2000 - 1932) / 2, (1200 - 1131) / 2));
    }

    /// On the owner's tall screen at 200%, 1080 DIPs across (no wider than the window can be), the settings asked from
    /// the gear at the foot of the panel's rail open as wide as the screen, as the system holds them, over the gear:
    /// clear of it above, on the screen whole (they were placed nine tenths as wide, right of the gear, then widened
    /// past the screen's side).
    #[test]
    fn the_settings_open_as_large_as_they_are_on_a_narrow_screen() {
        let work = rect(-2905, -1459, -745, 2381);
        let size = super::opening_size(work, 2.0);
        assert_eq!(size, (2160, 1520));
        assert!(size.0 >= super::least_size(work, 2.0).0 && size.1 >= super::least_size(work, 2.0).1);
        let gear = rect(-2863, 1188, -2803, 1248);
        let (x, y) = placed_clear_of(work, size, Some(gear), 24);
        assert_eq!((x, y + size.1), (-2905, 1188 - 24));
        // Where the screen is wider than the least: as large as it opens anywhere.
        assert_eq!(super::opening_size(rect(-745, -1440, 2695, -48), 1.5), (1950, 1140));
    }
}

#[cfg(test)]
mod bounce_tests {
    /// A button the settings land in grows at once (over a tenth larger within the first fifth), dips a little under
    /// its size, and settles back to it by the end, never twice as large nor half as small.
    #[test]
    fn a_button_landed_in_bounces_and_settles() {
        let at = |t: f32| super::grown(t);
        assert!((at(0.0) - 1.0).abs() < 1e-6);
        assert!((0..=20).map(|step| at(step as f32 / 100.0)).fold(0.0f32, f32::max) > 1.1);
        assert!((40..=80).map(|step| at(step as f32 / 100.0)).fold(2.0f32, f32::min) < 1.0);
        assert!((at(1.0) - 1.0).abs() < 0.01);
        assert!((0..=100).map(|step| at(step as f32 / 100.0)).all(|grown| grown > 0.5 && grown < 2.0));
    }
}

/// For tests: as if the settings were open, asked from `place` (none: closed).
#[cfg(test)]
pub fn pretend_asked_from(place: Option<RECT>) {
    *ASKED_FROM.lock().unwrap() = place;
}
/// Its thread is making it: asked again meanwhile, no second one is made.
static OPENING: AtomicBool = AtomicBool::new(false);
/// Its thread, kept a while before it is opened and after it is closed (see `run`).
static THREAD: std::sync::Mutex<Kept> = std::sync::Mutex::new(Kept::None);
/// Told to its thread, kept while the window is not up: the window made, at the page `WPARAM` names (see `To::told`),
/// poured out of the place `LPARAM` tells (see `genie::told`).
const AGAIN: u32 = WM_APP + 43;
/// How long its thread is kept, its graphics device made, with no window up: the gear pointed at and not pressed,
/// the window closed and not opened again.
const KEPT_FOR: Duration = Duration::from_secs(10);

/// The window's thread: none, being started (and asked for the window meanwhile: see `open_at`), or going, by its id.
enum Kept {
    None,
    Starting(Option<Asked>),
    Running(u32),
}

/// The window as it is asked for: at a page (none: the one last shown), poured out of the place it was asked from
/// (see `genie`: none, shown at once).
#[derive(Clone, Copy)]
struct Asked {
    to: Option<To>,
    place: Option<RECT>,
}
/// What other threads tell the window: settings changed elsewhere, to be taken up (see `follow_settings`); the
/// overlay's page asked for (see `open_at_overlay`).
const FOLLOW: u32 = WM_APP + 40;
const TO_OVERLAY: u32 = WM_APP + 41;
/// (Measuring only: see `procedure`.)
const PERF_TURN: u32 = WM_APP + 90;
/// (Measuring: the live backdrop's switch pressed, as a press on it presses it; see `perf`.)
const PERF_LIVE: u32 = WM_APP + 91;
/// Asked to turn to the workbench's page.
const TO_WORKBENCH: u32 = WM_APP + 42;
/// Asked to turn to the page of the reports sent.
const TO_REPORTS: u32 = WM_APP + 44;
/// Asked to begin the guide; told that what a step of it asks was done elsewhere (see `guide::saw`).
const TO_GUIDE: u32 = WM_APP + 46;
/// Asked to turn to the updates (the general page's): from a notification of a release found or of a notice.
const TO_UPDATES: u32 = WM_APP + 48;
const GUIDE_TOLD: u32 = WM_APP + 47;
/// What the window of a report's thread tells it (see `ask::thread::open`): the user sent what they wrote there,
/// closed the report or opened it again, or closed the window. Kept as told (`THREAD_SEND`, `THREAD_TURN`,
/// `THREAD_CLOSE`), and seen to as the window is next looked at (see `tick`), outside whatever holds its state then.
const THREAD_TOLD: u32 = WM_APP + 45;
static THREAD_OWED: AtomicUsize = AtomicUsize::new(0);
const THREAD_SEND: usize = 1;
const THREAD_CLOSE: usize = 2;
const THREAD_TURN: usize = 4;

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Whether it is open and not minimized: what it shows can be seen.
pub fn is_seen() -> bool {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    // (Hidden while made for the guide, till it shows it: see `guide`.)
    hwnd != 0 && unsafe { IsWindowVisible(HWND(hwnd as *mut _)) }.as_bool() && !unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(HWND(hwnd as *mut _)) }.as_bool()
}

/// Brings the settings window up, from any thread: raised if it is open, else made, by the panel's thread, poured
/// out of `place` (the button it is asked from, physical px: see `genie`), or, with none, shown at once.
pub fn show(place: Option<RECT>) {
    let hwnd = WINDOW.load(Ordering::Acquire);
    crate::perf::mark(|| format!("settings shown, {}", if hwnd == 0 { "asked of the panel" } else { "raised" }));
    if hwnd == 0 {
        crate::app().controller.open_settings(place);
        return;
    }
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        // (Asked of its own thread, not waited for: called from the tray's and the panel's.)
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
    crate::playing::look_now();
}

/// Makes the window, centred on the monitor the pointer is on, on the
/// The settings window, while it is open.
pub fn hwnd() -> Option<HWND> {
    let hwnd = WINDOW.load(Ordering::Acquire);
    (hwnd != 0).then_some(HWND(hwnd as *mut _))
}

/// thread of its own, which carries it till it is closed (see `run`): moved or sized by hand, the system holds that
/// thread in a loop of its own till it is let go, and the panel, the widgets and the lyrics go on meanwhile. Open,
/// it is raised. Poured out of `place` (see `show`).
pub fn open(place: Option<RECT>) {
    open_at(Asked { to: None, place });
}

/// Opens the window (as `open`) at the overlay's page: from the overlay's menu.
pub fn open_at_overlay(place: Option<RECT>) {
    open_at(Asked { to: Some(To::Overlay), place });
}

/// Opens the window (as `open`) at the workbench's page: from the station's menu.
pub fn open_at_workbench(place: Option<RECT>) {
    open_at(Asked { to: Some(To::Workbench), place });
}

/// Opens the window (as `open`) at the page of the reports this PC sent: from a notification of news of one.
pub fn open_at_reports(place: Option<RECT>) {
    open_at(Asked { to: Some(To::Reports), place });
}

/// Opens the window (as `open`) at the updates, on the general page: from a notification of a release found or of a
/// notice of Glance's site (which is marked as it is shown).
pub fn open_at_updates(place: Option<RECT>) {
    open_at(Asked { to: Some(To::Updates), place });
}

/// Begins the guide to Glance (see `guide`), from any thread: as Glance first starts with it, the window made for it
/// and shown by the guide as it comes up.
pub fn open_guide() {
    open_at(Asked { to: Some(To::Guide), place: None });
}

/// A page the window is opened at, other than the one last shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum To {
    Overlay,
    Workbench,
    Reports,
    /// The guide begun (see `guide`).
    Guide,
    Updates,
}

impl Asked {
    /// As `AGAIN` tells it.
    fn of_told(msg: &MSG) -> Asked {
        Asked { to: To::of_told(msg.wParam.0), place: super::genie::of_told(msg.lParam.0) }
    }
}

impl To {
    /// As it is told to the window's thread (see `AGAIN`).
    fn told(to: Option<To>) -> usize {
        match to {
            None => 0,
            Some(To::Overlay) => 1,
            Some(To::Workbench) => 2,
            Some(To::Reports) => 3,
            Some(To::Guide) => 4,
            Some(To::Updates) => 5,
        }
    }

    fn of_told(told: usize) -> Option<To> {
        match told {
            1 => Some(To::Overlay),
            2 => Some(To::Workbench),
            3 => Some(To::Reports),
            4 => Some(To::Guide),
            5 => Some(To::Updates),
            _ => None,
        }
    }
}

fn open_at(asked: Asked) {
    crate::perf::mark(|| "settings asked".into());
    let to = asked.to;
    if let Some(hwnd) = hwnd() {
        let message = match to {
            Some(To::Overlay) => Some(TO_OVERLAY),
            Some(To::Workbench) => Some(TO_WORKBENCH),
            Some(To::Reports) => Some(TO_REPORTS),
            Some(To::Guide) => Some(TO_GUIDE),
            Some(To::Updates) => Some(TO_UPDATES),
            None => None,
        };
        if let Some(message) = message {
            let _ = unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)) };
        }
        // (The guide brings the window up itself as it comes up, under its dialog.)
        if to == Some(To::Guide) {
            return;
        }
        return show(None);
    }
    if OPENING.swap(true, Ordering::AcqRel) {
        crate::perf::mark(|| "settings asked while being made: left".into());
        return;
    }
    // (Where it was asked from told at once, not as its thread comes up: what it was asked from, looking meanwhile,
    // would find it asked from nowhere and forget it.)
    *ASKED_FROM.lock().unwrap() = asked.place;
    let mut kept = THREAD.lock().unwrap();
    crate::perf::mark(|| format!("settings asked of its thread, {}", match &*kept { Kept::None => "none", Kept::Starting(_) => "starting", Kept::Running(_) => "running" }));
    match &mut *kept {
        // (Its thread going: told. Starting: told as it starts. None: one started, asked.)
        Kept::Running(thread) if unsafe { windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(*thread, AGAIN, WPARAM(To::told(to)), LPARAM(super::genie::told(asked.place))) }.is_ok() => {}
        Kept::Starting(meanwhile) => *meanwhile = Some(asked),
        _ => {
            *kept = Kept::None;
            if !start(&mut kept, Some(asked)) {
                OPENING.store(false, Ordering::Release);
                *ASKED_FROM.lock().unwrap() = None;
            }
        }
    }
}

/// The window's thread started, where none is (or is starting), its graphics device made, for the window to come up
/// at once as it is opened (what takes longest as it is made, a good part of a tenth of a second): the gear under
/// the pointer. Kept a while (see `KEPT_FOR`).
pub fn prepare() {
    let mut kept = THREAD.lock().unwrap();
    if matches!(*kept, Kept::None) && hwnd().is_none() {
        start(&mut kept, None);
    }
}

/// The window's thread started, asked for the window (`asked`, or with none only made ready): whether it was.
fn start(kept: &mut Kept, asked: Option<Asked>) -> bool {
    *kept = Kept::Starting(None);
    let started = std::thread::Builder::new().name("settings".into()).spawn(move || run(asked)).is_ok();
    if !started {
        *kept = Kept::None;
    }
    started
}

/// The window's thread: its graphics device made; the window made (`asked`, or as it is asked for: see `AGAIN`), and
/// drawn whenever it is due, between the messages that arrive, till it is gone; again as it is asked for, the thread
/// kept a while (`KEPT_FOR`) with no window up, then gone, its device with it.
fn run(mut asked: Option<Asked>) {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let _ending = Ending;
    // (Its message queue made before it is named: told at once, it has one to be told in.)
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, windows::Win32::UI::WindowsAndMessaging::PM_NOREMOVE);
    }
    {
        let mut kept = THREAD.lock().unwrap();
        if let Kept::Starting(meanwhile) = &mut *kept {
            asked = asked.or(meanwhile.take());
        }
        *kept = Kept::Running(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() });
    }
    crate::perf::mark(|| "settings thread".into());
    // (The device for the screen the window is to open on, at the pointer, made meanwhile.)
    let _ = crate::panel::work_area_at_cursor().map(|(work, _)| gfx::for_area(work));
    crate::perf::mark(|| "settings device".into());
    loop {
        let to = match asked.take() {
            Some(to) => to,
            None => match told(&mut msg) {
                Some(to) => to,
                None => return,
            },
        };
        let Some(again) = up(to) else { return };
        // Closed: what its device holds given back, and given back again once the card is done with what the window
        // let go of (see `Gfx::trim_when_idle`, on the timer its thread keeps while it is kept).
        // (Its devices kept only where a screen would be drawn on with one: see `gfx::let_go_of_strays`.)
        gfx::let_go_of_strays();
        for gfx in gfx::of_thread() {
            gfx.trim_now_and_again();
        }
        crate::perf::mark(|| "settings gone".into());
        asked = again;
    }
}

/// What the window's thread is told to do with no window up, its messages seen to (`msg` to take them in) till it is:
/// the window made, as it is asked for (see `AGAIN`). None, told of nothing for `KEPT_FOR` (the thread no longer kept:
/// asked for meanwhile, another is started), or told to end.
fn told(msg: &mut MSG) -> Option<Asked> {
    let until = Instant::now() + KEPT_FOR;
    loop {
        while unsafe { PeekMessageW(msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return None;
            }
            if msg.message == AGAIN && msg.hwnd.is_invalid() {
                return Some(Asked::of_told(msg));
            }
            unsafe {
                let _ = TranslateMessage(msg);
                DispatchMessageW(msg);
            }
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // (No longer told of: what was told before it is not, seen to now. Told to end meanwhile, which comes
            // whatever is looked for, it ends.)
            let mut kept = THREAD.lock().unwrap();
            if unsafe { PeekMessageW(msg, None, AGAIN, AGAIN, PM_REMOVE) }.as_bool() && msg.message == AGAIN {
                return Some(Asked::of_told(msg));
            }
            if matches!(*kept, Kept::Running(thread) if thread == unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }) {
                *kept = Kept::None;
            }
            return None;
        }
        let ms = left.as_micros().div_ceil(1000).min(u32::MAX as u128) as u32;
        unsafe { MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT) };
    }
}

/// The window made, as it is `asked` for, and up till it is gone. Its thread to go on (not told to end, the window
/// made), with the window asked for again meanwhile, as it went (see `AGAIN`), if it was.
fn up(asked: Asked) -> Option<Option<Asked>> {
    crate::perf::mark(|| "settings to make".into());
    // (Before the window is: shown, it tells the widgets, and the one it was asked from shows so at once.)
    *ASKED_FROM.lock().unwrap() = asked.place;
    // (Drawn with the device for the screen it opens on, at the pointer: see `make`.)
    let made = crate::panel::work_area_at_cursor().and_then(|(work, _)| gfx::for_area(work).ok()).and_then(|gfx| make(gfx, Some(asked.place), asked.to));
    crate::perf::mark(|| "settings made".into());
    if made.is_none() {
        *ASKED_FROM.lock().unwrap() = None;
    }
    if let Some(hwnd) = made {
        WINDOW.store(hwnd.0 as isize, Ordering::Release);
        // (What plays, looked at only while something on the screen can show it: at once, for the previews.)
        crate::playing::look_now();
    }
    OPENING.store(false, Ordering::Release);
    // (Not made: the thread ends, and what it made of it with it.)
    made?;
    let mut msg = MSG::default();
    let mut again = None;
    // What the next frame is waited on with, to the moment it is due: a wait of whole milliseconds rounds to the
    // system's tick (15.6 ms), and a frame asked for every 15 ms came every 31.
    let frame_timer = unsafe {
        CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
            .or_else(|_| CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0))
            .ok()
            .map(|timer| windows_core::Owned::new(timer))
    };
    loop {
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return None;
            }
            // (Asked for as it went, its window destroyed and its thread not done with it: made again after.)
            if msg.message == AGAIN && msg.hwnd.is_invalid() {
                again = Some(Asked::of_told(&msg));
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // Gone (closed).
        let Some(due) = tick(Instant::now()) else { return Some(again) };
        if !due.is_zero() {
            let ms = due.as_micros().div_ceil(1000).min(u32::MAX as u128) as u32;
            match frame_timer.as_deref().copied().filter(|_| due < Duration::from_secs(1)) {
                Some(timer) => unsafe {
                    let _ = SetWaitableTimer(timer, &-((due.as_nanos() / 100) as i64).max(1), 0, None, None, false);
                    MsgWaitForMultipleObjects(Some(&[timer]), false, ms, QS_ALLINPUT);
                },
                None => unsafe {
                    MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT);
                },
            }
        }
    }
}

/// The window's thread ending, however it ends: what it drew with let go of first, its window's, then its device (see
/// `gfx::let_go`), not left to the thread's own end; no longer kept.
struct Ending;

impl Drop for Ending {
    fn drop(&mut self) {
        // (Still kept, it was told to end, or the window not made: asked for meanwhile, it is not made now, and may be
        // asked for again.)
        let mut kept = THREAD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*kept, Kept::Running(thread) if thread == unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }) {
            *kept = Kept::None;
            OPENING.store(false, Ordering::Release);
        }
        drop(kept);
        drop(UI.with(|ui| ui.borrow_mut().take()));
        gfx::let_go();
        crate::perf::mark(|| "settings thread gone".into());
    }
}

/// The overlay's page, turned to.
fn to_overlay() {
    with_ui(|ui| ui.turn_as_asked(To::Overlay));
}

fn to_workbench() {
    with_ui(|ui| ui.turn_as_asked(To::Workbench));
}

/// The page of the reports sent, turned to, as they are now.
fn to_reports() {
    with_ui(|ui| ui.turn_as_asked(To::Reports));
}

/// Takes up settings changed elsewhere (the panel's bar, the overlay's menu, a drag), from any thread: an open window
/// shows them, and saves on top of them (told on its own thread).
pub fn follow_settings() {
    if let Some(hwnd) = hwnd() {
        let _ = unsafe { PostMessageW(Some(hwnd), FOLLOW, WPARAM(0), LPARAM(0)) };
    }
}

fn take_settings() {
    // (Owed outside the window's state: told while a dialog of it is up, which holds that state, it was dropped.)
    FOLLOW_OWED.store(true, Ordering::Release);
    with_ui(|ui| {
        ui.next_frame = Instant::now();
        ui.follow_when_free();
    });
}

/// `settings` as the window takes them, their preferences as it resolved them (`prefs`) and writes them back (see
/// `prefs::kept`): what it changes is then only what was changed in it. (Taken as they were, preferences never yet
/// written were none, and the first change of any put the window's in place of all of them, over one made
/// elsewhere.) With them, what earlier versions kept and the window does not write again (carried over into
/// `prefs`): as the window writes without it, it goes, and is not carried over again over what was changed since.
fn taken(settings: &Settings, prefs: &Prefs) -> Settings {
    fn over(held: &serde_json::Value, written: serde_json::Value) -> serde_json::Value {
        match (held, written) {
            (serde_json::Value::Object(held), serde_json::Value::Object(written)) => {
                let mut both = held.clone();
                for (key, value) in written {
                    let under = held.get(&key).cloned().unwrap_or(serde_json::Value::Null);
                    both.insert(key, over(&under, value));
                }
                serde_json::Value::Object(both)
            }
            (_, written) => written,
        }
    }
    let mut taken = settings.clone();
    taken.view = over(&settings.view, crate::ui::prefs::kept(prefs, &settings.view));
    taken
}

/// Whether Glance starts at sign-in and the phone remote is on, as the window last read them: shown so as it opens,
/// while they are read again (read before it showed, its opening waited on Task Scheduler).
static SWITCHES_READ: std::sync::Mutex<Option<(bool, bool)>> = std::sync::Mutex::new(None);

/// Settings changed elsewhere, to be taken up by the window once nothing is in hand (see `follow_when_free`).
static FOLLOW_OWED: AtomicBool = AtomicBool::new(false);
/// The overlay's page asked for, to be turned to (see `open_at_overlay`); kept as `FOLLOW_OWED` is.
static OVERLAY_OWED: AtomicBool = AtomicBool::new(false);
static WORKBENCH_OWED: AtomicBool = AtomicBool::new(false);
/// The screens changed, to be read again by the window (see `tick`): told outside its state, which a dialog of it
/// may hold.
static SCREENS_OWED: AtomicBool = AtomicBool::new(false);
/// The wallpaper changed, to be read again by the window: told as `SCREENS_OWED` is.
static WALLPAPER_OWED: AtomicBool = AtomicBool::new(false);
/// Whether the window was minimized as it was last sized (see WM_SIZE).
static MINIMIZED: AtomicBool = AtomicBool::new(false);

/// Draws the window if a frame is due, and says when the next one is; `None`
/// while there is no window.
fn tick(now: Instant) -> Option<Duration> {
    // Pouring out of its place, or into it: the next frame of that, the window shown as it has poured out, or closed as
    // it has poured in (outside its state, which its closing takes).
    let poured = with_ui(|ui| {
        let genie = ui.genie.as_mut()?;
        if genie.draw(now) {
            return Some(None);
        }
        let opened = genie.opening();
        if opened {
            super::genie::reveal(ui.hwnd);
            let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
        }
        let place = genie.place();
        ui.genie = None;
        Some(Some((opened, ui.hwnd, place)))
    })
    .flatten();
    match poured {
        Some(None) => return Some(Duration::ZERO),
        Some(Some((false, hwnd, place))) => {
            // (Before the window goes, which tells what they poured into: it bounces as it is drawn again.)
            *LANDED.lock().unwrap() = Some((place, Instant::now()));
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            return with_ui(|_| Duration::ZERO);
        }
        _ => {}
    }
    with_ui(|ui| {
        // The screens or the wallpaper changed (see WM_DISPLAYCHANGE and WM_SETTINGCHANGE): as they are now.
        let (screens, wallpaper) = (SCREENS_OWED.swap(false, Ordering::AcqRel), WALLPAPER_OWED.swap(false, Ordering::AcqRel));
        if screens || wallpaper {
            ui.desktops_changed(screens, wallpaper);
        }
        // What the window of a report's thread told.
        let told = THREAD_OWED.swap(0, Ordering::AcqRel);
        if told & THREAD_SEND != 0 {
            ui.send_in_thread();
        }
        if told & THREAD_TURN != 0 {
            ui.turn_in_thread();
        }
        if told & THREAD_CLOSE != 0 {
            ui.close_thread();
        }
        // Minimized, nothing is drawn (it is drawn whole as it is restored: see WM_SIZE), and nothing is looked at
        // again till a message comes (restored, it is told so).
        if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(ui.hwnd) }.as_bool() {
            return Duration::MAX;
        }
        if ui.relabel_at.is_some_and(|at| now >= at) {
            ui.relabel();
        }
        ui.follow_when_free();
        // Wallpaper thumbnails arriving on the Appearance page: repaint.
        if ui.page == Page::Appearance {
            let seen = crate::station::thumbs::changes();
            if seen != ui.theme_thumbs {
                ui.next_frame = now;
            }
        }
        // (The window left as it is under the guide's dialog, the guide alone drawn.)
        ui.take_in();
        let held = ui.guide_holds_window();
        if now >= ui.next_frame && !held {
            ui.draw(now);
        } else if now >= ui.guide_frame {
            ui.draw_guide(now);
        }
        // (Drawn or not, the desktops wanted read: see `draw`.)
        ui.read_wanted();
        let next = if held { ui.guide_frame } else { ui.next_frame.min(ui.guide_frame) };
        next.saturating_duration_since(now)
    })
}

pub(crate) fn windows_build() -> u32 {
    metrics::reg_string(w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"), w!("CurrentBuild")).parse().unwrap_or(0)
}

/// The switches turned on another thread (see `Ui::switching`).
#[derive(Clone, Copy)]
enum SwitchAsked {
    Startup,
    Remote,
}

/// Reads the desktops of lot `lot` (see `reads::Reads::lot`) as `wallpaper::desktops` reads them, a live wallpaper
/// printed once for them all, the preview's kept (see `DESKTOPS`), and sends each as it is read, then that the lot has
/// all come. On a thread of the caller's (which joins the process's apartment for the decoders it uses).
fn read_lot(lot: u64, asks: Vec<reads::Ask>, send: std::sync::mpsc::Sender<Read>) {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let areas: Vec<(RECT, u32, u32)> = asks.iter().map(reads::Ask::area).collect();
    for (ask, desktop) in asks.into_iter().zip(wallpaper::desktops(areas)) {
        if let reads::Ask::Stage { screen, work, size } = &ask {
            keep_desktop(screen, *work, *size, &desktop);
        }
        if send.send(Read::Came { lot, ask, desktop }).is_err() {
            return;
        }
    }
    let _ = send.send(Read::Done(lot));
}

/// Each screen's desktop as the preview last read it (its name, its work area, the size it was read at): kept for
/// as long as Glance runs, so that the settings open on the desktop as it was (read again as they open, and taken
/// if it has changed), not on an empty stage while it is read. Small: read at the preview's size, in DIPs.
static DESKTOPS: std::sync::Mutex<Vec<(String, RECT, (f32, f32), Capture)>> = std::sync::Mutex::new(Vec::new());

/// Keeps `desktop`, screen `screen`'s as read at `work` and `size` (see `DESKTOPS`).
fn keep_desktop(screen: &str, work: RECT, size: (f32, f32), desktop: &Capture) {
    if let Some(copy) = desktop.copied() {
        let mut kept = DESKTOPS.lock().unwrap();
        kept.retain(|(name, ..)| name != screen);
        kept.push((screen.to_string(), work, size, copy));
    }
}

/// Screen `screen`'s desktop as last read for the preview, if it was read at its work area `work` and `size`.
fn kept_desktop(screen: &str, work: RECT, size: (f32, f32)) -> Option<Capture> {
    DESKTOPS.lock().unwrap().iter().find(|(name, at, read, _)| name == screen && *at == work && *read == size).and_then(|(.., desktop)| desktop.copied())
}

/// Every screen as the preview shows it: its name, its work area (physical px), and that area's size (DIPs).
fn stages_wanted(screens: &[crate::screens::Screen]) -> Vec<(String, RECT, (f32, f32))> {
    screens
        .iter()
        .filter_map(|known| {
            let m = known.monitor;
            let contact = crate::mouse::monitor_at(POINT { x: (m.left + m.right) / 2, y: (m.top + m.bottom) / 2 })?;
            let work = contact.work;
            let size = (((work.right - work.left) as f32 / contact.scale).round(), ((work.bottom - work.top) as f32 / contact.scale).round());
            Some((known.id.clone(), work, size))
        })
        .collect()
}

/// Reads every screen's desktop for the preview and keeps it, before the settings are first opened: they open on
/// it. On a thread of the caller's.
pub fn read_desktops_ahead() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    // (A live wallpaper printed once for them all: see `wallpaper::desktops`.)
    let wanted = stages_wanted(&crate::screens::all());
    let asked: Vec<(RECT, u32, u32)> = wanted.iter().map(|(_, work, size)| (*work, size.0 as u32, size.1 as u32)).collect();
    for ((screen, work, size), desktop) in wanted.iter().zip(wallpaper::desktops(asked)) {
        keep_desktop(screen, *work, *size, &desktop);
    }
}

/// What a lot of desktops read on another thread for the settings brings (see `read_lot`): each desktop as it is read,
/// for the preview or for the map, then that the lot has all come.
enum Read {
    Came { lot: u64, ask: reads::Ask, desktop: Capture },
    Done(u64),
}

/// How the phone remote is, as its helper says (see `Ui::remote_asked`), a code asked of it with what for (none
/// asked: none), and the phones paired as it then has them.
struct RemoteAnswer {
    lights: bool,
    raised: bool,
    screens: bool,
    address: Option<String>,
    code: Option<Option<(crate::phone::Code, CodeFor)>>,
    phones: Vec<crate::phone::Phone>,
}

/// What the diagnostics are put together for.
#[derive(Clone, Copy)]
enum ReportFor {
    Copy,
    Send,
    /// To go with what the user adds to a report they sent (see `Ui::adding_asked`).
    Add,
}

/// The window's own colours: Windows 11's, whatever skin the panel wears.
#[derive(Clone)]
struct Palette {
    text: Color,
    text2: Color,
    text3: Color,
    rule: Color,
    hover: Color,
    control: Color,
    control_on: Color,
    /// The chosen option's ring, and its drop shadow (none in dark).
    control_ring: Color,
    control_drop: Option<Color>,
    switch_off: Color,
    switch_stroke: Color,
    switch_knob: Color,
    window: Color,
    card: Color,
    card_stroke: Color,
    /// A switch that does nothing for now: its outline or track, and its
    /// knob when on.
    disabled: Color,
    disabled_knob: Color,
    /// The accent in the shade for this theme, and the deeper one switches
    /// take in either (the pale dark-mode shade cannot carry a white knob).
    selection: Color,
    switch_on: Color,
    /// The ring of a slider's thumb.
    switch_knob_ring: Color,
}

impl Palette {
    fn new(dark: bool) -> Self {
        let (on, off) = (theme::accent(false), theme::accent(dark));
        let white = |a| Color::hex(0xFFFFFF, a);
        let black = |a| Color::hex(0x000000, a);
        if dark {
            Palette {
                text: white(1.0),
                text2: white(0.76),
                text3: white(0.5),
                rule: white(0.08),
                hover: white(0.06),
                control: white(0.06),
                control_on: white(0.16),
                control_ring: white(0.06),
                control_drop: None,
                switch_off: black(0.1),
                switch_stroke: white(0.6),
                switch_knob: white(0.8),
                window: Color::hex(0x202020, 1.0),
                card: white(0.05),
                card_stroke: black(0.2),
                disabled: white(0.16),
                disabled_knob: white(0.53),
                selection: off,
                switch_on: on,
                switch_knob_ring: Color::hex(0x454545, 1.0),
            }
        } else {
            Palette {
                text: black(0.9),
                text2: black(0.6),
                text3: black(0.44),
                rule: black(0.08),
                hover: black(0.04),
                control: black(0.05),
                control_on: white(0.95),
                control_ring: black(0.06),
                control_drop: Some(black(0.08)),
                switch_off: black(0.03),
                switch_stroke: black(0.6),
                switch_knob: black(0.6),
                window: Color::hex(0xF3F3F3, 1.0),
                card: white(0.7),
                card_stroke: black(0.06),
                disabled: black(0.22),
                disabled_knob: white(1.0),
                selection: on,
                switch_on: on,
                switch_knob_ring: white(1.0),
            }
        }
    }
}

/// The choices of module `id`'s own, shown in its card.
fn module_fields(id: &str) -> &'static [Field] {
    match id {
        "network" => &[Field::RateUnit, Field::PingTarget],
        "processes" => &[Field::ProcessCount],
        "station" => &[Field::StationKeep, Field::StationCopy, Field::StationAfterDrag],
        _ => &[],
    }
}

/// The drives Recent files' rows offer (see `Row::Drive`), by their letters: `drives`, those it reads now, and those
/// of `hidden` (left out) that are away, as they were last seen, to be listed again.
fn drives_offered(drives: &[crate::station::recent::Volume], hidden: &[crate::settings::HiddenDrive]) -> Vec<crate::station::recent::Volume> {
    let away = hidden.iter().filter(|hidden| !drives.iter().any(|drive| drive.volume == hidden.volume));
    let mut offered: Vec<crate::station::recent::Volume> = drives.iter().cloned().chain(away.map(|hidden| crate::station::recent::Volume { letter: hidden.letter, volume: hidden.volume.clone(), label: hidden.label.clone() })).collect();
    offered.sort_by_key(|drive| drive.letter);
    offered
}

/// The drive of `volume` listed by Recent files again, where it is among those left out (`hidden`), or else left
/// out, kept with its label and letter (as `drives`, those read now, have them) to name it while it is away.
fn turn_drive(hidden: &mut Vec<crate::settings::HiddenDrive>, drives: &[crate::station::recent::Volume], volume: &str) {
    if hidden.iter().any(|drive| drive.volume == volume) {
        hidden.retain(|drive| drive.volume != volume);
    } else if let Some(drive) = drives.iter().find(|drive| drive.volume == volume) {
        hidden.push(crate::settings::HiddenDrive { volume: drive.volume.clone(), label: drive.label.clone(), letter: drive.letter });
    }
}

/// When the overlay shows, as its choices offer it.
const OVERLAY_SHOWN: [OverlayShown; 3] = [OverlayShown::Off, OverlayShown::InGame, OverlayShown::Always];

/// The days the station keeps what is not pinned, as its settings offer them.
const STATION_DAYS: [u32; 3] = [1, 7, 30];

/// The settings, a group to a page: what each screen edge opens and how; how every component looks; each component's
/// own (the monitor's on tabs of their own, the workbench's, what plays); the phone; and Glance's as a whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Layout,
    Look,
    Monitor,
    Workbench,
    /// The dock: its tiles, its edge, and how its glass is drawn.
    Dock,
    /// The launchpad: how its page of programs lies.
    Launchpad,
    Music,
    Phone,
    // ---- System pages (OS category) ----
    /// A page of the machine's own settings: its date, time and zone.
    DateTime,
    /// The system's own look: its theme, mode, animation, transparency and accent.
    Appearance,
    /// Wi-Fi: scanning, connecting, known networks.
    Wifi,
    /// Bluetooth: adapters, paired devices, nearby scan.
    Bluetooth,
    /// The machine's pages Glance opens where Windows keeps them (see `Row::Do`).
    Display,
    Sound,
    Network,
    /// Power & battery: plan, sleep, screen timeout.
    Power,
    /// Focus / Do Not Disturb.
    Focus,
    Notifications,
    /// Screen time: daily usage per app.
    ScreenTime,
    /// Keyboard & mouse settings.
    Input,
    /// Accessibility: contrast, text size, cursor.
    Accessibility,
    Storage,
    /// Control centre / quick settings.
    ControlCenter,
    /// Desktop & taskbar settings.
    Desktop,
    /// Wallpaper picker.
    Wallpaper,
    Privacy,
    /// User account: lock screen, info.
    Account,
    Gaming,
    /// About this PC: device specs.
    About,
    General,
    /// The reports this PC sent, as threads: a page of General's, opened from it (see `Row::MyReports`).
    Reports,
    // ---- The 通用 hub's sub-items (see `Page::System`) ----
    /// The 通用 hub itself: a macOS page of sub-items, each showing its preview on the right
    /// without turning to it (see `Target::Preview`).
    System,
    /// Software update, as the hub names it: what WindowsUpdate's page keeps, told as macOS does.
    Update,
    /// Login items: what starts of its own accord at sign-in.
    Startup,
    /// Accessibility, as the hub names it: `Accessibility`'s page under its macOS word.
    Access,
    /// Language & region: the formats and the languages the machine keeps.
    Language,
    /// Nearby sharing: AirDrop and Handoff's Windows kin.
    Nearby,
    /// Sharing: the network and sharing centre.
    Sharing,
    /// Backup: file history, whether it is set.
    Backup,
    /// Recovery: the startup disk, and the environment to boot into.
    Recovery,
}

/// The monitor's settings, a group to a tab: what it shows, how it reads and warns, and its overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Shown,
    Data,
    Overlay,
}

const TABS: [Tab; 3] = [Tab::Shown, Tab::Data, Tab::Overlay];

impl Tab {
    fn name(self, lang: Lang) -> &'static str {
        match self {
            Tab::Shown => pick(lang, "显示内容", "Content"),
            Tab::Data => pick(lang, "数据与提醒", "Data and alerts"),
            Tab::Overlay => pick(lang, "悬浮窗", "Overlay"),
        }
    }
}

/// The pages' list as it reads: each page, and over the components' own pages their heading (a name, not a page).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NavItem {
    Page(Page),
    Components,
    /// The heading over the connecting pages: Wi-Fi, Bluetooth, network.
    Connect,
    /// The heading over the machine's own system pages.
    System,
    /// The heading over the personalisation pages.
    Personalise,
    /// The heading over the input and accessibility pages.
    Ease,
    /// The heading over the storage and privacy pages.
    Secure,
    /// The heading over the accounts, updates and about pages.
    More,
}

const NAV_ITEMS: [NavItem; 37] = [
    NavItem::Page(Page::Layout),
    NavItem::Page(Page::Look),
    NavItem::Components,
    NavItem::Page(Page::Monitor),
    NavItem::Page(Page::Workbench),
    NavItem::Page(Page::Dock),
    NavItem::Page(Page::Launchpad),
    NavItem::Page(Page::Music),
    NavItem::Page(Page::Phone),
    NavItem::Page(Page::General),
    NavItem::Connect,
    NavItem::Page(Page::Wifi),
    NavItem::Page(Page::Bluetooth),
    NavItem::Page(Page::Network),
    NavItem::System,
    NavItem::Page(Page::Display),
    NavItem::Page(Page::Sound),
    NavItem::Page(Page::Power),
    NavItem::Page(Page::Notifications),
    NavItem::Page(Page::Focus),
    NavItem::Page(Page::ScreenTime),
    NavItem::Personalise,
    NavItem::Page(Page::Appearance),
    NavItem::Page(Page::Wallpaper),
    NavItem::Page(Page::Desktop),
    NavItem::Ease,
    NavItem::Page(Page::Input),
    NavItem::Page(Page::Accessibility),
    NavItem::Secure,
    NavItem::Page(Page::Storage),
    NavItem::Page(Page::Privacy),
    NavItem::More,
    NavItem::Page(Page::Account),
    NavItem::Page(Page::System),
    NavItem::Page(Page::Gaming),
    NavItem::Page(Page::DateTime),
    NavItem::Page(Page::ControlCenter),
];

impl NavItem {
    fn name(self, lang: Lang) -> &'static str {
        match self {
            NavItem::Page(Page::Layout) => pick(lang, "布局", "Layout"),
            NavItem::Page(Page::Look) => pick(lang, "外观", "Appearance"),
            NavItem::Components => pick(lang, "组件", "Components"),
            NavItem::Connect => pick(lang, "网络与连接", "Network & connect"),
            NavItem::Personalise => pick(lang, "个性化", "Personalisation"),
            NavItem::Ease => pick(lang, "输入与辅助", "Input & accessibility"),
            NavItem::Secure => pick(lang, "存储与隐私", "Storage & privacy"),
            NavItem::More => pick(lang, "账户与更多", "Accounts & more"),
            NavItem::Page(Page::Monitor) => pick(lang, "监控", "Monitor"),
            NavItem::Page(Page::Workbench) => pick(lang, "工作台", "Workbench"),
            NavItem::Page(Page::Dock) => "Dock",
            NavItem::Page(Page::Launchpad) => pick(lang, "启动台", "Launchpad"),
            NavItem::Page(Page::Music) => pick(lang, "音乐", "Music"),
            NavItem::Page(Page::Phone) => pick(lang, "手机互联", "Phone"),
            NavItem::Page(Page::DateTime) => pick(lang, "时间与语言", "Time & Language"),
            NavItem::Page(Page::Appearance) => pick(lang, "系统外观", "System Appearance"),
            NavItem::Page(Page::General | Page::Reports) => pick(lang, "Glance", "Glance"),
            NavItem::System => pick(lang, "系统", "System"),
            NavItem::Page(Page::Display) => pick(lang, "显示", "Display"),
            NavItem::Page(Page::Sound) => pick(lang, "声音", "Sound"),
            NavItem::Page(Page::Network) => pick(lang, "网络和 Internet", "Network & internet"),
            NavItem::Page(Page::Notifications) => pick(lang, "通知", "Notifications"),
            NavItem::Page(Page::Storage) => pick(lang, "存储", "Storage"),
            NavItem::Page(Page::Privacy) => pick(lang, "隐私和安全性", "Privacy & security"),
            NavItem::Page(Page::Gaming) => pick(lang, "游戏", "Gaming"),
            NavItem::Page(Page::Wifi) => pick(lang, "WLAN", "Wi-Fi"),
            NavItem::Page(Page::Bluetooth) => pick(lang, "蓝牙", "Bluetooth"),
            NavItem::Page(Page::Power) => pick(lang, "电源与电池", "Power & battery"),
            NavItem::Page(Page::Focus) => pick(lang, "专注模式", "Focus"),
            NavItem::Page(Page::ScreenTime) => pick(lang, "屏幕使用时间", "Screen time"),
            NavItem::Page(Page::Input) => pick(lang, "键盘和鼠标", "Keyboard & mouse"),
            NavItem::Page(Page::Accessibility) => pick(lang, "辅助功能", "Accessibility"),
            NavItem::Page(Page::Desktop) => pick(lang, "桌面和任务栏", "Desktop & taskbar"),
            NavItem::Page(Page::Wallpaper) => pick(lang, "个性化壁纸", "Wallpaper"),
            NavItem::Page(Page::ControlCenter) => pick(lang, "快速设置", "Quick settings"),
            NavItem::Page(Page::Account) => pick(lang, "账户", "Accounts"),
            NavItem::Page(Page::System) => pick(lang, "通用", "General"),
            NavItem::Page(Page::Update) => pick(lang, "软件更新", "Software Update"),
            NavItem::Page(Page::Startup) => pick(lang, "登录项", "Login Items"),
            NavItem::Page(Page::Access) => pick(lang, "辅助功能", "Accessibility"),
            NavItem::Page(Page::Language) => pick(lang, "语言与地区", "Language & Region"),
            NavItem::Page(Page::Nearby) => pick(lang, "隔空投送与接力（就近共享）", "AirDrop & Handoff (nearby sharing)"),
            NavItem::Page(Page::Sharing) => pick(lang, "共享", "Sharing"),
            NavItem::Page(Page::Backup) => pick(lang, "时间机器（Windows 备份）", "Time Machine (Windows Backup)"),
            NavItem::Page(Page::Recovery) => pick(lang, "启动磁盘（恢复）", "Startup Disk (recovery)"),
            NavItem::Page(Page::About) => pick(lang, "关于本机", "About this PC"),
        }
    }

}

impl Page {
    fn icon(self) -> Icon {
        match self {
            Page::Layout => Icon::LayoutGrid,
            Page::Look => Icon::Palette,
            Page::Monitor => Icon::Gauge,
            Page::Workbench => Icon::PencilRuler,
            Page::Dock => Icon::ArrowDownToLine,
            Page::Launchpad => Icon::Grid3x3,
            Page::Music => Icon::Music,
            Page::Phone => Icon::Smartphone,
            Page::DateTime => Icon::Clock,
            Page::Appearance => Icon::Brush,
            Page::Display => Icon::AppWindow,
            Page::Sound => Icon::Volume,
            Page::Network => Icon::Network,
            Page::Notifications => Icon::MessageCircle,
            Page::Storage => Icon::Disk,
            Page::Privacy => Icon::Shield,
            Page::Gaming => Icon::Game,
            Page::Wifi => Icon::Wifi,
            Page::Bluetooth => Icon::Bluetooth,
            Page::Power => Icon::Battery,
            Page::Focus => Icon::Moon,
            Page::ScreenTime => Icon::Timer,
            Page::Input => Icon::Keyboard,
            Page::Accessibility => Icon::Eye,
            Page::Desktop => Icon::LayoutDashboard,
            Page::Wallpaper => Icon::Image,
            Page::ControlCenter => Icon::Sliders,
            Page::Account => Icon::User,
            Page::About => Icon::Info,
            Page::General | Page::Reports => Icon::Settings,
            Page::System => Icon::Settings,
            Page::Update => Icon::Update,
            Page::Startup => Icon::ArrowDownToLine,
            Page::Access => Icon::CircleDot,
            Page::Language => Icon::Sliders,
            Page::Nearby => Icon::Network,
            Page::Sharing => Icon::Link,
            Page::Backup => Icon::Folder,
            Page::Recovery => Icon::Wrench,
        }
    }

    fn name(self, lang: Lang) -> &'static str {
        match self {
            Page::Reports => pick(lang, "我的反馈", "My reports"),
            page => NavItem::Page(page).name(lang),
        }
    }

    /// The 通用 hub's own way of naming a sub-item: the macOS word for the pages
    /// that live only in its right-hand pane (see `Page::System`).
    fn hub_name(self, lang: Lang) -> &'static str {
        match self {
            Page::Update => pick(lang, "软件更新", "Software Update"),
            Page::Startup => pick(lang, "登录项", "Login Items"),
            Page::Access => pick(lang, "辅助功能", "Accessibility"),
            Page::Language => pick(lang, "语言与地区", "Language & Region"),
            Page::Nearby => pick(lang, "隔空投送与接力", "AirDrop & Handoff"),
            Page::Sharing => pick(lang, "共享", "Sharing"),
            Page::Backup => pick(lang, "时间机器", "Time Machine"),
            Page::Recovery => pick(lang, "启动磁盘", "Startup Disk"),
            page => page.name(lang),
        }
    }
}

/// A row of choices, one of which is picked.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Field {
    Theme,
    Language,
    Anchor,
    Columns,
    OverFullscreen,
    Push,
    CloseDelay,
    RateUnit,
    /// Where the network's latency is measured to (see `prefs::PingTarget`).
    PingTarget,
    /// What the music widgets are drawn on (see `prefs::MusicGround`).
    MusicGround,
    ProcessCount,
    Interval,
    Span,
    TempAlert,
    /// Set on a slider (see `slider`).
    OverlaySize,
    OverlayClear,
    OverlayLayout,
    OverlayRows,
    /// When the overlay shows: never, while a game plays, or always.
    OverlayShow,
    /// The player music is followed from (see `Ui::sources`).
    MusicSource,
    /// The station's (see `settings::StationSettings`): the folders the files come lately are not listed from (all
    /// listed again: under Recent files' group, see `Ui::recent_rows`), how long what is not pinned stays, files kept
    /// as a copy too, and what is dragged off it taken off. (What is copied kept, and the files come lately listed, are
    /// its items: see `Ui::items_here`.)
    StationHidden,
    StationKeep,
    StationCopy,
    StationAfterDrag,
    /// What Recent files lists from: the drives, each on or off, or only the folders chosen, whatever drive they are
    /// on (see `Settings::recent_only`). Each kept as the other is chosen.
    RecentScope,
    /// The dock's edge it stands along (see `settings::DockEdge`).
    DockEdge,
    /// The dock's, on a slider: a tile's side and the room between them (DIPs), the swell as itself, and how much of
    /// its glass is gone.
    DockSize,
    DockSpacing,
    DockPeak,
    DockClear,
    /// The launchpad's grid, slid: tiles across a page, and rows of them, 0 as the screen chooses (see `crate::launchpad`).
    PadCols,
    PadRows,
    /// The system's light/dark mode.
    SystemTheme,
    /// The accent colour.
    Accent,
    // ---- System settings pages (OS category) ----
    /// The power plan: best performance, balanced, power saver.
    PowerPlan,
    /// The screen's turn-off delay, slid in minutes.
    ScreenOff,
    /// The machine's sleep delay, slid in minutes.
    Sleep,
    /// A key's hold before it repeats, slid on Windows' 0-3 scale.
    KeyDelay,
    /// How fast a key repeats, slid on Windows' 0-31 scale.
    KeyRate,
    /// The pointer's speed, slid 0-20.
    MouseSpeed,
    /// The wheel's lines at a notch.
    ScrollLines,
    /// The text scale, slid in per cent.
    TextSize,
    /// The pointer's size, slid 1-15.
    CursorSize,
    /// The taskbar's edge.
    TaskbarPosition,
    /// How taskbar buttons combine.
    CombineIcons,
    /// The taskbar's size.
    TaskbarSize,
    /// Whether the taskbar's icons stand against the start's corner or in its middle.
    TaskbarAlign,
    /// How the wallpaper fills the screen.
    WallpaperStyle,
    /// The clock's hours: the 24, or the twelve with which half of the day after them.
    ClockHour,
    /// The tongue the display is worn in, chosen from the packs the machine carries.
    DisplayLanguage,
    /// The short date's pattern.
    ShortDate,
    /// The long date's pattern, the weekday and all.
    LongDate,
    /// The regional format: the locale whose shapes — numbers, money, calendar — the
    /// machine wears.
    RegionFormat,
    /// The country or region the machine stands in.
    Country,
    /// The clock's shape, to the minute or the second, on the 24 or the 12.
    TimePattern,
    /// The zone the clock stands in.
    TimeZone,
}

/// The short date patterns the language page offers, said as Windows keeps them.
const SHORT_DATES: [&str; 6] = ["yyyy/M/d", "yyyy-MM-dd", "M/d/yyyy", "d/M/yyyy", "MM/dd/yyyy", "yyyy'年'M'月'd'日'"];
/// The long date patterns, which carry the weekday too.
const LONG_DATES: [&str; 5] = ["yyyy'年'M'月'd'日'", "yyyy/MM/dd", "yyyy-MM-dd", "dddd, MMMM d, yyyy", "MMMM d, yyyy"];

/// What a row of `switch_card` ends with at its right: the machine's own switch with the
/// word that tells what it does beside it, or a box that opens a list of the words to
/// choose one from.
enum CardEnd {
    Switch(String, bool, bool, Target),
    Picker(Field),
}

/// How a button looks: outlined; filled in the accent (the one thing to
/// press); waiting for keys; or busy, greyed and not to be pressed.
#[derive(Clone, Copy, PartialEq)]
enum Button {
    Plain,
    Accent,
    Taking,
    Quiet,
}

/// A row of buttons: its name, its word, and its buttons, right to left
/// (text, what each does, how it looks).
type ButtonRow = (String, String, Vec<(String, Target, Button)>);

/// How a report sent stands, in a few words.
fn status_words(lang: Lang, status: &crate::reports::Status) -> String {
    use crate::reports::Status;
    match (status, lang) {
        (Status::Open, _) => pick(lang, "待处理", "Open").into(),
        (Status::Answered, _) => pick(lang, "已回复", "Answered").into(),
        (Status::Fixed(version), Lang::Zh) => format!("已在 {version} 修复"),
        (Status::Fixed(version), Lang::En) => format!("Fixed in {version}"),
        (Status::Closed, _) => pick(lang, "已关闭", "Closed").into(),
    }
}

/// When `ts` (ms since 1970) was, as a thread says it: its day and time.
fn thread_time(lang: Lang, ts: u64) -> String {
    let at = crate::ui::text::Local::at(ts);
    format!("{} {}", at.date(lang), at.time(false, lang).0)
}

/// How a report stands, as its thread's window colours it.
fn tone_of(status: &crate::reports::Status) -> super::ask::thread::Tone {
    use super::ask::thread::Tone;
    use crate::reports::Status;
    match status {
        Status::Open => Tone::Open,
        Status::Answered => Tone::Answered,
        Status::Fixed(_) => Tone::Fixed,
        Status::Closed => Tone::Closed,
    }
}

/// A report's thread as its window shows it (see `ask::thread`): its title (the first line of what the user wrote),
/// which report it is and when it was sent, how it stands and what that means for this Glance. Then, in time order,
/// what the user sent with its pictures and the diagnostics, the developer's replies, the user's additions, and each
/// change of its status (but the one to answered that the site makes with a reply, which the reply itself tells).
fn thread_of(lang: Lang, report: &crate::reports::Report) -> super::ask::thread::Thread {
    use super::ask::thread::{Item, Thread};
    use crate::reports::{Kind, Status};
    let attached = pick(lang, "附诊断信息", "With the diagnostics");
    let mut items = vec![Item::Mine { when: thread_time(lang, report.sent), words: report.words.clone(), pictures: crate::reports::pictures(&report.id, 0, report.pictures), attached: attached.into() }];
    let mut before: Option<&crate::reports::Message> = None;
    for message in &report.thread {
        let when = thread_time(lang, message.ts);
        let item = match message.kind {
            Kind::User => Some(Item::Mine {
                when,
                words: message.text.clone(),
                pictures: crate::reports::pictures(&report.id, message.n, message.pictures),
                attached: if message.diagnostics { attached.into() } else { String::new() },
            }),
            Kind::Reply => Some(Item::Theirs { who: pick(lang, "Glance 开发者", "Glance's developer").into(), when, words: message.text.clone() }),
            Kind::Answered if before.is_some_and(|before| before.kind == Kind::Reply && before.ts == message.ts) => None,
            kind => {
                let status = match kind {
                    Kind::Fixed => Status::Fixed(message.version.clone()),
                    Kind::Closed => Status::Closed,
                    Kind::Answered => Status::Answered,
                    _ => Status::Open,
                };
                let words = match (message.mine, &status, lang) {
                    (true, Status::Closed, Lang::Zh) => format!("你关闭了此反馈 · {when}"),
                    (true, Status::Closed, Lang::En) => format!("You closed this report · {when}"),
                    (true, _, Lang::Zh) => format!("你重新打开了此反馈 · {when}"),
                    (true, _, Lang::En) => format!("You reopened this report · {when}"),
                    (false, _, Lang::Zh) => format!("状态改为“{}” · {when}", status_words(lang, &status)),
                    (false, _, Lang::En) => format!("Status set to {} · {when}", status_words(lang, &status)),
                };
                Some(Item::Event { tone: tone_of(&status), words })
            }
        };
        items.extend(item);
        before = Some(message);
    }
    let status = report.status();
    let note = match &status {
        Status::Fixed(version) => match (update::newer(version, env!("CARGO_PKG_VERSION")), lang) {
            (true, Lang::Zh) => format!("当前版本为 {}，更新到 {version} 即可获得此修复。", env!("CARGO_PKG_VERSION")),
            (true, Lang::En) => format!("This is {}. Update to {version} to have the fix.", env!("CARGO_PKG_VERSION")),
            (false, Lang::Zh) => "当前版本已包含此修复。".to_string(),
            (false, Lang::En) => "This version has the fix.".to_string(),
        },
        _ => String::new(),
    };
    let (id, sent) = (&report.id, thread_time(lang, report.sent));
    let title = match (report.first_line(), lang) {
        ("", Lang::Zh) => format!("反馈 {id}"),
        ("", Lang::En) => format!("Report {id}"),
        (first, _) => first.to_string(),
    };
    let about = match lang {
        Lang::Zh => format!("反馈 {id} · {sent} 发送"),
        Lang::En => format!("Report {id} · sent {sent}"),
    };
    Thread { title, about, status: status_words(lang, &status), tone: tone_of(&status), note, items, closable: report.closable() }
}

/// What a report's thread's window says besides the thread.
fn thread_talk(lang: Lang, dark: bool) -> super::ask::thread::Talk<'static> {
    super::ask::thread::Talk {
        hint: pick(lang, "输入补充说明，截图可拖入或粘贴（最多 3 张）", "Add to this report. Pictures may be dropped or pasted here (up to 3)"),
        add: pick(lang, "添加截图", "Add a picture"),
        paste: pick(lang, "粘贴截图", "Paste the picture"),
        kinds: pick(lang, "图片", "Pictures"),
        option: pick(lang, "附上当前的诊断信息", "Attach diagnostics"),
        send: pick(lang, "发送", "Send"),
        close_report: pick(lang, "关闭反馈", "Close report"),
        reopen: pick(lang, "重新打开", "Reopen"),
        close: pick(lang, "关闭", "Close"),
        you: pick(lang, "你", "You"),
        dark,
    }
}

/// A row that is on or off.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Switch {
    Live,
    /// Whether pulling the panel off its edge tears it off as a widget.
    TearOff,
    Startup,
    /// Whether the tray's icon is hidden (see `Settings::tray_hidden`).
    TrayHidden,
    Updates,
    HeatAlert,
    /// The phone remote.
    Remote,
    /// Whether the remote sets the lights: the program that does, fetched or not.
    RemoteLights,
    /// Whether the remote's lights are set with an administrator's rights (what the memory's need).
    RemoteRaised,
    /// Whether the remote puts the screens out with their signal kept.
    RemoteScreens,
    /// The dock: whether it shows, and along it, how it stands and what it tells.
    DockOn,
    DockHide,
    DockLock,
    DockThrough,
    DockMagnify,
    DockRunning,
    DockShowOpen,
    /// Whether the clock shows seconds: the machine's own, read live (see `system_settings`).
    Seconds,
    /// The system's own switches: animation, transparency, accent on borders.
    AnimOn,
    Glass,
    AccentOn,
    // ---- System settings pages (OS category) ----
    /// The wireless radio, on or off.
    WifiOn,
    /// The Bluetooth adapter, enabled or disabled.
    BluetoothOn,
    /// Do Not Disturb, on or off.
    FocusOn,
    /// Whether notices may still show above the lock screen.
    FocusLock,
    /// High contrast, on or off.
    HighContrast,
    /// The taskbar hiding until the pointer comes.
    TaskbarAutohide,
    /// The taskbar's small icons.
    TaskbarSmall,
    /// The lock screen when the PC wakes.
    LockScreen,
    /// Game Mode taking over automatically.
    GameMode,
    /// The Game DVR background recording.
    GameDvr,
}

/// The remote's switches, the lights' program fetched (`lights`) or not, on a laptop or not: the lights set, the
/// memory's with an administrator's rights while that program is there (on no laptop, where the sensors run no
/// OpenRGB with those rights, see `openrgb::runs_on`, and a laptop's memory has no lights), and the screens put out.
fn remote_switches(lights: bool, laptop: bool) -> Vec<Switch> {
    let mut switches = vec![Switch::RemoteLights];
    if lights && !laptop {
        switches.push(Switch::RemoteRaised);
    }
    switches.push(Switch::RemoteScreens);
    switches
}

/// What a QR code shown is for.
#[derive(Clone, Copy, PartialEq)]
enum CodeFor {
    /// Where the phone app is to be had.
    App,
    /// A pairing, for the app to scan.
    Pairing,
    /// A pairing, for a phone's camera: it opens the remote's page.
    WebPairing,
    /// The remote's page, for a phone paired by it before.
    WebPage,
}

/// Which shortcut: the panel's, search's, or the preview's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keys {
    Panel,
    Search,
    Preview,
}

/// What a press lands on.
#[derive(Clone, PartialEq, Debug)]
enum Target {
    Skin(Skin),
    Choice(Field, usize),
    Switch(Switch),
    /// A module's switch, and the rest of its row, which opens and closes
    /// its card.
    Module(String),
    Expand(String),
    /// One of a module's items.
    Item(String, String),
    Grip(String),
    /// A shortcut's keys: pressed, the next combination becomes them.
    Shortcut(Keys),
    /// Clears the shortcut.
    ClearShortcut(Keys),
    /// A slider's track.
    Slider(Field),
    /// A component in the layout's library, to be dragged onto an edge.
    Library(Part),
    /// A component on an edge of a screen (by its name) in the layout's editor: dragged along it, to another, or off;
    /// pressed, the panel that edge opens shows.
    Chip(String, Edge, Part),
    /// The button on it that takes it off the edge.
    Unplace(String, Edge),
    /// An edge of a screen shown large in the editor: pressed, the panel it opens shows.
    Zone(String, Edge),
    /// Every screen small in the editor (none), or the one by its name, large.
    View(Option<String>),
    /// Every screen alike, or each set apart.
    Same,
    /// A page, in the pages' list.
    Nav(Page),
    /// On the 通用 hub: the sub-item whose preview to show on the right, without
    /// turning to its page (see `pick_preview`).
    Preview(Page),
    /// Nearby sharing let on to this user's devices, or off again.
    Nearby(bool),
    /// A restart into the recovery environment (true) or the firmware setup.
    RebootTo(bool),
    /// One of the machine's own login items let run (true) or held, the hive it is
    /// kept in and the name the registry holds it under.
    Startup(bool, String, bool),
    /// One of the monitor's tabs.
    Tab(Tab),
    /// One of what plays shows (see `Row::Element`).
    Element(&'static str),
    /// A drive Recent files reads, by its volume (see `Row::Drive`): listed, or left out.
    Drive(String),
    /// A folder taken off those Recent files lists only, and Windows' chooser opened, to add more.
    RecentFolder(std::path::PathBuf),
    AddRecentFolder,
    /// Opens or closes the fold of the modules few want.
    More,
    /// Asks for a newer release now.
    CheckNow,
    /// Opens the site's download page (see `update::download_page`).
    Download,
    /// The site's notice (see `notice`): read, and the page of the site it links to opened.
    NoticeRead,
    NoticeLink,
    Guide,
    /// Opens one of Windows' own pages (a ms-settings: URI), or any address:
    /// where a setting is the machine's to keep and Glance only leads there.
    Open(String),
    Diagnostics,
    Update,
    Uninstall,
    Quit,
    /// Shows the code that leads to the phone app.
    GetApp,
    /// Offers a pairing and shows its code: for the app, or (true) for a phone's browser.
    Pair(bool),
    /// Shows the code that opens the remote's web page again.
    OpenPage,
    /// Unpairs the phone of that id (not by its place among those shown: the list is read again every second, and a
    /// phone paired or removed meanwhile would move another into it).
    Unpair(String),
    /// Sends the diagnostics to Glance's site, shown first in a window of
    /// their own to be read and agreed to.
    SendReport,
    /// Turns to the page of the reports sent.
    MyReports,
    /// Opens the thread of the report sent of that id.
    Thread(String),
    /// Installs NetEase Cloud Music's lyrics plugin (asked first), takes it back (asked first), or starts NetEase
    /// again to load it.
    NetEase(NetEaseWork),
    NetEaseRestart,
    /// Picks a theme in the preview gallery (index into themes()).
    ThemePick(usize),
    /// Confirms the picked theme: wears it.
    ThemeConfirm,
    // ---- System settings pages (OS category) ----
    /// Joins a wireless network kept before, by its profile name.
    WifiJoin(String),
    /// Joins a network new to this PC, by its name; the key is asked first.
    WifiAsk(String),
    /// Forgets a wireless network, by its profile name.
    WifiForget(String),
    /// Drops the network joined now, without forgetting it.
    WifiDisconnect,
    /// Shows or hides the key of the network joined now.
    WifiKey,
    /// Picks a network to show in the preview, by its name.
    WifiSelect(String),
    /// Puts a wallpaper file on the desktop, by its path.
    SetWallpaper(String),
    /// Asks Windows to pair a Bluetooth device, by its name.
    BtPair(String),
    /// Picks a Bluetooth device to show in the preview, by its address.
    BtSelect(String),
    /// Picks a network adapter to show in the preview, by its name.
    NetSelect(String),
    /// Picks a display to show in the preview, by its GDI name.
    ScreenSelect(String),
    /// Picks a physical disk to show its volumes in the preview, by its number.
    StorageSelect(u32),
    /// Tells a display to wear a mode: its size and rate.
    ModeApply { device: String, wide: u32, tall: u32, hz: u32 },
    /// Warms the screen to a depth of the night light (0 to wash it off).
    NightSet(u32),
    /// Grows the desktop to a share: 100, 125, 150, 175, or 200.
    ScaleSet(u32),
    /// Crowns a display as the one things start on.
    SetPrimary(String),
    /// Turns a display: none, a quarter right, upside down, or a quarter left.
    RotateSet { device: String, orient: u32 },
    /// Picks a sound device to show in the preview, by its id.
    AudioSelect(String),
    /// Puts a sound device's volume, as a percent.
    AudioVolume { id: String, percent: u32 },
    /// Mutes or unmutes a sound device.
    AudioMute { id: String, muted: bool },
    /// Picks an app to show in the notification preview, by its id.
    NotifySelect(String),
    /// Lets or holds one app's banners.
    NotifyToggle { aumid: String, on: bool },
    /// Puts the master switch on notices, letting them or holding them all.
    NotifyMaster(bool),
    /// The temporary files cleared as far as they can be.
    CleanTemp,
    /// The recycle bin emptied.
    EmptyRecycle,
    /// Storage sense put on or off.
    StorageSenseApply(bool),
    /// Picks a capability to show its apps in the privacy preview, as Windows names it in the consent store.
    ConsentSelect(String),
    /// Lets or holds one app's use of a capability.
    ConsentSet { capability: String, keys: Vec<String>, packaged: bool, allow: bool },
}

/// How what was done to NetEase Cloud Music's lyrics plugin went: done, with whether the loader is Glance's now
/// and whether NetEase must start again to load the plugin.
type NetEaseDone = Result<(bool, bool), crate::netease::Failed>;

/// What is done to NetEase Cloud Music's lyrics plugin (see `netease`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NetEaseWork {
    Install,
    Remove,
    /// The icon row put in beside a lyrics plugin installed without it.
    IconRow,
}

/// Modules for developers: listed under "More" while off, out of the way.
const MORE: [&str; 2] = ["wsl", "docker"];

/// A row with a single button of its own, for a setting that is Windows' to
/// keep: Glance cannot change it here, only say what it is and open where it
/// can be. Its name, the word under it, and the button's text are each told in
/// both tongues (Chinese, English); `what` is where pressing leads.
struct Do {
    name: (&'static str, &'static str),
    word: (&'static str, &'static str),
    button: (&'static str, &'static str),
    what: Target,
}

enum Row {
    /// The page's name.
    Title,
    /// The monitor's tabs, under its page's name.
    Tabs,
    /// One of what plays shows, on or off (an item of the lyrics module: see `music::Shows`).
    Element(&'static str),
    /// A drive Recent files reads, by its volume, or one left out that is away (see `Ui::drives_offered`): its files
    /// listed or not.
    Drive(String),
    /// A folder Recent files lists only (while it does), and the row that adds more.
    RecentFolder(std::path::PathBuf),
    AddRecentFolder,
    /// Words of their own, under the rows they are about.
    Note(&'static str),
    /// The fold of the modules few want, and how many it holds.
    More(usize),
    Skins,
    /// The components, to be dragged onto the screens' edges in the preview's room (see `layout_page`).
    Library,
    Choice(Field),
    Switch(Switch),
    Module(String),
    /// In a module's open card: one of its items, or a choice of its own.
    Item(String, String),
    ModuleChoice(String, Field),
    /// A value set by sliding.
    Slider(Field),
    Shortcut(Keys),
    /// This Glance's version, how the last asking went, and asking now.
    Version,
    /// Shows the guide again (see `guide`).
    Guide,
    Diagnostics,
    /// The reports this PC sent, how many and with how much unread: opens their page.
    MyReports,
    /// A report sent, by its id, on their page.
    Report(String),
    Update,
    /// What Glance's site has to tell, till it is read (see `notice`).
    Notice,
    Uninstall,
    Quit,
    /// The phone remote's: pairing a phone (and where the app is to be had), the code last asked for, and each phone
    /// paired (its place in the remote's list, and its name).
    Pair,
    Code,
    /// Where the remote's page for a phone's browser is.
    WebPage,
    /// A phone paired with: its id and its name.
    Phone(String, String),
    /// What the rows under it are, said small above them: a group of their own.
    Group(&'static str),
    /// A line read from the machine, told rather than set: what one of its own pages shows but does not keep.
    Info(String),
    /// A door to one of Windows' own pages: a row with a button that opens
    /// what Glance cannot set here (see `Do`).
    Do(Do),
    /// A row that leads somewhere else in this window: (its name in Chinese and
    /// in English, a word under it in Chinese and in English, and what pressing
    /// it does). Its whole card is what gets pressed, and a chevron stands where
    /// a button would — the name is told once, not echoed back as the button's
    /// word. The 通用 hub's rows use it to show another page's preview.
    Nav(&'static str, &'static str, &'static str, &'static str, Target),
    /// NetEase Cloud Music's lyrics plugin (InfLink-rs): how it is, and installing or taking it back.
    NetEase,
    /// A wireless network near, on the WLAN page's left: its name and profile, signal, and whether it is secured,
    /// joined, or kept.
    WifiNet(String, String, u32, bool, bool, bool),
    /// A Bluetooth device, on the Bluetooth page's left: its name and address, and whether it is paired or in touch.
    BtDevice(String, String, bool, bool),
    /// A network adapter, on the Network page's left: its name, and whether it is up, and whether the internet
    /// leaves by this one.
    NetAdapter(String, bool, bool),
    /// A display, on the Display page's left: its GDI name, and whether things start on it.
    Screen(String, bool),
    /// A sound device, on the Sound page's left: its id, its name, and whether it is the default for its kind.
    AudioDevice(String, String, bool),
    /// An app with a notification setting, on the Notifications page's left: its name, its id, and whether it is let now.
    NotifyApp(String, String, bool),
    /// A privacy capability, on the Privacy page's left: as Windows names it in the consent store,
    /// its word in each tongue, and how many apps have asked for it (None while that is being read).
    ConsentCap(String, &'static str, &'static str, Option<usize>),
    /// A physical disk, on the Storage page's left: its number, its model, and the room it holds.
    Disk(u32, String, u64),
}

/// How wide a 通用 hub preview's column is: a fixed column reads as blank wall in a
/// window grown large, so past the comfortable least width it grows with the pane,
/// three parts in four, and keeps its margins at the most.
fn hub_column(room: f32, least: f32, margin: f32) -> f32 {
    (room - 2.0 * margin).min((room * 0.75).max(least))
}

/// How tall a group's name is, above its rows.
const GROUP: f32 = 28.0;
/// The room the site's notice's icon takes left of its words: the icon, and as far again as a row's controls are
/// apart.
const NOTICE_ICON_ROOM: f32 = 28.0;
/// How tall the monitor's tabs are, and how far below their names their marker is.
const TABS_ROW: f32 = 48.0;
/// How wide the bar under the tab shown is (as Windows' own), and how its ends move to another chosen: the end toward
/// it, then the other.
const TAB_MARK: f32 = 16.0;
const TAB_MARK_LEADS: (Duration, Easing) = (Duration::from_millis(200), Easing(0.55, 0.55, 0.0, 1.0));
const TAB_MARK_TRAILS: (Duration, Easing) = (Duration::from_millis(360), Easing(0.55, 0.55, 0.0, 1.0));
/// How tall the code's own square is, in its row.
const CODE: f32 = 176.0;

/// A byte count spoken as the shell would: the largest unit it fills, one figure where it is past
/// whole bytes.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// A module row being dragged: by how far below the row's top it was
/// taken, and where the pointer is (document DIPs).
struct Drag {
    id: String,
    grab: f32,
    pointer: f32,
}

/// A screen's desktop, small, for the map: the screen by its name, the
/// picture's size (physical px), and the picture as a bitmap once drawn.
struct MapArt {
    id: String,
    /// Where the screen was as the picture was taken (physical px).
    monitor: RECT,
    /// The size it was last asked for at (physical px): what it holds, or will once it is read.
    asked: (u32, u32),
    /// The picture held: none till the first is read (on another thread: see `Ui::read_wanted`), the one before while
    /// another is read. And the size it was read at, while it is the wallpaper's as it is now (none once that changed).
    desktop: Option<Capture>,
    read_at: Option<(u32, u32)>,
    bitmap: Option<ID2D1Bitmap1>,
}

/// The map's picture of the screen named `id` at `monitor` (see `MapArt`), wanted `size` physical pixels large: made,
/// where the map has none of it yet, and its desktop wanted of `reads` where it does not hold it as it is asked. The
/// picture held is drawn smaller or larger to the size wanted, and asked again only where it is smaller than that, or
/// more than twice as large, and not while the window is sized by hand (each size of it would read the desktop again):
/// once it is let go. Another screen's picture is left as it is (those of screens no longer there go as the screens
/// change: see `Ui::desktops_changed`).
fn map_art_of<'a>(arts: &'a mut Vec<MapArt>, reads: &mut reads::Reads, id: &str, monitor: RECT, size: (u32, u32), by_hand: bool) -> &'a mut MapArt {
    let holds = |asked: (u32, u32)| asked.0 >= size.0 && asked.1 >= size.1 && asked.0 <= size.0 * 2 && asked.1 <= size.1 * 2;
    let at = match arts.iter().position(|art| art.id == id && art.monitor == monitor) {
        Some(at) => at,
        None => {
            arts.push(MapArt { id: id.to_string(), monitor, asked: size, desktop: None, read_at: None, bitmap: None });
            arts.len() - 1
        }
    };
    let art = &mut arts[at];
    if !holds(art.asked) && !by_hand {
        art.asked = size;
    }
    if art.read_at != Some(art.asked) {
        reads.want(reads::Ask::Map { id: id.to_string(), monitor, size: art.asked });
    }
    art
}

/// Screen `id`'s desktop at `monitor`, read `size` large, taken by the map's picture of it where that is still asked at
/// that size (see `map_art_of`).
fn map_art_came(arts: &mut [MapArt], id: &str, monitor: RECT, size: (u32, u32), desktop: Capture) {
    if let Some(art) = arts.iter_mut().find(|art| art.id == id && art.monitor == monitor && art.asked == size) {
        art.desktop = Some(desktop);
        art.read_at = Some(size);
        art.bitmap = None;
    }
}

/// The desktop the preview shows, at the size of the work area (DIPs).
/// How `Ui::stage_panel` draws a panel on the stage.
#[derive(Clone, Copy)]
pub(super) struct Staging {
    /// The whole screen, else the whole height of it with the whole panel and a strip of desktop beside it.
    pub whole: bool,
    /// How far it is slid out from its edge: 1 all the way, 0 just past the edge.
    pub out: f32,
    /// As large as this, not as the settings have it on that screen.
    pub size: Option<f32>,
    /// Put down at this point of the screen (stage DIPs), its corner there, or its lane `lane`'s: away from its edge.
    pub at: Option<(f32, f32)>,
    /// Only this lane of it drawn.
    pub lane: Option<usize>,
    pub opacity: f32,
    /// The desktop drawn under it, else it over what is drawn already.
    pub desktop: bool,
    /// Drawn, else only measured.
    pub draw: bool,
    /// Which of the stage's panels it is.
    pub shown: Shown,
}

impl Default for Staging {
    fn default() -> Self {
        Staging { whole: false, out: 1.0, size: None, at: None, lane: None, opacity: 1.0, desktop: true, draw: true, shown: Shown::Preview }
    }
}

/// Which of the stage's panels one staged is: the preview's, the guide's plays', or a widget the guide's play tears
/// off it (the lane's, the whole panel's). Each is drawn in layers of its own and turns between its layouts on its own
/// (see `StageLayers`), however the others are drawn between its frames.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Shown {
    Preview,
    Guide,
    Torn(usize),
}

/// A staged panel's layers (see `render::PanelLayers`), and its turn from one layout into the next as the panel's
/// own turns (see `render::Turn`).
#[derive(Default)]
struct StageLayers {
    layers: PanelLayers,
    turn: render::Turn,
}

impl StageLayers {
    fn release(&mut self) {
        self.layers.release();
        self.turn.release();
    }
}

/// What `Ui::stage_panel` drew, and where (the window's DIPs).
pub(super) struct Staged {
    /// How fast the panel's charts move (physical pixels a second), where it has any.
    pub plot: Option<f32>,
    pub screen: Rect,
    /// The panel, its lanes and the bar at its foot, where there was one to draw.
    pub panel: Option<Rect>,
    pub lanes: Vec<Rect>,
    pub bar: Option<Rect>,
    /// How deep a lane's head is, by which a readings lane is torn off alone.
    pub head: f32,
}

struct Stage {
    size: (f32, f32),
    /// The screen it is, by its name, and its work area as the desktop
    /// was taken (physical px).
    screen: Option<String>,
    work: RECT,
    /// None till it has been read (on another thread: see `Ui::read_wanted`). And whether it was read from the wallpaper
    /// as it is now, since the window opened: one kept from before (see `DESKTOPS`), or read before the wallpaper
    /// changed, is shown till it is read again (see `wanted`).
    desktop: Option<Capture>,
    current: bool,
    /// The desktop as a bitmap, for the zoom it was made at.
    bitmap: Option<(u32, ID2D1Bitmap1)>,
    /// What was last measured of the desktop (see `tone` and `glass`): the same for as long as it is shown as it
    /// is, and measured pixel by pixel, which took longer than drawing the rest of the preview.
    measured: Measured,
}

/// How many areas' tones a stage keeps (see `Measured`).
const TONES_KEPT: usize = 6;

#[derive(Default)]
struct Measured {
    /// How light it is behind the areas last asked of (see `Capture::luminance`), the latest first: the preview's
    /// panel or what plays, and the layout's editor's pictures, each frame.
    tones: Vec<(RECT, (f32, f32))>,
    /// The glass of an overlay over it (its ring around, and the hole the overlay is: see `overlay::glass`).
    glass: Option<((RECT, RECT), super::overlay::Glass)>,
}

impl Stage {
    fn new(size: (f32, f32), screen: Option<String>, work: RECT, desktop: Option<Capture>) -> Self {
        Stage { size, screen, work, desktop, current: false, bitmap: None, measured: Measured::default() }
    }

    /// Its desktop, as it is to be read (see `reads::Ask`): while it holds none read from the wallpaper as it is now,
    /// and is a screen's.
    fn wanted(&self) -> Option<reads::Ask> {
        let screen = self.screen.clone().filter(|_| !self.current)?;
        Some(reads::Ask::Stage { screen, work: self.work, size: self.size })
    }

    /// How light the desktop is behind `area` (mean and spread, one value a pixel); grey till it has been read.
    fn tone(&mut self, area: RECT) -> (f32, f32) {
        let Some(desktop) = &self.desktop else { return (0.5, 0.0) };
        let tones = &mut self.measured.tones;
        let tone = match tones.iter().position(|(at, _)| *at == area) {
            Some(at) => tones.remove(at).1,
            None => desktop.luminance(area, 1.0),
        };
        tones.insert(0, (area, tone));
        tones.truncate(TONES_KEPT);
        tone
    }

    /// Its middle, where the layout's editor's pictures of the components are taken to be over it.
    fn middle(&self) -> RECT {
        let (w, h) = self.size;
        RECT { left: (w * 0.3) as i32, top: (h * 0.3) as i32, right: (w * 0.7) as i32, bottom: (h * 0.7) as i32 }
    }

    /// The theme `skin` is drawn in over `behind` on it (light or dark as the desktop is there, where the skin follows
    /// it; else as `pref` says), and how frosted glass is there.
    fn look(&mut self, skin: Skin, pref: ThemePref, behind: RECT) -> (Theme, f32) {
        let tone = self.tone(behind);
        let dark = theme::is_dark(pref, Some(tone.0).filter(|_| skin.sees_backdrop()));
        let frost = if skin == Skin::Glass { skins::frost(tone.0, tone.1, dark) } else { 0.0 };
        (Theme::new(skin, dark), frost)
    }

    /// The skin's ground under what plays drawn on it (see `prefs::MusicGround`), `size` large, in `layers`, placed by
    /// `local` at `scale` physical pixels a DIP of it; this desktop behind it from `at` on it (its DIPs as its bitmap
    /// was made, `covers` of them a DIP of what plays), where given.
    #[allow(clippy::too_many_arguments)]
    fn music_ground(&self, frame: &Frame, layers: &mut render::GroundLayers, theme: &Theme, frost: f32, size: (f32, f32), at: Option<((f32, f32), f32)>, local: Matrix3x2, scale: f32) {
        let digest = self.desktop.as_ref().map_or(0, |desktop| desktop.digest);
        let backdrop = at.zip(self.bitmap.as_ref()).map(|(((x, y), covers), (_, bitmap))| (bitmap, Vector2 { X: x / covers, Y: y / covers }, digest, covers));
        if let Err(error) = layers.draw_whole(frame, theme, size, backdrop, frost, false, local, scale) {
            frame.fail(error);
        }
    }

    /// The glass of an overlay at `hole`, tinted for the desktop in `ring` around it (as the overlay measures it,
    /// see `Overlay::look`).
    fn glass(&mut self, ring: RECT, hole: RECT) -> super::overlay::Glass {
        match self.measured.glass {
            Some((at, glass)) if at == (ring, hole) => glass,
            _ => {
                let glass = super::overlay::glass(&mut self.desktop.as_ref().map_or(Vec::new(), |desktop| desktop.luminances(ring, hole, 1)), None).0;
                self.measured.glass = Some(((ring, hole), glass));
                glass
            }
        }
    }
}

/// The phone's page as it is used from here: how the remote last said the
/// PC is, what of the page can be pressed and where, how far the page is
/// moved up, what is in hand, and the thread the remote is asked on (what
/// to ask goes to it; how the PC then is comes back).
struct PhonePage {
    state: Option<serde_json::Value>,
    parts: Vec<(Rect, super::phone_art::Part)>,
    /// The phone's screen in the window, and how many of the window's DIPs one of its own is.
    at: Option<(Rect, f32)>,
    scroll: f32,
    /// How far the page can be moved up at most.
    most: f32,
    /// A slider in hand: its track in the window and how far along it is; or what else was pressed.
    slid: Option<(super::phone_art::Part, Rect, f32)>,
    pressed: Option<super::phone_art::Part>,
    /// A colour being made for the lights, while the ring it is made on is open.
    making: Option<super::phone_art::Making>,
    /// Since when what is pressed has been held (a colour held a while: the colours kept are taken away); and
    /// whether they are being.
    held_since: Instant,
    removing: bool,
    link: Option<(std::sync::mpsc::Sender<(String, String, String)>, std::sync::mpsc::Receiver<Option<serde_json::Value>>)>,
    /// When the remote was last asked how the PC is, and how many askings are unanswered.
    asked: Instant,
    waiting: usize,
}

/// How often the remote is asked how the PC is, while its page shows.
const PHONE_ASKED_EVERY: Duration = Duration::from_millis(1500);

struct Ui {
    hwnd: HWND,
    /// Pouring out of its place, or into it, while it does (see `genie`): into it, it is hidden, and closed once it is
    /// done.
    genie: Option<super::genie::Genie>,
    gfx: Rc<Gfx>,
    /// Counted as shown on its device while it is (see `life`).
    showing: Option<crate::ui::life::Showing>,
    /// Out of the struct while a frame is drawn on it.
    surface: Option<Surface>,
    mica: bool,
    /// Physical pixels per DIP.
    scale: f32,
    settings: Settings,
    /// The settings as this window last took them: what it changed since
    /// is where `settings` differs.
    base: Settings,
    prefs: Prefs,
    /// The page shown, and the monitor's tab (shown on its page).
    page: Page,
    tab: Tab,
    /// On the 通用 hub, the sub-item whose preview the right pane shows; none till
    /// one is picked (see `Target::Preview`).
    preview: Option<Page>,
    /// The 通用 hub's right pane scrolled when its sub-item's cards outgrow it, eased
    /// as the list's own is. `hub_most` is how far it may go, told by `hub_tall`: the
    /// height the pane's cards are laid out to.
    hub_scroll: f32,
    hub_scroll_target: f32,
    hub_tall: f32,
    /// The lowest point the hub's cards reached as they were laid out (a window
    /// coordinate, told by `grouped_card` and `switch_card`; see `hub_tall`).
    drawn: f32,
    /// How far the sentence under a hub's last card is told to run: the cards know
    /// where they end, the wrapped words do not say it back (see `hub_tall`).
    hanging: f32,
    /// The players music can be followed from, as the music page offers them (as Windows identifies each, and its
    /// name): those playing music now, and the one chosen, playing or not; taken anew each frame.
    sources: Vec<(String, String)>,
    /// What plays, as the music page's preview draws it; since when the example has played (see
    /// `music::paint_example`).
    music: super::music::Music,
    /// The skin's ground under it, drawn on the skin (in the preview, and in the layout's editor).
    music_ground: render::GroundLayers,
    example_from: Instant,
    lang: Lang,
    dark: bool,
    palette: Palette,
    autostart: bool,
    /// The phone remote: whether this Glance came with it, whether it is
    /// on, the code shown (and what for), and the phones paired, as last
    /// read (and when).
    remote_there: bool,
    remote: bool,
    /// Whether the remote has the program it sets lights by; that being
    /// fetched or taken away just now (on another thread: it takes a
    /// while), and whether the last fetching failed.
    remote_lights: bool,
    /// Whether the remote puts the screens out with their signal kept.
    remote_screens: bool,
    fetching: Option<std::thread::JoinHandle<bool>>,
    fetch_failed: bool,
    /// Whether the lights are set with an administrator's rights, that being changed (on a thread: it may have to
    /// fetch), and whether the last change failed.
    remote_raised: bool,
    raising: Option<std::thread::JoinHandle<bool>>,
    raise_failed: bool,
    /// A switch that needs the sensors' rights, being turned (on another thread: their process is asked, and
    /// answers when the system has done it); what it is then.
    switching: Option<(SwitchAsked, std::thread::JoinHandle<bool>)>,
    /// Whether Glance starts at sign-in and the phone remote is on, being read as the window opens (on another
    /// thread: Task Scheduler is asked); shown meanwhile as last read (see `SWITCHES_READ`).
    reading_switches: Option<std::thread::JoinHandle<(bool, bool)>>,
    /// The diagnostics being put together (on another thread: the sensors' part is asked of their process), and
    /// what for.
    reporting: Option<(ReportFor, std::thread::JoinHandle<String>)>,
    /// NetEase Cloud Music as found (None: not installed), how much of its lyrics plugin is on it, and whether it must be
    /// started again to load the plugin just installed; the plugin being installed or taken back (on another thread:
    /// it is downloaded, and NetEase is closed), and how that last went.
    netease: Option<crate::netease::NetEase>,
    netease_has: crate::netease::Installed,
    netease_restart: bool,
    netease_work: Option<(NetEaseWork, std::thread::JoinHandle<NetEaseDone>)>,
    netease_failed: Option<(NetEaseWork, crate::netease::Failed)>,
    code: Option<(crate::phone::Code, CodeFor)>,
    phones: Vec<crate::phone::Phone>,
    /// When the remote had last written the phones, as they were last read.
    phones_written: Option<std::time::SystemTime>,
    /// The phone remote's helper asked something (and how it is then) on another thread: each answer of its is a
    /// program started and waited for, not to be waited for by the thread that draws every window.
    remote_asked: Option<std::thread::JoinHandle<RemoteAnswer>>,
    phones_at: Instant,
    /// Whether this copy may start with Windows (see elevation.rs).
    may_autostart: bool,
    /// Whether this machine is a laptop (see `laptop`), where no OpenRGB is run with an administrator's rights.
    laptop: bool,
    /// The installer's uninstaller beside this executable, if it was
    /// installed somewhere ordinary programs cannot change.
    uninstaller: Option<std::path::PathBuf>,
    /// When the diagnostics were last copied.
    copied_at: Option<Instant>,
    /// Where the remote's page for a phone's browser is.
    web_address: Option<String>,
    /// The diagnostics being sent (on a thread of its own), and how the
    /// last sending went: the name the site gave them, or none.
    sending: Option<std::thread::JoinHandle<Option<String>>>,
    sent: Option<Option<String>>,
    /// The reports sent, as last read (see `reports`), and when.
    followed: Vec<crate::reports::Report>,
    followed_at: Instant,
    /// What the user adds to a report (its id, and what they gave), waiting for the diagnostics to go with it, to be
    /// shown first.
    adding_asked: Option<(String, super::ask::Said)>,
    /// What the user adds to a report, being sent (on a thread of its own), and how the last went.
    adding: Option<(String, std::thread::JoinHandle<bool>)>,
    added: Option<(String, bool)>,
    /// A report being closed by the user, or opened again (on a thread of its own): its id.
    turning: Option<(String, std::thread::JoinHandle<bool>)>,
    /// The report whose thread is open in its window (see `ask::thread`): its id, the window, and the report as the
    /// window was last given it.
    thread: Option<(String, HWND, crate::reports::Report)>,
    /// The drives Recent files reads, as last read, their reading (on a thread of its own: a drive asked of may take
    /// a while to wake), and when it was last asked for (again every two seconds while the workbench's page shows,
    /// for a drive plugged in or taken out).
    drives: Vec<crate::station::recent::Volume>,
    reading_drives: Option<std::thread::JoinHandle<Vec<crate::station::recent::Volume>>>,
    drives_at: Option<Instant>,
    /// Windows' chooser of folders up (on a thread of its own, a single-threaded apartment as the shell wants: this
    /// one is not), with what is chosen in it once it is closed.
    choosing_folders: Option<std::thread::JoinHandle<Vec<std::path::PathBuf>>>,
    /// Since when the fans in the phone remote's picture have been turning.
    phone_from: Instant,
    phone: PhonePage,
    scroll: f32,
    scroll_target: f32,
    /// The nav column's own scroll, eased as the list's is and held at 0 while its items fit the window.
    nav_scroll: f32,
    nav_scroll_target: f32,
    /// Which theme is picked for preview (None = the worn theme; set on a chip click, cleared on confirm).
    theme_picked: Option<usize>,
    /// The thumbs::changes() counter at last paint: repaint the appearance preview when new wallpaper thumbs arrive.
    theme_thumbs: u64,
    /// The key of the network joined now, shown in the WLAN preview once asked (None = hidden).
    wifi_key: Option<String>,
    /// The network picked to show in the WLAN preview, by its name (None = the one joined now).
    wifi_selected: Option<String>,
    /// The Bluetooth device picked to show in its preview, by its address (None = the first paired).
    bt_selected: Option<String>,
    /// The network adapter picked to show in its preview, by its name (None = the one in use).
    net_selected: Option<String>,
    display_selected: Option<String>,
    /// The physical disk picked to show its volumes in the storage preview, by its number (None = the first).
    storage_selected: Option<u32>,
    audio_selected: Option<String>,
    notify_selected: Option<String>,
    /// The privacy capability whose app list is shown, as Windows names it in the consent store.
    privacy_capability: String,
    /// The choice row whose list of options stands open, and the box that list hangs from.
    open_dropdown: Option<Field>,
    dropdown_anchor: Option<Rect>,
    /// A hub pane's open list, kept back to be drawn over the whole pane once every card of it
    /// is laid (see `paint_hub_menu`), and how far that list is turned by the wheel.
    hub_menu: Option<(Field, Rect)>,
    dropdown_scroll: f32,
    motion: HashMap<String, Transition>,
    /// Thumbs land where they belong without sliding (after relabelling).
    settle_thumbs: bool,
    /// Sized (or moved) by hand, in Windows' own loop: what is drawn is sized as it is (see `gfx::room`).
    by_hand: bool,
    /// Sized by hand in a frame since it was last drawn at rest (see `Gfx::settle`).
    was_sized: bool,
    /// Taking a new shortcut (which one): the next key combination pressed, and whether
    /// the last one lacked Ctrl, Alt or Win.
    recording: Option<Keys>,
    needs_modifier: bool,
    pointer: Option<(f32, f32)>,
    pressed: Option<Target>,
    /// What the pointer is over as the frame being drawn is drawn: found among the last frame's targets before they
    /// are made anew (found among those being made, it was none, and nothing lit under the pointer).
    hovered_now: Option<Target>,
    /// Where what is drawn follows the pointer itself, not what it is over (what plays, in its preview), as the last
    /// frame was drawn: a move there, or out of there, is drawn.
    follows_pointer: Vec<Rect>,
    /// The screens, as the map shows them.
    screens: Vec<crate::screens::Screen>,
    /// The edge last pressed in the layout's editor: the panel it opens slides out from it there.
    shown_edge: Option<Edge>,
    /// The screen the layout's editor shows large, by its name (none: every screen, small).
    view: Option<String>,
    /// A component carried in the layout's editor, and its edges as last drawn (where they take it).
    carry: Option<Carry>,
    zones: Vec<Zone>,
    /// What plays, as the layout's library shows it; the readings and the workbench, as it shows them (each drawn
    /// in layers of its own: see `render::PanelLayers`).
    library_music: super::music::Music,
    library_music_ground: render::GroundLayers,
    library_layers: [PanelLayers; 2],
    /// The panel the guide's plays show on the stage, and the widgets its play tears off it, the lane's and the whole
    /// panel's (see `Shown`).
    guide_layers: StageLayers,
    torn_layers: [StageLayers; 2],
    map_art: Vec<MapArt>,
    /// The desktops of the other screens the preview has shown, kept: turning to a page that shows another screen
    /// (the overlay's) and back takes them as they are, not read from the wallpaper and scaled anew each time.
    kept_stages: Vec<Stage>,
    /// The desktops read on other threads (every screen's for the preview as the window opens, and any one wanted
    /// since: see `read_wanted`), each asked once, and what they bring as it comes: for the preview, or for the map.
    reads: reads::Reads,
    read: (std::sync::mpsc::Sender<Read>, std::sync::mpsc::Receiver<Read>),
    /// The control the keyboard acts on, and whether the keyboard has been
    /// used since the last click (the focus is shown only then).
    focus: Option<Target>,
    keyboard: bool,
    drag: Option<Drag>,
    /// A slider being slid, and where its track is.
    sliding: Option<(Field, Rect)>,
    /// The sliders' thumbs' dots, easing (see `thumb::Dots`), and whether one was on its way at the last frame.
    slider_dots: crate::ui::thumb::Dots<Field>,
    dots_moving: bool,
    /// The modules whose cards are open (all closed as the window opens).
    expanded: HashSet<String>,
    /// The modules of `MORE` listed with the rest (those on as the page was
    /// turned to: one switched on or off there stays where it is till then),
    /// and whether the fold of the others is open.
    raised: HashSet<String>,
    more_open: bool,
    targets: Vec<(Rect, Target)>,
    relabel_at: Option<Instant>,
    stage: Stage,
    layers: StageLayers,
    last_frame: Instant,
    next_frame: Instant,
    /// When the guide is next drawn, apart from the window (see `guide`).
    guide_frame: Instant,
    /// The client size it was last drawn at (physical pixels), if it has been drawn.
    drawn_size: Option<(u32, u32)>,
    /// Something it shows taken in since it was last drawn (see `take_in`).
    taken_unseen: bool,
    /// The guide to Glance, while it is up (see `guide`).
    guide: Option<guide::Guide>,
    /// Where the layout's editor was last drawn (window DIPs), for the guide's step on it to point at.
    editor_at: Option<Rect>,
    /// Where the layout's components were last drawn (window DIPs), for the guide's tip to stay off them.
    library_at: Option<Rect>,
}

impl Lives for Ui {
    fn device(&self) -> &Rc<Gfx> {
        &self.gfx
    }

    fn showing(&mut self) -> &mut Option<crate::ui::life::Showing> {
        &mut self.showing
    }

    /// Its surface and its guide's, its preview's layers, and the desktops it shows made bitmaps.
    fn let_go_of_shown(&mut self) {
        self.surface = None;
        if let Some(guide) = &mut self.guide {
            guide.let_go_of_surfaces();
        }
        self.layers.release();
        self.guide_layers.release();
        self.stage.bitmap = None;
        for stage in &mut self.kept_stages {
            stage.bitmap = None;
        }
        for art in &mut self.map_art {
            art.bitmap = None;
        }
        self.music.release();
        self.library_music.release();
        for layers in &mut self.library_layers {
            layers.release();
        }
        for layers in &mut self.torn_layers {
            layers.release();
        }
    }
}

/// The window made and drawn, and, `shown` (poured out of the place given, if one is: see `genie`), shown.
/// How far the window keeps from the button it was asked from where it is moved clear of it (DIPs).
const CLEAR_OF: f32 = 12.0;

/// Where a window `size` large (physical px) opens in `work`: in its middle; where that would cover the place it was
/// asked from (`from`: its button, lit while it is open, which it pours out of and back into), moved clear of it, `gap`
/// from it, on the side it is moved least to, within `work`; in the middle still where it fits on no side of it.
fn placed_clear_of(work: RECT, (width, height): (i32, i32), from: Option<RECT>, gap: i32) -> (i32, i32) {
    let middle = (work.left + (work.right - work.left - width) / 2, work.top + (work.bottom - work.top - height) / 2);
    let Some(from) = from else { return middle };
    let covers = |(x, y): (i32, i32)| x < from.right && from.left < x + width && y < from.bottom && from.top < y + height;
    if !covers(middle) {
        return middle;
    }
    let fits = |(x, y): (i32, i32)| x >= work.left && y >= work.top && x + width <= work.right && y + height <= work.bottom;
    [
        (from.left - gap - width, middle.1),
        (from.right + gap, middle.1),
        (middle.0, from.top - gap - height),
        (middle.0, from.bottom + gap),
    ]
    .into_iter()
    .filter(|at| fits(*at))
    .min_by_key(|(x, y)| (x - middle.0).abs() + (y - middle.1).abs())
    .unwrap_or(middle)
}

/// The least the window can be sized to in `work` at `scale` (physical px): `MIN_SIZE`, as far as `work` holds it. (A
/// screen narrower than that, as a tall one at 200%, was widened past by the system, the window placed for less: off
/// the screen's side, where no screen shows it.)
fn least_size(work: RECT, scale: f32) -> (i32, i32) {
    (((MIN_SIZE.0 * scale) as i32).min(work.right - work.left), ((MIN_SIZE.1 * scale) as i32).min(work.bottom - work.top))
}

/// The size the window opens at in `work` at `scale` (physical px): `SIZE`, within nine tenths of `work`, never less
/// than it can be (see `least_size`).
fn opening_size(work: RECT, scale: f32) -> (i32, i32) {
    let least = least_size(work, scale);
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    (((SIZE.0 * scale) as i32).min(work_w * 9 / 10).max(least.0), ((SIZE.1 * scale) as i32).min(work_h * 9 / 10).max(least.1))
}

fn make(gfx: Rc<Gfx>, shown: Option<Option<RECT>>, to: Option<To>) -> Option<HWND> {
    let (work, scale) = crate::panel::work_area_at_cursor()?;
    let scale = scale as f32;
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let icon = |size| LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_SHARED).ok().map(|h| HICON(h.0));
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("GlanceSettings"),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
            hIcon: icon(GetSystemMetrics(SM_CXICON)).unwrap_or_default(),
            hIconSm: icon(GetSystemMetrics(SM_CXSMICON)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&class);
        // Sized and centred in physical pixels of the monitor it opens on.
        let (width, height) = opening_size(work, scale);
        let placed = placed_clear_of(work, (width, height), shown.flatten(), (CLEAR_OF * scale).round() as i32);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("GlanceSettings"),
            w!("Glance"),
            WS_OVERLAPPEDWINDOW,
            placed.0,
            placed.1,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        // Nothing in the window is typed: no input method, which would take
        // the keys of a shortcut being chosen for itself (they come as
        // VK_PROCESSKEY while it is on).
        let _ = ImmAssociateContextEx(hwnd, HIMC::default(), 0);
        // Mica, the Windows 11 window material, where the system has it: the
        // window's content leaves it showing through.
        let mica = windows_build() >= FIRST_MICA_BUILD;
        if mica {
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_MAINWINDOW as *const _ as *const _, 4);
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
        }
        crate::perf::mark(|| "settings window".into());
        let surface = Some(Surface::new(&gfx, hwnd).ok()?);
        crate::perf::mark(|| "settings surface".into());
        let app = crate::app();
        let settings = app.settings.lock().unwrap().clone();
        let prefs = Prefs::resolve(&settings.view, &app.controller.known_modules());
        let size = (((work.right - work.left) as f32 / scale).round(), ((work.bottom - work.top) as f32 / scale).round());
        let centre = POINT { x: (work.left + work.right) / 2, y: (work.top + work.bottom) / 2 };
        let screen = crate::screens::all().into_iter().find(|screen| crate::mouse::monitor_at(centre).is_some_and(|contact| contact.monitor == screen.monitor)).map(|screen| screen.id);
        // On the desktop as it was last read, if it was (see `DESKTOPS`): light or dark as it is from the first frame
        // (read again on another thread, the window showing meanwhile).
        let desktop = screen.as_deref().and_then(|screen| kept_desktop(screen, work, size));
        let stage = Stage::new(size, screen, work, desktop);
        let window_scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        // (Taken out first: locked twice in the one statement below, it waited on itself.)
        let switches_read = SWITCHES_READ.lock().unwrap().unwrap_or((false, false));
        let netease = crate::netease::found(false);
        let place = shown.flatten();
        let pours = super::genie::wanted(place);
        let mut ui = Ui {
            hwnd,
            genie: None,
            gfx,
            surface,
            mica,
            scale: window_scale,
            lang: Lang::resolve(prefs.language),
            dark: false,
            palette: Palette::new(false),
            base: taken(&settings, &prefs),
            settings,
            prefs,
            page: Page::Layout,
            tab: Tab::Shown,
            preview: None,
            hub_scroll: 0.0,
            hub_scroll_target: 0.0,
            hub_tall: 0.0,
            drawn: 0.0,
            hanging: 0.0,
            sources: Vec::new(),
            music: super::music::Music::default(),
            music_ground: render::GroundLayers::default(),
            example_from: Instant::now(),
            autostart: switches_read.0,
            remote_there: crate::phone::helper().is_some(),
            remote: switches_read.1,
            reading_switches: Some(std::thread::spawn(|| (crate::source::autostart(), crate::phone::on()))),
            // (As its helper says, asked as the window opens: see `remote_asked`.)
            remote_lights: false,
            remote_screens: false,
            fetching: None,
            fetch_failed: false,
            remote_raised: false,
            raising: None,
            raise_failed: false,
            switching: None,
            reporting: None,
            netease_has: netease.as_ref().map_or(crate::netease::Installed::No, crate::netease::installed),
            netease,
            netease_restart: false,
            netease_work: None,
            netease_failed: None,
            code: None,
            phones_written: crate::phone::phones_written(),
            phones: crate::phone::phones(),
            remote_asked: None,
            phones_at: Instant::now(),
            drives: Vec::new(),
            reading_drives: None,
            drives_at: None,
            choosing_folders: None,
            laptop: crate::laptop::this_machine(),
            may_autostart: elevation::may_start_unasked(),
            // Run elevated, so only where no ordinary program can change it.
            copied_at: None,
            web_address: None,
            sending: None,
            sent: None,
            followed: crate::reports::list(),
            followed_at: Instant::now(),
            adding_asked: None,
            adding: None,
            turning: None,
            added: None,
            thread: None,
            phone_from: Instant::now(),
            phone: PhonePage { state: None, parts: Vec::new(), at: None, scroll: 0.0, most: 0.0, slid: None, pressed: None, making: None, held_since: Instant::now(), removing: false, link: None, asked: Instant::now(), waiting: 0 },
            uninstaller: std::env::current_exe().ok().and_then(|exe| elevation::trusted(&exe.with_file_name("uninstall.exe"))),
            scroll: 0.0,
            scroll_target: 0.0,
            nav_scroll: 0.0,
            nav_scroll_target: 0.0,
            theme_picked: None,
            theme_thumbs: 0,
            wifi_key: None,
            wifi_selected: None,
            bt_selected: None,
            net_selected: None,
            display_selected: None,
            storage_selected: None,
            audio_selected: None,
            notify_selected: None,
            privacy_capability: "Location".into(),
            open_dropdown: None,
            dropdown_anchor: None,
            hub_menu: None,
            dropdown_scroll: 0.0,
            motion: HashMap::new(),
            settle_thumbs: true,
            by_hand: false,
            was_sized: false,
            recording: None,
            needs_modifier: false,
            pointer: None,
            pressed: None,
            hovered_now: None,
            follows_pointer: Vec::new(),
            screens: crate::screens::all(),
            shown_edge: None,
            view: None,
            carry: None,
            zones: Vec::new(),
            library_music: super::music::Music::default(),
            library_music_ground: render::GroundLayers::default(),
            library_layers: [PanelLayers::default(), PanelLayers::default()],
            guide_layers: StageLayers::default(),
            torn_layers: [StageLayers::default(), StageLayers::default()],
            map_art: Vec::new(),
            kept_stages: Vec::new(),
            reads: reads::Reads::default(),
            read: std::sync::mpsc::channel(),
            focus: None,
            keyboard: false,
            drag: None,
            sliding: None,
            slider_dots: crate::ui::thumb::Dots::new(),
            dots_moving: false,
            expanded: HashSet::new(),
            raised: HashSet::new(),
            more_open: false,
            targets: Vec::new(),
            relabel_at: None,
            stage,
            layers: StageLayers::default(),
            showing: None,
            last_frame: Instant::now(),
            next_frame: Instant::now(),
            guide_frame: Instant::now(),
            drawn_size: None,
            taken_unseen: false,
            guide: None,
            editor_at: None,
            library_at: None,
        };
        crate::perf::mark(|| "settings state".into());
        ui.raise_more();
        if ui.remote_there {
            ui.ask_remote(|| None);
        }
        ui.read_stages();
        ui.restyle();
        // (At the page asked for before it is first drawn: it pours out showing it, not the last shown.)
        if let Some(to) = to {
            ui.turn_as_asked(to);
        }
        crate::perf::mark(|| "settings dressed".into());
        ui.draw(Instant::now());
        UI.with(|cell| *cell.borrow_mut() = Some(ui));
        // (Made for the guide: shown by it as it comes up, see `guide`.)
        if shown.is_none() || to == Some(To::Guide) {
            return Some(hwnd);
        }
        // Poured out of its place: hidden as it is shown, then pictured (see `genie`).
        if pours.is_some() {
            super::genie::hide_to_open(hwnd);
        }
        // Glance started hidden (by the scheduler, an installer, a script)
        // has the system apply that to the first window of its own it
        // shows, in place of what is asked: shown again, it shows.
        let _ = ShowWindow(hwnd, SW_SHOW);
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        crate::perf::mark(|| "settings shown, cloaked".into());
        let _ = SetForegroundWindow(hwnd);
        crate::perf::mark(|| "settings in front".into());
        if let Some(place) = pours {
            match super::genie::opening(hwnd, place) {
                Some(genie) => {
                    with_ui(|ui| ui.genie = Some(genie));
                }
                None => super::genie::reveal(hwnd),
            }
        }
        Some(hwnd)
    }
}

/// Runs `act` on the window's state, unless it is already in use further up
/// this thread's stack (a message sent while handling another).
fn with_ui<R>(act: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut ui| ui.as_mut().map(act)))
}

fn point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as u16 as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// Whether `window` is `owner`'s own: owned by it, or by one of its own.
fn owned_by(window: HWND, owner: HWND) -> bool {
    let mut at = window;
    while let Ok(up) = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindow(at, windows::Win32::UI::WindowsAndMessaging::GW_OWNER) } {
        if up == owner {
            return true;
        }
        at = up;
    }
    false
}

/// The settings `hwnd` no longer above all, gone from the front (see `WM_ACTIVATE`).
fn leave_the_front(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
    }
    crate::app().controller.settings_front(pouring());
}

/// The window the settings pour away on, while they do.
fn pouring() -> Option<HWND> {
    let pouring = POURING.load(Ordering::Acquire);
    (pouring != 0).then(|| HWND(pouring as *mut _))
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        FOLLOW => {
            take_settings();
            LRESULT(0)
        }
        TO_OVERLAY => {
            OVERLAY_OWED.store(true, Ordering::Release);
            to_overlay();
            LRESULT(0)
        }
        TO_WORKBENCH => {
            WORKBENCH_OWED.store(true, Ordering::Release);
            to_workbench();
            LRESULT(0)
        }
        TO_REPORTS => {
            to_reports();
            LRESULT(0)
        }
        TO_GUIDE => {
            with_ui(|ui| ui.turn_as_asked(To::Guide));
            LRESULT(0)
        }
        TO_UPDATES => {
            with_ui(|ui| ui.turn_as_asked(To::Updates));
            LRESULT(0)
        }
        GUIDE_TOLD => {
            with_ui(|ui| ui.next_frame = Instant::now());
            LRESULT(0)
        }
        THREAD_TOLD => {
            THREAD_OWED.fetch_or(
                match wparam.0 {
                    1 => THREAD_SEND,
                    2 => THREAD_TURN,
                    _ => THREAD_CLOSE,
                },
                Ordering::AcqRel,
            );
            LRESULT(0)
        }
        // (Measuring: a page, and a tab of the monitor's, turned to as a press on it turns to it; see `perf`.)
        PERF_TURN if crate::perf::on() => {
            const PAGES: [Page; 11] = [Page::Layout, Page::Look, Page::Monitor, Page::Workbench, Page::Dock, Page::Launchpad, Page::Music, Page::Phone, Page::DateTime, Page::Appearance, Page::General];
            const TABS: [Tab; 3] = [Tab::Shown, Tab::Data, Tab::Overlay];
            with_ui(|ui| {
                crate::perf::mark(|| format!("settings turn {} {}", wparam.0, lparam.0));
                ui.tab = TABS[lparam.0 as usize % TABS.len()];
                ui.turn_to(PAGES[wparam.0 % PAGES.len()]);
            });
            LRESULT(0)
        }
        PERF_LIVE if crate::perf::on() => {
            with_ui(|ui| {
                ui.flip(Switch::Live);
                crate::perf::mark(|| format!("settings live {}", ui.settings.live_backdrop));
            });
            LRESULT(0)
        }
        // The screens changed: this thread asks for them anew too (see `screens::changed`).
        WM_DISPLAYCHANGE => {
            crate::screens::changed();
            gfx::screens_changed();
            SCREENS_OWED.store(true, Ordering::Release);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // The wallpaper changed (as Windows tells every window when a picture or colour is set for it).
        windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE if wparam.0 as u32 == windows::Win32::UI::WindowsAndMessaging::SPI_SETDESKWALLPAPER.0 => {
            WALLPAPER_OWED.store(true, Ordering::Release);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_GETMINMAXINFO => {
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
            let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
            let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor) }.as_bool() {
                let (x, y) = least_size(monitor.rcWork, scale);
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize = POINT { x, y };
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            with_ui(|ui| ui.scale = ((wparam.0 & 0xFFFF) as f32) / 96.0);
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        // Sized or moved by hand from here till it is let go: drawn as it is sized (see `gfx::sizing`), and once
        // more at rest as it is let go.
        WM_ENTERSIZEMOVE => {
            with_ui(|ui| ui.by_hand = true);
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            with_ui(|ui| {
                ui.by_hand = false;
                ui.draw(Instant::now());
            });
            LRESULT(0)
        }
        // Drawn at once, so the content keeps up while the window is resized.
        WM_SIZE => {
            // Drawn at its new size; minimized, or told of the size it was last drawn at (as it is first shown,
            // drawn already), nothing new to draw. (Its surface keeps what was drawn.)
            let size = ((lparam.0 & 0xFFFF) as u32, ((lparam.0 >> 16) & 0xFFFF) as u32);
            let minimized = wparam.0 == windows::Win32::UI::WindowsAndMessaging::SIZE_MINIMIZED as usize;
            // Restored: on the screen again, what plays is looked at at once for the previews (see `playing::watch`).
            if MINIMIZED.swap(minimized, Ordering::Relaxed) && !minimized {
                crate::playing::look_now();
            }
            if !minimized {
                with_ui(|ui| {
                    if ui.drawn_size != Some(size) {
                        ui.draw(Instant::now());
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lparam);
            with_ui(|ui| {
                if ui.pointer.is_none() {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = unsafe { TrackMouseEvent(&mut track) };
                }
                ui.moved(x as f32 / ui.scale, y as f32 / ui.scale);
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_ui(|ui| {
                // (Over nothing lit, nor what follows it, nothing changes as it goes.)
                if ui.hovered().is_some() || ui.over_followed() {
                    ui.next_frame = Instant::now();
                }
                ui.pointer = None;
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            with_ui(|ui| ui.press(x as f32 / ui.scale, y as f32 / ui.scale));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lparam);
            with_ui(|ui| ui.release(x as f32 / ui.scale, y as f32 / ui.scale));
            LRESULT(0)
        }
        // The pointer taken away mid-drag (Alt+Tab, another program): let go
        // where it is. (Let go by the window itself, it is already.)
        WM_CAPTURECHANGED => {
            with_ui(|ui| ui.let_go());
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
            // Bit 30: the key was already down (an auto-repeat).
            let repeat = lparam.0 & (1 << 30) != 0;
            // A key an input method has taken comes as VK_PROCESSKEY: the key
            // pressed is asked of it.
            let key = match VIRTUAL_KEY(wparam.0 as u16) {
                VK_PROCESSKEY => VIRTUAL_KEY(unsafe { ImmGetVirtualKey(hwnd) } as u16),
                key => key,
            };
            match with_ui(|ui| ui.key(key, shift, alt, repeat)) {
                Some(true) => LRESULT(0),
                _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
            }
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam.0 >> 16) as u16 as i16;
            with_ui(|ui| ui.wheel(delta));
            LRESULT(0)
        }
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == 1 => {
            // Over the client area: a hand on what can be dragged.
            let grip = with_ui(|ui| matches!(ui.hovered(), Some(Target::Grip(_) | Target::Library(_) | Target::Chip(..))) || ui.drag.is_some() || ui.carry.as_ref().is_some_and(|carry| carry.moved)).unwrap_or(false);
            if grip {
                unsafe { SetCursor(LoadCursorW(None, IDC_HAND).ok()) };
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // Closed: everything it drew with is let go; the device stays with
        // the panel.
        // Gone to another window: a new shortcut is no longer being taken. Another program's, or another of Glance's:
        // no longer above all, gone from the front (see `WM_ACTIVATE`, below). One of its own (a question it asks):
        // in front still, above all with it, which stays over it (else the widgets above all come over the question).
        WM_ACTIVATE if wparam.0 & 0xFFFF == WA_INACTIVE as usize => {
            with_ui(|ui| ui.record(None));
            if !owned_by(HWND(lparam.0 as *mut _), hwnd) {
                leave_the_front(hwnd);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // Another program come to the front while a question of its own was (the question no longer in front, it was
        // not told above): gone from the front.
        windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATEAPP if wparam.0 == 0 => {
            leave_the_front(hwnd);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // In front: the panel, staying up, gives way to it.
        // (Above all while in front, the panel and the widgets below it: they never leave the windows above all to
        // give way to it, as one in front of a window held over a whole screen could not come back among them.)
        WM_ACTIVATE => {
            unsafe {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
            }
            crate::app().controller.settings_front(Some(hwnd));
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // Closed: poured into the place it was asked from first, if it was, and closed once it has (see `tick`).
        WM_CLOSE if with_ui(|ui| ui.pour_away()).unwrap_or(false) => LRESULT(0),
        WM_DESTROY => {
            POURING.store(0, Ordering::Release);
            *ASKED_FROM.lock().unwrap() = None;
            crate::app().controller.settings_front(None);
            // The shortcut, if let go while a new one was taken, is taken up again.
            crate::tray::hold_hotkey(false);
            // Its surfaces and bitmaps go first; then the device gives back
            // what they held (unless desktop widgets still draw with it).
            // Closed mid-drag: what was dragged is kept.
            with_ui(|ui| ui.let_go());
            // (All it drew with goes: see `life`.)
            if let Some(mut ui) = UI.with(|cell| cell.borrow_mut().take()) {
                ui.gone();
            }
            WINDOW.store(0, Ordering::Release);
            crate::playing::show_in_settings(false);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Sets `field`, a slider's (the overlay's), to `value` in `settings`.
fn put_slider(settings: &mut Settings, field: Field, value: f32) {
    match field {
        Field::OverlayClear => settings.overlay.clear = value,
        Field::DockSize => settings.dock.size = value,
        Field::DockSpacing => settings.dock.spacing = value,
        Field::DockPeak => settings.dock.peak = value,
        Field::DockClear => settings.dock.clear = value,
        // (The launchpad's grid, whole tiles to a stop: 0 back to the screen's own choice.)
        Field::PadCols => settings.dock.pad_cols = value.round() as usize,
        Field::PadRows => settings.dock.pad_rows = value.round() as usize,
        _ => settings.overlay.size = value,
    }
}

/// The text of the window in both languages.
/// What the update row says under its name as taking the update stands, and its buttons, right to left. Where it cannot
/// be taken here (its installer not downloaded, or not passing its check: signed otherwise than this Glance trusts, as
/// after a change of certificate), the site's download page is offered too, so that it is still had: first where
/// trying again will not help.
fn update_offer(state: update::State, lang: Lang) -> (String, Vec<(String, Target, Button)>) {
    let p = |zh, en| pick(lang, zh, en).to_string();
    let download = |kind| (p("前往下载", "Download"), Target::Download, kind);
    match state {
        update::State::Ready => (p("下载并运行安装程序", "Downloads and runs the installer"), vec![(p("更新", "Update"), Target::Update, Button::Accent)]),
        update::State::Downloading => (p("正在下载安装程序…", "Downloading the installer…"), vec![(p("下载中", "Downloading"), Target::Update, Button::Quiet)]),
        update::State::Failed => (
            p("下载失败，请重试或从官网下载", "The download failed. Retry, or download it from the website"),
            vec![(p("重试", "Retry"), Target::Update, Button::Accent), download(Button::Plain)],
        ),
        update::State::Rejected => (
            p("安装程序未通过验证，请从官网下载", "The installer did not pass its check. Download it from the website"),
            vec![download(Button::Accent), (p("重试", "Retry"), Target::Update, Button::Plain)],
        ),
    }
}

fn pick(lang: Lang, zh: &'static str, en: &'static str) -> &'static str {
    lang.pick(zh, en)
}

/// The last `count` samples' network rates (bytes a second), oldest first, from the
/// controller's live history; a sample that read none stands as 0, the line flat where
/// nothing moved.
fn net_series(count: usize) -> (Vec<f64>, Vec<f64>) {
    let history = crate::app().controller.history.read().unwrap();
    let recent = history.len().saturating_sub(count);
    let down = history.iter().skip(recent).map(|s| s.net_down.unwrap_or(0.0)).collect();
    let up = history.iter().skip(recent).map(|s| s.net_up.unwrap_or(0.0)).collect();
    (down, up)
}

/// A link's rate, of the bits a second its driver says: whole gigabits where it
/// reaches them, else megabits, else what the driver had to give. Nothing told
/// for a link that stands still.
fn net_rate(bps: u64) -> String {
    let m = bps as f64 / 1_000_000.0;
    if bps == 0 {
        String::from("—")
    } else if m >= 1000.0 {
        format!("{:.1} Gbps", m / 1000.0)
    } else if m >= 1.0 {
        format!("{m:.0} Mbps")
    } else {
        format!("{:.0} Kbps", m * 1000.0)
    }
}

/// The newest sample's battery, where the history holds one.
fn battery_latest() -> Option<crate::reading::BatterySample> {
    crate::app().controller.history.read().unwrap().back().and_then(|s| s.battery.clone())
}

/// The last `count` samples' battery charge (percent), oldest first; samples without a
/// battery go unshown, so this is empty while the machine runs on mains.
fn battery_series(count: usize) -> Vec<f32> {
    let history = crate::app().controller.history.read().unwrap();
    let recent = history.len().saturating_sub(count);
    history.iter().skip(recent).filter_map(|s| s.battery.as_ref().map(|b| b.percent as f32)).collect()
}

impl Ui {
    /// Asked to close: poured into the place it was asked from where that is still seen, else into Glance's icon in the
    /// tray (see `genie::back_into`), hidden meanwhile, and closed once it has.
    /// Whether it is (pouring already, it goes on). No longer the settings window open: asked for meanwhile, another
    /// is made once it is gone. Pouring out of its place still, that is done with first.
    fn pour_away(&mut self) -> bool {
        if self.genie.as_ref().is_some_and(|genie| !genie.opening()) {
            return true;
        }
        if self.genie.take().is_some() {
            super::genie::reveal(self.hwnd);
        }
        // (Where what it was asked from is now: see `asked_from_moved`.)
        let Some(place) = super::genie::wanted(asked_from()).and_then(|place| super::genie::back_into(self.hwnd, place)) else { return false };
        if unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(self.hwnd) }.as_bool() {
            return false;
        }
        // (Over the panel and the widgets as it pours, into a button on one of them: they give way to it as they gave
        // way to the window, till it is closed.)
        let told = |pouring: HWND| {
            POURING.store(pouring.0 as isize, Ordering::Release);
            crate::app().controller.settings_front(Some(pouring));
        };
        let Some(genie) = super::genie::closing(self.hwnd, place, told) else { return false };
        self.genie = Some(genie);
        WINDOW.store(0, Ordering::Release);
        true
    }

    /// Follows the theme and the language now chosen.
    fn restyle(&mut self) {
        // Following the backdrop is the panel's; the window follows the system then.
        self.dark = match self.prefs.theme {
            ThemePref::Light => false,
            ThemePref::Dark => true,
            ThemePref::System | ThemePref::Backdrop => crate::os::apps_dark(),
        };
        self.palette = Palette::new(self.dark);
        unsafe {
            let dark = BOOL::from(self.dark);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(pick(self.lang, "Glance 设置", "Glance Settings")));
        }
        self.next_frame = Instant::now();
    }

    /// Takes the language now chosen, once its thumb has slid there.
    fn relabel(&mut self) {
        self.relabel_at = None;
        self.lang = Lang::resolve(self.prefs.language);
        self.settle_thumbs = true;
        self.restyle();
    }

    /// Keeps what was changed here and hands it to the panel: only that,
    /// over the settings as they are now (changed elsewhere meanwhile,
    /// perhaps, before this window followed).
    fn save(&mut self) {
        self.settings.view = crate::ui::prefs::kept(&self.prefs, &self.settings.view);
        let app = crate::app();
        let mut now = None;
        app.change(|held| {
            *held = crate::settings::merged(held, &self.base, &self.settings);
            now = Some(held.clone());
        });
        self.take(now.unwrap());
        self.next_frame = Instant::now();
    }

    /// Takes `settings`, as they are now, as its own.
    fn take(&mut self, settings: Settings) {
        self.prefs = Prefs::resolve(&settings.view, &crate::app().controller.known_modules());
        self.base = taken(&settings, &self.prefs);
        self.settings = settings;
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Title];
        match (self.page, self.tab) {
            (Page::Layout, _) => rows.extend([
                Row::Library,
                Row::Group(pick(self.lang, "呼出方式", "How it opens")),
                Row::Choice(Field::Push),
                Row::Choice(Field::CloseDelay),
                Row::Choice(Field::Anchor),
                Row::Choice(Field::OverFullscreen),
                Row::Switch(Switch::TearOff),
                Row::Group(pick(self.lang, "快捷键", "Shortcuts")),
                Row::Shortcut(Keys::Panel),
                Row::Shortcut(Keys::Search),
                Row::Shortcut(Keys::Preview),
            ]),
            (Page::Look, _) => {
                rows.extend([Row::Skins, Row::Choice(Field::Theme)]);
                // (Only a skin that shows what is behind it has it to show live.)
                if Skin::named(&self.settings.skin).sees_backdrop() {
                    rows.push(Row::Switch(Switch::Live));
                }
            }
            (Page::Monitor, tab) => {
                rows.push(Row::Tabs);
                self.monitor_rows(&mut rows, tab);
            }
            (Page::Workbench, _) => {
                rows.push(Row::Choice(Field::Columns));
                for entry in &self.prefs.workbench {
                    let id = format!("{WORKBENCH}{}", entry.id);
                    rows.push(Row::Module(id.clone()));
                    if self.opened(&id) > 0.0 {
                        rows.extend(self.items_here(&id).into_iter().map(|name| Row::Item(id.clone(), name)));
                        rows.extend(self.fields_here(&entry.id).map(|field| Row::ModuleChoice(id.clone(), field)));
                    }
                }
                if self.settings.station.recent {
                    self.recent_rows(&mut rows);
                }
            }
            (Page::Dock, _) => {
                rows.extend([
                    Row::Switch(Switch::DockOn),
                    Row::Choice(Field::DockEdge),
                    Row::Slider(Field::DockSize),
                    Row::Slider(Field::DockSpacing),
                    Row::Switch(Switch::DockMagnify),
                ]);
                // (Only where the pointer swells anything: with it off, the peak shapes nothing.)
                if self.settings.dock.magnify {
                    rows.push(Row::Slider(Field::DockPeak));
                }
                rows.extend([
                    Row::Slider(Field::DockClear),
                    Row::Group(pick(self.lang, "行为", "Behaviour")),
                    Row::Switch(Switch::DockHide),
                    Row::Switch(Switch::DockLock),
                    Row::Switch(Switch::DockThrough),
                    Row::Switch(Switch::DockRunning),
                    Row::Switch(Switch::DockShowOpen),
                    Row::Note(pick(
                        self.lang,
                        "右键点击 Dock 上的图标可添加、移除或调整位置；在工作台中右键点击程序，选择“固定到 Dock”可将其添加进来。鼠标滚轮在图标栏上可调节玻璃透明度",
                        "Right-click an icon to add, remove or move it. In the workbench, right-click a program and choose “Fix to the dock” to add it. The wheel over the bar turns its glass up or down",
                    )),
                ]);
            }
            (Page::Launchpad, _) => {
                rows.extend([
                    Row::Slider(Field::PadCols),
                    Row::Slider(Field::PadRows),
                    Row::Note(pick(
                        self.lang,
                        "拖动定下每页的列数与行数，图标跟着它们自动缩放；拖到“自动”则按屏幕大小自己定",
                        "Drag to set the tiles across a page and the rows of them: the icons scale to fit. Dragged to Automatic, the screen chooses",
                    )),
                ]);
            }
            (Page::Music, _) => {
                rows.push(Row::Choice(Field::MusicSource));
                // (Only where NetEase is installed.)
                if self.netease.is_some() {
                    rows.push(Row::NetEase);
                }
                rows.push(Row::Group(pick(self.lang, "显示内容", "What it shows")));
                rows.push(Row::Choice(Field::MusicGround));
                // (Its elements as they stand in the widget, top to bottom.)
                let mut elements: Vec<&'static str> = super::prefs::items("lyrics").iter().map(|item| item.name).collect();
                elements.sort_by_key(|name| ["cover", "title", "translation", "spectrum", "controls"].iter().position(|at| at == name));
                rows.extend(elements.into_iter().map(Row::Element));
                rows.push(Row::Note(pick(
                    self.lang,
                    "在线查找歌词时，会将歌名发送至网易云音乐、QQ 音乐、酷狗或 LRCLIB。本地歌曲优先使用文件内嵌的歌词。同步歌词需要播放器提供播放进度。",
                    "To find lyrics online, the track's title is sent to NetEase Cloud Music, QQ Music, Kugou or LRCLIB. A local track uses the lyrics in its file first. Synced lyrics need the player to report its position.",
                )));
            }
            (Page::Phone, _) => self.remote_rows(&mut rows),
            (Page::Reports, _) => {
                rows.extend(self.followed.iter().map(|report| Row::Report(report.id.clone())));
                rows.push(Row::Note(pick(
                    self.lang,
                    "此处列出近 90 天内从本机发送的诊断信息。Glance 检查更新时一并获取开发者的回复与处理状态，有新消息时会提示。打开一份反馈可查看全部往来，并补充说明、截图或新的诊断信息。",
                    "Here are the diagnostics sent from this PC in the last 90 days. As Glance checks for updates, it also fetches the developer's replies and how each report stands, and tells you of anything new. Open a report to read all that was said and to add words, pictures or fresh diagnostics.",
                )));
            }
            (Page::General, _) => {
                rows.push(Row::Choice(Field::Language));
                rows.push(Row::Group(pick(self.lang, "系统", "System")));
                rows.extend([Row::Switch(Switch::Startup), Row::Switch(Switch::TrayHidden)]);
                rows.push(Row::Group(pick(self.lang, "更新", "Updates")));
                if crate::notice::unread().is_some() {
                    rows.push(Row::Notice);
                }
                rows.push(Row::Version);
                if update::available().is_some() {
                    rows.push(Row::Update);
                }
                rows.push(Row::Switch(Switch::Updates));
                rows.push(Row::Group(pick(self.lang, "帮助与反馈", "Help and feedback")));
                rows.push(Row::Guide);
                rows.push(Row::Diagnostics);
                if !self.followed.is_empty() {
                    rows.push(Row::MyReports);
                }
                if self.uninstaller.is_some() {
                    rows.push(Row::Group(pick(self.lang, "卸载与退出", "Uninstall and quit")));
                    rows.push(Row::Uninstall);
                } else {
                    rows.push(Row::Group(pick(self.lang, "退出", "Quit")));
                }
                rows.push(Row::Quit);
            }
            // The machine's own date and time: the clock's face, its seconds, the zone read live.
            (Page::DateTime, _) => {
                let lang = self.lang;
                rows.push(Row::Group(pick(lang, "日期与时间", "Date & Time")));
                rows.push(Row::Choice(Field::ClockHour));
                rows.push(Row::Switch(Switch::Seconds));
                let zone = super::system_settings::current_zone();
                rows.push(Row::Info(format!(
                    "{} · {}",
                    pick(lang, "当前时区", "Current zone"),
                    if zone.is_empty() { pick(lang, "未知", "Unknown") } else { zone.as_str() }
                )));
                // (The zone and the clock are Windows' to keep: Glance reads them
                //  above and opens the page where they are set.)
                rows.push(Row::Do(Do {
                    name: ("日期和时间", "Date & time"),
                    word: ("更改时区、自动校时与显示格式均由系统设置", "The zone, keeping the clock, and the formats are set here by Windows"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:dateandtime".into()),
                }));
                rows.push(Row::Group(pick(lang, "区域和语言", "Region & language")));
                rows.push(Row::Do(Do {
                    name: ("语言和区域", "Language & region"),
                    word: ("Windows 显示语言、区域格式与键盘输入法", "Windows' display language, regional formats, and keyboard input methods"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:regionlanguage".into()),
                }));
            }
            // The system's own look: its theme, mode, animation, transparency, and accent.
            (Page::Appearance, _) => {
                let lang = self.lang;
                let current = super::system_settings::theme_choices().1
                    .and_then(|i| super::system_settings::themes().get(i).map(|(n, _)| n.clone()))
                    .unwrap_or_else(|| pick(lang, "未保存", "Unsaved").to_string());
                rows.extend([
                    Row::Group(pick(lang, "系统的外观", "The system's own")),
                    Row::Info(format!("{} · {}", pick(lang, "当前主题", "Current theme"), current)),
                    Row::Choice(Field::SystemTheme),
                    Row::Switch(Switch::AnimOn),
                    Row::Switch(Switch::Glass),
                    Row::Switch(Switch::AccentOn),
                    Row::Choice(Field::Accent),
                ]);
            }
            (Page::Display, _) => {
                let lang = self.lang;
                // The machine's screens down the left; the picked one's modes, scale and night light to the right.
                rows.push(Row::Group(pick(lang, "显示器", "Displays")));
                if self.screens.is_empty() {
                    rows.push(Row::Info(pick(lang, "没有读到显示器", "No display was read").to_string()));
                } else {
                    rows.extend(self.screens.iter().map(|s| Row::Screen(crate::sys::device_of(&s.monitor), s.primary)));
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的显示设置", "Windows' display settings"),
                    word: ("方向、亮度、HDR 与多屏排列由系统进行", "Orientation, brightness, HDR, and how the screens lie are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:display".into()),
                }));
            }
            (Page::Sound, _) => {
                let lang = self.lang;
                // The machine's sound devices down the left, each way round; the picked one's volume and mute to the right.
                match crate::audio::snapshot() {
                    Some(shot) if !shot.devices.is_empty() => {
                        rows.push(Row::Group(pick(lang, "输出", "Output")));
                        let mut out = false;
                        for d in shot.devices.iter().filter(|d| d.render) {
                            rows.push(Row::AudioDevice(d.id.clone(), d.name.clone(), d.default));
                            out = true;
                        }
                        if !out {
                            rows.push(Row::Info(pick(lang, "没有播放设备", "No output device").to_string()));
                        }
                        rows.push(Row::Group(pick(lang, "输入", "Input")));
                        let mut inn = false;
                        for d in shot.devices.iter().filter(|d| !d.render) {
                            rows.push(Row::AudioDevice(d.id.clone(), d.name.clone(), d.default));
                            inn = true;
                        }
                        if !inn {
                            rows.push(Row::Info(pick(lang, "没有录音设备", "No input device").to_string()));
                        }
                    }
                    Some(_) => rows.push(Row::Info(pick(lang, "没有发现音频设备", "No sound devices found").to_string())),
                    None => rows.push(Row::Info(pick(lang, "正在读取音频设备…", "Reading the sound devices…").to_string())),
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的声音设置", "Windows' sound settings"),
                    word: ("格式、增强与其余的设备细节由系统进行", "Formats, enhancements, and the rest of the device details are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:sound".into()),
                }));
            }
            (Page::Network, _) => {
                let lang = self.lang;
                // The machine's cards down the left; the picked one's addresses, and the speed now, to the right.
                rows.push(Row::Group(pick(lang, "网卡列表", "Network adapters")));
                match crate::network::snapshot() {
                    Some(shot) if !shot.adapters.is_empty() => {
                        rows.extend(shot.adapters.iter().map(|a| Row::NetAdapter(a.name.clone(), a.up, a.current)));
                    }
                    Some(_) => rows.push(Row::Info(pick(lang, "没有发现可用的网卡", "No adapters found").to_string())),
                    None => rows.push(Row::Info(pick(lang, "正在读取网卡…", "Reading the adapters…").to_string())),
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的网络设置", "Windows' network settings"),
                    word: ("共享、代理与其余的连接细节由系统进行", "Sharing, proxies, and the rest of the connections are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:network-status".into()),
                }));
            }
            (Page::Notifications, _) => {
                let lang = self.lang;
                // The apps that may leave a notice, down the left; the picked one's letting, and the master switch, to the right.
                match crate::sys::notify_snapshot() {
                    Some(apps) if !apps.is_empty() => {
                        rows.push(Row::Group(pick(lang, "应用", "Apps")));
                        rows.extend(apps.into_iter().map(|a| Row::NotifyApp(a.display, a.aumid, a.enabled)));
                    }
                    Some(_) => rows.push(Row::Info(pick(lang, "没有应用留下通知设置", "No app has left a notification setting").to_string())),
                    None => rows.push(Row::Info(pick(lang, "正在读取通知设置…", "Reading the notification settings…").to_string())),
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的通知设置", "Windows' notification settings"),
                    word: ("横幅与锁屏的显示样式由系统进行", "How banners and the lock screen show them is the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:notifications".into()),
                }));
            }
            (Page::Storage, _) => {
                let lang = self.lang;
                // Each physical disk the machine carries, down the left: its model and the room it holds,
                // the letters mounted on it told in the preview to the right. Picking one shows its volumes.
                rows.push(Row::Group(pick(lang, "本机存储", "This PC's storage")));
                let disks = super::system_settings::disks();
                if disks.is_empty() {
                    rows.push(Row::Info(pick(lang, "没读到可用的磁盘。", "No drive could be read.").to_string()));
                }
                for d in disks {
                    rows.push(Row::Disk(d.number, d.model, d.size));
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的存储设置", "Windows' storage settings"),
                    word: ("各盘的用量与存储感知的细则由系统进行", "How full each drive is, and storage sense's own rules, are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:storagesense".into()),
                }));
            }
            (Page::Privacy, _) => {
                // The three reaches apps can ask for, each a row of its own: which apps ask for it, and
                // whether each is let, is shown in the preview to the right. Each reading runs PowerShell,
                // so all three are taken off-thread.
                for (cap, zh, en) in [
                    ("Location", "位置", "Location"),
                    ("Microphone", "麦克风", "Microphone"),
                    ("Webcam", "摄像头", "Camera"),
                ] {
                    let told = crate::sys::consent_snapshot(cap).map(|apps| apps.len());
                    rows.push(Row::ConsentCap(cap.to_string(), zh, en, told));
                }
                rows.push(Row::Do(Do {
                    name: ("Windows 的隐私设置", "Windows' privacy settings"),
                    word: ("权限的总览与其余类别由系统进行", "The overview of permissions and the other categories are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:privacy".into()),
                }));
            }
            (Page::Gaming, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "游戏", "Gaming")),
                    Row::Switch(Switch::GameMode),
                    Row::Switch(Switch::GameDvr),
                    Row::Note(pick(
                        lang,
                        "游戏 DVR 用于在后台录制游戏片段；更细的捕获与快捷键设置由系统进行",
                        "Game DVR records clips in the background; the finer capture and shortcut settings are the system's to make",
                    )),
                ]);
                rows.push(Row::Do(Do {
                    name: ("Xbox Game Bar", "Xbox Game Bar"),
                    word: ("游戏栏、其录制与覆盖层由系统进行", "The Game Bar, its recording, and its overlays are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:gaming-gamebar".into()),
                }));
            }
            // Wi-Fi: the radio's own switch, then the networks split — joined, near, and kept but out of range.
            (Page::Wifi, _) => {
                let lang = self.lang;
                rows.push(Row::Group(pick(lang, "WLAN", "Wi-Fi")));
                rows.push(Row::Switch(Switch::WifiOn));
                match crate::wifi::snapshot() {
                    Some(shot) if shot.on => {
                        let connected: Vec<_> = shot.nets.iter().filter(|n| n.connected).collect();
                        let near: Vec<_> = shot.nets.iter().filter(|n| !n.connected).collect();
                        if !connected.is_empty() {
                            rows.push(Row::Group(pick(lang, "已连接", "Connected")));
                            rows.extend(connected.iter().map(|n| Row::WifiNet(n.ssid.clone(), n.profile.clone(), n.signal, n.secure, n.connected, n.known)));
                        }
                        rows.push(Row::Group(pick(lang, "可用的网络", "Available networks")));
                        if near.is_empty() {
                            rows.push(Row::Info(pick(lang, "附近没有发现其他网络", "No other networks found nearby").to_string()));
                        }
                        rows.extend(near.iter().map(|n| Row::WifiNet(n.ssid.clone(), n.profile.clone(), n.signal, n.secure, n.connected, n.known)));
                        if !shot.saved.is_empty() {
                            rows.push(Row::Group(pick(lang, "已保存的网络", "Saved networks")));
                            rows.extend(shot.saved.iter().map(|n| Row::WifiNet(n.ssid.clone(), n.profile.clone(), n.signal, n.secure, n.connected, n.known)));
                        }
                    }
                    Some(_) => rows.push(Row::Info(pick(lang, "WLAN 已关闭，打开后即可搜索附近网络", "WLAN is off. Turn it on to see the networks near").to_string())),
                    None => rows.push(Row::Info(pick(lang, "正在搜索附近的网络…", "Looking for networks nearby…").to_string())),
                }
            }
            // Bluetooth: the devices split — those kept, and those near — down the left; the picked one's details to the right.
            (Page::Bluetooth, _) => {
                let lang = self.lang;
                rows.push(Row::Group(pick(lang, "蓝牙", "Bluetooth")));
                rows.push(Row::Switch(Switch::BluetoothOn));
                match crate::bluetooth::snapshot() {
                    Some(shot) if shot.radios > 0 => {
                        if !shot.paired.is_empty() {
                            rows.push(Row::Group(pick(lang, "已配对的设备", "Paired devices")));
                            rows.extend(shot.paired.iter().map(|d| Row::BtDevice(d.name.clone(), d.addr.clone(), true, d.connected)));
                        }
                        rows.push(Row::Group(pick(lang, "附近的设备", "Devices nearby")));
                        if shot.nearby.is_empty() {
                            rows.push(Row::Info(pick(lang, "附近没有发现设备，让设备处于可被发现的状态后重试", "Nothing found nearby. Make the device discoverable and try again").to_string()));
                        }
                        rows.extend(shot.nearby.iter().map(|d| Row::BtDevice(d.name.clone(), d.addr.clone(), d.remembered, d.connected)));
                        rows.push(Row::Do(Do { name: (("添加设备"), ("Add device")), word: (("由系统的蓝牙页完成配对"), ("A pairing is finished in Windows' Bluetooth page")), button: (("打开"), ("Open")), what: Target::Open("ms-settings:bluetooth".into()) }));
                    }
                    Some(_) if crate::bluetooth::adapter() => rows.push(Row::Info(pick(lang, "蓝牙已关闭，打开后即可搜索附近设备", "Bluetooth is off. Turn it on to see the devices nearby").to_string())),
                    Some(_) => rows.push(Row::Info(pick(lang, "本机没有蓝牙无线电", "This PC has no Bluetooth radio").to_string())),
                    None => rows.push(Row::Info(pick(lang, "正在读取蓝牙设备…", "Reading Bluetooth devices…").to_string())),
                }
            }
            // Power & battery: the plan, the screen and sleep delays, and what the battery says.
            (Page::Power, _) => {
                let lang = self.lang;
                let zh = lang == Lang::Zh;
                rows.extend([
                    Row::Group(pick(lang, "电源模式", "Power mode")),
                    Row::Choice(Field::PowerPlan),
                    Row::Group(pick(lang, "屏幕与睡眠", "Screen and sleep")),
                    Row::Slider(Field::ScreenOff),
                    Row::Slider(Field::Sleep),
                    Row::Group(pick(lang, "电池", "Battery")),
                ]);
                let line = match (super::system_settings::battery(), super::system_settings::battery_health()) {
                    (Some((pct, charging)), health) => format!(
                        "{}{} · {}",
                        pick(lang, "剩余", "Remaining"),
                        format!(" {pct}%"),
                        if charging { pick(lang, "正在充电", "charging").to_string() } else { pick(lang, "未充电", "not charging").to_string() }
                    ) + &health.map(|h| format!(" · {} {h}%", pick(lang, "健康度", "Health"))).unwrap_or_default(),
                    (None, _) => pick(lang, "本机没有电池", "This PC has no battery").to_string(),
                };
                rows.push(Row::Info(line));
                rows.push(Row::Do(Do {
                    name: ("Windows 的电源设置", "Windows' power settings"),
                    word: ("合盖、按钮与计划的高级选项由系统进行", "Lids, buttons, and the plan's deeper options are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:power".into()),
                }));
                let _ = zh;
            }
            // Focus / Do Not Disturb.
            (Page::Focus, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "专注模式", "Focus")),
                    Row::Switch(Switch::FocusOn),
                    Row::Switch(Switch::FocusLock),
                    Row::Note(pick(
                        lang,
                        "开启后系统将不再弹出通知横幅；定时的自动规则由系统在专注模式页设置",
                        "While on, Windows holds its banners back; the timed automatic rules are set in Focus by the system",
                    )),
                ]);
                rows.push(Row::Do(Do {
                    name: ("Windows 的专注模式设置", "Windows' focus settings"),
                    word: ("定时免打扰与重复规则由系统进行", "The do-not-disturb schedule and its repeating rules are the system's to set"),
                    button: ("打开", "Open"),
                    what: Target::Open("ms-settings:notifications".into()),
                }));
            }
            // Screen time: the day's totals, with the week's bars in the preview.
            (Page::ScreenTime, _) => {
                let lang = self.lang;
                let zh = lang == Lang::Zh;
                rows.extend([
                    Row::Group(pick(lang, "今日使用", "Today")),
                    Row::Info(super::screen_time::today_spoken(zh)),
                    Row::Group(pick(lang, "昨日使用", "Yesterday")),
                    Row::Info(super::screen_time::yesterday_spoken(zh)),
                    Row::Note(pick(
                        lang,
                        "仅记录在最前的程序所用的时间，每 10 秒采集一次，保留 21 天",
                        "Only the time the frontmost program holds is kept, asked every ten seconds for twenty-one days",
                    )),
                ]);
            }
            // Keyboard & mouse.
            (Page::Input, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "键盘", "Keyboard")),
                    Row::Slider(Field::KeyDelay),
                    Row::Slider(Field::KeyRate),
                    Row::Group(pick(lang, "鼠标", "Mouse")),
                    Row::Slider(Field::MouseSpeed),
                    Row::Choice(Field::ScrollLines),
                    Row::Group(pick(lang, "指针", "Pointer")),
                    Row::Info(format!("{} · {}", pick(lang, "光标方案", "Pointer scheme"), super::system_settings::cursor_scheme())),
                ]);
            }
            // Accessibility.
            (Page::Accessibility, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "显示", "Display")),
                    Row::Switch(Switch::HighContrast),
                    Row::Slider(Field::TextSize),
                    Row::Slider(Field::CursorSize),
                    Row::Note(pick(
                        lang,
                        "文字缩放的完全生效需注销并重新登录",
                        "A change to the text size takes full hold after signing out and back in",
                    )),
                ]);
            }
            // Desktop & taskbar.
            (Page::Desktop, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "任务栏", "Taskbar")),
                    Row::Switch(Switch::TaskbarAutohide),
                    Row::Switch(Switch::TaskbarSmall),
                    Row::Choice(Field::TaskbarPosition),
                    Row::Choice(Field::TaskbarAlign),
                    Row::Choice(Field::TaskbarSize),
                    Row::Choice(Field::CombineIcons),
                    Row::Note(pick(
                        lang,
                        "自动隐藏、小图标、对齐与合并即刻生效；位置与大小由资源管理器启动时读取，改后要注销一次才换上",
                        "Hiding, the small icons, the alignment and how buttons combine take hold at once; the edge and the size are read when Explorer starts, so they change at the next sign-in",
                    )),
                ]);
            }
            // Wallpaper: the way it fills the screen, and the picker in the preview.
            (Page::Wallpaper, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Group(pick(lang, "显示方式", "Fit")),
                    Row::Choice(Field::WallpaperStyle),
                    Row::Note(pick(
                        lang,
                        "点右侧缩略图即可换壁纸；图片取自系统的墙纸目录与主题所用",
                        "Press a picture at the right to set it as the wallpaper; they come from Windows' own shelves and the themes'",
                    )),
                ]);
            }
            // Control centre / quick settings.
            (Page::ControlCenter, _) => {
                let lang = self.lang;
                rows.push(Row::Group(pick(lang, "已显示的快捷开关", "Buttons shown")));
                let buttons = super::system_settings::quick_buttons();
                if buttons.is_empty() {
                    rows.push(Row::Info(pick(lang, "快捷开关由系统管理", "The quick buttons are kept by the system").to_string()));
                } else {
                    rows.extend(buttons.into_iter().map(Row::Info));
                }
                rows.push(Row::Do(Do { name: (("编辑快捷开关"), ("Edit the quick buttons")), word: (("增减与排列由系统在任务栏设置中完成"), ("Adding, taking away and the order are done in the taskbar's settings")), button: (("打开"), ("Open")), what: Target::Open("ms-settings:personalization-taskbar".into()) }));
            }
            // Account.
            (Page::Account, _) => {
                let lang = self.lang;
                let zh = lang == Lang::Zh;
                rows.extend([
                    Row::Info(format!("{} · {}", pick(lang, "账户名", "Signed in as"), super::system_settings::display_name())),
                    Row::Group(pick(lang, "登录", "Sign-in")),
                    Row::Switch(Switch::LockScreen),
                ]);
                let _ = zh;
                rows.push(Row::Do(Do { name: (("管理账户"), ("Manage accounts")), word: (("姓名、图片、密码与 Microsoft 账户由系统设置"), ("The name, picture, password and Microsoft account are set by Windows")), button: (("打开"), ("Open")), what: Target::Open("ms-settings:yourinfo".into()) }));
            }
            // About this PC: the machine's own facts, read line by line.
            (Page::About, _) => {
                let lang = self.lang;
                let zh = lang == Lang::Zh;
                rows.push(Row::Group(pick(lang, "设备规格", "Device specifications")));
                rows.extend(super::system_settings::about_facts(zh).into_iter().map(|(name, value)| Row::Info(format!("{name} · {value}"))));
            }
            // The machine's own general settings, gathered as macOS lays them out
            // under 系统设置 > 通用: a sidebar page of its own, under 系统. Each row
            // leads a page kept here; where Windows keeps the thing and Glance does
            // not, a word for the one left out.
            (Page::System, _) => {
                let lang = self.lang;
                rows.extend([
                    Row::Nav("关于本机", "About this PC", "硬件、系统版本与序列号", "Its hardware, system version and serial", Target::Preview(Page::About)),
                    Row::Nav("软件更新", "Software Update", "检查、下载与安装更新", "Checking for and installing updates", Target::Preview(Page::Update)),
                    Row::Nav("储存空间", "Storage", "各磁盘的用量与清理", "What each drive holds, and clearing it", Target::Preview(Page::Storage)),
                    Row::Nav("登录项", "Login Items", "登录时启动的应用", "What starts of its own accord at sign-in", Target::Preview(Page::Startup)),
                    Row::Nav("日期与时间", "Date & Time", "时区、格式与校时", "The zone, the formats, the clock", Target::Preview(Page::DateTime)),
                    Row::Nav("辅助功能", "Accessibility", "高对比度与文字大小", "High contrast and the text's size", Target::Preview(Page::Access)),
                    // The tongue and the date patterns are Glance's to change, so the page is
                    // not among those left to Windows.
                    Row::Nav("语言与地区", "Language & Region", "显示语言、地区格式与时区", "The display language, the formats and the zone", Target::Preview(Page::Language)),
                ]);
                rows.push(Row::Group(pick(lang, "由 Windows 的设置页接管", "Left to Windows' own pages")));
                rows.extend([
                    Row::Nav("隔空投送与接力（就近共享）", "AirDrop & Handoff (nearby sharing)", "就近收发文件、跨机接续任务", "Files to a nearby PC, a task carried across", Target::Preview(Page::Nearby)),
                    Row::Nav("共享（网络和共享中心）", "Sharing (network and sharing)", "文件夹与打印机的共享", "The folders and printers shared out", Target::Preview(Page::Sharing)),
                    Row::Nav("时间机器（Windows 备份）", "Time Machine (Windows Backup)", "备份与还原", "Backup and restore", Target::Preview(Page::Backup)),
                    Row::Nav("启动磁盘（恢复）", "Startup Disk (recovery)", "开机修复与恢复环境", "Startup repair and the recovery tools", Target::Preview(Page::Recovery)),
                ]);
                rows.push(Row::Note(pick(
                    lang,
                    "「传输与还原」需在两台电脑之间用 Microsoft 的传输工具完成，Glance 不代做。",
                    "Moving data in from another machine is done between the two with Microsoft's own transfer tool; Glance leaves this to Windows.",
                )));
            }
            // The hub's sub-items shown only in its right pane: the hub's rows name
            // them, and their preview is drawn from the readings; they turn to no
            // page of their own.
            (Page::Update, _)
            | (Page::Startup, _)
            | (Page::Access, _)
            | (Page::Language, _)
            | (Page::Nearby, _)
            | (Page::Sharing, _)
            | (Page::Backup, _)
            | (Page::Recovery, _) => {}
        }
        rows
    }

    /// The rows of the monitor's tab `tab`, after the tabs.
    fn monitor_rows(&self, rows: &mut Vec<Row>, tab: Tab) {
        match tab {
            Tab::Shown => {
                rows.push(Row::Choice(Field::Columns));
                let module = |rows: &mut Vec<Row>, id: &str| {
                    rows.push(Row::Module(id.to_string()));
                    // A card's rows, while it is open or still closing.
                    if self.opened(id) > 0.0 {
                        rows.extend(self.items_here(id).into_iter().map(|name| Row::Item(id.to_string(), name)));
                        rows.extend(self.fields_here(id).map(|field| Row::ModuleChoice(id.to_string(), field)));
                    }
                };
                let (folded, listed): (Vec<&prefs::ModuleEntry>, Vec<&prefs::ModuleEntry>) =
                    self.prefs.modules.iter().filter(|entry| entry.id != "lyrics").partition(|entry| self.folded(&entry.id));
                for entry in listed {
                    module(rows, &entry.id);
                }
                if !folded.is_empty() {
                    rows.push(Row::More(folded.len()));
                    if self.more_open {
                        for entry in folded {
                            module(rows, &entry.id);
                        }
                    }
                }
            }
            Tab::Overlay => {
                rows.extend([Row::Choice(Field::OverlayShow), Row::Choice(Field::OverlayLayout)]);
                // Rows only for a strip.
                if self.settings.overlay.layout == Layout::Strip {
                    rows.push(Row::Choice(Field::OverlayRows));
                }
                rows.extend([Row::Slider(Field::OverlaySize), Row::Slider(Field::OverlayClear)]);
                // What it shows, group by group, as the panel's modules are.
                // (A battery's only on a machine that has shown one.)
                let battery = crate::app().controller.seen.lock().unwrap().battery;
                for entry in self.settings.overlay.groups.iter().filter(|entry| battery || entry.id != "battery") {
                    let id = format!("{OVERLAY}{}", entry.id);
                    rows.push(Row::Module(id.clone()));
                    if self.opened(&id) > 0.0 {
                        rows.extend(self.items_here(&id).into_iter().map(|name| Row::Item(id.clone(), name)));
                    }
                }
            }
            Tab::Data => rows.extend([Row::Choice(Field::Interval), Row::Choice(Field::Span), Row::Choice(Field::TempAlert), Row::Switch(Switch::HeatAlert)]),
        }
    }

    /// Recent files' rows, while it is on: what it lists from (see `Field::RecentScope`), and by drive, each drive it
    /// reads (listed or left out), else the folders chosen and the row that adds more; then the folders not listed,
    /// while there are any.
    fn recent_rows(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Group(pick(self.lang, "最近文件", "Recent files")));
        rows.push(Row::Choice(Field::RecentScope));
        if self.settings.recent_only {
            rows.extend(self.settings.recent_folders.iter().cloned().map(Row::RecentFolder));
            rows.push(Row::AddRecentFolder);
        } else {
            rows.extend(drives_offered(&self.drives, &self.settings.recent_hidden_drives).into_iter().map(|drive| Row::Drive(drive.volume)));
        }
        if !self.settings.recent_hidden.is_empty() {
            rows.push(Row::Choice(Field::StationHidden));
        }
    }

    /// A drive's row (see `Row::Drive`): its name ("Data (D:)", Windows' name for a drive with no label), whether it is
    /// away, and whether its files are listed.
    fn drive_row(&self, volume: &str) -> (String, Option<&'static str>, bool) {
        let drive = drives_offered(&self.drives, &self.settings.recent_hidden_drives).into_iter().find(|drive| drive.volume == volume);
        let (letter, label) = drive.map_or((' ', String::new()), |drive| (drive.letter, drive.label));
        let label = if label.is_empty() { pick(self.lang, "本地磁盘", "Local Disk").to_string() } else { label };
        let away = !self.drives.iter().any(|drive| drive.volume == volume);
        let listed = !self.settings.recent_hidden_drives.iter().any(|hidden| hidden.volume == volume);
        (format!("{label} ({letter}:)"), away.then(|| pick(self.lang, "未连接", "Not connected")), listed)
    }

    /// `chosen` (in Windows' chooser) added to the folders Recent files lists only, those not among them already.
    fn add_recent_folders(&mut self, chosen: Vec<std::path::PathBuf>) {
        let mut added = false;
        for folder in chosen {
            if !self.settings.recent_folders.contains(&folder) {
                self.settings.recent_folders.push(folder);
                added = true;
            }
        }
        if added {
            self.save();
        }
    }

    /// The phone's rows: the remote on or off, and while it is, what it does and the phones paired with it.
    fn remote_rows(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Switch(Switch::Remote));
        if self.remote {
            rows.extend(remote_switches(self.remote_lights, self.laptop).into_iter().map(Row::Switch));
            // A code shows under the row that asked for it.
            let shown = self.code.as_ref().map(|(_, what)| *what);
            rows.push(Row::Pair);
            if matches!(shown, Some(CodeFor::App | CodeFor::Pairing | CodeFor::WebPairing)) {
                rows.push(Row::Code);
            }
            if self.web_address.is_some() {
                rows.push(Row::WebPage);
                if shown == Some(CodeFor::WebPage) {
                    rows.push(Row::Code);
                }
            }
            // The phones paired, each a row.
            rows.extend(self.phones.iter().map(|phone| Row::Phone(phone.id.clone(), phone.name.clone())));
        }
    }

    /// Shows the monitor's tab `tab`, from its top.
    fn turn_tab(&mut self, tab: Tab) {
        if tab != self.tab {
            self.tab = tab;
            self.scroll = 0.0;
            self.scroll_target = 0.0;
            self.drag = None;
            self.raise_more();
        }
        self.next_frame = Instant::now();
    }

    /// Shows page `page`, from its top.
    /// The page `to` turned to: the overlay's, the workbench's, or that of the reports sent (as they are now).
    fn turn_as_asked(&mut self, to: To) {
        match to {
            To::Overlay => {
                OVERLAY_OWED.store(false, Ordering::Release);
                self.tab = Tab::Overlay;
                self.turn_to(Page::Monitor);
            }
            To::Workbench => {
                WORKBENCH_OWED.store(false, Ordering::Release);
                self.turn_to(Page::Workbench);
            }
            To::Reports => {
                self.followed = crate::reports::list();
                self.turn_to(Page::Reports);
            }
            To::Guide => self.begin_guide(),
            // The notice there, if there is one, marked: what the notification was of.
            To::Updates => {
                self.turn_to(Page::General);
                let mut flash = Transition::settled(1.0);
                flash.retarget(0.0, FLASH.0, FLASH.1, Instant::now());
                self.motion.insert("flash:notice".into(), flash);
            }
        }
    }

    fn turn_to(&mut self, page: Page) {
        if page != self.page {
            self.page = page;
            // Leaving the hub: the sub-item's preview shown goes with it.
            self.preview = None;
            self.scroll = 0.0;
            self.scroll_target = 0.0;
            // The hub's right pane is read from its top again: its scroll goes with the preview.
            self.hub_scroll = 0.0;
            self.hub_scroll_target = 0.0;
            self.hub_tall = 0.0;
            self.drag = None;
            self.open_dropdown = None;
            self.dropdown_anchor = None;
            self.hub_menu = None;
            self.dropdown_scroll = 0.0;
            self.raise_more();
        }
        self.next_frame = Instant::now();
    }

    /// The 通用 hub: pick which sub-item's preview the right pane shows, without
    /// turning to its page.
    fn pick_preview(&mut self, page: Page) {
        self.preview = Some(page);
        // Each sub-item is read from the top of its own pane.
        self.hub_scroll = 0.0;
        self.hub_scroll_target = 0.0;
        self.hub_tall = 0.0;
        self.next_frame = Instant::now();
    }

    /// How far the hub's right pane may go: what its cards last drew past its bottom,
    /// kept between the turns so it eases back to 0 rather than jumping there.
    fn hub_most(&self) -> f32 {
        (self.hub_tall - self.hub_room()).max(0.0)
    }

    /// The height of the hub's right pane: the window's, less the title above it and
    /// the padding under.
    fn hub_room(&self) -> f32 {
        (self.client().1 - PAD_TOP - 40.0 - PAD_TOP).max(0.0)
    }

    /// The hub's right pane scrolled by `scroll`: the clickables the page pushed at
    /// their unshifted places are moved to where the cards were drawn, and those
    /// scrolled clean out of the pane dropped. A hairline thumb marks the pane as
    /// scrollable once its cards outgrow it. The cards themselves are drawn shifted
    /// by the caller, which clips and translates around the paint.
    fn hub_shifted(&mut self, frame: &Frame, palette: &Palette, area: Rect, painted: usize, scroll: f32) {
        // What the page pushed while it was drawn, taken back to be set where it
        // now stands and dropped where the scroll leaves it out of the pane.
        if scroll > 0.0 {
            let laid = self.targets.split_off(painted);
            for (r, target) in laid {
                let moved = Rect { x: r.x, y: r.y - scroll, w: r.w, h: r.h };
                // (Scrolled clean out of the pane: neither drawn there nor clickable.)
                if moved.y + moved.h > area.y - 8.0 && moved.y < area.y + area.h + 8.0 {
                    self.targets.push((moved, target));
                }
            }
        }
        let most = self.hub_most();
        if most <= 0.0 {
            return;
        }
        // A hairline thumb at the pane's edge once the cards outgrow it, so it reads as scrollable.
        let room = area.h;
        let thumb = (room * room / (room + most)).max(24.0);
        let at = area.y + (room - thumb) * (scroll / most).clamp(0.0, 1.0);
        let over = self.pointer.is_some_and(|(x, _)| x >= area.x);
        fill(frame, palette.text3.alpha(if over { 0.7 } else { 0.4 }), area.x + area.w - 1.0, at, 2.0, thumb, 1.0);
    }

    /// The modules of `MORE` listed with the rest from here on: those on; the
    /// fold closed.
    fn raise_more(&mut self) {
        self.raised = self.prefs.modules.iter().filter(|entry| MORE.contains(&entry.id.as_str()) && entry.on).map(|entry| entry.id.clone()).collect();
        self.more_open = false;
    }

    /// Whether module `id` is listed in the fold rather than with the rest.
    fn folded(&self, id: &str) -> bool {
        MORE.contains(&id) && !self.raised.contains(id)
    }

    /// Each row with its top and height, from the top of the list (DIPs).
    fn layout(&self) -> Vec<(Row, f32, f32)> {
        let mut y = PAD_TOP;
        let mut after_heading = false;
        let mut placed = Vec::new();
        // The card whose rows are being placed: its module, where its rows
        // start, and how open it is. Its rows keep their places inside it;
        // what follows it moves up by the part of them still hidden.
        let mut card: Option<(String, f32, f32)> = None;
        for row in self.rows() {
            let inside = match &row {
                Row::Item(id, _) | Row::ModuleChoice(id, _) => Some(id.clone()),
                _ => None,
            };
            if card.as_ref().is_some_and(|(id, ..)| Some(id) != inside.as_ref()) {
                let (_, start, open) = card.take().unwrap();
                y -= (1.0 - open) * (y - start);
            }
            if let Some(id) = inside.filter(|_| card.is_none()) {
                card = Some((id.clone(), y, self.opened(&id)));
            }
            // A heading's space above it takes in the title's below; the
            // first row of a group sits 8 under its heading, the rest 4 apart.
            let (before, height) = match &row {
                Row::Title => (0.0, TITLE),
                Row::Tabs => (0.0, TABS_ROW),
                Row::Note(words) => (12.0, self.gfx.wrapped_height(words, Font::new(Family::Segoe, HINT, 400.0), self.cards() - 8.0) + 8.0),
                Row::Info(words) => (12.0, self.gfx.wrapped_height(words, Font::new(Family::Segoe, HINT, 400.0), self.cards() - 8.0) + 8.0),
                Row::Slider(_) => (if after_heading { 8.0 } else { ROW_GAP }, ROW),
                Row::Skins => (if after_heading { 8.0 } else { ROW_GAP }, SKINS_ROW),
                Row::Library => (if after_heading { 8.0 } else { ROW_GAP }, self.library_height()),
                // The taskbar's edge, chosen from cards of the screen; its size and where
                // its icons stand, from a box that opens a list: a line for their own.
                Row::Choice(Field::TaskbarPosition) => (if after_heading { 8.0 } else { ROW_GAP }, ROW + POSITION_CARDS),
                Row::Choice(field) if matches!(field, Field::TaskbarSize | Field::TaskbarAlign) => (if after_heading { 8.0 } else { ROW_GAP }, ROW),
                // A choice with a word on it: the word above, the choices on
                // a line of their own.
                Row::Choice(field) if self.choice_hint(*field).is_some() => {
                    let options = self.choices(*field).1;
                    let words = self.words_height(&row).max(ROW);
                    let lines = self.choice_lines(&options, self.choices_room()).len() as f32;
                    (if after_heading { 8.0 } else { ROW_GAP }, if self.stacked(&options) { words + CHOICE_LINE * lines } else { words })
                }
                // Inside its module's card, right under the row above.
                // (With a word under its name, as tall as a card's row with one: as much room above and below them.)
                Row::Item(..) => {
                    let words = self.words_height(&row);
                    (0.0, if words > 0.0 { words.max(ROW) } else { ITEM_ROW })
                }
                // Its choices under its name where they do not fit beside it.
                Row::ModuleChoice(_, field) => {
                    let (name, options, _) = self.choices(*field);
                    let room = self.module_choices_room();
                    let under = if self.beside_name(name, &options, room) { 0.0 } else { CHOICE_LINE * self.choice_lines(&options, room).len() as f32 };
                    (0.0, ITEM_ROW + under)
                }
                Row::Choice(field) if self.choice_hint(*field).is_none() && !self.beside_name(self.choices(*field).0, &self.choices(*field).1, self.choices_room()) => {
                    let lines = self.choice_lines(&self.choices(*field).1, self.choices_room()).len() as f32;
                    (if after_heading { 8.0 } else { ROW_GAP }, ROW + CHOICE_LINE * lines)
                }
                // As tall as the code, or as the words beside it where those are taller.
                Row::Code => {
                    let words = self.code.as_ref().map_or(0.0, |(_, what)| 22.0 + self.gfx.wrapped_height(&code_words(self.lang, *what, self.web_address.as_deref()).1, Font::new(Family::Segoe, HINT, 400.0), self.code_words_room()));
                    (ROW_GAP, CODE.max(words) + 2.0 * ROW_SIDE)
                }
                Row::Group(_) => (20.0, GROUP),
                _ => (if after_heading { 8.0 } else { ROW_GAP }, self.words_height(&row).max(ROW)),
            };
            y += before;
            after_heading = matches!(row, Row::Title);
            placed.push((row, y, height));
            y += height;
        }
        if let Some((_, start, open)) = card {
            y -= (1.0 - open) * (y - start);
        }
        let _ = y;
        placed
    }

    /// The preview shows the screen named `screen`: its desktop, at its
    /// size.
    fn show_screen(&mut self, screen: &str) {
        let Some(m) = self.screens.iter().find(|known| known.id == screen).map(|known| known.monitor) else { return };
        let Some(contact) = crate::mouse::monitor_at(POINT { x: (m.left + m.right) / 2, y: (m.top + m.bottom) / 2 }) else { return };
        let work = contact.work;
        let size = (((work.right - work.left) as f32 / contact.scale).round(), ((work.bottom - work.top) as f32 / contact.scale).round());
        // Shown already, and as it is now (its size, scale and place the same).
        let is = |stage: &Stage| stage.screen.as_deref() == Some(screen) && stage.work == work && stage.size == size;
        if is(&self.stage) {
            return;
        }
        // Shown before, as it is now: taken as it was kept; else read from the wallpaper on another thread (shown
        // without it meanwhile: see `take_in`).
        let next = match self.kept_stages.iter().position(is) {
            Some(at) => self.kept_stages.swap_remove(at),
            None => Stage::new(size, Some(screen.to_string()), work, None),
        };
        let before = std::mem::replace(&mut self.stage, next);
        // One kept for each screen, as it last was.
        if let Some(of) = before.screen.clone() {
            self.kept_stages.retain(|kept| kept.screen.as_deref() != Some(of.as_str()));
            self.kept_stages.push(before);
        }
        let known: Vec<&str> = self.screens.iter().map(|known| known.id.as_str()).collect();
        self.kept_stages.retain(|kept| kept.screen.as_deref().is_some_and(|id| known.contains(&id)));
    }

    /// Asks the phone remote's helper `ask` (a code, or what to do), then how the remote is, on another thread (see
    /// `remote_asked`); one at a time, a press meanwhile not taken.
    fn ask_remote(&mut self, ask: impl FnOnce() -> Option<Option<(crate::phone::Code, CodeFor)>> + Send + 'static) {
        if self.remote_asked.as_ref().is_some_and(|asked| !asked.is_finished()) {
            return;
        }
        self.remote_asked = Some(std::thread::spawn(move || {
            let code = ask();
            RemoteAnswer { lights: crate::phone::lights(), raised: crate::phone::lights_raised(), screens: crate::phone::screens_kept(), address: crate::phone::web_address(), code, phones: crate::phone::phones() }
        }));
        self.next_frame = Instant::now();
    }

    /// Reads every screen's desktop as the preview shows it (its wallpaper, at its work area's size), on another
    /// thread (see `read_wanted`): each kept as it comes (see `take_in`).
    fn read_stages(&mut self) {
        let wanted = stages_wanted(&self.screens);
        // As they were last read, at once (read again below, and taken if changed).
        for (screen, work, size) in &wanted {
            if let Some(desktop) = kept_desktop(screen, *work, *size) {
                self.kept_stages.retain(|kept| kept.screen.as_deref() != Some(screen.as_str()));
                self.kept_stages.push(Stage::new(*size, Some(screen.clone()), *work, Some(desktop)));
            }
        }
        for (screen, work, size) in wanted {
            self.reads.want(reads::Ask::Stage { screen, work, size });
        }
    }

    /// The desktops wanted since the last lot went (see `reads::Reads`), read together on another thread, once the lot
    /// before has come or been given up (see `reads::GIVEN_UP_AFTER`: what it had yet to bring is then shown as a
    /// desktop not read is, till the screens or the wallpaper change).
    fn read_wanted(&mut self) {
        let now = Instant::now();
        if self.reads.give_up(now) {
            crate::journal::note(format!("settings: the desktops being read had not come after {} s, given up", reads::GIVEN_UP_AFTER.as_secs()));
        }
        let Some((lot, asks)) = self.reads.lot(now) else { return };
        let send = self.read.0.clone();
        std::thread::spawn(move || read_lot(lot, asks, send));
    }

    /// The screens or the wallpaper changed (see WM_DISPLAYCHANGE and WM_SETTINGCHANGE): the screens as they are now,
    /// the map's pictures and the desktops kept of those no longer where and as large as they were let go, and every
    /// desktop may be read again (one given up too). The wallpaper changed, each desktop shown or kept is read again,
    /// shown as it was till it comes (wanted by what holds it till then: see `take_in` and `map_art_of`, so the screens
    /// changing meanwhile does not lose it).
    fn desktops_changed(&mut self, screens: bool, wallpaper: bool) {
        if screens {
            self.screens = crate::screens::all();
            let present = &self.screens;
            self.map_art.retain(|art| present.iter().any(|screen| screen.id == art.id && screen.monitor == art.monitor));
            let wanted = stages_wanted(present);
            self.kept_stages.retain(|kept| wanted.iter().any(|(screen, work, size)| kept.screen.as_ref() == Some(screen) && kept.work == *work && kept.size == *size));
        }
        self.reads.forget();
        if wallpaper {
            for art in &mut self.map_art {
                art.read_at = None;
            }
            for stage in std::iter::once(&mut self.stage).chain(&mut self.kept_stages) {
                stage.current = false;
            }
        }
        self.next_frame = Instant::now();
    }

    /// The screen the preview shows, by its name: on the layout's page, the
    /// one its editor shows large; on the overlay's, the one
    /// the overlay was put on; and the one this window is on, for the
    /// other pages and until either was.
    fn stage_screen(&self) -> Option<String> {
        let at = |point: POINT| {
            let here = crate::mouse::monitor_at(point)?.monitor;
            self.screens.iter().find(|screen| screen.monitor == here).map(|screen| screen.id.clone())
        };
        let chosen = match self.page {
            Page::Layout => self.editor_screen(),
            Page::Monitor if self.tab == Tab::Overlay => self.settings.overlay.screen.and_then(|(x, y)| at(POINT { x, y })),
            _ => None,
        };
        chosen.or_else(|| {
            let mut window = RECT::default();
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut window) };
            at(POINT { x: (window.left + window.right) / 2, y: (window.top + window.bottom) / 2 })
        })
    }

    /// The component the preview shows: the workbench on its page, what plays on the music's, else the readings.
    fn previewed(&self) -> Part {
        match self.page {
            Page::Workbench => Part::Workbench,
            Page::Music => Part::Music,
            _ => Part::Monitor,
        }
    }

    /// The edge the preview's panel opens from, on the screen it shows: on the desktop's page, the edge Windows
    /// keeps its taskbar on (the panel has no foot of its own, so a taskbar kept there leaves the places to speak,
    /// as on every other page); else the first of that screen's that opens what it shows, else the first that opens
    /// anything; where none does, the one a panel opened by hand opens from.
    fn preview_edge(&self) -> Edge {
        if self.page == Page::Desktop {
            match super::system_settings::taskbar_position() {
                1 => return Edge::Top,
                0 => return Edge::Left,
                2 => return Edge::Right,
                _ => {}
            }
        }
        let places = self.settings.places();
        let screen = self.stage.screen.as_deref().unwrap_or_default();
        let lit = places.lit(screen);
        let part = self.previewed();
        lit.each().find(|edge| places.part(screen, *edge) == Some(part)).or(lit.each().next()).unwrap_or(places.by_hand().0)
    }

    /// The desktop of the screen named `id` at `monitor`, `size` physical
    /// pixels large, for the layout's editor: taken once, and again when its
    /// size changes.
    fn map_picture(&mut self, frame: &Frame, id: &str, monitor: RECT, size: (u32, u32)) -> Option<ID2D1Bitmap1> {
        let art = map_art_of(&mut self.map_art, &mut self.reads, id, monitor, size, self.by_hand);
        if art.bitmap.is_none() {
            art.bitmap = art.desktop.as_ref().and_then(|desktop| desktop.bitmap(&frame.dc, 1.0).ok());
        }
        art.bitmap.clone()
    }

    /// How tall a row has to be for its name and its word, the word broken
    /// into lines in the room it has beside the row's controls; none taller
    /// than a row for a row without a word.
    fn words_height(&self, row: &Row) -> f32 {
        let width = self.cards();
        let font = Font::new(Family::Segoe, 14.0, 400.0);
        let button = |text: &str| self.gfx.measure(text, font) + 32.0;
        let (word, room): (Cow<'static, str>, f32) = match row {
            Row::Switch(switch) => match self.switch(*switch).1 {
                Some(word) => (word, width - 2.0 * ROW_SIDE - 40.0 - 16.0),
                None => return 0.0,
            },
            Row::Choice(field) => {
                let Some(word) = self.choice_hint(*field) else { return 0.0 };
                let options = self.choices(*field).1;
                let room = if self.stacked(&options) { width - 2.0 * ROW_SIDE } else { width - 2.0 * ROW_SIDE - self.segmented_width(&options) - 16.0 };
                (Cow::Borrowed(word), room)
            }
            Row::Drive(volume) => match self.drive_row(volume).1 {
                Some(word) => (Cow::Borrowed(word), width - 2.0 * ROW_SIDE - 40.0 - 16.0),
                None => return 0.0,
            },
            Row::Element(name) => match self.item_label("lyrics", name).1 {
                Some(word) => (Cow::Borrowed(word), width - 2.0 * ROW_SIDE - 40.0 - 16.0),
                None => return 0.0,
            },
            Row::Item(id, name) => match self.item_label(id, name).1 {
                Some(word) => (Cow::Borrowed(word), width - ROW_SIDE - CHEVRON_ROOM - 40.0 - 12.0 - ITEM_INSET),
                None => return 0.0,
            },
            // (Its word beside the grip, left of its switch and chevron: see `module_row`.)
            Row::Module(id) => match self.module(id).1 {
                Some(word) => (Cow::Owned(word), width - ROW_SIDE - CHEVRON_ROOM - 40.0 - 12.0 - ITEM_INSET),
                None => return 0.0,
            },
            // The hub's row: its word beside the chevron that stands where a button would.
            Row::Nav(_, _, hint_zh, hint_en, _) => {
                (Cow::Borrowed(pick(self.lang, hint_zh, hint_en)), width - 2.0 * ROW_SIDE - CHEVRON_ROOM - 12.0)
            }
            _ => match self.button_row(row, Instant::now()) {
                Some((_, word, buttons)) => {
                    let taken: f32 = buttons.iter().map(|(text, ..)| button(text) + 8.0).sum();
                    let icon = if matches!(row, Row::Notice) { NOTICE_ICON_ROOM } else { 0.0 };
                    (Cow::Owned(word), width - 2.0 * ROW_SIDE - taken - 8.0 - icon)
                }
                None => return 0.0,
            },
        };
        // A 20 DIP line for the name, 2 apart, the word's lines, and room
        // above and below.
        22.0 + self.gfx.wrapped_height(&word, Font::new(Family::Segoe, HINT, 400.0), room.max(40.0)) + 18.0
    }

    /// How wide each of a row's choices is.
    fn choice_widths(&self, options: &[String]) -> Vec<f32> {
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        options.iter().map(|o| (self.gfx.measure(o, font) + 24.0).max(44.0)).collect()
    }

    /// How wide a row of choices is, on one line.
    fn segmented_width(&self, options: &[String]) -> f32 {
        self.choice_widths(options).iter().sum::<f32>() + 4.0
    }

    /// A row's choices, line by line: as many whole to a line as fit in `room` (on their own line in the card, all
    /// of its width: more than that, as the players music is followed from may be, ran past its left side).
    fn choice_lines(&self, options: &[String], room: f32) -> Vec<std::ops::Range<usize>> {
        lines_of(&self.choice_widths(options), room)
    }

    /// The room a row's choices have on a line of their own in their card.
    fn choices_room(&self) -> f32 {
        self.cards() - 2.0 * ROW_SIDE
    }

    /// How wide the list's cards are: wider as the window is (see `CARDS`).
    fn cards(&self) -> f32 {
        (CARDS.0 + (self.client().0 - MIN_SIZE.0) * CARDS_GROW).clamp(CARDS.0, CARDS.1)
    }

    /// The list's column: its side's padding, its cards, and the room past them to the preview.
    fn pane(&self) -> f32 {
        PAD_SIDE + self.cards() + LIST_GAP
    }

    /// The room for the words beside a code: what its row has left.
    fn code_words_room(&self) -> f32 {
        self.cards() - 2.0 * ROW_SIDE - CODE - 20.0
    }

    /// Whether a row of choices with a word goes on a line of its own under
    /// its name: when beside it, it would leave the word too narrow a column
    /// to read (a few characters to a line).
    fn stacked(&self, options: &[String]) -> bool {
        self.choices_room() - self.segmented_width(options) - 16.0 < MIN_WORDS
    }

    /// Whether a row of choices named `name` (with no word under it) has them beside its name, `room` across from
    /// where the name begins to where they end: else they go on lines of their own under it (beside it, as in a
    /// longer language or at a larger size, they lay over it).
    fn beside_name(&self, name: &str, options: &[String], room: f32) -> bool {
        self.gfx.measure(name, Font::new(Family::Segoe, 14.0, 400.0)) + 16.0 + self.segmented_width(options) <= room
    }

    /// The room a row of choices has in a module's card, from where its name begins to where they end.
    fn module_choices_room(&self) -> f32 {
        self.choices_room() + 2.0 * ROW_SIDE - ITEM_INSET - ROW_SIDE - CHEVRON_ROOM
    }

    /// Whether the general page has what the user has not seen: a newer Glance, a notice of the site unread, news of a
    /// report sent (marked by a dot beside the page's name).
    fn general_has_news(&self) -> bool {
        update::available().is_some() || crate::notice::unread().is_some() || self.followed.iter().any(crate::reports::Report::unread)
    }

    /// What a row of buttons says, its word, and its buttons, right to left:
    /// each one's text, what it does, and how it looks. None for a row of
    /// another kind, or an update row with no update.
    fn button_row(&self, row: &Row, now: Instant) -> Option<ButtonRow> {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        Some(match row {
            Row::Notice => {
                let notice = crate::notice::unread()?;
                let (title, text) = notice.words(lang);
                let mut buttons = vec![(p("知道了", "Got it"), Target::NoticeRead, Button::Accent)];
                if notice.link.is_some() {
                    buttons.push((p("了解详情", "Learn more"), Target::NoticeLink, Button::Plain));
                }
                (title.to_string(), text.to_string(), buttons)
            }
            // The hub's row is drawn whole and pressed whole (see `paint_list`): its name
            // is told once, not echoed back as a button's word.
            Row::Nav(..) => return None,
            Row::Do(d) => (
                p(d.name.0, d.name.1),
                p(d.word.0, d.word.1),
                vec![(p(d.button.0, d.button.1), d.what.clone(), Button::Accent)],
            ),
            Row::Update => {
                let (version, state) = update::available()?;
                let name = match lang {
                    Lang::Zh => format!("Glance {version} 可用"),
                    Lang::En => format!("Glance {version} is available"),
                };
                let (detail, buttons) = update_offer(state, lang);
                (name, detail, buttons)
            }
            Row::Version => {
                let version = env!("CARGO_PKG_VERSION");
                let name = match lang {
                    Lang::Zh => format!("当前版本 {version}"),
                    Lang::En => format!("Version {version}"),
                };
                let detail = match update::check() {
                    update::Check::Checking => p("正在检查…", "Checking…"),
                    update::Check::Answered if update::available().is_some() => p("发现新版本", "A newer version is available"),
                    update::Check::Answered => p("已是最新版本", "Up to date"),
                    update::Check::Unanswered => p("无法连接更新服务器，请稍后重试", "The update server could not be reached. Try again later"),
                    update::Check::Idle => p("尚未检查", "Not checked yet"),
                };
                let button = if update::check() == update::Check::Checking {
                    (p("检查中", "Checking"), Target::CheckNow, Button::Quiet)
                } else {
                    (p("检查更新", "Check for updates"), Target::CheckNow, Button::Plain)
                };
                (name, detail, vec![button])
            }
            &Row::Shortcut(which) => {
                let (name, what, taken) = match which {
                    Keys::Panel => (p("面板", "Panel"), p("打开或收起面板", "Opens and closes the panel"), crate::tray::hotkey_taken()),
                    Keys::Search => (p("搜索", "Search"), p("在任意位置打开全局搜索", "Opens search from anywhere"), crate::tray::search_hotkey_taken()),
                    Keys::Preview => (p("预览", "Preview"), p("预览指针所指的项目或搜索中选中的结果", "Previews the item under the pointer, or the result picked in search"), crate::ui::look::key_taken()),
                };
                let recording = self.recording == Some(which);
                let shortcut = self.shortcut(which);
                let detail = match (recording, self.needs_modifier, shortcut) {
                    (true, true, _) if which == Keys::Preview => p("组合键须包含 Ctrl 或 Alt", "Include Ctrl or Alt"),
                    (true, true, _) => p("组合键须包含 Ctrl、Alt 或 Win", "Include Ctrl, Alt or Win"),
                    (true, false, _) => p("请按下组合键，按 Esc 取消", "Press the keys. Esc cancels"),
                    (false, _, Some(_)) if taken => p("该组合键已被其他程序占用，请重新设置", "Another program uses these keys. Choose others"),
                    (false, _, _) => what,
                };
                let keys = match (recording, shortcut) {
                    (true, _) => (p("请按键…", "Press keys…"), Target::Shortcut(which), Button::Taking),
                    (false, Some(shortcut)) => (crate::tray::shortcut_name(shortcut), Target::Shortcut(which), Button::Plain),
                    (false, None) => (p("未设置", "None"), Target::Shortcut(which), Button::Plain),
                };
                // With a shortcut set, a button to clear it, right of its keys.
                let mut buttons = Vec::new();
                if shortcut.is_some() {
                    buttons.push((p("清除", "Clear"), Target::ClearShortcut(which), Button::Plain));
                }
                buttons.push(keys);
                (name, detail, buttons)
            }
            Row::Diagnostics => {
                // "Copied" for a moment after the press.
                let copied = self.copied_at.is_some_and(|at| now.duration_since(at) < COPIED_FOR);
                let action = if copied { p("已复制", "Copied") } else { p("复制", "Copy") };
                // What became of the last sending, once there was one.
                let (detail, send) = match (&self.sending, &self.sent) {
                    (Some(_), _) => (p("正在发送…", "Sending…"), (p("发送中", "Sending"), Target::SendReport, Button::Quiet)),
                    (None, Some(Some(id))) => (
                        match lang {
                            Lang::Zh => format!("已发送，编号 {id}。反馈问题时请附上此编号"),
                            Lang::En => format!("Sent as {id}. Quote it when reporting the problem"),
                        },
                        (p("再发一次", "Send again"), Target::SendReport, Button::Plain),
                    ),
                    (None, Some(None)) => (p("发送失败，可复制后附在反馈中", "Sending failed. Copy it into a report instead"), (p("重试", "Retry"), Target::SendReport, Button::Plain)),
                    (None, None) => (p("硬件识别有误，或灯光、显示器无法控制时，可发送或复制诊断信息", "When hardware is read wrongly, or lights or monitors cannot be controlled, send or copy these"), (p("发送", "Send"), Target::SendReport, Button::Plain)),
                };
                (p("诊断信息", "Diagnostics"), detail, vec![(action, Target::Diagnostics, Button::Plain), send])
            }
            Row::MyReports => {
                let unread = self.followed.iter().filter(|report| report.unread()).count();
                let detail = match (lang, unread) {
                    (Lang::Zh, 0) => format!("已发送 {} 份，可查看回复与处理状态", self.followed.len()),
                    (Lang::En, 0) => format!("{} sent. See the replies and how each stands", self.followed.len()),
                    (Lang::Zh, unread) => format!("已发送 {} 份，其中 {unread} 份有新消息", self.followed.len()),
                    (Lang::En, unread) => format!("{} sent, {unread} with news", self.followed.len()),
                };
                let kind = if unread > 0 { Button::Accent } else { Button::Plain };
                (p("我的反馈", "My reports"), detail, vec![(p("查看", "View"), Target::MyReports, kind)])
            }
            Row::Report(id) => {
                let report = self.followed.iter().find(|report| report.id == *id)?;
                let first = report.first_line();
                let name = if first.is_empty() { id.clone() } else { format!("{id} · {first}") };
                let sent = crate::ui::text::Local::at(report.sent).date(lang);
                let detail = match (&self.adding, &self.added) {
                    (Some((adding, _)), _) if adding == id => p("正在发送补充内容…", "Sending what you added…"),
                    (None, Some((added, true))) if added == id => p("补充内容已发送", "What you added was sent"),
                    (None, Some((added, false))) if added == id => p("补充内容发送失败，请稍后重试", "What you added could not be sent. Try again later"),
                    _ => {
                        let status = status_words(lang, &report.status());
                        match (lang, report.unread()) {
                            (Lang::Zh, true) => format!("有新消息 · {status} · {sent}发送"),
                            (Lang::En, true) => format!("News · {status} · sent {sent}"),
                            (Lang::Zh, false) => format!("{status} · {sent}发送"),
                            (Lang::En, false) => format!("{status} · sent {sent}"),
                        }
                    }
                };
                let kind = if report.unread() { Button::Accent } else { Button::Plain };
                (name, detail, vec![(p("打开", "Open"), Target::Thread(id.clone()), kind)])
            }
            Row::NetEase => {
                let netease = self.netease.as_ref()?;
                let install = |text| (text, Target::NetEase(NetEaseWork::Install), Button::Accent);
                let remove = (p("移除", "Remove"), Target::NetEase(NetEaseWork::Remove), Button::Plain);
                let icon_row = |text| (text, Target::NetEase(NetEaseWork::IconRow), Button::Accent);
                let (detail, buttons) = match (self.netease_work.as_ref().map(|(work, _)| *work), self.netease_failed) {
                    (Some(NetEaseWork::Install), _) => (p("正在下载并安装…", "Downloading and installing…"), vec![(p("安装中", "Installing"), Target::NetEase(NetEaseWork::Install), Button::Quiet)]),
                    (Some(NetEaseWork::Remove), _) => (p("正在移除…", "Removing…"), vec![(p("移除中", "Removing"), Target::NetEase(NetEaseWork::Remove), Button::Quiet)]),
                    (Some(NetEaseWork::IconRow), _) => (p("正在更新…", "Updating…"), vec![(p("更新中", "Updating"), Target::NetEase(NetEaseWork::IconRow), Button::Quiet)]),
                    (None, Some((NetEaseWork::Install, crate::netease::Failed::Taken))) => (p("下载失败或文件未通过校验，请稍后重试", "The download failed or did not pass its check. Try again later"), vec![install(p("重试", "Retry"))]),
                    (None, Some((NetEaseWork::Install, crate::netease::Failed::Put))) => (p("未能写入插件文件，安装未完成", "The plugin's files could not be written. It was not installed"), vec![install(p("重试", "Retry"))]),
                    (None, Some((NetEaseWork::Remove, _))) => (p("未能移除，请关闭网易云音乐后重试", "It could not be removed. Close NetEase Cloud Music and try again"), vec![remove]),
                    (None, Some((NetEaseWork::IconRow, _))) => (p("未能写入插件文件，更新未完成", "The plugin's file could not be written. It was not updated"), vec![icon_row(p("重试", "Retry"))]),
                    (None, None) if self.netease_has == crate::netease::Installed::Yes && self.netease_restart => (
                        p("已安装，重新启动网易云音乐后生效", "Installed. It works once NetEase Cloud Music starts again"),
                        vec![(p("重启网易云音乐", "Restart NetEase"), Target::NetEaseRestart, Button::Accent), remove],
                    ),
                    (None, None) if self.netease_has == crate::netease::Installed::Yes => (p("已安装，网易云音乐可提供播放进度以显示同步歌词", "Installed. NetEase Cloud Music says how far into a track it is, for synced lyrics"), vec![remove]),
                    (None, None) if self.netease_has == crate::netease::Installed::NoIconRow => (
                        p(
                            "已安装。BetterNCM 按钮会使网易云音乐右上角的图标排成两行，更新后可排在同一行",
                            "Installed. The BetterNCM button pushes NetEase Cloud Music's icons at the top right onto a second row. Updating puts them back on one",
                        ),
                        vec![icon_row(p("更新", "Update")), remove],
                    ),
                    (None, None) => {
                        let detail = match (lang, netease.tested()) {
                            (Lang::Zh, true) => "安装 InfLink-rs 插件后，网易云音乐可提供播放进度以显示同步歌词".to_string(),
                            (Lang::En, true) => "With the InfLink-rs plugin, NetEase Cloud Music says how far into a track it is, for synced lyrics".to_string(),
                            (Lang::Zh, false) => format!("安装 InfLink-rs 插件后可显示同步歌词。当前版本 {} 未经该插件测试", netease.version()),
                            (Lang::En, false) => format!("With the InfLink-rs plugin, lyrics are synced. It was not tested on version {}", netease.version()),
                        };
                        (detail, vec![install(p("安装", "Install"))])
                    }
                };
                (p("网易云音乐歌词插件", "NetEase Cloud Music lyrics plugin"), detail, buttons)
            }
            Row::Uninstall => (
                p("卸载 Glance", "Uninstall Glance"),
                p("卸载程序并删除设置，执行前会再次确认", "Removes Glance and its settings, after asking"),
                vec![(p("卸载", "Uninstall"), Target::Uninstall, Button::Plain)],
            ),
            Row::Pair => (
                p("配对手机", "Pair a phone"),
                p("安卓手机请先安装 Glance APP，iPhone 请使用网页配对", "On Android, install the Glance app first. On iPhone, pair with the web page"),
                vec![
                    (p("用 APP", "With the app"), Target::Pair(false), Button::Accent),
                    (p("用网页", "With the web"), Target::Pair(true), Button::Plain),
                    (if self.code.as_ref().is_some_and(|(_, what)| *what == CodeFor::App) { p("收起", "Hide") } else { p("下载 APP", "Get the app") }, Target::GetApp, Button::Plain),
                ],
            ),
            Row::WebPage => (
                p("网页遥控", "Web remote"),
                p("已通过网页配对的手机，扫码即可再次打开", "A phone paired with the web page scans this to open it again"),
                vec![(if self.code.as_ref().is_some_and(|(_, what)| *what == CodeFor::WebPage) { p("收起", "Hide") } else { p("显示二维码", "Show the code") }, Target::OpenPage, Button::Plain)],
            ),
            Row::Phone(at, name) => (
                if name.is_empty() { p("未命名的手机", "Unnamed phone") } else { name.clone() },
                p("已配对", "Paired"),
                vec![(p("取消配对", "Unpair"), Target::Unpair(at.clone()), Button::Plain)],
            ),
            // A folder by its name, where it is under it (a drive's top, by itself).
            Row::RecentFolder(folder) => (
                folder.file_name().map_or_else(|| folder.display().to_string(), |name| name.to_string_lossy().into_owned()),
                folder.display().to_string(),
                vec![(p("移除", "Remove"), Target::RecentFolder(folder.clone()), Button::Plain)],
            ),
            Row::AddRecentFolder => (
                p("添加文件夹", "Add a folder"),
                p("所选文件夹的子文件夹一并列出，不列出的文件夹除外", "Their subfolders are listed too, but for folders not listed"),
                vec![(if self.choosing_folders.is_some() { (p("选择中", "Choosing"), Target::AddRecentFolder, Button::Quiet) } else { (p("选择", "Choose"), Target::AddRecentFolder, Button::Plain) })],
            ),
            Row::Guide => (
                p("使用引导", "Guide"),
                p("再次查看如何呼出面板、调整布局与使用快捷键", "Shows again how to open panels, arrange them and use the shortcuts"),
                vec![(p("查看", "Show"), Target::Guide, Button::Plain)],
            ),
            Row::Quit => (
                p("退出 Glance", "Quit Glance"),
                p("面板与托盘图标将一并关闭", "Closes the panels and the tray icon"),
                vec![(p("退出", "Quit"), Target::Quit, Button::Plain)],
            ),
            // A network near: its name, its signal and kind, and what can be done to it here.
            Row::WifiNet(ssid, profile, signal, secure, connected, known) => {
                let state = if *connected {
                    p("已连接", "Connected")
                } else if *known {
                    p("已保存", "Saved")
                } else {
                    p("未连接", "Not connected")
                };
                let detail = if *signal == 0 {
                    state
                } else {
                    format!("{signal}% · {state} · {}", if *secure { p("加密", "Secured") } else { p("开放", "Open") })
                };
                let mut buttons = Vec::new();
                if *connected {
                    buttons.push((p("断开", "Disconnect"), Target::WifiDisconnect, Button::Plain));
                } else {
                    if *known && !profile.is_empty() {
                        buttons.push((p("忘记", "Forget"), Target::WifiForget(profile.clone()), Button::Plain));
                    }
                    let join = if *known && !profile.is_empty() { Target::WifiJoin(profile.clone()) } else { Target::WifiAsk(ssid.clone()) };
                    buttons.push((p("连接", "Join"), join, Button::Accent));
                }
                (ssid.clone(), detail, buttons)
            }
            // A device near or kept: its name and address, and where it is not yet paired, a button to ask Windows.
            Row::BtDevice(name, addr, paired, connected) => {
                let state = if *connected {
                    p("已连接", "Connected")
                } else if *paired {
                    p("已配对", "Paired")
                } else {
                    p("未配对", "Not paired")
                };
                let detail = format!("{addr} · {state}");
                let buttons = if *paired {
                    Vec::new()
                } else {
                    vec![(p("配对", "Pair"), Target::BtPair(name.clone()), Button::Accent)]
                };
                (name.clone(), detail, buttons)
            }
            // An adapter: its name and address, and its state. Nothing to press here — the row's body picks it
            // for the preview.
            Row::NetAdapter(name, up, current) => {
                let state = if *current {
                    p("使用中", "In use")
                } else if *up {
                    p("已连接", "Connected")
                } else {
                    p("未连接", "Not connected")
                };
                let ipv4 = crate::network::snapshot()
                    .and_then(|s| s.adapters.iter().find(|a| a.name == *name).map(|a| a.ipv4.clone()))
                    .filter(|v| !v.is_empty());
                let detail = match ipv4 {
                    Some(v) => format!("{v} · {state}"),
                    None => state,
                };
                (name.clone(), detail, Vec::new())
            }
            // A display: its name and the mode it wears now. Nothing to press here — the row's body picks it for the preview.
            Row::Screen(name, primary) => {
                let state = if *primary { p("主显示器", "Main display") } else { p("显示器", "Display") };
                let mode = crate::sys::current(name)
                    .map(|m| format!("{}×{} · {} Hz", m.wide, m.tall, m.hz))
                    .unwrap_or_default();
                let detail = if mode.is_empty() { state } else { format!("{mode} · {state}") };
                // Titled by the monitor's own name (the model it reports), not the raw GDI path.
                (crate::sys::friendly(name), detail, Vec::new())
            }
            // A sound device: its name, how loud it stands and whether muted. Nothing to press here — the body picks it for the preview.
            Row::AudioDevice(id, name, default) => {
                let read = crate::audio::snapshot().and_then(|s| s.devices.iter().find(|d| &d.id == id).cloned());
                let loud = match &read {
                    Some(d) if d.muted => p("已静音", "Muted"),
                    Some(d) => format!("{}%", d.volume),
                    None => String::new(),
                };
                let detail = if *default && !loud.is_empty() {
                    format!("{loud} · {}", p("默认", "Default"))
                } else if *default {
                    p("默认", "Default").to_string()
                } else {
                    loud
                };
                (name.clone(), detail, Vec::new())
            }
            // An app's notice setting: its name and whether it is let now. Nothing to press here — the body picks it for the preview.
            Row::NotifyApp(display, _, enabled) => {
                let detail = if *enabled { p("允许通知", "Allowed") } else { p("已禁止", "Off") };
                (display.clone(), detail, Vec::new())
            }
            // A privacy capability: its word, and how many apps have asked for it. Nothing to press here
            // — the body picks it to show its apps in the preview.
            Row::ConsentCap(_, zh, en, told) => {
                let detail = match told {
                    Some(0) => p("没有应用留下记录", "No app has left a mark"),
                    Some(n) => if lang == Lang::Zh { format!("{n} 个应用") } else { format!("{n} apps") },
                    None => p("正在读取授权…", "Reading the permissions…"),
                };
                (p(zh, en), detail, Vec::new())
            }
            // A physical disk: its model (or a plain number when it reports none) and the room it holds.
            // Nothing to press here — the body picks it to show its volumes in the preview.
            Row::Disk(number, model, size) => {
                let name = if model.is_empty() { format!("{} {number}", p("磁盘", "Disk")) } else { model.clone() };
                (name, human_bytes(*size), Vec::new())
            }
            _ => return None,
        })
    }

    /// How open module `id`'s card is: 0 closed, 1 open, between while it
    /// slides.
    fn opened(&self, id: &str) -> f32 {
        let open = if self.expanded.contains(id) { 1.0 } else { 0.0 };
        self.motion.get(&format!("open:{id}")).map_or(open, |t| t.value(Instant::now()).clamp(0.0, 1.0))
    }

    /// Opens or closes module `id`'s card, sliding unless `at_once`.
    fn set_open(&mut self, id: &str, open: bool, at_once: bool) {
        if open {
            self.expanded.insert(id.to_string());
        } else {
            self.expanded.remove(id);
        }
        self.animate(format!("open:{id}"), if open { 1.0 } else { 0.0 }, SLIDE, Instant::now(), at_once);
        self.next_frame = Instant::now();
    }

    fn content_height(&self) -> f32 {
        self.layout().last().map_or(0.0, |(_, y, h)| y + h) + PAD_BOTTOM
    }

    /// Choices shown as a box that opens a list rather than as a row of pills: those whose
    /// words are longer than they are few, and the clock's hours, which Windows itself puts
    /// in a list.
    fn dropped(field: Field) -> bool {
        matches!(field, Field::TaskbarSize | Field::TaskbarAlign | Field::ClockHour | Field::DisplayLanguage | Field::ShortDate | Field::LongDate | Field::RegionFormat | Field::Country | Field::TimePattern | Field::TimeZone)
    }

    /// The labels of a row of choices, and which is chosen.
    fn choices(&self, field: Field) -> (&'static str, Vec<String>, Option<usize>) {
        let lang = self.lang;
        let s = |zh: &'static str, en: &'static str| lang.pick(zh, en).to_string();
        let seconds = |n: &str| if lang == Lang::Zh { format!("{n} 秒") } else { format!("{n} s") };
        let minutes = |n: &str| if lang == Lang::Zh { format!("{n} 分") } else { format!("{n} min") };
        let (settings, prefs) = (&self.settings, &self.prefs);
        let at = |values: &[u64], value: u64| values.iter().position(|&v| v == value);
        match field {
            Field::Theme => (
                pick(lang, "明暗", "Theme"),
                vec![s("跟随系统", "System"), s("浅色", "Light"), s("深色", "Dark"), s("跟随背景", "Match background")],
                [ThemePref::System, ThemePref::Light, ThemePref::Dark, ThemePref::Backdrop].iter().position(|&t| t == prefs.theme),
            ),
            Field::Language => (
                pick(lang, "语言", "Language"),
                vec![s("跟随系统", "System"), "中文".into(), "English".into()],
                [LanguagePref::System, LanguagePref::Zh, LanguagePref::En].iter().position(|&l| l == prefs.language),
            ),
            Field::OverlayLayout => (
                pick(lang, "布局", "Layout"),
                vec![s("卡片", "Card"), s("横条", "Strip")],
                [Layout::Card, Layout::Strip].iter().position(|&l| l == settings.overlay.layout),
            ),
            Field::MusicSource => {
                let mut options = vec![s("自动", "Automatic")];
                options.extend(self.sources.iter().map(|(_, name)| name.clone()));
                let chosen = match &settings.music_source {
                    Some(source) => self.sources.iter().position(|(id, _)| *id == source.id).map(|at| at + 1),
                    None => Some(0),
                };
                (pick(lang, "音乐来源", "Music from"), options, chosen)
            }
            Field::OverlayShow => (
                pick(lang, "显示悬浮窗", "Show the overlay"),
                vec![s("关闭", "Off"), s("游戏时", "While playing"), s("始终", "Always")],
                OVERLAY_SHOWN.iter().position(|&when| when == settings.overlay.shown()),
            ),
            Field::OverlayRows => (
                pick(lang, "行数", "Rows"),
                ROWS.iter().map(|rows| rows.to_string()).collect(),
                ROWS.iter().position(|&rows| rows == settings.overlay.rows),
            ),
            Field::Anchor => (
                pick(lang, "面板位置", "Position"),
                vec![s("鼠标处", "At the pointer"), s("边缘居中", "Centred")],
                [Anchor::Pointer, Anchor::Center].iter().position(|&a| a == settings.anchor),
            ),
            Field::OverFullscreen => (
                pick(lang, "全屏游戏中呼出", "Over fullscreen games"),
                vec![s("不呼出", "Never"), s("仅快捷键", "Shortcut"), s("快捷键与边缘", "Shortcut and edge")],
                [OverFullscreen::Never, OverFullscreen::Shortcut, OverFullscreen::Both].iter().position(|&o| o == settings.over_fullscreen),
            ),
            Field::Columns => (
                pick(lang, "面板列数", "Columns"),
                vec![s("自动", "Auto"), "1".into(), "2".into(), "3".into(), "4".into()],
                [None, Some(1), Some(2), Some(3), Some(4)].iter().position(|&c| c == settings.columns_of(self.previewed())),
            ),
            Field::Push => (
                pick(lang, "推边缘呼出", "Push into the edge"),
                vec![s("轻", "Light"), s("中", "Medium"), s("重", "Firm")],
                [Sensitivity::Light, Sensitivity::Medium, Sensitivity::Firm].iter().position(|&p| p == settings.sensitivity),
            ),
            Field::CloseDelay => (
                pick(lang, "离开后收起", "Close after"),
                vec![s("立即", "At once"), seconds("0.5"), seconds("1")],
                at(&[200, 500, 1000], settings.close_delay_ms),
            ),
            Field::RateUnit => (pick(lang, "网速单位", "Speed unit"), vec!["MB/s".into(), "Mbps".into()], Some(prefs.network.bits as usize)),
            Field::PingTarget => (
                pick(lang, "延迟测试地址", "Latency measured to"),
                vec![s("网关", "Gateway"), s("阿里", "Alibaba"), s("腾讯", "Tencent"), "Cloudflare".into()],
                prefs::PingTarget::ALL.iter().position(|&to| to == prefs.network.ping_to(lang)),
            ),
            Field::MusicGround => (
                pick(lang, "背景", "Background"),
                vec![s("封面", "Cover"), s("跟随外观", "Skin"), s("无", "None")],
                MusicGround::ALL.iter().position(|&ground| ground == prefs.music.ground),
            ),
            Field::ProcessCount => (
                pick(lang, "显示进程数", "Processes shown"),
                vec!["5".into(), "8".into(), "12".into()],
                at(&[5, 8, 12], prefs.processes.count as u64),
            ),
            Field::Interval => (
                pick(lang, "刷新间隔", "Refresh interval"),
                vec![seconds("0.5"), seconds("1"), seconds("2")],
                at(&[500, 1000, 2000], settings.interval_ms),
            ),
            Field::Span => (
                pick(lang, "曲线时长", "Chart span"),
                vec![seconds("30"), minutes("1"), minutes("2"), minutes("5")],
                at(&[30, 60, 120, 300], prefs.chart_seconds as u64),
            ),
            Field::OverlaySize | Field::OverlayClear => (self.slider(field).0, Vec::new(), None),
            // (Shown only while there are any: see `rows`.)
            Field::StationHidden => {
                let n = settings.recent_hidden.len();
                (pick(lang, "不列出的文件夹", "Folders not listed"), vec![if lang == Lang::Zh { format!("{n} 个") } else { format!("{n}") }, s("全部恢复", "List all again")], Some(0))
            }
            Field::StationKeep => (
                pick(lang, "未固定项目保留", "Unpinned items kept"),
                vec![s("1 天", "1 day"), s("7 天", "7 days"), s("30 天", "30 days")],
                at(&STATION_DAYS.map(u64::from), u64::from(settings.station.keep_days)),
            ),
            Field::StationCopy => (pick(lang, "拖入的文件", "Files dropped in"), vec![s("仅记位置", "Linked"), s("保留副本", "Copied")], Some(usize::from(settings.station.always_copy))),
            Field::StationAfterDrag => (pick(lang, "拖出以后", "Once dragged out"), vec![s("保留", "Kept"), s("移除", "Removed")], Some(usize::from(settings.station.remove_after_drag))),
            Field::RecentScope => (pick(lang, "列出范围", "Listed from"), vec![s("按磁盘", "By drive"), s("指定文件夹", "Chosen folders")], Some(usize::from(settings.recent_only))),
            Field::TempAlert => (
                pick(lang, "温度警示", "Temperature alert"),
                vec!["75 °C".into(), "85 °C".into(), "95 °C".into()],
                at(&[75, 85, 95], prefs.hot_temp as u64),
            ),
            Field::DockEdge => (
                pick(lang, "位置", "Position"),
                vec![s("屏幕底部", "At the foot of the screen"), s("屏幕顶部", "At the top of the screen")],
                Some(match settings.dock.edge {
                    DockEdge::Bottom => 0,
                    DockEdge::Top => 1,
                }),
            ),
            // (Slid rather than chosen: see `slider`. The grid's 0 is the screen's own choice.)
            Field::DockSize | Field::DockSpacing | Field::DockPeak | Field::DockClear | Field::PadCols | Field::PadRows => (self.slider(field).0, Vec::new(), None),
            Field::SystemTheme => (
                pick(lang, "模式", "Mode"),
                vec![s("浅色", "Light"), s("深色", "Dark")],
                Some(usize::from(super::system_settings::system_dark())),
            ),
            Field::Accent => {
                let names = ACCENTS.iter().map(|(_, zh, en)| s(zh, en)).collect();
                let current = crate::os::accent(false).and_then(|rgb| ACCENTS.iter().position(|(c, ..)| *c == rgb));
                (pick(lang, "强调色", "Accent"), names, current)
            }
            // The power plan, read from the active scheme.
            Field::PowerPlan => {
                let (names, chosen) = super::system_settings::plan_choices(lang == Lang::Zh);
                (pick(lang, "电源模式", "Power mode"), names, chosen)
            }
            Field::ScrollLines => (
                pick(lang, "每次滚动", "Scrolls at a notch"),
                vec![s("一行行", "One line"), s("3 行", "3 lines"), s("5 行", "5 lines"), s("一整屏", "A whole screen")],
                [1u32, 3, 5, u32::MAX].iter().position(|&n| n == super::system_settings::scroll_lines().max(1)),
            ),
            Field::TaskbarPosition => (
                pick(lang, "任务栏位置", "Taskbar position"),
                vec![s("底部", "Bottom"), s("顶部", "Top"), s("左侧", "Left"), s("右侧", "Right")],
                // StuckRects3 numbers edges 0 left, 1 top, 2 right, 3 bottom; shown bottom first.
                Some(match super::system_settings::taskbar_position() {
                    3 => 0,
                    1 => 1,
                    0 => 2,
                    2 => 3,
                    _ => 0,
                }),
            ),
            Field::CombineIcons => (
                pick(lang, "合并任务栏按钮", "Combine taskbar buttons"),
                vec![s("始终", "Always"), s("任务栏满时", "When full"), s("从不", "Never")],
                [0u32, 1, 2].iter().position(|&n| n == super::system_settings::combine_icons()),
            ),
            Field::TaskbarSize => (
                pick(lang, "任务栏大小", "Taskbar size"),
                vec![s("小", "Small"), s("标准", "Medium"), s("大", "Large")],
                [0u32, 1, 2].iter().position(|&n| n == super::system_settings::taskbar_size()),
            ),
            Field::TaskbarAlign => (
                pick(lang, "任务栏图标对齐", "Taskbar icon alignment"),
                vec![s("靠左", "Left"), s("居中", "Center")],
                [0u32, 1].iter().position(|&n| n == super::system_settings::taskbar_align()),
            ),
            Field::WallpaperStyle => (
                pick(lang, "显示方式", "Choose a fit"),
                vec![s("填充", "Fill"), s("适合", "Fit"), s("拉伸", "Stretch"), s("居中", "Center"), s("平铺", "Tile")],
                Some(match super::system_settings::wallpaper_style() {
                    10 => 0,
                    6 => 1,
                    2 => 2,
                    0 => 3,
                    _ => 4,
                }),
            ),
            Field::ClockHour => (
                pick(lang, "小时制", "Hours"),
                vec![s("24 小时", "24-hour"), s("12 小时", "12-hour")],
                Some(if super::system_settings::clock_24() { 0 } else { 1 }),
            ),
            // The tongues are the packs this PC carries, and no others: a language without
            // its pack is not worn whatever is asked, so it is not offered.
            Field::DisplayLanguage => {
                let tags = super::system_settings::ui_languages();
                let names = tags.iter().map(|tag| format!("{} · {tag}", super::system_settings::language_name(tag, lang))).collect();
                (pick(lang, "显示语言", "Display language"), names, super::system_settings::worn_language())
            }
            Field::ShortDate => (
                pick(lang, "短日期", "Short date"),
                SHORT_DATES.iter().map(|pattern| pattern.to_string()).collect(),
                SHORT_DATES.iter().position(|pattern| pattern.eq_ignore_ascii_case(&super::system_settings::short_date())),
            ),
            Field::LongDate => (
                pick(lang, "长日期", "Long date"),
                LONG_DATES.iter().map(|pattern| pattern.to_string()).collect(),
                LONG_DATES.iter().position(|pattern| pattern.eq_ignore_ascii_case(&super::system_settings::long_date())),
            ),
            // The rest of what the locale holds: the format the shapes come from, the
            // country the machine stands in, the clock's shape and the zone it keeps.
            Field::RegionFormat => {
                let tags = super::system_settings::regional_formats();
                let names = tags.iter().map(|tag| format!("{} · {tag}", super::system_settings::format_name(tag))).collect();
                (pick(lang, "地区格式", "Regional format"), names, super::system_settings::current_format())
            }
            Field::Country => {
                let list = super::system_settings::countries();
                let names = list.iter().map(|(_, name, code)| format!("{name} · {code}")).collect();
                (pick(lang, "国家或地区", "Country or region"), names, super::system_settings::current_country())
            }
            Field::TimePattern => (
                pick(lang, "时间", "Time"),
                super::system_settings::time_patterns().iter().map(|pattern| pattern.to_string()).collect(),
                super::system_settings::current_time_pattern(),
            ),
            Field::TimeZone => {
                let list = super::system_settings::zone_choices();
                let names = list.iter().map(|(id, said)| if said.is_empty() { id.clone() } else { said.clone() }).collect();
                (pick(lang, "时区", "Time zone"), names, super::system_settings::current_zone_at())
            }
            // (The rest are slid rather than chosen: see `slider`.)
            Field::ScreenOff | Field::Sleep | Field::KeyDelay | Field::KeyRate | Field::MouseSpeed | Field::TextSize | Field::CursorSize => (self.slider(field).0, Vec::new(), None),
        }
    }

    fn choose(&mut self, field: Field, index: usize) {
        // (A word taken from an open list puts the list away.)
        self.open_dropdown = None;
        let part = self.previewed();
        let (settings, prefs) = (&mut self.settings, &mut self.prefs);
        match field {
            Field::Theme => prefs.theme = [ThemePref::System, ThemePref::Light, ThemePref::Dark, ThemePref::Backdrop][index],
            Field::Language => prefs.language = [LanguagePref::System, LanguagePref::Zh, LanguagePref::En][index],
            Field::Anchor => settings.anchor = [Anchor::Pointer, Anchor::Center][index],
            Field::OverlayLayout => settings.overlay.layout = [Layout::Card, Layout::Strip][index],
            Field::OverlayRows => settings.overlay.rows = ROWS[index],
            Field::OverlayShow => settings.overlay.show(OVERLAY_SHOWN[index]),
            Field::MusicSource => {
                settings.music_source = index.checked_sub(1).and_then(|at| self.sources.get(at)).map(|(id, name)| crate::settings::MusicSource { id: id.clone(), name: name.clone() });
            }
            Field::Columns => settings.set_columns(part, [None, Some(1), Some(2), Some(3), Some(4)][index]),
            Field::OverFullscreen => settings.over_fullscreen = [OverFullscreen::Never, OverFullscreen::Shortcut, OverFullscreen::Both][index],
            Field::Push => settings.sensitivity = [Sensitivity::Light, Sensitivity::Medium, Sensitivity::Firm][index],
            Field::CloseDelay => settings.close_delay_ms = [200, 500, 1000][index],
            Field::RateUnit => prefs.network.bits = index == 1,
            Field::PingTarget => prefs.network.ping = Some(prefs::PingTarget::ALL[index]),
            Field::MusicGround => prefs.music.ground = MusicGround::ALL[index],
            Field::ProcessCount => prefs.processes.count = [5, 8, 12][index],
            Field::Interval => settings.interval_ms = [500, 1000, 2000][index],
            Field::Span => prefs.chart_seconds = [30.0, 60.0, 120.0, 300.0][index],
            Field::OverlaySize | Field::OverlayClear => {}
            Field::TempAlert => prefs.hot_temp = [75.0, 85.0, 95.0][index],
            Field::StationHidden if index == 1 => settings.recent_hidden.clear(),
            Field::StationHidden => {}
            Field::StationKeep => settings.station.keep_days = STATION_DAYS[index],
            Field::StationCopy => settings.station.always_copy = index == 1,
            Field::StationAfterDrag => settings.station.remove_after_drag = index == 1,
            Field::RecentScope => settings.recent_only = index == 1,
            Field::DockEdge => settings.dock.edge = [DockEdge::Bottom, DockEdge::Top][index],
            // (The dock's and the grid's are set on a slider, not chosen: see `set_slider`.)
            Field::DockSize | Field::DockSpacing | Field::DockPeak | Field::DockClear | Field::PadCols | Field::PadRows => {}
            Field::SystemTheme => super::system_settings::set_system_dark(index == 1),
            Field::Accent => {
                if let Some((rgb, _, _)) = ACCENTS.get(index) {
                    super::system_settings::set_accent_colour(*rgb);
                }
            }
            Field::PowerPlan => super::system_settings::set_plan(index),
            Field::ScrollLines => super::system_settings::set_scroll_lines([1, 3, 5, u32::MAX][index]),
            Field::TaskbarPosition => super::system_settings::set_taskbar_position([3, 1, 0, 2][index]),
            Field::CombineIcons => super::system_settings::set_combine_icons([0, 1, 2][index]),
            Field::TaskbarSize => super::system_settings::set_taskbar_size([0, 1, 2][index]),
            Field::TaskbarAlign => super::system_settings::set_taskbar_align([0, 1][index]),
            Field::WallpaperStyle => super::system_settings::set_wallpaper_style([10, 6, 2, 0, 0][index]),
            Field::ClockHour => super::system_settings::set_clock_24(index == 0),
            Field::DisplayLanguage => {
                if let Some(tag) = super::system_settings::ui_languages().get(index).cloned() {
                    super::system_settings::set_preferred_language(&tag);
                }
            }
            Field::ShortDate => super::system_settings::set_short_date(SHORT_DATES[index]),
            Field::LongDate => super::system_settings::set_long_date(LONG_DATES[index]),
            Field::RegionFormat => {
                if let Some(tag) = super::system_settings::regional_formats().get(index).cloned() {
                    super::system_settings::set_regional_format(&tag);
                }
            }
            Field::Country => {
                if let Some((id, _, code)) = super::system_settings::countries().get(index) {
                    super::system_settings::set_country(*id, code);
                }
            }
            Field::TimePattern => {
                if let Some(pattern) = super::system_settings::time_patterns().get(index) {
                    super::system_settings::set_time_pattern(pattern);
                }
            }
            Field::TimeZone => {
                if let Some((id, _)) = super::system_settings::zone_choices().get(index) {
                    super::system_settings::set_zone(id);
                }
            }
            // (Slid rather than chosen: see `set_slider`.)
            Field::ScreenOff | Field::Sleep | Field::KeyDelay | Field::KeyRate | Field::MouseSpeed | Field::TextSize | Field::CursorSize => {}
        }
        self.save();
        match field {
            Field::Theme => self.restyle(),
            // Everything is relabelled in the new language once the thumb
            // has slid; mid-way it would jump.
            Field::Language => self.relabel_at = Some(Instant::now() + SLIDE.0),
            _ => {}
        }
    }

    /// Starts or stops taking a new shortcut. Meanwhile the old one is let
    /// go, or its keys would never come here.
    fn record(&mut self, which: Option<Keys>) {
        if which == self.recording {
            return;
        }
        self.recording = which;
        self.needs_modifier = false;
        crate::tray::hold_hotkey(which.is_some());
        self.next_frame = Instant::now();
    }

    /// The shortcut `which` as it is set.
    fn shortcut(&self, which: Keys) -> Option<Shortcut> {
        match which {
            Keys::Panel => self.settings.shortcut,
            Keys::Search => self.settings.search_shortcut,
            Keys::Preview => self.settings.preview_shortcut,
        }
    }

    /// The shortcut `which` set to `to` (none: nothing opens what it opened by keys).
    fn set_shortcut(&mut self, which: Keys, to: Option<Shortcut>) {
        match which {
            Keys::Panel => self.settings.shortcut = to,
            Keys::Search => self.settings.search_shortcut = to,
            Keys::Preview => self.settings.preview_shortcut = to,
        }
        self.save();
        self.record(None);
    }

    /// A key pressed while taking a new shortcut: Escape gives up, Backspace
    /// or Delete clears the shortcut, a modifier alone waits for the key it
    /// goes with, and a key with Ctrl, Alt or Win held becomes the shortcut.
    fn take_shortcut(&mut self, which: Keys, key: VIRTUAL_KEY) {
        self.next_frame = Instant::now();
        let down = |key: VIRTUAL_KEY| unsafe { GetKeyState(key.0 as i32) } < 0;
        let modifiers = [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN];
        if key == VK_ESCAPE {
            self.record(None);
            return;
        }
        if key == VK_BACK || key == VK_DELETE {
            self.set_shortcut(which, None);
            return;
        }
        if modifiers.contains(&key) {
            return;
        }
        let shortcut = Shortcut { ctrl: down(VK_CONTROL), alt: down(VK_MENU), shift: down(VK_SHIFT), win: down(VK_LWIN) || down(VK_RWIN), key: key.0 };
        // (The preview's is pressed as words are typed in search's field: Win alone would leave a key that types.)
        if if which == Keys::Preview { !shortcut.previews() } else { !shortcut.usable() } {
            // Noted, for a report of a shortcut that would not take.
            crate::journal::note(format!("shortcut: key 0x{:02X} without the modifiers it needs", key.0));
            self.needs_modifier = true;
            return;
        }
        self.set_shortcut(which, Some(shortcut));
    }

    /// How large the panel of what the preview shows is drawn on the screen the window shows (see
    /// `Settings::panel_size_on`).
    fn panel_size(&self) -> f32 {
        self.settings.panel_size_on(self.previewed(), self.stage.screen.as_deref())
    }

    /// A slider's name, its least and most values, its step, and its value.
    fn slider(&self, field: Field) -> (&'static str, f32, f32, f32, f32) {
        let lang = self.lang;
        let (sizes, overlay) = (SIZES, &self.settings.overlay);
        match field {
            Field::OverlayClear => (pick(lang, "玻璃透明度", "Glass clarity"), 0.0, 1.0, 0.05, overlay.clear.clamp(0.0, 1.0)),
            // The dock's own, in the units it is measured in: a tile's side and the room between them in DIPs, the
            // swell as itself (1: not at all), the glass as how much of it is gone.
            Field::DockSize => (pick(lang, "图标大小", "Icon size"), 24.0, 96.0, 2.0, self.settings.dock.size.clamp(24.0, 96.0)),
            Field::DockSpacing => (pick(lang, "图标间距", "Room between icons"), 0.0, 40.0, 1.0, self.settings.dock.spacing.clamp(0.0, 40.0)),
            Field::DockPeak => (pick(lang, "悬停放大", "Swell under the pointer"), 1.0, 2.2, 0.05, self.settings.dock.peak.clamp(1.0, 2.2)),
            Field::DockClear => (pick(lang, "玻璃透明度", "Glass clarity"), 0.0, 1.0, 0.05, self.settings.dock.clear.clamp(0.0, 1.0)),
            // (The launchpad's grid: tiles across a page, and rows of them, one at a step, 0 for the screen's own
            // choice — the denser the ask, the smaller every tile comes out (see `crate::ui::launchpad`).)
            Field::PadCols => (pick(lang, "每页列数", "Tiles across a page"), 0.0, 12.0, 1.0, self.settings.dock.pad_cols as f32),
            Field::PadRows => (pick(lang, "每页行数", "Rows of tiles"), 0.0, 10.0, 1.0, self.settings.dock.pad_rows as f32),
            // The machine's own settings, read live and written back to the registry.
            Field::ScreenOff => (pick(lang, "关闭屏幕前", "Turn off after"), 0.0, 60.0, 1.0, super::system_settings::screen_timeout() as f32),
            Field::Sleep => (pick(lang, "进入睡眠前", "Sleep after"), 0.0, 60.0, 1.0, super::system_settings::sleep_timeout() as f32),
            Field::KeyDelay => (pick(lang, "重复前的延迟", "Delay before repeat"), 0.0, 3.0, 1.0, super::system_settings::key_delay() as f32),
            Field::KeyRate => (pick(lang, "重复速率", "Repeat rate"), 0.0, 31.0, 1.0, super::system_settings::key_rate() as f32),
            Field::MouseSpeed => (pick(lang, "指针速度", "Pointer speed"), 0.0, 20.0, 1.0, super::system_settings::mouse_speed() as f32),
            Field::TextSize => (pick(lang, "文字大小", "Text size"), 100.0, 225.0, 25.0, super::system_settings::text_scale() as f32),
            Field::CursorSize => (pick(lang, "光标大小", "Pointer size"), 1.0, 15.0, 1.0, super::system_settings::cursor_size() as f32),
            _ => (pick(lang, "大小", "Size"), sizes.0, sizes.1, 0.05, overlay.size.clamp(sizes.0, sizes.1)),
        }
    }

    /// A slider's value, as its row says it: a share, except where the dock is measured in its own units.
    fn slider_shown(&self, field: Field, value: f32) -> String {
        match field {
            Field::DockSize | Field::DockSpacing => format!("{value:.0} {}", pick(self.lang, "像素", "px")),
            Field::DockPeak => format!("{value:.2}×"),
            // (The launchpad's grid: none asked for is the screen's own choice, said rather than shown as a 0.)
            Field::PadCols | Field::PadRows if value < 0.5 => pick(self.lang, "自动", "Automatic").to_string(),
            Field::PadCols | Field::PadRows => format!("{value:.0}"),
            // The machine's own sliders, said in their own units.
            Field::ScreenOff | Field::Sleep if value < 0.5 => pick(self.lang, "从不", "Never").to_string(),
            Field::ScreenOff | Field::Sleep => format!("{value:.0} {}", pick(self.lang, "分", "min")),
            Field::TextSize => format!("{value:.0}%"),
            Field::KeyDelay | Field::KeyRate | Field::MouseSpeed | Field::CursorSize => format!("{value:.0}"),
            _ => format!("{:.0}%", value * 100.0),
        }
    }

    /// Sets a slider's value, to its nearest step; the panel and the overlay
    /// show it at once, with the settings as they are held (it is kept by
    /// `keep_slider`).
    fn set_slider(&mut self, field: Field, value: f32) {
        let (_, min, max, step, now) = self.slider(field);
        let value = (min + ((value - min) / step).round() * step).clamp(min, max);
        // (Slid within a step, nothing changes: the panel and the overlay are not made to draw themselves anew
        // at every move of the pointer.)
        if value == now {
            return;
        }
        // The machine's own sliders write the registry, not Glance's settings.
        match field {
            Field::ScreenOff => super::system_settings::set_screen_timeout(value as u32),
            Field::Sleep => super::system_settings::set_sleep_timeout(value as u32),
            Field::KeyDelay => super::system_settings::set_key_delay(value as u32),
            Field::KeyRate => super::system_settings::set_key_rate(value as u32),
            Field::MouseSpeed => super::system_settings::set_mouse_speed(value as u32),
            Field::TextSize => super::system_settings::set_text_scale(value as u32),
            Field::CursorSize => super::system_settings::set_cursor_size(value as u32),
            _ => {
                put_slider(&mut self.settings, field, value);
                crate::app().preview(|live| put_slider(live, field, value));
            }
        }
        self.next_frame = Instant::now();
    }

    /// Keeps a slider's value, and only it: the settings changed elsewhere
    /// while it slid (the overlay placed, a game's offer made) stand.
    fn keep_slider(&mut self, field: Field) {
        // (The machine's own sliders are already written to the registry; nothing of Glance's settings holds them.)
        if matches!(field, Field::ScreenOff | Field::Sleep | Field::KeyDelay | Field::KeyRate | Field::MouseSpeed | Field::TextSize | Field::CursorSize) {
            self.next_frame = Instant::now();
            return;
        }
        let value = self.slider(field).4;
        let mut now = None;
        crate::app().change(|held| {
            put_slider(held, field, value);
            now = Some(held.clone());
        });
        self.take(now.unwrap());
        self.next_frame = Instant::now();
    }

    /// A slider slid to `x` along its `track`.
    fn slide(&mut self, field: Field, track: Rect, x: f32) {
        let (_, min, max, ..) = self.slider(field);
        let share = ((x - track.x - SLIDER_THUMB) / (track.w - 2.0 * SLIDER_THUMB)).clamp(0.0, 1.0);
        self.set_slider(field, min + share * (max - min));
    }

    /// A word under a row of choices' name, for the few that need one.
    fn choice_hint(&self, field: Field) -> Option<&'static str> {
        match field {
            Field::MusicSource => Some(pick(
                self.lang,
                "自动时仅跟随音乐播放器。在网页中听歌时，请选择所用的浏览器。时间与媒体不受此项影响",
                "Automatic follows music players only. For music in a web page, choose the browser. The time and media are not affected",
            )),
            Field::MusicGround => Some(pick(
                self.lang,
                "封面为模糊的专辑封面，跟随外观时与其他组件的底色、文字颜色一致，无背景时文字加描边",
                "The cover blurred, the skin's ground and ink as every other widget has, or none with its words edged",
            )),
            Field::RecentScope => Some(pick(
                self.lang,
                "选择指定文件夹时，仅列出下列文件夹及其子文件夹中的文件",
                "Chosen folders lists only the files in the folders below and their subfolders",
            )),
            Field::Push => Some(pick(self.lang, "鼠标抵住屏幕边缘后再向外推，即可呼出面板。力度越重，越不易误触", "Push the pointer on past the screen's edge to open the panel. Firmer is harder to set off by accident")),
            Field::OverFullscreen => Some(pick(
                self.lang,
                "包括独占全屏与无边框全屏的游戏。独占全屏的游戏会暂时切出，收起面板后自动恢复",
                "Exclusive and borderless fullscreen games alike. An exclusive one steps out until the panel closes",
            )),
            _ => None,
        }
    }

    /// A switch's label, its hint, and whether it is on.
    fn switch(&self, switch: Switch) -> (&'static str, Option<Cow<'static, str>>, bool) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en);
        let (name, hint, on) = match switch {
            Switch::Live => (
                p("背景实时刷新", "Live background"),
                Some(p("玻璃背后的画面随桌面实时更新，背景变化时会占用一定的处理器与显卡资源", "What shows through the glass follows the desktop as it changes, at some cost in processor and graphics time")),
                self.settings.live_backdrop,
            ),
            Switch::TearOff => (
                p("拖出面板成为小组件", "Drag the panel off as a widget"),
                Some(p("按住面板拖离屏幕边缘，即可将其放在桌面上", "Drag the panel away from the screen's edge to keep it on the desktop")),
                self.settings.tear_off,
            ),
            Switch::DockOn => (
                p("显示 Dock", "Show the dock"),
                Some(p("沿屏幕边缘放一排图标：程序、文件、文件夹，以及监控、工作台、音乐、悬浮窗、设置与回收站", "A row of icons along the screen's edge: programs, files and folders, and the doors of the monitors, the workbench, what plays, the overlay and the settings, and the Recycle Bin")),
                self.settings.dock.on,
            ),
            Switch::DockHide => (
                p("自动隐藏", "Hide until the pointer comes"),
                Some(p("鼠标移到它所在的那条屏幕边缘才出现，离开片刻后收起", "It comes out when the pointer reaches the screen's edge it stands at, and goes back in once the pointer has left")),
                self.settings.dock.auto_hide,
            ),
            Switch::DockLock => (
                p("锁定位置", "Hold it in place"),
                Some(p("不能整条拖动图标栏改变位置或换到屏幕另一头；单个图标仍可拖动排序", "The bar cannot be dragged to another place or edge; a tile may still be dragged along the row")),
                self.settings.dock.locked,
            ),
            Switch::DockThrough => (
                p("点击穿透", "Clicks pass through"),
                Some(p("Dock 不再接收鼠标点击。开启后无法在 Dock 上右键改回，请在此处关闭", "The dock takes no clicks. Once it is on, the dock's own menu cannot be opened to turn it back off — do it here")),
                self.settings.dock.click_through,
            ),
            Switch::DockMagnify => (
                p("悬停放大", "Swell under the pointer"),
                Some(p("鼠标经边的图标放大，并带动相邻的图标", "The icon the pointer passes grows, and its neighbours with it")),
                self.settings.dock.magnify,
            ),
            Switch::DockRunning => (
                p("运行标记", "Dots under what is running"),
                Some(p("已在运行的程序图标下方显示一个小圆点", "A small dot under the icon of what is running")),
                self.settings.dock.running,
            ),
            Switch::DockShowOpen => (
                p("显示已打开的窗口", "Show open windows"),
                Some(p("已打开但未列入图标栏的程序也显示在末尾，点击可切换到其窗口", "Programs with a window open, kept nowhere on the row, are shown at its end; a press goes to their window")),
                self.settings.dock.show_open,
            ),
            // Only a copy no ordinary program can replace (one the installer
            // put in Program Files) may start elevated unasked.
            Switch::Startup => (
                p("开机时启动", "Start with Windows"),
                (!self.may_autostart).then(|| {
                    p("当前安装位置不受保护，无法启用。点击查看原因", "Unavailable in this location. Click for details")
                }),
                self.autostart,
            ),
            Switch::TrayHidden => (
                p("隐藏托盘图标", "Hide the tray icon"),
                Some(p("隐藏后，再次启动 Glance 即可打开设置，通知仍会显示", "With it hidden, starting Glance again opens the settings. Notifications still show")),
                self.settings.tray_hidden,
            ),
            Switch::HeatAlert => (
                p("过热提醒", "Heat alert"),
                Some(p("CPU 或 GPU 温度持续 30 秒达到警示值时，通过托盘通知", "A tray notice once the CPU or a GPU has been at the alert for 30 seconds")),
                self.settings.heat_alert,
            ),
            Switch::RemoteLights => {
                let hint = if self.fetching.is_some() {
                    p("正在下载，请稍候…", "Downloading…")
                } else if self.fetch_failed {
                    p("下载失败，请稍后重试", "The download failed. Try again later")
                } else {
                    p("主板、显卡等的 RGB 灯光通过开源软件 OpenRGB 控制。开启时从其官方地址下载（约 22 MB），关闭时删除", "Board and card RGB lights are set through OpenRGB, an open-source program. Turned on, it is downloaded from its project (about 22 MB). Turned off, it is removed")
                };
                return (p("RGB 灯光控制", "RGB lighting"), Some(Cow::Borrowed(hint)), self.remote_lights);
            }
            Switch::RemoteRaised => {
                let hint = if self.raising.is_some() {
                    p("正在处理，请稍候…", "Working…")
                } else if self.raise_failed {
                    p("操作失败，请稍后重试", "That failed. Try again later")
                } else {
                    p(
                        "内存灯光须由具有管理员权限的 OpenRGB 控制。开启后，OpenRGB 将随 Glance 以管理员权限运行。其他厂商的灯光软件可能覆盖此处的设置",
                        "Memory lights can only be set by OpenRGB with administrator rights. When on, OpenRGB runs with those rights alongside Glance. Other makers' lighting programs may override what is set here",
                    )
                };
                return (p("内存灯光控制", "Memory lighting"), Some(Cow::Borrowed(hint)), self.remote_raised);
            }
            Switch::RemoteScreens => (
                p("关屏时保持显示器连接", "Keep monitors connected while off"),
                Some(p(
                    "避免关屏后显示器切换输入源，或窗口移到其他屏幕。可单独关闭的显示器照常关闭，其余显示器改为黑屏并调至最暗，背光保持开启",
                    "Keeps a monitor from switching its input, or windows from moving to other screens, while it is off. A monitor that can be turned off alone still is. The others go black and as dim as they go, their backlight on",
                )),
                self.remote_screens,
            ),
            Switch::Remote => {
                let hint = if self.remote_there {
                    p("通过手机关闭屏幕与灯光，调节音量与亮度。仅在本地网络中可用", "Screens, lights, volume and brightness from a phone, over the local network only")
                } else {
                    p("此版本的 Glance 未包含手机互联程序", "This copy of Glance came without the phone remote")
                };
                return (p("开启手机互联", "Phone remote"), Some(Cow::Borrowed(hint)), self.remote);
            }
            Switch::Seconds => (p("显示秒数", "Show seconds"), None, super::system_settings::clock_seconds()),
            Switch::AnimOn => (
                p("动画效果", "Animations"),
                Some(p("最小化、淡入这些过渡，系统一起跟着", "Minimise, fade and the like, the system follows them")),
                super::system_settings::system_animation(),
            ),
            Switch::Glass => (
                p("透明效果", "Transparency"),
                Some(p("开始、任务栏与窗口的磨砂玻璃", "Frosted glass on Start, the taskbar, and windows")),
                super::system_settings::system_transparent(),
            ),
            Switch::AccentOn => (
                p("在开始、任务栏和窗口边框上用强调色", "Accent on Start, taskbar, and borders"),
                None,
                super::system_settings::color_prevalence(),
            ),
            // The machine's own switches, read live.
            Switch::WifiOn => (
                p("WLAN", "Wi-Fi"),
                Some(p("无线网络的开关；关闭后本机不再连接任何无线网络", "The wireless radio's own switch; with it off, nothing is joined over Wi-Fi")),
                crate::wifi::state().unwrap_or(false),
            ),
            Switch::BluetoothOn => (
                p("蓝牙", "Bluetooth"),
                Some(p("蓝牙无线电的开关；关闭后本机不再连接任何蓝牙设备", "The Bluetooth radio's own switch; with it off, no Bluetooth device is in touch")),
                crate::bluetooth::on(),
            ),
            Switch::FocusOn => (
                p("专注模式（勿扰）", "Do not disturb"),
                Some(p("开启后系统挂起通知横幅，直至关闭", "While on, Windows holds its banners back until it is turned off")),
                super::system_settings::dnd_on(),
            ),
            Switch::FocusLock => (
                p("锁屏时显示通知", "Notices above the lock screen"),
                Some(p("允许通知在锁屏上出现；关闭后锁屏保持安静", "Whether notices may show on the lock screen; with it off, the lock screen stays quiet")),
                super::system_settings::lock_toast(),
            ),
            Switch::HighContrast => (
                p("高对比度", "High contrast"),
                Some(p("以强烈配色显示界面，适合视力较弱时阅读", "Shows the interface in strong colours, easier to read")),
                super::system_settings::high_contrast(),
            ),
            Switch::TaskbarAutohide => (
                p("自动隐藏任务栏", "Auto-hide the taskbar"),
                Some(p("指针移到任务栏所在边缘时才出现", "It shows when the pointer reaches the edge it stands at")),
                super::system_settings::taskbar_autohide(),
            ),
            Switch::TaskbarSmall => (
                p("小任务栏图标", "Small taskbar icons"),
                None,
                super::system_settings::taskbar_small(),
            ),
            Switch::LockScreen => (
                p("唤醒时显示锁屏界面", "Show the lock screen on wake"),
                None,
                super::system_settings::lock_screen_on(),
            ),
            Switch::GameMode => (
                p("游戏模式", "Game Mode"),
                Some(p("玩游戏时让系统把资源优先分给游戏", "While gaming, Windows gives the game first call on resources")),
                super::system_settings::game_mode(),
            ),
            Switch::GameDvr => (
                p("游戏 DVR", "Game DVR"),
                Some(p("在后台录制游戏片段的记录器", "The recorder that captures game clips in the background")),
                super::system_settings::game_flag("AppCaptureEnabled"),
            ),
            // The version this is, where the user looks for a newer one.
            Switch::Updates => {
                return (p("自动检查更新", "Check for updates"), Some(Cow::Borrowed(p("每天检查一次，并向 Glance 网站告知当前版本，用于统计使用情况。发送过诊断信息时，一并获取其回复。不包含可识别这台电脑的信息", "Once a day. Glance's site is told this version, to count how many run it, and asked for replies to the diagnostics sent. Nothing that tells this PC from another"))), self.settings.check_updates);
            }
        };
        (name, hint.map(Cow::Borrowed), on)
    }

    /// Why Start with Windows cannot be turned on for this copy, and how it can.
    fn explain_no_autostart(&self) {
        let place = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.display().to_string())).unwrap_or_default();
        let (title, text) = match self.lang {
            Lang::Zh => (
                "无法启用开机时启动".to_string(),
                format!(
                    "Glance 当前位于：\n{place}\n\n这个位置不受保护：普通程序可以修改它或它的上级文件夹，因此可能把 Glance 替换掉。开机时启动会让 Glance 不经确认就以管理员身份运行，只有在受保护的位置才能安全地这样做。\n\n如需启用，请使用安装程序将 Glance 安装到 Program Files，或磁盘根目录下的新文件夹（例如 D:\\Glance）。"
                ),
            ),
            Lang::En => (
                "Start with Windows is unavailable".to_string(),
                format!(
                    "Glance is currently located in:\n{place}\n\nThis location is not protected: ordinary programs can change it or a folder above it, and so could replace Glance. Starting with Windows runs Glance with administrator rights without asking, which is safe only from a protected location.\n\nTo turn it on, use the installer to install Glance to Program Files, or to a new folder at the root of a drive (for example D:\\Glance)."
                ),
            ),
        };
        unsafe { MessageBoxW(Some(self.hwnd), &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_ICONINFORMATION) };
    }

    fn flip(&mut self, switch: Switch) {
        let settings = &mut self.settings;
        match switch {
            Switch::Live => settings.live_backdrop ^= true,
            Switch::TearOff => settings.tear_off ^= true,
            Switch::DockOn => settings.dock.on ^= true,
            Switch::DockHide => settings.dock.auto_hide ^= true,
            Switch::DockLock => settings.dock.locked ^= true,
            Switch::DockThrough => settings.dock.click_through ^= true,
            Switch::DockMagnify => settings.dock.magnify ^= true,
            Switch::DockRunning => settings.dock.running ^= true,
            Switch::DockShowOpen => settings.dock.show_open ^= true,
            // The machine's own clock switches write the registry and go on: nothing of Glance's settings changes.
            Switch::Seconds => super::system_settings::toggle_seconds(),
            Switch::AnimOn => super::system_settings::set_system_animation(!super::system_settings::system_animation()),
            Switch::Glass => super::system_settings::set_system_transparent(!super::system_settings::system_transparent()),
            Switch::AccentOn => super::system_settings::set_color_prevalence(!super::system_settings::color_prevalence()),
            // The machine's own switches write the stack or the registry, then go on.
            Switch::WifiOn => {
                // A quick call into the stack: the switch reads the radio's real state back, so a refused
                // toggle (no rights, no radio) leaves it where it was rather than lying.
                let want = !crate::wifi::state().unwrap_or(false);
                crate::wifi::set_on(want);
            }
            Switch::BluetoothOn => {
                // Enabling or disabling an adapter is the system's to take its time over, so it is done off
                // the frame; the switch reads the radio's real state back meanwhile, a refused toggle leaving
                // it where it was rather than lying.
                let want = !crate::bluetooth::on();
                std::thread::spawn(move || {
                    crate::bluetooth::set(want);
                    crate::ui::settings_window::follow_settings();
                });
            }
            Switch::FocusOn => super::system_settings::set_dnd(!super::system_settings::dnd_on()),
            Switch::FocusLock => super::system_settings::set_lock_toast(!super::system_settings::lock_toast()),
            Switch::HighContrast => super::system_settings::set_high_contrast(!super::system_settings::high_contrast()),
            Switch::TaskbarAutohide => super::system_settings::set_taskbar_autohide(!super::system_settings::taskbar_autohide()),
            Switch::TaskbarSmall => super::system_settings::set_taskbar_small(!super::system_settings::taskbar_small()),
            Switch::LockScreen => super::system_settings::set_lock_screen(!super::system_settings::lock_screen_on()),
            Switch::GameMode => super::system_settings::set_game_mode(!super::system_settings::game_mode()),
            Switch::GameDvr => super::system_settings::toggle_game_flag("AppCaptureEnabled"),
            Switch::Updates => settings.check_updates ^= true,
            Switch::HeatAlert => settings.heat_alert ^= true,
            Switch::TrayHidden => settings.tray_hidden ^= true,
            Switch::Startup if !self.may_autostart && !self.autostart => {
                self.explain_no_autostart();
                return;
            }
            Switch::RemoteLights => {
                // Fetched (or taken away) on a thread of its own: the window goes on meanwhile.
                if self.fetching.is_none() {
                    let wanted = !self.remote_lights;
                    self.fetch_failed = false;
                    let raised = self.remote_raised;
                    self.fetching = Some(std::thread::spawn(move || {
                        // (Its program taken away, none is left to run with an administrator's rights.)
                        if !wanted && raised {
                            crate::phone::raise_lights(false);
                        }
                        crate::phone::set_lights(wanted) == wanted
                    }));
                }
                self.next_frame = Instant::now();
                return;
            }
            Switch::RemoteRaised => {
                if self.raising.is_none() {
                    let wanted = !self.remote_raised;
                    self.raise_failed = false;
                    self.raising = Some(std::thread::spawn(move || crate::phone::raise_lights(wanted) == wanted));
                }
                self.next_frame = Instant::now();
                return;
            }
            Switch::RemoteScreens => {
                // What the remote then says, not what was asked (see `tick`).
                let kept = !self.remote_screens;
                self.ask_remote(move || {
                    crate::phone::keep_screens(kept);
                    None
                });
                self.next_frame = Instant::now();
                return;
            }
            Switch::Remote => {
                // What the system then reports, not what was asked (see `tick`).
                if self.remote_there && self.switching.is_none() && self.reading_switches.is_none() {
                    let on = !self.remote;
                    self.switching = Some((SwitchAsked::Remote, std::thread::spawn(move || crate::source::switch(crate::link::Switch::Remote, on))));
                }
                self.next_frame = Instant::now();
                return;
            }
            Switch::Startup => {
                // What the system then reports, not what was asked (see `tick`).
                if self.switching.is_none() && self.reading_switches.is_none() {
                    let on = !self.autostart;
                    self.switching = Some((SwitchAsked::Startup, std::thread::spawn(move || crate::source::switch(crate::link::Switch::Autostart, on))));
                }
                self.next_frame = Instant::now();
                return;
            }
        }
        self.save();
    }

    /// The diagnostics put together: shown whole, with room for the user's own words and pictures, and sent if
    /// they agree.
    fn send_report(&mut self, report: String) {
        let lang = self.lang;
        let asked = super::ask::Asked {
            title: pick(lang, "发送诊断信息", "Send diagnostics"),
            intro: pick(
                lang,
                "以下是将发送至 Glance（glancepc.com）的全部内容：问题描述、所添加的截图，以及底部的诊断信息（本机硬件、Glance 读取的数值及近期日志）。不包含任何文件、账号或网络地址。",
                "Below is all that would be sent to Glance (glancepc.com): what you write, the pictures you add, and the diagnostics at the bottom (this PC's hardware, what Glance reads of it, and Glance's recent log). Nothing of your files, accounts or network address.",
            ),
            own: Some(super::ask::Own {
                words: pick(lang, "问题描述（选填）", "Your own words (may be left empty)"),
                hint: pick(lang, "例如：主板温度未显示，或风扇转速与其他软件不一致", "For instance: the board's temperature is missing, or a fan's speed differs from another program's"),
                pictures: pick(lang, "截图（选填，最多 3 张）", "Pictures (if you like, up to 3)"),
                add: pick(lang, "添加截图", "Add a picture"),
                ways: pick(lang, "点击选择、拖入窗口，或按 Ctrl+V 粘贴", "Click to choose one, drop it here, or paste it with Ctrl+V"),
                paste: pick(lang, "粘贴剪贴板中的截图", "Paste the clipboard's"),
                waiting: pick(lang, "剪贴板中有截图，按 Ctrl+V 即可粘贴", "There is a picture on the clipboard: Ctrl+V pastes it"),
                kinds: pick(lang, "图片", "Pictures"),
            }),
            option: None,
            sent: Some((pick(lang, "诊断信息", "Diagnostics"), &report)),
            from_end: false,
            yes: pick(lang, "发送", "Send"),
            no: pick(lang, "取消", "Cancel"),
            dark: self.dark,
        };
        if let Some(said) = super::ask::show(self.hwnd, &asked) {
            // (Their words first, set off from what Glance says of the PC.)
            let report = if said.words.is_empty() { report } else { format!("The user says: {}

{report}", said.words) };
            self.sent = None;
            self.sending = Some(std::thread::spawn(move || crate::diagnostics::send(&report, &said.words, &said.pictures)));
        }
    }

    /// The thread of the report `id` sent from here, opened in its window (see `ask::thread`), over this one, as a
    /// conversation the user adds to: their words and pictures, and, if they choose, fresh diagnostics (shown first,
    /// see `add_with_diagnostics`). Read, as it is shown.
    fn open_thread(&mut self, id: &str) {
        if self.thread.is_some() {
            return;
        }
        let Some(report) = crate::reports::list().into_iter().find(|report| report.id == id) else { return };
        let Some(window) = super::ask::thread::open(self.hwnd, THREAD_TOLD, thread_of(self.lang, &report), &thread_talk(self.lang, self.dark)) else { return };
        // What is being added to it already (sent before it was last closed): shown as being sent.
        if self.adding.as_ref().is_some_and(|(adding, _)| adding == id) || self.adding_asked.as_ref().is_some_and(|(asked, _)| asked == id) {
            super::ask::thread::sending(window, pick(self.lang, "正在发送…", "Sending…"));
        }
        crate::reports::read(id);
        self.followed = crate::reports::list();
        self.thread = Some((id.to_string(), window, report));
        self.next_frame = Instant::now();
    }

    /// The window of the thread of the report `id`, if it is the one open.
    fn thread_window(&self, id: &str) -> Option<HWND> {
        self.thread.as_ref().filter(|(open, ..)| open == id).map(|(_, window, _)| *window)
    }

    /// The thread open, as the reports sent are now (`followed`): given again to its window if anything was said in
    /// it since, and read.
    fn follow_thread(&mut self) {
        let Some((id, window, shown)) = &self.thread else { return };
        let Some(report) = self.followed.iter().find(|report| report.id == *id) else { return };
        if report.thread == shown.thread {
            return;
        }
        let (id, window, report) = (id.clone(), *window, report.clone());
        super::ask::thread::update(window, thread_of(self.lang, &report));
        crate::reports::read(&id);
        self.followed = crate::reports::list();
        self.thread = Some((id, window, report));
    }

    /// What the user wrote in the thread open, sent, as its window told: with fresh diagnostics, those put together
    /// first and shown whole (see `tick`), and without them at once, on a thread of its own. One addition at a time.
    fn send_in_thread(&mut self) {
        let Some((id, window)) = self.thread.as_ref().map(|(id, window, _)| (id.clone(), *window)) else { return };
        if self.adding.is_some() || self.adding_asked.is_some() || self.reporting.is_some() {
            return;
        }
        let Some(said) = super::ask::thread::said(window) else { return };
        let lang = self.lang;
        self.added = None;
        if said.option {
            super::ask::thread::sending(window, pick(lang, "正在收集诊断信息…", "Gathering the diagnostics…"));
            self.adding_asked = Some((id, said));
            self.reporting = Some((ReportFor::Add, std::thread::spawn(crate::diagnostics::report)));
        } else {
            super::ask::thread::sending(window, pick(lang, "正在发送…", "Sending…"));
            self.adding = Some((id.clone(), std::thread::spawn(move || crate::reports::add(&id, &said.words, None, &said.pictures))));
        }
        self.next_frame = Instant::now();
    }

    /// The report of the thread open closed by the user, or opened again, as its window told and as it stands: on a
    /// thread of its own, while nothing is being sent to it.
    fn turn_in_thread(&mut self) {
        let Some((id, window)) = self.thread.as_ref().map(|(id, window, _)| (id.clone(), *window)) else { return };
        if self.turning.is_some() || self.adding.as_ref().is_some_and(|(adding, _)| *adding == id) || self.adding_asked.as_ref().is_some_and(|(asked, _)| *asked == id) {
            return;
        }
        let Some(close) = self.followed.iter().find(|report| report.id == id).map(crate::reports::Report::closable) else { return };
        super::ask::thread::sending(window, if close { pick(self.lang, "正在关闭…", "Closing…") } else { pick(self.lang, "正在重新打开…", "Reopening…") });
        self.turning = Some((id.clone(), std::thread::spawn(move || crate::reports::turn(&id, close))));
        self.next_frame = Instant::now();
    }

    /// The thread open, closed by the user: its window taken down, it read, and the reports as they are now.
    fn close_thread(&mut self) {
        let Some((id, window, _)) = self.thread.take() else { return };
        super::ask::thread::close(window);
        crate::reports::read(&id);
        self.followed = crate::reports::list();
        self.next_frame = Instant::now();
    }

    /// The fresh diagnostics the user chose to add to a report, put together: shown whole (over its thread's window,
    /// if it is open), and sent with what they added if they agree. Nothing is sent if they do not.
    fn add_with_diagnostics(&mut self, report: String) {
        let Some((id, said)) = self.adding_asked.take() else { return };
        let lang = self.lang;
        let window = self.thread_window(&id);
        let intro = match lang {
            Lang::Zh => format!("以下诊断信息将连同补充说明与截图一并发送至反馈 {id}。不包含任何文件、账号或网络地址。"),
            Lang::En => format!("These diagnostics go to report {id}, with the words and pictures you added. Nothing of your files, accounts or network address."),
        };
        let asked = super::ask::Asked {
            title: pick(lang, "发送诊断信息", "Send diagnostics"),
            intro: &intro,
            own: None,
            option: None,
            sent: Some((pick(lang, "诊断信息", "Diagnostics"), &report)),
            from_end: false,
            yes: pick(lang, "发送", "Send"),
            no: pick(lang, "取消", "Cancel"),
            dark: self.dark,
        };
        if super::ask::show(window.unwrap_or(self.hwnd), &asked).is_some() {
            if let Some(window) = window {
                super::ask::thread::sending(window, pick(lang, "正在发送…", "Sending…"));
            }
            self.adding = Some((id.clone(), std::thread::spawn(move || crate::reports::add(&id, &said.words, Some(&report), &said.pictures))));
            self.next_frame = Instant::now();
        } else if let Some(window) = window {
            super::ask::thread::writing(window);
        }
    }

    /// Installs NetEase Cloud Music's lyrics plugin, or takes it back (`work`), once the user agrees to what it does,
    /// on another thread (see `paint_list`).
    fn netease_asked(&mut self, work: NetEaseWork) {
        let Some(netease) = self.netease.clone() else { return };
        let lang = self.lang;
        let folder = netease.folder.display().to_string();
        let plugins = crate::netease::plugin_folder().display().to_string();
        let ours = self.settings.netease_loader;
        let (title, intro, yes) = match (work, lang) {
            (NetEaseWork::Install, Lang::Zh) => {
                let mut intro = format!("将安装以下开源项目（GPL-3.0 许可），文件取自 Glance 网站的副本或该项目在 GitHub 发布的版本，安装前核对 SHA-256 校验值。\n\nInfLink-rs 3.4.0：使网易云音乐提供播放进度的插件，放入 {plugins}。");
                if !netease.has_loader() {
                    intro += &format!("\nBetterNCM 1.3.4：网易云音乐的插件加载器，以 msimg32.dll 为名放入网易云音乐的安装目录 {folder}。该目录需要管理员权限时，Windows 会请求确认。");
                }
                intro += "\n\n另将放入 Glance 自带的插件 Glance Icon Row（MIT 许可，无需下载），使 BetterNCM 按钮与网易云音乐右上角的其他图标排在同一行。";
                if !netease.tested() {
                    intro += &format!("\n\n当前网易云音乐版本 {} 未经 InfLink-rs 测试，插件可能无法正常工作。", netease.version());
                }
                intro += "\n\n安装后可随时在此处移除。";
                ("安装网易云音乐歌词插件？", intro, "安装")
            }
            (NetEaseWork::Install, Lang::En) => {
                let mut intro = format!("These open-source projects (GPL-3.0) are installed, taken from Glance's site or from where each project publishes it on GitHub, and checked by SHA-256 first.\n\nInfLink-rs 3.4.0, the plugin that has NetEase Cloud Music say how far into a track it is, put in {plugins}.");
                if !netease.has_loader() {
                    intro += &format!("\nBetterNCM 1.3.4, NetEase Cloud Music's plugin loader, put in its folder {folder} as msimg32.dll. Where that folder needs an administrator's rights, Windows asks first.");
                }
                intro += "\n\nGlance's own plugin Glance Icon Row (MIT, nothing is downloaded) is put in too. It keeps the BetterNCM button on one row with NetEase Cloud Music's other icons at the top right.";
                if !netease.tested() {
                    intro += &format!("\n\nInfLink-rs was not tested on this NetEase Cloud Music, version {}. It may not work.", netease.version());
                }
                intro += "\n\nIt can be removed here at any time.";
                ("Install the NetEase Cloud Music lyrics plugin?", intro, "Install")
            }
            (NetEaseWork::Remove, Lang::Zh) => {
                let mut intro = format!("将删除 {plugins} 中的 InfLink-rs 插件和 Glance Icon Row 插件。");
                if ours {
                    intro += &format!("\nGlance 放入 {folder} 的 BetterNCM 加载器（msimg32.dll）也将删除。该目录需要管理员权限时，Windows 会请求确认。");
                }
                intro += "\n\n网易云音乐正在运行时，将先关闭，移除后重新打开。";
                ("移除网易云音乐歌词插件？", intro, "移除")
            }
            (NetEaseWork::Remove, Lang::En) => {
                let mut intro = format!("The InfLink-rs and Glance Icon Row plugins are deleted from {plugins}.");
                if ours {
                    intro += &format!("\nThe BetterNCM loader (msimg32.dll) Glance put in {folder} is deleted too. Where that folder needs an administrator's rights, Windows asks first.");
                }
                intro += "\n\nNetEase Cloud Music, if running, is closed first and opened again after.";
                ("Remove the NetEase Cloud Music lyrics plugin?", intro, "Remove")
            }
            (NetEaseWork::IconRow, Lang::Zh) => (
                "更新网易云音乐歌词插件？",
                format!("将在 {plugins} 中放入 Glance 自带的插件 Glance Icon Row（MIT 许可，无需下载），使 BetterNCM 按钮与网易云音乐右上角的其他图标排在同一行。移除歌词插件时将一并删除。\n\n重新启动网易云音乐后生效。"),
                "更新",
            ),
            (NetEaseWork::IconRow, Lang::En) => (
                "Update the NetEase Cloud Music lyrics plugin?",
                format!("Glance's own plugin Glance Icon Row (MIT, nothing is downloaded) is put in {plugins}. It keeps the BetterNCM button on one row with NetEase Cloud Music's other icons at the top right, and is removed with the lyrics plugin.\n\nIt works once NetEase Cloud Music starts again."),
                "Update",
            ),
        };
        let asked = super::ask::Asked { title, intro: &intro, own: None, option: None, sent: None, from_end: false, yes, no: pick(lang, "取消", "Cancel"), dark: self.dark };
        if super::ask::show(self.hwnd, &asked).is_none() {
            return;
        }
        self.netease_failed = None;
        self.netease_work = Some((
            work,
            std::thread::spawn(move || match work {
                NetEaseWork::Install => crate::netease::install(&netease).map(|ours| (ours, crate::netease::running(&netease))),
                NetEaseWork::Remove => crate::netease::remove(&netease, ours).map(|()| (false, false)),
                NetEaseWork::IconRow => crate::netease::put_icon_row().map(|()| (false, crate::netease::running(&netease))),
            }),
        ));
        self.next_frame = Instant::now();
    }

    /// A module's title and what it describes on this machine.
    fn module(&self, id: &str) -> (String, Option<String>) {
        if let Some(bare) = id.strip_prefix(WORKBENCH) {
            return self.module(bare);
        }
        let (lang, info) = (self.lang, &crate::app().info());
        let p = |zh, en| pick(lang, zh, en).to_string();
        if let Some(group) = id.strip_prefix(OVERLAY) {
            return match group {
                "frames" => (p("帧率", "Frame rate"), Some(p("没有游戏运行时显示“—”", "“—” while no game runs"))),
                "cpu" => ("CPU".into(), None),
                "gpu" => ("GPU".into(), None),
                "memory" => (p("内存", "Memory"), None),
                "vram" => (p("显存", "Video memory"), None),
                "network" => (p("网速", "Network"), None),
                "clock" => (p("时间", "Time"), None),
                "battery" => (p("电池", "Battery"), None),
                _ => (p("麦克风", "Microphone"), Some(p("默认麦克风是否静音", "Whether the default microphone is muted"))),
            };
        }
        match id {
            "cpu" => ("CPU".into(), Some(info.cpu_name.clone())),
            _ if id.starts_with("gpu:") => ("GPU".into(), info.gpu_of(id).map(|i| info.gpus[i].name.clone())),
            "memory" => (p("内存", "Memory"), info.memory_modules.clone()),
            "network" => (p("网络", "Network"), Some(info.network_adapter.clone().unwrap_or_else(|| p("所有物理网卡的上传与下载速度", "Upload and download over every physical adapter")))),
            "disk" => (p("磁盘", "Disk"), (!info.drives.is_empty()).then(|| info.drives.join(", "))),
            "processes" => (p("进程", "Processes"), Some(p("占用最多的程序，在面板上点击表头即可排序", "The busiest programs. Click a heading on the panel to sort them"))),
            "storage" => (p("存储", "Storage"), Some(p("各分区的剩余空间", "Space left on each drive"))),
            // A board whose sensors are not read says so, rather than
            // looking like a switch that does nothing.
            "board" if crate::app().controller.seen.lock().unwrap().board.is_none() => (
                p("主板", "Motherboard"),
                Some(p("未能读取主板传感器，原因见“通用”页中的诊断信息", "This board's sensors could not be read. The diagnostics on the General page say why")),
            ),
            "board" => (p("主板", "Motherboard"), (!info.board.is_empty()).then(|| info.board.clone())),
            "battery" => (p("电池", "Battery"), Some(p("电量、充放电功率与健康度", "Charge, power and health"))),
            "game" => (p("游戏", "Game"), Some(p("全屏游戏的帧率、帧时间与资源占用", "A fullscreen game's frame rate, frame times and use"))),
            "docker" => ("Docker".into(), Some(p("运行中容器的处理器、内存、网络与磁盘占用，仅在 Docker 运行时显示", "What each running container uses of the processor, memory, network and disk, shown only while Docker runs"))),
            "wsl" => ("WSL".into(), Some(p("WSL 2 的处理器与内存占用，仅在其运行时显示，不会启动或唤醒发行版", "WSL 2's processor and memory use, shown only while it runs, without starting or waking a distribution"))),
            "time" => (p("时间与媒体", "Time and media"), Some(p("时间、日期、音量与正在播放的内容", "The time, the date, the volume and what is playing"))),
            "station" => (p("中转站", "Station"), Some(p("临时存放拖入的文件、图片、文字，以及最近复制的内容", "Files, pictures and text dropped in for a while, and what was copied lately"))),
            "lyrics" => (
                p("音乐", "Music"),
                Some(p(
                    "正在播放的歌曲：封面、播放进度、播放控制、音量，以及随播放滚动的歌词。查找歌词时会把歌名发给网易云音乐、QQ 音乐、酷狗或 LRCLIB，本地歌曲先使用文件自带的歌词。歌词需要播放器提供播放进度（网易云音乐需要 InfLink-rs 插件）",
                    "What plays: its cover, its track, its controls, how loud, and its lyrics as it plays. To find lyrics the track's title is sent to NetEase Cloud Music, QQ Music, Kugou or LRCLIB. A track played from a file shows its own first. Lyrics need the player to say how far into the track it is (NetEase Cloud Music, with the InfLink-rs plugin)",
                )),
            ),
            "notes" => (p("便签", "Notes"), Some(p("想法与备忘，每条保存为一个 Markdown 文件，可放在桌面上", "Thoughts and things to remember, each kept as a Markdown file, which can be put on the desktop"))),
            "todo" => (
                p("待办", "To-do"),
                Some(p(
                    "要做的事项，可设定日期与时间，勾选后移入已完成。全部事项保存在便签文件夹的“待办.md”中",
                    "Things to do, each with a day and a time if wanted, moved among the finished once ticked. All are kept in “To-do.md” in the notes folder",
                )),
            ),
            "apps" => (p("快捷启动", "Quick launch"), Some(p("常用的程序、文件、文件夹与网址", "Programs, files, folders and web addresses used often"))),
            "search" => (p("搜索", "Search"), Some(p("搜索应用、设置、文件和便签", "Search apps, settings, files and notes"))),
            _ => (p("系统", "System"), Some(p("开机时长、进程数、线程数与句柄数", "Uptime, processes, threads and handles"))),
        }
    }

    /// Whether module `id` is on.
    fn module_on(&self, id: &str) -> bool {
        if let Some(bare) = id.strip_prefix(WORKBENCH) {
            return self.prefs.workbench.iter().any(|entry| entry.id == bare && entry.on);
        }
        match id.strip_prefix(OVERLAY) {
            Some(group) => self.settings.overlay.groups.iter().any(|entry| entry.id == group && entry.on),
            None => self.prefs.modules.iter().any(|entry| entry.id == id && entry.on),
        }
    }

    /// The list a row's module (`id`) is in, the readings', the workbench's or the overlay's, and its id there.
    fn list<'a>(&mut self, id: &'a str) -> (&mut Vec<super::prefs::ModuleEntry>, &'a str) {
        if let Some(bare) = id.strip_prefix(WORKBENCH) {
            return (&mut self.prefs.workbench, bare);
        }
        match id.strip_prefix(OVERLAY) {
            Some(group) => (&mut self.settings.overlay.groups, group),
            None => (&mut self.prefs.modules, id),
        }
    }

    /// Whether item `name` of a row's module `id` is chosen.
    fn item_on(&self, id: &str, name: &str) -> bool {
        if let Some(on) = self.station_item(id, name) {
            return on;
        }
        if let Some(bare) = id.strip_prefix(WORKBENCH) {
            return prefs::shows_in(&self.prefs.workbench, bare, name);
        }
        match id.strip_prefix(OVERLAY) {
            Some(group) => super::overlay::shows(&self.settings.overlay.groups, group, name),
            None => self.prefs.shows(id, name),
        }
    }

    /// The settings of module `id` (bare) its card lists.
    fn fields_here(&self, id: &str) -> impl Iterator<Item = Field> + '_ {
        module_fields(id).iter().copied()
    }

    /// The station's items (`id` its module, and the workbench's): what is copied kept, and the files come lately
    /// listed. Of the station's settings, not of what the panel shows (see `settings::StationSettings`).
    fn station_item(&self, id: &str, name: &str) -> Option<bool> {
        (id.trim_start_matches(WORKBENCH) == "station").then_some(())?;
        match name {
            "history" => Some(self.settings.station.history),
            "recent" => Some(self.settings.station.recent),
            _ => None,
        }
    }

    /// The items of module `id` this machine has shown it can read: only
    /// those are listed.
    fn items_here(&self, id: &str) -> Vec<String> {
        if let Some(bare) = id.strip_prefix(WORKBENCH) {
            return self.items_here(bare);
        }
        if id == "station" {
            return ["history", "recent"].map(String::from).to_vec();
        }
        // An overlay's group of one item has no card: its switch is the item's.
        if let Some(group) = id.strip_prefix(OVERLAY) {
            let items = super::overlay::GROUPS.iter().find(|(g, _)| *g == group).map_or(&[][..], |(_, items)| *items);
            return if items.len() > 1 { items.iter().map(|(name, _)| name.to_string()).collect() } else { Vec::new() };
        }
        let app = crate::app();
        let seen = app.controller.seen.lock().unwrap();
        let info = &app.info();
        let gpu = info.gpu_of(id).map(|index| (&info.gpus[index], seen.gpus.get(index).cloned().unwrap_or_default()));
        // A module this machine has shown nothing of (no battery, no board
        // sensors, a GPU not read) has no lane, and nothing to set.
        let present = match id.split(':').next().unwrap_or(id) {
            "battery" => seen.battery,
            "board" => seen.board.is_some(),
            "gpu" => gpu.as_ref().is_some_and(|(_, seen)| seen.present),
            _ => true,
        };
        if !present {
            return Vec::new();
        }
        prefs::items(id)
            .iter()
            .map(|item| item.name)
            .filter(|name| match (id.split(':').next().unwrap_or(id), *name) {
                (_, "chart") => true,
                ("cpu", "temp") => seen.cpu_temp,
                ("cpu", "clock") => seen.cpu_clock,
                ("cpu", "power") => seen.cpu_power,
                ("cpu", "fans") => seen.board.as_ref().is_some_and(|board| !board.turning.is_empty()),
                ("cpu", "ccds") => seen.ccds.len() > 1,
                ("cpu", "threads") => seen.threads > 0,
                ("gpu", name) => gpu.as_ref().is_some_and(|(info, seen)| match name {
                    "temp" => seen.temp,
                    "vram" => info.mem_total > 0,
                    "clock" => seen.clock,
                    "power" => seen.power,
                    "fan" => seen.fan,
                    "hotspot" => seen.hot_spot,
                    "shared" => info.shared_total > 0,
                    _ => !seen.engines.is_empty(),
                }),
                ("memory", "dimms") => seen.dimms > 0,
                ("memory", "speed") => info.memory_speed.is_some(),
                ("battery", "power") => seen.battery_power,
                ("battery", "health") => seen.battery_health,
                ("network", "address") => seen.address,
                ("network", "wifi") => seen.wifi,
                ("network", "link") => seen.link,
                ("disk", "drives") => !seen.drives.is_empty() || info.drives.len() > 1,
                ("disk", "active") => seen.disk_active,
                ("storage", "volumes") => !seen.volumes.is_empty(),
                ("time", "lyrics") => prefs::LYRICS_OFFERED,
                _ => true,
            })
            .flat_map(|name| match (id, name) {
                // A switch for each sensor the board has shown (for the CPU's, each fan seen turning: a header with
                // none on it reads 0).
                ("board", "temps" | "fans") | ("cpu", "fans") => {
                    let board = seen.board.clone().unwrap_or_default();
                    let sensors = match (id, name) {
                        (_, "temps") => board.temps,
                        ("cpu", _) => board.fans.into_iter().filter(|fan| board.turning.contains(fan)).collect(),
                        _ => board.fans,
                    };
                    sensors.into_iter().map(|sensor| format!("{name}:{sensor}")).collect()
                }
                // A switch for each drive.
                ("storage", "volumes") => seen.volumes.iter().map(|volume| format!("volumes:{volume}")).collect(),
                _ => vec![name.to_string()],
            })
            .collect()
    }

    /// Whether module `id` has a card to open: items, or choices of its own.
    fn opens(&self, id: &str) -> bool {
        !module_fields(id.trim_start_matches(WORKBENCH)).is_empty() || !self.items_here(id).is_empty()
    }

    /// What an item of module `id` is called, and a word on it.
    fn item_label(&self, id: &str, name: &str) -> (Cow<'static, str>, Option<&'static str>) {
        let id = id.trim_start_matches(WORKBENCH);
        match name.split_once(':') {
            // A drive, by its letter.
            Some(("volumes", volume)) => (Cow::Owned(volume.to_string()), None),
            // One of the board's sensors, by its name on the panel.
            Some((group, sensor)) => (Cow::Owned(super::view::board_sensor(sensor, group, self.lang)), None),
            None => {
                let (label, word) = self.module_item_label(id, name);
                (Cow::Borrowed(label), word)
            }
        }
    }

    /// What one of the items every module of kind `id` has is called, and a word on it.
    fn module_item_label(&self, id: &str, name: &str) -> (&'static str, Option<&'static str>) {
        let p = |zh, en| pick(self.lang, zh, en);
        match (id.split(':').next().unwrap_or(id), name) {
            ("overlay", "fps") => (p("帧率", "Frame rate"), None),
            ("overlay", "time") => (p("时间", "Time"), None),
            ("station", "history") => (p("记录最近复制", "Keep what is copied"), Some(p("关闭后不再记录，已有记录保留", "Off, nothing more is kept. What was is still there"))),
            ("station", "recent") => (p("列出最近文件", "List recent files"), None),
            ("time", "sound") => (p("音量", "Volume"), None),
            ("time", "media") => (p("正在播放", "Now playing"), None),
            ("time", "lyrics") => (p("歌词", "Lyrics"), Some(p("正在演唱的一句与下一句", "The line sung now and the next"))),
            ("lyrics", "translation") => (p("翻译", "Translation"), Some(p("有翻译时显示在当前歌词下方，否则显示下一句", "Under the line sung now where there is one, else the next line"))),
            ("lyrics", "cover") => (p("封面", "Cover"), None),
            ("lyrics", "title") => (p("歌名与歌手", "Title and artist"), None),
            ("lyrics", "spectrum") => (p("律动条", "Spectrum"), None),
            ("lyrics", "controls") => (p("常驻播放控件", "Controls always shown"), Some(p("关闭时，播放控件在鼠标移入后出现", "Off, they show as the pointer comes over it"))),
            ("overlay", "low") => (p("1% low", "1% low"), Some(p("最近 10 秒内最慢 1% 的帧换算成的帧率", "The slowest 1% of frames of the last 10 seconds, as frames a second"))),
            ("overlay", "frametime") => (p("帧时间", "Frame time"), Some(p("最近 1 秒内最长一帧的耗时", "The longest frame of the last second"))),
            ("overlay", "cpu" | "gpu") => (p("占用", "Use"), None),
            ("overlay", "down") => (p("下载", "Download"), None),
            ("overlay", "up") => (p("上传", "Upload"), None),
            ("overlay", "battery") => (p("电量", "Charge"), None),
            ("overlay", "battery_power") => (p("功率", "Power"), Some(p("充电或放电的功率", "Watts going in or out"))),
            ("overlay", _) if name.ends_with("_temp") => (p("温度", "Temperature"), None),
            ("overlay", _) if name.ends_with("_power") => (p("功耗", "Power"), None),
            ("network" | "disk", "chart") => (p("速率图表", "Rate chart"), None),
            ("battery", "chart") => (p("电量图表", "Charge chart"), None),
            ("battery", "power") => (p("功率", "Power"), Some(p("充电或放电的功率，使用电池时即为整机功耗", "Watts going in or out. On battery, what the whole machine draws"))),
            ("battery", "health") => (p("健康度", "Health"), Some(p("满电容量与设计容量之比", "Full charge against the design capacity"))),
            ("game", "chart") => (p("帧率图表", "Frame rate chart"), None),
            ("game", "low") => (p("1% low", "1% low"), Some(p("最近 10 秒内最慢 1% 的帧换算成的帧率", "The slowest 1% of frames of the last 10 seconds, as frames a second"))),
            ("game", "longest") => (p("最长一帧", "Longest frame"), Some(p("最近 1 秒内最长一帧的耗时，卡顿时数值变大", "The longest frame of the last second. A stutter raises it"))),
            ("game", "frametimes") => (p("帧时间图表", "Frame time chart"), Some(p("每秒最长一帧的走势，卡顿显示为尖峰", "The longest frame of each second. A stutter shows as a spike"))),
            ("game", "usage") => (p("游戏占用", "Game's use"), Some(p("游戏进程自身的 CPU 与 GPU 占用", "The CPU and GPU the game itself uses"))),
            ("game", "memory") => (p("游戏内存", "Game's memory"), Some(p("游戏进程自身占用的内存与显存", "The memory and video memory the game itself holds"))),
            ("game", "limit") => (p("显卡限制", "GPU limit"), Some(p("NVIDIA 显卡的频率是否受功耗或温度限制", "Whether an NVIDIA GPU's clock is held back by its power or temperature limit"))),
            ("game", "mic") => (p("麦克风", "Microphone"), Some(p("默认麦克风是否静音", "Whether the default microphone is muted"))),
            ("game", "time") => (p("游玩时长", "Time played"), Some(p("本次游戏的时长，中断不足 5 分钟时继续计时", "This session's length. A break under 5 minutes carries on"))),
            ("wsl", "memory") => (p("内存", "Memory"), Some(p("已用内存与内存上限", "Memory used, and the most it may use"))),
            ("wsl", "distros") => (p("运行中的发行版", "Distributions running"), None),
            ("wsl", "gpu") => ("GPU", Some(p("WSL 中的程序占用的 GPU", "The GPU programs in WSL use"))),
            (_, "chart") => (p("占用图表", "Usage chart"), Some(p("关闭后仅显示数值", "Off, the figures alone"))),
            ("system", "uptime") => (p("开机时长", "Uptime"), None),
            ("system", "processes") => (p("进程数", "Processes"), None),
            ("system", "threads") => (p("线程数", "Threads"), None),
            ("system", "handles") => (p("句柄数", "Handles"), None),
            (_, "temp") => (p("温度", "Temperature"), None),
            (_, "clock") => (p("频率", "Clock"), None),
            (_, "power") => (p("功耗", "Power"), None),
            (_, "ccds") => (p("各 CCD 温度", "Each CCD's temperature"), None),
            (_, "threads") => (p("各线程占用", "Each thread"), None),
            (_, "vram") => (p("显存", "Video memory"), None),
            (_, "fan") => (p("风扇", "Fan"), None),
            (_, "hotspot") => (p("热点温度", "Hot spot"), Some(p("芯片上最高一点的温度", "The warmest point of the chip"))),
            (_, "shared") => (p("共享显存", "Shared memory"), Some(p("显卡占用的系统内存", "System memory the GPU uses"))),
            (_, "engines") => (p("各引擎", "Engines"), Some(p("3D、复制与视频编解码", "3D, copy and video"))),
            (_, "dimms") => (p("内存条温度", "Module temperatures"), None),
            ("memory", "speed") => (p("内存速率", "Memory speed"), Some(p("低于额定速率时一并显示额定值", "With the rated speed too, where it runs below it"))),
            (_, "committed") => (p("已提交", "Committed"), None),
            (_, "cached") => (p("缓存", "Cached"), None),
            (_, "adapter") => (p("网卡型号", "Adapter"), None),
            (_, "wifi") => (p("Wi-Fi 名称", "Wi-Fi name"), Some(p("需要开启 Windows 定位服务", "Needs Windows' location service on"))),
            (_, "address") => (p("IP 地址", "IP address"), None),
            (_, "link") => (p("链路速度", "Link speed"), None),
            (_, "totals") => (p("开机以来的流量", "Traffic since boot"), None),
            (_, "latency") => (p("网络延迟", "Latency"), Some(p("定时向下方所选地址发送 ping 并记录往返时间", "Pings the address chosen below at each refresh and shows the round trip"))),
            (_, "drives") => (p("各硬盘温度", "Drive temperatures"), None),
            (_, "active") => (p("活动时间", "Active time"), None),
            _ => (p("其他", "Other"), None),
        }
    }

    // ---- Input ----

    fn hovered(&self) -> Option<Target> {
        let (x, y) = self.pointer?;
        self.targets.iter().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, t)| t.clone())
    }

    fn moved(&mut self, x: f32, y: f32) {
        let was = (self.hovered(), self.over_followed());
        self.pointer = Some((x, y));
        // Drawn again only where the pointer changes what shows: what it is over (lit), what it has in hand (a
        // slider, a row, a component carried: they follow it), or where what is drawn follows it. Moved over the
        // same, nothing changes: a frame drawn at every move had the window drawn whole as often as the mouse
        // reports, the preview with it.
        let held = self.phone.slid.is_some() || self.sliding.is_some() || self.drag.is_some() || self.carry.is_some();
        if let Some((part, track, at)) = &mut self.phone.slid {
            *at = part.along(*track, x, y);
        }
        if let Some((field, track)) = self.sliding {
            self.slide(field, track, x);
        }
        if let Some(drag) = &mut self.drag {
            drag.pointer = y + self.scroll;
            self.reorder();
        }
        self.carry_to(x, y);
        if held || was != (self.hovered(), self.over_followed()) || was.1 {
            self.next_frame = Instant::now();
        }
    }

    /// Whether the pointer is where what is drawn follows it (see `follows_pointer`).
    fn over_followed(&self) -> bool {
        self.pointer.is_some_and(|(x, y)| self.follows_pointer.iter().any(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h))
    }

    fn press(&mut self, x: f32, y: f32) {
        self.stop_drag();
        self.pointer = Some((x, y));
        if self.page == Page::Phone && self.press_phone(x, y) {
            return;
        }
        self.pressed = self.hovered();
        // A press anywhere else gives up taking a new shortcut.
        if self.recording.is_some() && !matches!(self.pressed, Some(Target::Shortcut(_) | Target::ClearShortcut(_))) {
            self.record(None);
        }
        // A press moves the focus there too, without showing it.
        if let Some(target) = &self.pressed {
            self.keyboard = false;
            self.focus = Some(self.stop_for(target));
        }
        // A slider takes the value where it is pressed, and follows the
        // pointer until it is let go.
        if let Some(Target::Slider(field)) = self.pressed {
            if let Some((track, _)) = self.targets.iter().find(|(_, target)| *target == Target::Slider(field)) {
                let track = *track;
                self.sliding = Some((field, track));
                self.slide(field, track, x);
                unsafe { SetCapture(self.hwnd) };
            }
        }
        if let Some(target) = self.pressed.clone() {
            if self.take_up(&target, x, y) {
                self.next_frame = Instant::now();
                return;
            }
        }
        if let Some(Target::Grip(id)) = &self.pressed {
            // A card travels closed.
            let id = id.clone();
            self.set_open(&id, false, true);
            let id = &id;
            let top = self.row_top(id);
            self.drag = Some(Drag { id: id.clone(), grab: y + self.scroll - top, pointer: y + self.scroll });
            unsafe { SetCapture(self.hwnd) };
        }
        self.next_frame = Instant::now();
    }

    /// A slider or a module let go (or the pointer taken away mid-drag, by
    /// another window or the window closing): kept where it was left.
    /// Settings changed elsewhere, taken up once nothing is in hand: a slider slid shows its value live through
    /// the panel's settings, a row or a component dragged is kept as it is let go; taken meanwhile, what is in hand
    /// went back where it was.
    fn follow_when_free(&mut self) {
        if OVERLAY_OWED.load(Ordering::Acquire) {
            OVERLAY_OWED.store(false, Ordering::Release);
            self.tab = Tab::Overlay;
            self.turn_to(Page::Monitor);
        }
        if WORKBENCH_OWED.load(Ordering::Acquire) {
            WORKBENCH_OWED.store(false, Ordering::Release);
            self.turn_to(Page::Workbench);
        }
        if !FOLLOW_OWED.load(Ordering::Acquire) || self.sliding.is_some() || self.drag.is_some() || self.carry.is_some() {
            return;
        }
        FOLLOW_OWED.store(false, Ordering::Release);
        let settings = crate::app().settings.lock().unwrap().clone();
        self.take(settings);
        self.next_frame = Instant::now();
    }

    fn let_go(&mut self) {
        // A component in hand is put down nowhere: where it came from, it stays. (Taken up and not yet moved too:
        // left, it went on following the pointer with no button held, and the next press put it down.)
        if self.carry.take().is_some() {
            self.pressed = None;
        }
        // The phone's slider held is let go where it was, nothing sent (left, it followed the pointer, and the
        // next press anywhere sent its value).
        self.phone.slid = None;
        // Shown live as it slid.
        if let Some((field, _)) = self.sliding.take() {
            self.pressed = None;
            self.keep_slider(field);
        }
        if let Some(drag) = self.drag.take() {
            // The held row settles into its place.
            let slot = self.row_top(&drag.id);
            let now = Instant::now();
            let mut settle = Transition::settled(drag.pointer - drag.grab - slot);
            settle.retarget(0.0, SETTLE.0, SETTLE.1, now);
            self.motion.insert(format!("row:{}", drag.id), settle);
            self.pressed = None;
            self.save();
        }
    }

    fn release(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        // A click that lands off an open list takes the list down with it.
        if self.open_dropdown.is_some() && !matches!(self.hovered(), Some(Target::Choice(f, _)) if self.open_dropdown == Some(f)) {
            self.open_dropdown = None;
        }
        if self.release_phone(x, y) {
            return;
        }
        if self.sliding.is_some() || self.drag.is_some() {
            super::window::let_go_of_capture(self.hwnd);
            self.let_go();
            return;
        }
        if self.carry.is_some() {
            self.put_down();
            self.next_frame = Instant::now();
            return;
        }
        let pressed = self.pressed.take();
        if pressed.is_none() || pressed != self.hovered() {
            self.next_frame = Instant::now();
            return;
        }
        // The focus follows the choice a click made.
        let target = pressed.unwrap();
        self.activate(target.clone());
        self.focus = Some(self.stop_for(&target));
    }

    /// Does what pressing `target` does.
    fn activate(&mut self, target: Target) {
        match target {
            Target::Skin(skin) => {
                self.settings.skin = match skin {
                    Skin::Paper => "paper",
                    Skin::Glass => "glass",
                    Skin::Fluent => "fluent",
                }
                .into();
                self.save();
            }
            Target::Choice(field, index) => {
                // A dropdown's box pressed on the word it already reads opens or shuts its
                // list rather than setting anything; a word pressed in the list sets it and shuts.
                if Self::dropped(field) && self.choices(field).2 == Some(index) {
                    self.open_dropdown = if self.open_dropdown == Some(field) { None } else { Some(field) };
                    // The list opens with the worn word in its middle, however far down it lies.
                    if self.open_dropdown.is_some() {
                        self.dropdown_scroll = (index as f32 * DROP_LINE - DROP_CAP / 2.0 + DROP_LINE / 2.0).max(0.0);
                    }
                    self.next_frame = Instant::now();
                } else {
                    self.choose(field, index);
                }
            }
            // (Carried, or pressed and let go: see `put_down`.)
            Target::Library(_) | Target::Chip(..) => {}
            Target::View(_) | Target::Same | Target::Zone(..) | Target::Unplace(..) => {
                self.press_editor(&target);
            }
            Target::Switch(switch) => self.flip(switch),
            Target::Shortcut(which) => self.record(if self.recording == Some(which) { None } else { Some(which) }),
            Target::ClearShortcut(which) => self.set_shortcut(which, None),
            Target::Nav(page) => self.turn_to(page),
            Target::Preview(page) => self.pick_preview(page),
            // Nearby sharing let on or off, then read back so the switch never
            // lies about what the machine now holds.
            Target::Nearby(on) => {
                super::system_settings::set_nearby_sharing(on);
                self.next_frame = Instant::now();
            }
            // A restart into the recovery environment or the firmware setup: the
            // machine's own doing, asked with elevation.
            Target::RebootTo(recovery) => {
                super::system_settings::reboot_to(recovery);
            }
            // One login item let run or held: the shell's own mark written, the list
            // read again when it lands — where the write is refused, the old state
            // comes back rather than a lie.
            Target::Startup(machine, name, on) => {
                crate::sys::startup_apply(machine, name, on);
                self.next_frame = Instant::now();
            }
            Target::Tab(tab) => self.turn_tab(tab),
            Target::Element(name) => {
                let on = self.item_on("lyrics", name);
                self.prefs.set_item("lyrics", name, !on);
                self.save();
            }
            Target::Drive(volume) => {
                turn_drive(&mut self.settings.recent_hidden_drives, &self.drives, &volume);
                self.save();
            }
            Target::RecentFolder(folder) => {
                self.settings.recent_folders.retain(|kept| *kept != folder);
                self.save();
            }
            // (One at a time: pressed again while it is up, nothing more.)
            Target::AddRecentFolder if self.choosing_folders.is_none() => {
                let (hwnd, lang) = (self.hwnd.0 as isize, self.lang);
                self.choosing_folders = Some(std::thread::spawn(move || unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                    super::beside::choose(HWND(hwnd as *mut _), super::beside::Kinds::Folders, lang)
                }));
            }
            Target::AddRecentFolder => {}
            Target::CheckNow => update::check_now(update::Asker::Settings),
            Target::Module(id) => {
                let (list, bare) = self.list(&id);
                if let Some(entry) = list.iter_mut().find(|entry| entry.id == bare) {
                    entry.on ^= true;
                }
                self.save();
            }
            Target::Expand(id) => {
                let open = !self.expanded.contains(&id);
                self.set_open(&id, open, false);
            }
            Target::More => {
                self.more_open ^= true;
                self.animate("chevron:more".into(), if self.more_open { 180.0 } else { 0.0 }, SLIDE, Instant::now(), false);
            }
            // An item is changed only while its module is on.
            Target::Item(id, name) if self.module_on(&id) => {
                let on = self.item_on(&id, &name);
                match (id.strip_prefix(OVERLAY), id.strip_prefix(WORKBENCH)) {
                    _ if self.station_item(&id, &name).is_some() => match name.as_str() {
                        "history" => self.settings.station.history = !on,
                        _ => self.settings.station.recent = !on,
                    },
                    (Some(group), _) => super::overlay::set_item(&mut self.settings.overlay.groups, group, &name, !on),
                    (None, Some(bare)) => prefs::set_item_in(&mut self.prefs.workbench, bare, &name, !on),
                    (None, None) => self.prefs.set_item(&id, &name, !on),
                }
                self.save();
            }
            Target::Item(..) => {}
            // The uninstaller asks first and closes Glance itself.
            Target::Uninstall => {
                // Checked again as it is run: the window may have been open a while.
                if let Some(uninstaller) = self.uninstaller.as_deref().and_then(elevation::trusted) {
                    let path = HSTRING::from(uninstaller.as_os_str());
                    unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &path, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
                }
            }
            Target::Update => update::install(),
            Target::Download => {
                let page = HSTRING::from(update::download_page(self.lang));
                unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &page, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
            }
            Target::NoticeRead => {
                if let Some(notice) = crate::notice::unread() {
                    crate::notice::read(&notice.id);
                }
                self.focus = None;
            }
            Target::NoticeLink => {
                if let Some(link) = crate::notice::unread().and_then(|notice| notice.link) {
                    let link = HSTRING::from(link);
                    unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &link, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
                }
            }
            Target::NetEase(work) if self.netease_work.is_none() => self.netease_asked(work),
            Target::NetEase(_) => {}
            Target::NetEaseRestart => {
                if let Some(netease) = self.netease.clone() {
                    self.netease_restart = false;
                    std::thread::spawn(move || crate::netease::restart(&netease));
                }
            }
            Target::Diagnostics if self.reporting.is_none() => self.reporting = Some((ReportFor::Copy, std::thread::spawn(crate::diagnostics::report))),
            Target::Diagnostics => {}
            Target::Quit => crate::quit(),
            Target::Guide => self.begin_guide(),
            // Put together first (see `tick`), then shown whole: nothing leaves this PC that was not read and agreed to.
            Target::SendReport if self.sending.is_none() && self.reporting.is_none() => self.reporting = Some((ReportFor::Send, std::thread::spawn(crate::diagnostics::report))),
            Target::SendReport => {}
            Target::MyReports => {
                self.followed = crate::reports::list();
                self.turn_to(Page::Reports);
            }
            Target::Thread(id) => self.open_thread(&id),
            // (Pressed again, its code is put away.)
            Target::GetApp if self.code.as_ref().is_some_and(|(_, what)| *what == CodeFor::App) => self.code = None,
            // (Each asked of the remote's helper on another thread: see `tick`.)
            Target::GetApp => self.ask_remote(|| Some(crate::phone::app_code().map(|code| (code, CodeFor::App)))),
            Target::Pair(web) => self.ask_remote(move || Some(crate::phone::offer(web).map(|code| (code, if web { CodeFor::WebPairing } else { CodeFor::Pairing })))),
            Target::OpenPage if self.code.as_ref().is_some_and(|(_, what)| *what == CodeFor::WebPage) => self.code = None,
            // (Where it is now: this PC's address may have changed.)
            Target::OpenPage => self.ask_remote(|| Some(crate::phone::web_address().as_deref().and_then(crate::phone::code_of).map(|code| (code, CodeFor::WebPage)))),
            Target::Unpair(id) => {
                self.ask_remote(move || {
                    crate::phone::unpair(&id);
                    None
                });
            }
            // A door to Windows' own page: opened as a link, its URI already
            // in the shell's tongue.
            Target::Open(page) => {
                let uri = HSTRING::from(page);
                unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &uri, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
            }
            Target::ThemePick(index) => {
                self.theme_picked = Some(index);
                self.next_frame = Instant::now();
            }
            Target::ThemeConfirm => {
                if let Some(index) = self.theme_picked.take() {
                    super::system_settings::wear_theme(index);
                    // (The theme takes a moment to put on; read again after it.)
                    self.next_frame = Instant::now() + Duration::from_millis(1200);
                }
            }
            // The wireless networks, acted on through the stack.
            Target::WifiJoin(profile) => {
                if !crate::wifi::connect(&profile) {
                    crate::journal::note(format!("wifi: could not join {profile}"));
                }
            }
            // (A new network's key is Windows' to be given: opened where it is asked.)
            Target::WifiAsk(ssid) => {
                let uri = HSTRING::from(format!("ms-settings:network-wifi"));
                let _ = ssid;
                unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &uri, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
            }
            Target::WifiForget(profile) => {
                crate::wifi::forget(&profile);
            }
            // Letting go of the network joined now, without forgetting it.
            Target::WifiDisconnect => {
                if !crate::wifi::disconnect() {
                    crate::journal::note("wifi: could not disconnect");
                }
                self.wifi_key = None;
                self.next_frame = Instant::now();
            }
            // Showing, then hiding, the key kept for the network shown now.
            Target::WifiKey => {
                if self.wifi_key.take().is_none() {
                    let profile = self.wifi_chosen_profile().unwrap_or_default();
                    self.wifi_key = Some(crate::wifi::password(&profile).unwrap_or_default());
                }
                self.next_frame = Instant::now();
            }
            // Picking a network to show in the preview: its key is put away again.
            Target::WifiSelect(ssid) => {
                self.wifi_selected = Some(ssid);
                self.wifi_key = None;
                self.next_frame = Instant::now();
            }
            // A wallpaper put on the desktop.
            Target::SetWallpaper(path) => super::system_settings::set_wallpaper(&path),
            // A Bluetooth pairing is the shell's to be trusted.
            Target::BtPair(_name) => {
                let uri = HSTRING::from("ms-settings:bluetooth");
                unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), &uri, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
            }
            // Picking a device to show in the preview.
            Target::BtSelect(addr) => {
                self.bt_selected = Some(addr);
                self.next_frame = Instant::now();
            }
            // Picking an adapter to show in the preview.
            Target::NetSelect(name) => {
                self.net_selected = Some(name);
                self.next_frame = Instant::now();
            }
            // Picking a physical disk to show its volumes in the preview.
            Target::StorageSelect(number) => {
                self.storage_selected = Some(number);
                self.next_frame = Instant::now();
            }
            // Picking a display to show in the preview.
            Target::ScreenSelect(name) => {
                self.display_selected = Some(name);
                self.next_frame = Instant::now();
            }
            // A display told to wear a mode; left as it was if the driver will not draw it.
            Target::ModeApply { device, wide, tall, hz } => {
                if !crate::sys::set_mode(&device, crate::sys::Mode { wide, tall, hz }) {
                    crate::journal::note(format!("settings: the screen would not wear {wide}×{tall}"));
                }
                self.next_frame = Instant::now();
            }
            // The night light warmed, or washed off at depth none.
            Target::NightSet(level) => {
                crate::sys::set_night(level);
                self.next_frame = Instant::now();
            }
            // The desktop grown to a share; open windows wait to be shown again.
            Target::ScaleSet(percent) => {
                crate::sys::set_scale(percent);
                self.next_frame = Instant::now();
            }
            // Another screen crowned as the one things start on; the layout read again.
            Target::SetPrimary(device) => {
                crate::sys::set_primary(&device);
                self.screens = crate::screens::all();
                self.next_frame = Instant::now();
            }
            // A display turned; left as it was where the driver will not draw it so.
            Target::RotateSet { device, orient } => {
                crate::sys::set_orientation(&device, orient);
                self.next_frame = Instant::now();
            }
            // Picking a sound device to show in the preview.
            Target::AudioSelect(id) => {
                self.audio_selected = Some(id);
                self.next_frame = Instant::now();
            }
            // A device's volume put as asked; the write done off-thread, the list read again when it lands.
            Target::AudioVolume { id, percent } => {
                crate::audio::set(&id, Some(percent), None);
                self.next_frame = Instant::now();
            }
            // A device muted or unmuted, its volume otherwise left be.
            Target::AudioMute { id, muted } => {
                crate::audio::set(&id, None, Some(muted));
                self.next_frame = Instant::now();
            }
            // Picking an app to show in the notification preview.
            Target::NotifySelect(aumid) => {
                self.notify_selected = Some(aumid);
                self.next_frame = Instant::now();
            }
            // An app's banners let or held; the write done off-thread, the list read again when it lands.
            Target::NotifyToggle { aumid, on } => {
                crate::sys::notify_apply(aumid, on);
                self.next_frame = Instant::now();
            }
            // The master switch on notices put on or off, the list read again when it lands.
            Target::NotifyMaster(on) => {
                crate::sys::notify_master_apply(on);
                self.next_frame = Instant::now();
            }
            // The temporary files cleared, off-thread, the figures read again when it lands.
            Target::CleanTemp => {
                crate::sys::storage_clean();
                self.next_frame = Instant::now();
            }
            // The recycle bin emptied, off-thread, the figures read again when it lands.
            Target::EmptyRecycle => {
                crate::sys::storage_empty();
                self.next_frame = Instant::now();
            }
            // Storage sense put on or off, the window woken when it lands.
            Target::StorageSenseApply(on) => {
                crate::sys::storage_sense_apply(on);
                self.next_frame = Instant::now();
            }
            // Picking a capability to show its apps in the privacy preview.
            Target::ConsentSelect(capability) => {
                self.privacy_capability = capability;
                self.next_frame = Instant::now();
            }
            // Letting or holding one app's use of a capability, the write done off-thread.
            Target::ConsentSet { capability, keys, packaged, allow } => {
                crate::sys::consent_apply(capability, keys, packaged, allow);
                self.next_frame = Instant::now();
            }
            // Slid by the pointer, stepped by the arrows; pressing alone does nothing more.
            Target::Grip(_) | Target::Slider(_) => {}
        }
        self.next_frame = Instant::now();
    }

    // ---- Keyboard ----

    /// The focus stop a target belongs to: a row of choices, or the skin
    /// cards, is one stop, at the option chosen.
    fn stop_for(&self, target: &Target) -> Target {
        match target {
            Target::Choice(field, _) => Target::Choice(*field, self.choices(*field).2.unwrap_or(0)),
            Target::Skin(_) => Target::Skin(Skin::named(&self.settings.skin)),
            Target::Grip(id) => Target::Module(id.clone()),
            other => other.clone(),
        }
    }

    /// Every focus stop, in the order the list shows them (scrolled out of
    /// view or not).
    fn stops(&self) -> Vec<Target> {
        let pages = NAV_ITEMS.iter().filter_map(|item| match item {
            NavItem::Page(page) => Some(Target::Nav(*page)),
            _ => None,
        });
        let editor = if self.page == Page::Layout { self.editor_stops() } else { Vec::new() };
        pages.chain(self.layout().into_iter()
            .flat_map(|(row, ..)| match row {
                Row::Skins => vec![Target::Skin(Skin::named(&self.settings.skin))],
                // The screens; then each edge where the pointer stops at
                // it, then where it passes on.
                Row::Library => vec![],
                Row::Choice(field) => vec![Target::Choice(field, self.choices(field).2.unwrap_or(0))],
                // A switch's own button comes first.
                Row::Shortcut(which) if self.shortcut(which).is_some() => vec![Target::Shortcut(which), Target::ClearShortcut(which)],
                Row::Shortcut(which) => vec![Target::Shortcut(which)],
                Row::Version if update::check() == update::Check::Checking => vec![],
                Row::Version => vec![Target::CheckNow],
                Row::Switch(switch) => vec![Target::Switch(switch)],
                Row::Slider(field) => vec![Target::Slider(field)],
                // A module's row, which opens its card if it has one, then its switch.
                Row::Module(id) if self.opens(&id) => vec![Target::Expand(id.clone()), Target::Module(id)],
                Row::Module(id) => vec![Target::Module(id)],
                Row::Item(id, name) if self.module_on(&id) && self.expanded.contains(&id) => vec![Target::Item(id, name)],
                Row::ModuleChoice(id, field) if self.module_on(&id) && self.expanded.contains(&id) => vec![Target::Choice(field, self.choices(field).2.unwrap_or(0))],
                Row::Item(..) | Row::ModuleChoice(..) => vec![],
                Row::Update if update::available().is_some_and(|(_, state)| state == update::State::Downloading) => vec![],
                Row::Update => update::available().map_or(Vec::new(), |(_, state)| update_offer(state, self.lang).1.into_iter().map(|(_, target, _)| target).collect()),
                Row::Notice => self.button_row(&Row::Notice, Instant::now()).map_or(Vec::new(), |(_, _, buttons)| buttons.into_iter().map(|(_, target, _)| target).collect()),
                Row::Diagnostics => vec![Target::SendReport, Target::Diagnostics],
                Row::MyReports => vec![Target::MyReports],
                Row::Report(id) => vec![Target::Thread(id)],
                Row::Uninstall => vec![Target::Uninstall],
                Row::NetEase if self.netease_work.is_some() => vec![],
                Row::NetEase if self.netease_has == crate::netease::Installed::Yes && self.netease_restart && self.netease_failed.is_none() => vec![Target::NetEaseRestart, Target::NetEase(NetEaseWork::Remove)],
                Row::NetEase => match (self.netease_failed.map(|(work, _)| work), self.netease_has) {
                    (Some(work), _) => vec![Target::NetEase(work)],
                    (None, crate::netease::Installed::Yes) => vec![Target::NetEase(NetEaseWork::Remove)],
                    (None, crate::netease::Installed::NoIconRow) => vec![Target::NetEase(NetEaseWork::IconRow), Target::NetEase(NetEaseWork::Remove)],
                    (None, crate::netease::Installed::No) => vec![Target::NetEase(NetEaseWork::Install)],
                },
                Row::Quit => vec![Target::Quit],
                Row::Guide => vec![Target::Guide],
                Row::Pair => vec![Target::Pair(false), Target::Pair(true), Target::GetApp],
                Row::WebPage => vec![Target::OpenPage],
                Row::Phone(id, _) => vec![Target::Unpair(id)],
                Row::Code | Row::Group(_) => vec![],
                Row::More(_) => vec![Target::More],
                Row::Tabs => TABS.iter().map(|tab| Target::Tab(*tab)).collect(),
                Row::Element(name) => vec![Target::Element(name)],
                Row::Drive(volume) => vec![Target::Drive(volume)],
                Row::RecentFolder(folder) => vec![Target::RecentFolder(folder)],
                Row::AddRecentFolder if self.choosing_folders.is_some() => vec![],
                Row::AddRecentFolder => vec![Target::AddRecentFolder],
                Row::Title | Row::Note(_) | Row::Info(_) => vec![],
                Row::WifiNet(..) => vec![],
                Row::BtDevice(..) => vec![],
                Row::NetAdapter(..) => vec![],
                Row::Screen(..) => vec![],
                Row::AudioDevice(..) => vec![],
                Row::NotifyApp(..) => vec![],
                Row::ConsentCap(..) => vec![],
                Row::Disk(..) => vec![],
                Row::Do(d) => vec![d.what],
                Row::Nav(.., target) => vec![target.clone()],
            }))
            .chain(editor)
            .collect()
    }

    /// A key pressed (`repeat` when it is held down and auto-repeating): Tab
    /// and Shift+Tab move between controls, the arrows change a row's
    /// choice, Space and Enter press, Alt with the up and down arrows moves a
    /// module, Escape closes the window. Returns whether the key was used;
    /// any other combination with Alt is left to the system.
    fn key(&mut self, key: VIRTUAL_KEY, shift: bool, alt: bool, repeat: bool) -> bool {
        self.stop_drag();
        if let Some(which) = self.recording {
            self.take_shortcut(which, key);
            return true;
        }
        let moves_module = alt && (key == VK_UP || key == VK_DOWN) && matches!(self.focus, Some(Target::Module(_) | Target::Expand(_)));
        if alt && !moves_module {
            return false;
        }
        // The focus shows once a key that moves it or acts on it is used
        // (not for a key that does nothing here: one of a shortcut, say).
        if matches!(key, VK_TAB | VK_SPACE | VK_RETURN | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN) {
            self.keyboard = true;
        }
        self.next_frame = Instant::now();
        let stops = self.stops();
        let at = self.focus.as_ref().and_then(|focus| stops.iter().position(|stop| stop == focus));
        match key {
            VK_TAB if !stops.is_empty() => {
                let next = match (at, shift) {
                    (None, false) => 0,
                    (None, true) => stops.len() - 1,
                    (Some(i), false) => (i + 1) % stops.len(),
                    (Some(i), true) => (i + stops.len() - 1) % stops.len(),
                };
                self.focus = Some(stops[next].clone());
                self.reveal_focus();
            }
            VK_ESCAPE => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            // Holding the key presses once.
            VK_SPACE | VK_RETURN if repeat => {}
            VK_SPACE | VK_RETURN => {
                if let Some(focus) = self.focus.clone() {
                    self.activate(focus);
                }
            }
            VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
                let forward = key == VK_RIGHT || key == VK_DOWN;
                match self.focus.clone() {
                    Some(Target::Module(id) | Target::Expand(id)) if moves_module => self.shift_module(&id, forward),
                    Some(Target::Slider(field)) => {
                        let (_, min, max, step, value) = self.slider(field);
                        let next = (value + if forward { step } else { -step }).clamp(min, max);
                        if next != value {
                            self.set_slider(field, next);
                            self.keep_slider(field);
                        }
                    }
                    Some(Target::Choice(field, index)) => {
                        let count = self.choices(field).1.len();
                        let next = if forward { (index + 1).min(count - 1) } else { index.saturating_sub(1) };
                        if next != index {
                            self.activate(Target::Choice(field, next));
                            self.focus = Some(Target::Choice(field, next));
                        }
                    }
                    Some(Target::Skin(skin)) => {
                        let order = [Skin::Paper, Skin::Glass, Skin::Fluent];
                        let index = order.iter().position(|s| *s == skin).unwrap_or(0);
                        let next = if forward { (index + 1).min(order.len() - 1) } else { index.saturating_sub(1) };
                        if next != index {
                            self.activate(Target::Skin(order[next]));
                            self.focus = Some(Target::Skin(order[next]));
                        }
                    }
                    // Elsewhere the up and down arrows scroll the list.
                    _ if key == VK_UP || key == VK_DOWN => self.scroll_by(if forward { WHEEL / 2.0 } else { -WHEEL / 2.0 }),
                    _ => return false,
                }
            }
            VK_PRIOR | VK_NEXT => {
                let page = self.client().1 * 0.8;
                self.scroll_by(if key == VK_NEXT { page } else { -page });
            }
            _ => return false,
        }
        true
    }

    /// Moves module `id` one place up or down the list.
    fn shift_module(&mut self, id: &str, down: bool) {
        let before = self.row_tops();
        // Past the next module listed with it: one in the fold, or listed
        // there, is passed over.
        let folded = |other: &str| !id.starts_with(OVERLAY) && !id.starts_with(WORKBENCH) && self.folded(other);
        if folded(id) {
            return;
        }
        let skip: Vec<String> = self.prefs.modules.iter().filter(|entry| folded(&entry.id) || entry.id == "lyrics").map(|entry| entry.id.clone()).collect();
        let (list, bare) = self.list(id);
        let Some(at) = list.iter().position(|entry| entry.id == bare) else { return };
        let mut to = at;
        loop {
            to = if down { to + 1 } else { to.wrapping_sub(1) };
            if to >= list.len() {
                return;
            }
            if !skip.contains(&list[to].id) {
                break;
            }
        }
        let entry = list.remove(at);
        list.insert(to, entry);
        self.glide_rows(before, "");
        self.save();
        self.reveal_focus();
    }

    fn scroll_by(&mut self, distance: f32) {
        let most = (self.content_height() - self.client().1).max(0.0);
        self.scroll_target = (self.scroll_target + distance).clamp(0.0, most);
    }

    /// Scrolls the list so the focused control is in view.
    fn reveal_focus(&mut self) {
        let Some(focus) = self.focus.clone() else { return };
        let row = self.layout().into_iter().find(|(row, ..)| match (row, &focus) {
            (Row::Skins, Target::Skin(_)) | (Row::Update, Target::Update | Target::Download) | (Row::Notice, Target::NoticeRead | Target::NoticeLink) | (Row::Diagnostics, Target::Diagnostics | Target::SendReport) | (Row::Uninstall, Target::Uninstall) | (Row::Quit, Target::Quit) | (Row::Guide, Target::Guide) => true,
            (Row::Version, Target::CheckNow) => true,
            (Row::Shortcut(a), Target::Shortcut(b) | Target::ClearShortcut(b)) => a == b,
            (Row::Pair, Target::Pair(_) | Target::GetApp) | (Row::WebPage, Target::OpenPage) => true,
            (Row::NetEase, Target::NetEase(_) | Target::NetEaseRestart) => true,
            (Row::Phone(a, _), Target::Unpair(b)) => a == b,
            (Row::MyReports, Target::MyReports) => true,
            (Row::Report(a), Target::Thread(b)) => a == b,
            (Row::Drive(a), Target::Drive(b)) => a == b,
            (Row::RecentFolder(a), Target::RecentFolder(b)) => a == b,
            (Row::AddRecentFolder, Target::AddRecentFolder) => true,
            (Row::Choice(f), Target::Choice(g, _)) => f == g,
            (Row::Switch(s), Target::Switch(t)) => s == t,
            (Row::Element(a), Target::Element(b)) => a == b,
            (Row::Tabs, Target::Tab(_)) => true,
            (Row::Slider(f), Target::Slider(g)) => f == g,
            (Row::Module(m), Target::Module(n) | Target::Expand(n)) => m == n,
            (Row::Item(m, a), Target::Item(n, b)) => m == n && a == b,
            (Row::ModuleChoice(_, f), Target::Choice(g, _)) => f == g,
            _ => false,
        });
        let Some((_, top, height)) = row else { return };
        let view = self.client().1;
        let margin = 24.0;
        let most = (self.content_height() - view).max(0.0);
        if top - margin < self.scroll_target {
            self.scroll_target = (top - margin).clamp(0.0, most);
        } else if top + height + margin > self.scroll_target + view {
            self.scroll_target = (top + height + margin - view).clamp(0.0, most);
        }
    }

    fn wheel(&mut self, delta: i16) {
        let Some((x, y)) = self.pointer else { return };
        // Over the phone's screen, its page is what moves.
        if let Some((_, k)) = self.phone.at.filter(|(at, _)| self.page == Page::Phone && x >= at.x && x < at.x + at.w && y >= at.y && y < at.y + at.h) {
            self.phone.scroll = (self.phone.scroll - delta as f32 / 120.0 * WHEEL / k).clamp(0.0, self.phone.most);
            self.next_frame = Instant::now();
            return;
        }
        if x < NAV {
            let most = (self.nav_content_height() - self.client().1).max(0.0);
            self.nav_scroll_target = (self.nav_scroll_target - delta as f32 / 120.0 * WHEEL).clamp(0.0, most);
            self.next_frame = Instant::now();
            return;
        }
        if !(NAV..NAV + self.pane()).contains(&x) {
            // Past the list: the right pane is what moves — the 通用 hub's column of cards, or a
            // page's own preview when its content reaches below the pane's bottom (Display's rows,
            // Privacy's apps).
            // While a picker's list stands open over the pane, the wheel turns the list itself.
            if let Some((field, _)) = self.hub_menu {
                let full = DROP_LINE * self.choices(field).1.len() as f32 + 8.0;
                let most = (full - DROP_CAP).max(0.0);
                self.dropdown_scroll = (self.dropdown_scroll - delta as f32 / 120.0 * WHEEL).clamp(0.0, most);
                self.next_frame = Instant::now();
                return;
            }
            if matches!(self.page, Page::System | Page::Display | Page::Privacy) {
                let most = self.hub_most();
                if most > 0.0 {
                    self.hub_scroll_target = (self.hub_scroll_target - delta as f32 / 120.0 * WHEEL).clamp(0.0, most);
                    self.next_frame = Instant::now();
                }
            }
            return;
        }
        let height = self.client().1;
        let most = (self.content_height() - height).max(0.0);
        self.scroll_target = (self.scroll_target - delta as f32 / 120.0 * WHEEL).clamp(0.0, most);
        self.next_frame = Instant::now();
    }

    /// Where a module's row is laid out (document DIPs).
    fn row_top(&self, id: &str) -> f32 {
        self.layout().iter().find(|(row, ..)| matches!(row, Row::Module(m) if m == id)).map_or(0.0, |(_, y, _)| *y)
    }

    /// Moves the dragged row to where its middle now is; the rows it passes
    /// glide from where they were to where they now belong.
    fn reorder(&mut self) {
        let Some(drag) = &self.drag else { return };
        // (Each row as tall as its words make it.)
        let spans: HashMap<String, (f32, f32)> =
            self.layout().into_iter().filter_map(|(row, y, height)| if let Row::Module(id) = row { Some((id, (y, height))) } else { None }).collect();
        let middle = drag.pointer - drag.grab + spans.get(&drag.id).map_or(0.0, |(_, height)| height / 2.0);
        let before = self.row_tops();
        let id = drag.id.clone();
        let prefix = [OVERLAY, WORKBENCH].into_iter().find(|prefix| id.starts_with(prefix)).unwrap_or("");
        let (list, bare) = self.list(&id);
        let held = list.iter().position(|entry| entry.id == bare).unwrap();
        let entry = list.remove(held);
        let next = list.iter().position(|other| before.get(&format!("{prefix}{}", other.id)).is_some_and(|top| middle < top + spans[&format!("{prefix}{}", other.id)].1 / 2.0)).unwrap_or(list.len());
        list.insert(next, entry);
        if next == held {
            return;
        }
        self.glide_rows(before, &id);
    }

    /// Where each module's row is laid out (document DIPs).
    fn row_tops(&self) -> HashMap<String, f32> {
        self.layout().into_iter().filter_map(|(row, y, _)| if let Row::Module(id) = row { Some((id, y)) } else { None }).collect()
    }

    /// After the modules' order changed: every row but `moved` glides from
    /// where it was (`before`) to where it now belongs.
    fn glide_rows(&mut self, before: HashMap<String, f32>, moved: &str) {
        let now = Instant::now();
        let after = self.row_tops();
        let id = moved.to_string();
        for (other, was) in before {
            let Some(&now_at) = after.get(&other) else { continue };
            if other == id || now_at == was {
                continue;
            }
            let key = format!("row:{other}");
            let current = self.motion.get(&key).map_or(0.0, |t| t.value(now));
            let mut glide = Transition::settled(was - now_at + current);
            glide.retarget(0.0, GLIDE.0, GLIDE.1, now);
            self.motion.insert(key, glide);
        }
    }

    fn client(&self) -> (f32, f32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        ((rect.right - rect.left) as f32 / self.scale, (rect.bottom - rect.top) as f32 / self.scale)
    }

    /// A transition's value, made at `start` if it is new, and sent toward
    /// `to` if that changed.
    fn animate(&mut self, key: String, to: f32, how: (Duration, Easing), now: Instant, jump: bool) -> f32 {
        let transition = self.motion.entry(key).or_insert_with(|| Transition::settled(to));
        if jump {
            transition.jump(to);
        } else if transition.target() != to {
            transition.retarget(to, how.0, how.1, now);
        }
        transition.value(now)
    }

    // ---- Drawing ----

    /// Takes up the thread's graphics device for the screen the window is on if it is another (the last was lost, or
    /// the window was moved to a screen another adapter drives: see `gfx::on_screen`), making the window's surface and
    /// the preview's bitmaps again on it.
    fn follow_device(&mut self) -> bool {
        let Ok(current) = gfx::for_window(self.hwnd) else { return false };
        if Rc::ptr_eq(&current, &self.gfx) && self.surface.is_some() {
            return true;
        }
        // All it drew with was the other device's (see `life`). (A window has one composition target: the old one
        // goes first.)
        self.gone();
        let surface = match Surface::new(&current, self.hwnd) {
            Ok(surface) => surface,
            Err(error) => {
                gfx::failed("settings", &current, Some(&error));
                return false;
            }
        };
        self.surface = Some(surface);
        self.gfx = current;
        true
    }

    /// What has come in since it last looked, taken in: desktops read meanwhile (the one shown, if it waited for it;
    /// else kept, unless kept already; the map's in place, if it is still of that screen as it is), and the screen the
    /// preview shows, as it is now, its desktop and those kept wanted till each holds the wallpaper as it is now (read with
    /// those the frame drawn next wants: see `read_wanted`). On every turn of the loop, the window drawn or not (held under the guide, whose small desktops
    /// are the preview's): what it shows taken in is noted as not yet drawn (see `guide_holds_window`).
    fn take_in(&mut self) {
        while let Ok(read) = self.read.1.try_recv() {
            let (lot, ask, desktop) = match read {
                Read::Came { lot, ask, desktop } => (lot, ask, desktop),
                Read::Done(lot) => {
                    self.reads.done(lot);
                    continue;
                }
            };
            if !self.reads.came(lot, &ask) {
                continue;
            }
            // (Drawn with it now, as far as it is drawn: see `guide_holds_window`.)
            self.taken_unseen = true;
            self.next_frame = Instant::now();
            match ask {
                reads::Ask::Stage { screen, work, size } => {
                    let stage = Stage { current: true, ..Stage::new(size, Some(screen), work, Some(desktop)) };
                    let same = |other: &Stage| other.screen == stage.screen && other.work == stage.work && other.size == stage.size;
                    // (What was shown at once, as it was kept: taken again only if the desktop has changed since, and
                    // known to be as it is now either way.)
                    let changed = |other: &Stage| other.desktop.as_ref().is_none_or(|was| stage.desktop.as_ref().is_some_and(|now| now.digest != was.digest));
                    let held = if same(&self.stage) { Some(&mut self.stage) } else { self.kept_stages.iter_mut().find(|kept| same(kept)) };
                    match held {
                        Some(held) if changed(held) => *held = stage,
                        Some(held) => held.current = true,
                        None => self.kept_stages.push(stage),
                    }
                }
                reads::Ask::Map { id, monitor, size } => map_art_came(&mut self.map_art, &id, monitor, size, desktop),
            }
        }
        if let Some(screen) = self.stage_screen() {
            self.show_screen(&screen);
        }
        // (Each one shown or kept, till it holds the wallpaper as it is now: so a refresh asked for is not lost to the
        // screens changing as it is read.)
        for ask in std::iter::once(&self.stage).chain(&self.kept_stages).filter_map(Stage::wanted).collect::<Vec<_>>() {
            self.reads.want(ask);
        }
    }

    fn draw(&mut self, now: Instant) {
        let _timed = crate::perf::Timed::start("settings frame", || format!("{:?} {:?}", self.page, self.tab));
        self.take_in();
        self.taken_unseen = false;
        crate::playing::show_in_settings(matches!(self.page, Page::Music | Page::Layout));
        self.sources = crate::playing::music_players();
        if let Some(chosen) = self.settings.music_source.as_ref().filter(|chosen| !self.sources.iter().any(|(id, _)| *id == chosen.id)) {
            self.sources.push((chosen.id.clone(), chosen.name.clone()));
        }
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.scroll += (self.scroll_target - self.scroll) * (1.0 - E.powf(-dt / SCROLL_EASE));
        if (self.scroll_target - self.scroll).abs() < 0.05 {
            self.scroll = self.scroll_target;
        }
        self.nav_scroll += (self.nav_scroll_target - self.nav_scroll) * (1.0 - E.powf(-dt / SCROLL_EASE));
        if (self.nav_scroll_target - self.nav_scroll).abs() < 0.05 {
            self.nav_scroll = self.nav_scroll_target;
        }
        self.hub_scroll += (self.hub_scroll_target - self.hub_scroll) * (1.0 - E.powf(-dt / SCROLL_EASE));
        if (self.hub_scroll_target - self.hub_scroll).abs() < 0.05 {
            self.hub_scroll = self.hub_scroll_target;
        }
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        let size = ((rect.right - rect.left).max(1) as u32, (rect.bottom - rect.top).max(1) as u32);
        if !self.follow_device() {
            self.next_frame = now + DEVICE_RETRY;
            return;
        }
        // Hidden (made for the guide, about to be shown by it): only the guide drawn (see `guide`).
        if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            self.next_frame = now + HIDDEN_LOOKED_AT;
            self.draw_guide(now);
            return;
        }
        self.shown();
        let gfx = self.gfx.clone();
        let mut surface = self.surface.take().unwrap();
        let mut creep = None;
        let began = Instant::now();
        let sizing = gfx::sizing(self.by_hand);
        let drawn = surface.draw(&gfx, size, self.scale, |frame| {
            frame.origin(0.0, 0.0);
            creep = self.paint(frame, now);
        });
        drop(sizing);
        // A frame slow enough to be seen, noted (as the widgets' are).
        let took = began.elapsed();
        if took > SLOW_FRAME {
            let ms = |d: Duration| d.as_secs_f32() * 1000.0;
            let (given, handed) = (surface.took.given, surface.took.handed);
            crate::widget::note_sparingly("settings frame", || {
                format!("settings: a frame took {:.1} ms (its surface given in {:.1}, painted {:.1}, handed to the compositor {:.1})", ms(took), ms(given), ms(took.saturating_sub(given + handed)), ms(handed))
            });
        }
        self.surface = Some(surface);
        self.drawn_size = drawn.is_ok().then_some(size);
        // Not drawn: drawn again shortly (the device gone, on a new one).
        if let Err(error) = &drawn {
            gfx::failed("settings", &self.gfx, Some(error));
            self.next_frame = now + DEVICE_RETRY;
            return;
        }
        self.gfx.sweep();
        self.guide_saw_window_drawn();
        // Sized by hand, and now drawn at rest: what was made for it as it was sized is let go (see `Gfx::settle`).
        if self.by_hand {
            self.was_sized = true;
        } else if std::mem::take(&mut self.was_sized) {
            self.gfx.settle();
        }
        // (A row held still moves only as the pointer does, which asks for a frame itself.)
        let moving = self.dots_moving || self.motion.values().any(|t| !t.done(now)) || self.scroll != self.scroll_target || self.layers.turn.turning(now);
        self.settle_thumbs = false;
        self.next_frame = if moving {
            now
        } else {
            // Otherwise as often as the preview's charts creep a quarter pixel.
            now + creeping(creep).min(Duration::from_millis(250))
        };
        // (The desktops wanted as it was drawn read together, those taken in before it was too: see `take_in`.)
        self.read_wanted();
        self.draw_guide(now);
    }

    /// Draws everything, and returns how fast the preview's charts move
    /// (physical pixels a second).
    fn paint(&mut self, frame: &Frame, now: Instant) -> Option<f32> {
        // (The guide's demonstration on the layout page moved on, where what it moves to is as last drawn.)
        self.play_drag(now);
        let palette = self.palette.clone();
        let (width, height) = self.client();
        if !self.mica {
            fill(frame, palette.window, 0.0, 0.0, width, height, 0.0);
        }
        self.hovered_now = self.hovered();
        self.targets.clear();
        self.follows_pointer.clear();
        self.paint_nav(frame, &palette);
        // The preview before the list (they do not meet): it finds how large the panel can be on its screen, which the
        // panel size's slider reaches to (drawn first, the list's slider took the size before it was found, the
        // largest there is, and its knob jumped as the window first showed).
        let creep = {
            let _timed = crate::perf::Timed::start("settings preview", String::new);
            self.paint_preview(frame, &palette, width, height)
        };
        unsafe { frame.dc.PushAxisAlignedClip(&rect(NAV, 0.0, self.pane(), height), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        {
            let _timed = crate::perf::Timed::start("settings list", String::new);
            self.paint_list(frame, &palette, now, height);
        }
        unsafe { frame.dc.PopAxisAlignedClip() };
        self.paint_played_pointer(frame);
        creep
    }

    /// How tall the nav's items are all told, so the column can scroll when they outgrow the window: what `paint_nav`
    /// lays from below its title down. Its scroll is held at 0 while they fit (see `nav_scroll`).
    fn nav_content_height(&self) -> f32 {
        NAV_ITEMS
            .iter()
            .map(|item| match item {
                NavItem::Page(Page::Phone) => NAV_ITEM + 4.0 + NAV_GAP,
                NavItem::Page(_) => NAV_ITEM + 4.0,
                _ => NAV_GAP + NAV_HEADING,
            })
            .sum()
    }

    /// The pages' list: the window's name, then each page, the one shown lit and marked in the accent. The components'
    /// own pages come under their heading (a name, not a page), and a rule sets them apart from the phone's and
    /// Glance's own after them.
    fn paint_nav(&mut self, frame: &Frame, palette: &Palette) {
        let lang = self.lang;
        let hovered = self.hovered_now.clone();
        let left = 16.0;
        let width = NAV - 2.0 * left + 8.0;
        let title = Font::new(Family::SegoeDisplay, 20.0, 600.0);
        text_centred(frame, pick(lang, "设置", "Settings"), title, palette.text, left + 8.0, PAD_TOP + 18.0, width, Align::Start);
        let label = Font::new(Family::Segoe, 14.0, 400.0);
        let heading = Font::new(Family::Segoe, 12.0, 600.0);
        let top = PAD_TOP + 52.0;
        let view = self.client().1;
        let mut y = top - self.nav_scroll;
        unsafe { frame.dc.PushAxisAlignedClip(&rect(0.0, top, NAV, (view - top).max(0.0)), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        for item in NAV_ITEMS {
            let page = match item {
                NavItem::Page(page) => page,
                _ => {
                    y += NAV_GAP;
                    text_centred(frame, item.name(lang), heading, palette.text3, left + 12.0, y + NAV_HEADING / 2.0, width, Align::Start);
                    y += NAV_HEADING;
                    continue;
                }
            };
            if page == Page::Phone {
                y += NAV_GAP / 2.0;
                fill(frame, palette.rule, left + 12.0, y - 0.5, width - 24.0, 1.0, 0.0);
                y += NAV_GAP / 2.0;
            }
            let r = Rect { x: left, y, w: width, h: NAV_ITEM };
            y += NAV_ITEM + 4.0;
            // (Scrolled clean out of the column: neither drawn nor clickable.)
            if r.y + r.h < top || r.y > view {
                continue;
            }
            let target = Target::Nav(page);
            // (The reports' page is General's own.)
            let (shown, hover) = (page == self.page || (page == Page::General && self.page == Page::Reports), hovered.as_ref() == Some(&target));
            if shown || hover {
                fill(frame, if shown { palette.control } else { palette.hover }, r.x, r.y, r.w, r.h, 6.0);
            }
            if shown {
                fill(frame, palette.selection, r.x, r.y + 12.0, 3.0, r.h - 24.0, 1.5);
            }
            let cy = r.y + r.h / 2.0;
            frame.icon(page.icon(), Point { x: r.x + 22.0, y: cy }, 16.0, palette.text);
            text_centred(frame, item.name(lang), label, palette.text, r.x + 46.0, cy, r.w - 50.0, Align::Start);
            // A newer Glance is out, the site has a notice unread, or a report sent has news unread: a dot on the page
            // they are seen from.
            if page == Page::General && self.general_has_news() {
                fill(frame, palette.selection, r.x + r.w - 20.0, cy - 3.0, 6.0, 6.0, 3.0);
            }
            self.targets.push((r, target));
        }
        unsafe { frame.dc.PopAxisAlignedClip() };
        // A hairline thumb beside the column once its items outgrow the window, so it reads as scrollable.
        let room = (view - top).max(0.0);
        let most = (self.nav_content_height() - room).max(0.0);
        if most > 0.0 {
            let thumb = (room * room / self.nav_content_height()).max(24.0);
            let at = top + (room - thumb) * self.nav_scroll / most;
            let over = self.pointer.is_some_and(|(x, _)| x < NAV);
            fill(frame, palette.text3.alpha(if over { 0.7 } else { 0.4 }), NAV - 5.0, at, 2.0, thumb, 1.0);
        }
    }

    fn paint_list(&mut self, frame: &Frame, palette: &Palette, now: Instant, height: f32) {
        let lang = self.lang;
        let left = NAV + PAD_SIDE;
        let width = self.cards();
        // (The open list hangs from the box as it is drawn; gone with it if the box is not drawn at all.)
        self.dropdown_anchor = None;
        let scroll = self.scroll;
        let hovered = self.hovered_now.clone();
        let over = match hovered {
            Some(Target::Slider(field)) => Some(field),
            _ => None,
        };
        self.dots_moving = self.slider_dots.step(now, over, self.sliding.map(|(field, _)| field));
        let label = Font::new(Family::Segoe, 14.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        let mut dragged = None;
        // The diagnostics sent: how that went, once it is over.
        if self.sending.as_ref().is_some_and(|sending| sending.is_finished()) {
            self.sent = Some(self.sending.take().unwrap().join().ok().flatten());
        }
        if self.sending.is_some() {
            self.next_frame = now + Duration::from_millis(300);
        }
        // What was added to a report: how that went, once it is over.
        if self.adding.as_ref().is_some_and(|(_, adding)| adding.is_finished()) {
            let (id, adding) = self.adding.take().unwrap();
            let sent = adding.join().unwrap_or(false);
            // Its thread's window, open: written in afresh, or what was written kept to be sent again.
            if let Some(window) = self.thread_window(&id) {
                if sent {
                    super::ask::thread::sent(window);
                } else {
                    super::ask::thread::failed(window, pick(lang, "发送失败，请稍后重试", "It could not be sent. Try again later"));
                }
            }
            self.added = Some((id, sent));
            self.followed = crate::reports::list();
            self.follow_thread();
        }
        if self.adding.is_some() {
            self.next_frame = now + Duration::from_millis(300);
        }
        // A report closed or opened again: how that went, once it is over.
        if self.turning.as_ref().is_some_and(|(_, turning)| turning.is_finished()) {
            let (id, turning) = self.turning.take().unwrap();
            let done = turning.join().unwrap_or(false);
            if let Some(window) = self.thread_window(&id) {
                if done {
                    super::ask::thread::writing(window);
                } else {
                    super::ask::thread::failed(window, pick(lang, "操作失败，请稍后重试", "It could not be done. Try again later"));
                }
            }
            self.followed = crate::reports::list();
            self.follow_thread();
        }
        if self.turning.is_some() {
            self.next_frame = now + Duration::from_millis(300);
        }
        // The reports sent, as they are now (news is heard on the update check's thread), and the thread open with them.
        if now.duration_since(self.followed_at) > Duration::from_secs(1) {
            self.followed = crate::reports::list();
            self.followed_at = now;
            self.follow_thread();
        }
        // The switches as they are, read.
        if self.reading_switches.as_ref().is_some_and(|reading| reading.is_finished()) {
            if let Ok((autostart, remote)) = self.reading_switches.take().unwrap().join() {
                self.autostart = autostart;
                self.remote = remote;
            }
            *SWITCHES_READ.lock().unwrap() = Some((self.autostart, self.remote));
            self.next_frame = now;
        }
        // A switch turned: as it is now.
        if self.switching.as_ref().is_some_and(|(_, turning)| turning.is_finished()) {
            let (what, turning) = self.switching.take().unwrap();
            let on = turning.join().unwrap_or(false);
            match what {
                SwitchAsked::Startup => self.autostart = on,
                SwitchAsked::Remote => {
                    self.remote = on;
                    self.code = None;
                    self.phones = crate::phone::phones();
                    self.ask_remote(|| None);
                }
            }
            *SWITCHES_READ.lock().unwrap() = Some((self.autostart, self.remote));
            self.next_frame = now;
        }
        // The diagnostics put together: copied, or shown to be sent.
        if self.reporting.as_ref().is_some_and(|(_, putting)| putting.is_finished()) {
            let (what, putting) = self.reporting.take().unwrap();
            if let Ok(report) = putting.join() {
                match what {
                    ReportFor::Copy => {
                        if crate::diagnostics::copy(self.hwnd, &report) {
                            self.copied_at = Some(Instant::now());
                        }
                    }
                    ReportFor::Send => self.send_report(report),
                    ReportFor::Add => self.add_with_diagnostics(report),
                }
            }
            self.next_frame = now;
        }
        // The remote's helper answered: as it is now, and what was asked of it.
        if self.remote_asked.as_ref().is_some_and(|asked| asked.is_finished()) {
            if let Ok(answer) = self.remote_asked.take().unwrap().join() {
                self.remote_lights = answer.lights;
                self.remote_raised = answer.raised;
                self.remote_screens = answer.screens;
                self.web_address = answer.address;
                if let Some(code) = answer.code {
                    self.code = code;
                }
                self.phones = answer.phones;
            }
            self.next_frame = now;
        }
        if self.switching.is_some() || self.reading_switches.is_some() || self.reporting.is_some() || self.remote_asked.is_some() {
            self.next_frame = self.next_frame.min(now + Duration::from_millis(100));
        }
        // The lights' program fetched (or taken away): how that went, once it is over.
        // (Once what was being asked of the remote is over: that would not have heard of this, and another asked
        // meanwhile is not taken.)
        let free = self.remote_asked.is_none();
        if free && self.fetching.as_ref().is_some_and(|fetching| fetching.is_finished()) {
            let done = self.fetching.take().unwrap().join().unwrap_or(false);
            self.fetch_failed = !done;
            self.ask_remote(|| None);
        }
        if free && self.remote_asked.is_none() && self.raising.as_ref().is_some_and(|raising| raising.is_finished()) {
            let done = self.raising.take().unwrap().join().unwrap_or(false);
            self.raise_failed = !done;
            self.ask_remote(|| None);
        }
        if self.fetching.is_some() || self.raising.is_some() {
            self.next_frame = now + Duration::from_millis(300);
        }
        if self.netease_work.as_ref().is_some_and(|(_, work)| work.is_finished()) {
            let (work, handle) = self.netease_work.take().unwrap();
            match handle.join().unwrap_or(Err(crate::netease::Failed::Put)) {
                Ok((ours, restart)) => {
                    // Whose the loader is now: Glance's only as it put it in (taken back, no longer).
                    if work == NetEaseWork::Remove || ours {
                        self.settings.netease_loader = work == NetEaseWork::Install;
                        self.save();
                    }
                    self.netease_restart = restart;
                    self.netease_failed = None;
                }
                Err(failed) => self.netease_failed = Some((work, failed)),
            }
            self.netease_has = self.netease.as_ref().map_or(crate::netease::Installed::No, crate::netease::installed);
        }
        if self.netease_work.is_some() {
            self.next_frame = now + Duration::from_millis(300);
        }
        // The drives Recent files reads, read again now and then while they are offered (a drive plugged in or taken
        // out comes and goes), taken as they are read.
        if self.reading_drives.as_ref().is_some_and(|reading| reading.is_finished()) {
            if let Ok(drives) = self.reading_drives.take().unwrap().join() {
                self.drives = drives;
            }
            self.next_frame = now;
        }
        if self.page == Page::Workbench && self.settings.station.recent && self.reading_drives.is_none() && self.drives_at.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(2)) {
            self.reading_drives = Some(std::thread::spawn(crate::station::recent::volumes));
            self.drives_at = Some(now);
        }
        // The folders chosen in Windows' chooser, once it is closed.
        if self.choosing_folders.as_ref().is_some_and(|choosing| choosing.is_finished()) {
            let chosen = self.choosing_folders.take().unwrap().join().unwrap_or_default();
            self.add_recent_folders(chosen);
            self.next_frame = now;
        }
        // The phones paired, read again now and then while they are shown: one that has just taken the pairing offered appears, and its code goes.
        // (Read only once the remote has written them since: looked at, not read, every second.)
        if self.page == Page::Phone && self.remote && now.duration_since(self.phones_at) > Duration::from_secs(1) {
            let written = crate::phone::phones_written();
            if written != self.phones_written {
                let phones = crate::phone::phones();
                if phones.len() > self.phones.len() && self.code.as_ref().is_some_and(|(_, what)| matches!(what, CodeFor::Pairing | CodeFor::WebPairing)) {
                    self.code = None;
                }
                self.phones = phones;
                self.phones_written = written;
            }
            self.phones_at = now;
            self.next_frame = now + Duration::from_secs(1);
        }
        let placed = self.layout();
        // How far each module's card reaches down: its row, and as it opens,
        // that share of the rows inside it; and where the card's top is.
        // A card's rows follow its module's.
        let mut reach: HashMap<String, f32> = HashMap::new();
        let mut card_top: HashMap<String, f32> = HashMap::new();
        let mut own: HashMap<String, f32> = HashMap::new();
        for (row, top, row_height) in &placed {
            match row {
                Row::Module(id) => {
                    card_top.insert(id.clone(), *top);
                    reach.insert(id.clone(), *row_height);
                    own.insert(id.clone(), *row_height);
                }
                Row::Item(id, _) | Row::ModuleChoice(id, _) => {
                    reach.insert(id.clone(), top + row_height - card_top[id]);
                }
                _ => {}
            }
        }
        let open: HashMap<String, f32> = card_top.keys().map(|id| (id.clone(), self.opened(id))).collect();
        for (id, extent) in reach.iter_mut() {
            // (Closed, as tall as the module's own row, its words in as many lines as they take.)
            *extent = own[id] + (*extent - own[id]) * open[id];
        }
        for (row, top, row_height) in placed {
            let y = top - scroll;
            let extent = if let Row::Module(id) = &row { reach[id] } else { row_height };
            if y > height || y + extent + 60.0 < 0.0 {
                continue;
            }
            // A row inside a card shows only within the card as far as it
            // has opened, and takes clicks once it is open.
            let inside = match &row {
                Row::Item(id, _) | Row::ModuleChoice(id, _) => {
                    let glide = self.motion.get(&format!("row:{id}")).map_or(0.0, |t| t.value(now));
                    let top = card_top[id] - scroll + glide;
                    unsafe { frame.dc.PushAxisAlignedClip(&rect(left, top, width, reach[id]), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
                    Some(open[id] >= 1.0)
                }
                _ => None,
            };
            let usable = inside.unwrap_or(true);
            match row {
                Row::More(count) => self.more_row(frame, palette, count, left, y, width, row_height, now),
                Row::Title => {
                    let title = Font::new(Family::SegoeDisplay, 28.0, 600.0);
                    text_centred(frame, self.page.name(lang), title, palette.text, left, y + 22.0, width, Align::Start);
                }
                // Each tab's name, the one shown marked under its middle by a short bar in the accent, as Windows' own
                // tabs are (a selector bar's).
                Row::Tabs => {
                    let font = |shown: bool| Font::new(Family::Segoe, 14.0, if shown { 600.0 } else { 400.0 });
                    // The first tab's name flush with the cards under it, no rule across.
                    let base = y + row_height - 8.0;
                    let mut x = left;
                    let mut middle = None;
                    for tab in TABS {
                        let name = tab.name(lang);
                        let shown = tab == self.tab;
                        let w = frame.gfx.measure(name, font(true));
                        let target = Target::Tab(tab);
                        let hover = hovered.as_ref() == Some(&target);
                        let r = Rect { x: x - 12.0, y, w: w + 24.0, h: row_height - 8.0 };
                        if hover && !shown {
                            fill(frame, palette.hover, r.x, r.y + 4.0, r.w, r.h - 8.0, 6.0);
                        }
                        text_centred(frame, name, font(shown), if shown { palette.text } else { palette.text2 }, x, y + (row_height - 8.0) / 2.0, w + 12.0, Align::Start);
                        if shown {
                            middle = Some(x + w / 2.0);
                        }
                        self.targets.push((r, target));
                        x += w + 28.0;
                    }
                    // Another tab chosen, the bar goes to it as Windows' does: its end toward it first, stretching it,
                    // its other end after, drawing it in again.
                    if let Some(middle) = middle {
                        let (to_left, to_right) = (middle - TAB_MARK / 2.0, middle + TAB_MARK / 2.0);
                        let rightward = self.motion.get("tab-mark:left").is_some_and(|edge| to_left > edge.target());
                        let (left_how, right_how) = if rightward { (TAB_MARK_TRAILS, TAB_MARK_LEADS) } else { (TAB_MARK_LEADS, TAB_MARK_TRAILS) };
                        let from = self.animate("tab-mark:left".into(), to_left, left_how, now, false);
                        let to = self.animate("tab-mark:right".into(), to_right, right_how, now, false);
                        fill(frame, palette.selection, from, base - 2.0, to - from, 3.0, 1.5);
                    }
                }
                Row::Element(name) => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let (title, word) = self.item_label("lyrics", name);
                    let on = self.item_on("lyrics", name);
                    let switch_left = left + width - ROW_SIDE - 40.0;
                    field_label(frame, palette, &title, word, label, hint, left + ROW_SIDE, y + row_height / 2.0, switch_left - 16.0 - left - ROW_SIDE);
                    let target = Target::Element(name);
                    let pressed = self.pressed == Some(target.clone());
                    self.toggle(frame, palette, format!("element:{name}"), on, true, switch_left, y + row_height / 2.0, now, pressed);
                    self.targets.push((Rect { x: left, y, w: width, h: row_height }, target));
                }
                Row::Drive(volume) => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let (name, word, listed) = self.drive_row(&volume);
                    let switch_left = left + width - ROW_SIDE - 40.0;
                    field_label(frame, palette, &name, word, label, hint, left + ROW_SIDE, y + row_height / 2.0, switch_left - 16.0 - left - ROW_SIDE);
                    let target = Target::Drive(volume.clone());
                    let pressed = self.pressed == Some(target.clone());
                    self.toggle(frame, palette, format!("drive:{volume}"), listed, true, switch_left, y + row_height / 2.0, now, pressed);
                    self.targets.push((Rect { x: left, y, w: width, h: row_height }, target));
                }
                Row::Note(words) => frame.text_wrapped(words, hint, palette.text2, left + 4.0, y + 4.0, width - 8.0),
                Row::Info(words) => frame.text_wrapped(&words, label, palette.text, left + 4.0, y + 4.0, width - 8.0),
                Row::Slider(field) => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let (name, min, max, _, value) = self.slider(field);
                    let cy = y + row_height / 2.0;
                    // The value on the right; the track before it; the name
                    // in the room left.
                    let right = left + width - ROW_SIDE;
                    let track = Rect { x: right - SLIDER_VALUE - 12.0 - SLIDER_WIDTH, y, w: SLIDER_WIDTH, h: row_height };
                    text_centred(frame, name, label, palette.text, left + ROW_SIDE, cy, track.x - 12.0 - (left + ROW_SIDE), Align::Start);
                    let shown = self.slider_shown(field, value);
                    text_centred(frame, &shown, label, palette.text2, right - SLIDER_VALUE, cy, SLIDER_VALUE, Align::End);
                    let (start, end) = (track.x + SLIDER_THUMB, track.x + track.w - SLIDER_THUMB);
                    // (A slider whose most is its least, the screen holding no more than that: at its start.)
                    let at = start + if max > min { (value - min) / (max - min) * (end - start) } else { 0.0 };
                    fill(frame, palette.switch_stroke.alpha(0.45), start, cy - 2.0, end - start, 4.0, 2.0);
                    fill(frame, palette.switch_on, start, cy - 2.0, at - start, 4.0, 2.0);
                    // As Windows' own (see `thumb`): a ring around a dot in the accent, the dot growing under the
                    // pointer and shrinking as it is held.
                    crate::ui::thumb::draw(frame, Point { x: at, y: cy }, SLIDER_THUMB, self.slider_dots.of(field), palette.switch_knob_ring, palette.card_stroke, palette.switch_on);
                    self.targets.push((track, Target::Slider(field)));
                }
                Row::Skins => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let gap = 12.0;
                    let cell = (width - 2.0 * ROW_SIDE - 2.0 * gap) / 3.0;
                    let names = [
                        (Skin::Paper, pick(lang, "记录纸", "Chart paper")),
                        (Skin::Glass, pick(lang, "磨砂玻璃", "Frosted glass")),
                        (Skin::Fluent, "Windows 11"),
                    ];
                    let current = theme::Skin::named(&self.settings.skin);
                    for (i, (skin, name)) in names.into_iter().enumerate() {
                        let x = left + ROW_SIDE + i as f32 * (cell + gap);
                        let box_ = Rect { x, y: y + 16.0, w: cell, h: 72.0 };
                        let chosen = skin == current;
                        let hover = hovered == Some(Target::Skin(skin));
                        skin_miniature(frame, skin, box_);
                        let ring = if chosen { (palette.selection, 2.0) } else { (palette.card_stroke, 1.0) };
                        if hover && !chosen {
                            let _ = skins::shadows(frame, &[(box_, 6.0)], &[Shadow { y: 2.0, blur: 8.0, spread: 0.0, color: Color::hex(0, 0.08) }]);
                        }
                        stroke_outside(frame, box_, 6.0, ring.0, ring.1);
                        let font = if chosen { Font::new(Family::Segoe, 13.0, 600.0) } else { Font::new(Family::Segoe, 13.0, 400.0) };
                        text_centred(frame, name, font, if chosen { palette.text } else { palette.text2 }, x, y + 16.0 + 72.0 + 10.0 + 9.0, cell, Align::Start);
                        self.targets.push((Rect { x, y: y + 16.0, w: cell, h: 72.0 + 10.0 + 18.0 }, Target::Skin(skin)));
                    }
                }
                Row::Library => self.paint_library(frame, palette, left, y, width, row_height, &hovered),
                Row::Choice(field) => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let (name, options, chosen) = self.choices(field);
                    let right = left + width - ROW_SIDE;
                    // The taskbar's edge is chosen from four cards of the screen, as Windows'
                    // own settings show it; its size and where its icons stand, from a box
                    // that opens a list.
                    if field == Field::TaskbarPosition {
                        text_centred(frame, name, label, palette.text, left + ROW_SIDE, y + ROW / 2.0, width - 2.0 * ROW_SIDE, Align::Start);
                        self.position_cards(frame, palette, field, &options, chosen, left + ROW_SIDE, y + ROW, width - 2.0 * ROW_SIDE, &hovered);
                    } else if Self::dropped(field) {
                        self.dropdown_row(frame, palette, field, name, &options, chosen, left, y, width, row_height, label, hint, now, &hovered);
                    } else {
                        match self.choice_hint(field) {
                        Some(word) if self.stacked(&options) => {
                            let lines = self.choice_lines(&options, self.choices_room()).len() as f32;
                            let words = row_height - CHOICE_LINE * lines;
                            field_label(frame, palette, name, Some(word), label, hint, left + ROW_SIDE, y + words / 2.0, width - 2.0 * ROW_SIDE);
                            self.segmented(frame, palette, field, &options, chosen, true, right, y + words + CHOICE_LINE / 2.0 - 8.0, self.choices_room(), now, &hovered);
                        }
                        Some(word) => {
                            let room = width - 2.0 * ROW_SIDE - self.segmented_width(&options) - 16.0;
                            field_label(frame, palette, name, Some(word), label, hint, left + ROW_SIDE, y + row_height / 2.0, room);
                            self.segmented(frame, palette, field, &options, chosen, true, right, y + row_height / 2.0, f32::INFINITY, now, &hovered);
                        }
                        None if self.beside_name(name, &options, self.choices_room()) => {
                            text_centred(frame, name, label, palette.text, left + ROW_SIDE, y + row_height / 2.0, width / 2.0, Align::Start);
                            self.segmented(frame, palette, field, &options, chosen, true, right, y + row_height / 2.0, f32::INFINITY, now, &hovered);
                        }
                        // (Not beside its name: on lines of their own under it.)
                        None => {
                            let lines = self.choice_lines(&options, self.choices_room()).len() as f32;
                            let words = row_height - CHOICE_LINE * lines;
                            text_centred(frame, name, label, palette.text, left + ROW_SIDE, y + words / 2.0, width - 2.0 * ROW_SIDE, Align::Start);
                            self.segmented(frame, palette, field, &options, chosen, true, right, y + words + CHOICE_LINE / 2.0 - 8.0, self.choices_room(), now, &hovered);
                        }
                        }
                    }
                }
                Row::Switch(switch) => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let (name, detail, on) = self.switch(switch);
                    let switch_left = left + width - ROW_SIDE - 40.0;
                    field_label(frame, palette, name, detail.as_deref(), label, hint, left + ROW_SIDE, y + row_height / 2.0, switch_left - 16.0 - left - ROW_SIDE);
                    let key = format!("switch:{switch:?}");
                    let pressed = self.pressed == Some(Target::Switch(switch));
                    self.toggle(frame, palette, key, on, true, left + width - ROW_SIDE - 40.0, y + row_height / 2.0, now, pressed);
                    self.targets.push((Rect { x: left, y, w: width, h: row_height }, Target::Switch(switch)));
                }
                Row::Module(id) => {
                    let offset = self.motion.get(&format!("row:{id}")).map_or(0.0, |t| t.value(now));
                    if self.drag.as_ref().is_some_and(|drag| drag.id == id) {
                        dragged = Some((id, row_height));
                        continue;
                    }
                    self.module_row(frame, palette, &id, left, y + offset, width, row_height, reach[&id], now, &hovered, false);
                }
                // The rows inside a module's card (drawn with its row) glide
                // with it, each under a hairline; while the module is off they
                // are dimmed and do nothing, and keep their state.
                Row::Item(id, name) => {
                    let y = y + self.motion.get(&format!("row:{id}")).map_or(0.0, |t| t.value(now));
                    fill(frame, palette.rule, left + 1.0, y, width - 2.0, 1.0, 0.0);
                    let active = self.module_on(&id);
                    let (name_text, detail) = self.item_label(&id, &name);
                    let target = Target::Item(id.clone(), name.clone());
                    let colors = if active { (palette.text, palette.text2) } else { (palette.text3, palette.text3) };
                    let switch_left = left + width - ROW_SIDE - CHEVRON_ROOM - 40.0;
                    field_label_in(frame, colors, &name_text, detail, label, hint, left + ITEM_INSET, y + row_height / 2.0, switch_left - 12.0 - left - ITEM_INSET);
                    let pressed = self.pressed == Some(target.clone());
                    let on = self.item_on(&id, &name);
                    self.toggle(frame, palette, format!("item:{id}:{name}"), on, active, switch_left, y + row_height / 2.0, now, pressed);
                    if active && usable {
                        self.targets.push((Rect { x: left, y, w: width, h: row_height }, target));
                    }
                }
                Row::ModuleChoice(id, field) => {
                    let y = y + self.motion.get(&format!("row:{id}")).map_or(0.0, |t| t.value(now));
                    fill(frame, palette.rule, left + 1.0, y, width - 2.0, 1.0, 0.0);
                    let active = self.module_on(&id);
                    let (name, options, chosen) = self.choices(field);
                    let color = if active { palette.text } else { palette.text3 };
                    let right = left + width - ROW_SIDE - CHEVRON_ROOM;
                    let targets = self.targets.len();
                    let room = self.module_choices_room();
                    if self.beside_name(name, &options, room) {
                        text_centred(frame, name, label, color, left + ITEM_INSET, y + row_height / 2.0, width / 2.0 - ITEM_INSET, Align::Start);
                        self.segmented(frame, palette, field, &options, chosen, active, right, y + row_height / 2.0, f32::INFINITY, now, &hovered);
                    } else {
                        // (Not beside its name: on lines of their own under it.)
                        text_centred(frame, name, label, color, left + ITEM_INSET, y + ITEM_ROW / 2.0, room, Align::Start);
                        self.segmented(frame, palette, field, &options, chosen, active, right, y + ITEM_ROW + CHOICE_LINE / 2.0 - 8.0, room, now, &hovered);
                    }
                    if !usable {
                        self.targets.truncate(targets);
                    }
                }
                Row::Group(name) => {
                    let small = Font::new(Family::Segoe, 13.0, 600.0);
                    text_centred(frame, name, small, palette.text2, left + 4.0, y + row_height / 2.0, width, Align::Start);
                }
                // A row that leads somewhere else in this window: its name and a
                // word under it, a chevron where a button would stand, and the
                // whole card pressed — so its name is not said twice over.
                Row::Nav(zh, en, hint_zh, hint_en, target) => {
                    let here = matches!(target, Target::Preview(page) if self.preview == Some(page));
                    self.leads_row(
                        frame,
                        palette,
                        pick(lang, zh, en),
                        Some(pick(lang, hint_zh, hint_en)),
                        target,
                        here,
                        left,
                        y,
                        width,
                        row_height,
                        label,
                        hint,
                    );
                }
                Row::Code => {
                    card(frame, palette, left, y, width, row_height, palette.card);
                    if let Some((code, what)) = &self.code {
                        // Dark on white whatever the theme (what a phone's camera reads), a margin of four cells round it.
                        let (x0, y0) = (left + ROW_SIDE, y + (row_height - CODE) / 2.0);
                        fill(frame, Color::hex(0xFFFFFF, 1.0), x0, y0, CODE, CODE, 8.0);
                        let cell = CODE / (code.0.len() as f32 + 8.0);
                        for (down, row) in code.0.iter().enumerate() {
                            // (Each run of dark cells as one piece, and a hair over the row below: no seams between them.)
                            let mut across = 0;
                            while across < row.len() {
                                let run = row[across..].iter().take_while(|dark| **dark).count();
                                if run > 0 {
                                    fill(frame, Color::hex(0x000000, 1.0), x0 + (across as f32 + 4.0) * cell, y0 + (down as f32 + 4.0) * cell, run as f32 * cell, cell + 0.3, 0.0);
                                }
                                across += run.max(1);
                            }
                        }
                        let (title, word) = code_words(lang, *what, self.web_address.as_deref());
                        let word = word.as_ref();
                        let text_left = x0 + CODE + 20.0;
                        field_label(frame, palette, title, Some(word), label, hint, text_left, y + row_height / 2.0, self.code_words_room());

                    }
                }
                Row::Notice => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    // Tinted in the accent a moment, opened at from its notification.
                    let flash = self.motion.get("flash:notice").map_or(0.0, |flash| flash.value(now));
                    card(frame, palette, left, y, width, row_height, view::over(palette.card, palette.selection.alpha(0.14 * flash)));
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    // Its icon in the accent, beside its title (the first line of its words: see `field_label`).
                    let words_left = left + ROW_SIDE + NOTICE_ICON_ROOM;
                    let room = (right - 8.0 - words_left).max(40.0);
                    let top = y + row_height / 2.0 - (22.0 + frame.gfx.wrapped_height(&detail, hint, room)) / 2.0;
                    frame.icon(Icon::Megaphone, Point { x: left + ROW_SIDE + 8.0, y: top + 10.0 }, 16.0, palette.selection);
                    field_label(frame, palette, &name, Some(&detail), Font::new(Family::Segoe, 14.0, 600.0), hint, words_left, y + row_height / 2.0, room);
                }
                Row::Update | Row::Version | Row::Shortcut(_) | Row::Diagnostics | Row::Guide | Row::MyReports | Row::Report(_) | Row::Uninstall | Row::Quit | Row::Pair | Row::WebPage | Row::Phone(..) | Row::NetEase | Row::RecentFolder(_) | Row::AddRecentFolder | Row::Do(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE);
                }
                // A network near: its wifi glyph, its name and state, and what can be done to it.
                Row::WifiNet(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let (connected, ssid) = match &row {
                        Row::WifiNet(ssid, _, _, _, _, connected) => (*connected, ssid.clone()),
                        _ => (false, String::new()),
                    };
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    // The wifi glyph, lit when this is the network joined.
                    let ink = if connected { palette.selection } else { palette.text.alpha(0.85) };
                    frame.icon(Icon::Wifi, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, ink);
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    // The row's body, left of its buttons, picks the network to show in the preview.
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::WifiSelect(ssid)));
                }
                // A device: its bluetooth glyph, its name and address, and where unpaired a button to ask Windows.
                Row::BtDevice(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let addr = match &row {
                        Row::BtDevice(_, addr, ..) => addr.clone(),
                        _ => String::new(),
                    };
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    frame.icon(Icon::Bluetooth, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, palette.text.alpha(0.85));
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::BtSelect(addr)));
                }
                // An adapter: its glyph lit by its state, its name and address; the body picks it for the preview.
                Row::NetAdapter(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let up = matches!(&row, Row::NetAdapter(_, true, _));
                    let current = matches!(&row, Row::NetAdapter(_, _, true));
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    let glyph = if current { palette.selection } else if up { palette.text.alpha(0.85) } else { palette.text3 };
                    frame.icon(Icon::Network, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, glyph);
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::NetSelect(name)));
                }
                // A display: its glyph lit when things start on it; the body picks it for the preview.
                Row::Screen(..) => {
                    let Some((title, detail, buttons)) = self.button_row(&row, now) else { continue };
                    // The row is picked by its GDI name, though it is titled by the monitor's own.
                    let device = match &row {
                        Row::Screen(d, _) => d.clone(),
                        _ => continue,
                    };
                    let primary = matches!(&row, Row::Screen(_, true));
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    let glyph = if primary { palette.selection } else { palette.text.alpha(0.85) };
                    frame.icon(Icon::AppWindow, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, glyph);
                    field_label(frame, palette, &title, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::ScreenSelect(device)));
                }
                // A sound device: its glyph lit when it is the default of its kind; the body picks it for the preview.
                Row::AudioDevice(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let id = match &row {
                        Row::AudioDevice(id, ..) => id.clone(),
                        _ => continue,
                    };
                    let default = matches!(&row, Row::AudioDevice(_, _, true));
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    let glyph = if default { palette.selection } else { palette.text.alpha(0.85) };
                    frame.icon(Icon::Volume, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, glyph);
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::AudioSelect(id)));
                }
                // An app: its glyph lit when it is let; the body picks it for the preview.
                Row::NotifyApp(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let aumid = match &row {
                        Row::NotifyApp(_, aumid, _) => aumid.clone(),
                        _ => continue,
                    };
                    let enabled = matches!(&row, Row::NotifyApp(_, _, true));
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    let glyph = if enabled { palette.selection } else { palette.text.alpha(0.85) };
                    frame.icon(Icon::MessageCircle, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, glyph);
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::NotifySelect(aumid)));
                }
                // A privacy capability: its glyph lit while its apps are the ones shown in the preview;
                // the body picks it for the preview.
                Row::ConsentCap(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let cap = match &row {
                        Row::ConsentCap(cap, ..) => cap.clone(),
                        _ => continue,
                    };
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    let glyph = if self.privacy_capability == cap { palette.selection } else { palette.text.alpha(0.85) };
                    frame.icon(Icon::Shield, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, glyph);
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::ConsentSelect(cap)));
                }
                // A physical disk: its glyph, its model and the room it holds; the body picks it for the preview.
                Row::Disk(..) => {
                    let Some((name, detail, buttons)) = self.button_row(&row, now) else { continue };
                    let number = match &row {
                        Row::Disk(n, ..) => *n,
                        _ => continue,
                    };
                    card(frame, palette, left, y, width, row_height, palette.card);
                    let mut right = left + width - ROW_SIDE;
                    for (text, target, kind) in buttons {
                        right = self.button(frame, palette, &text, right, y + row_height / 2.0, target, kind, &hovered) - 8.0;
                    }
                    frame.icon(Icon::Disk, Point { x: left + ROW_SIDE + 11.0, y: y + row_height / 2.0 }, 17.0, palette.text.alpha(0.85));
                    field_label(frame, palette, &name, Some(&detail), label, hint, left + ROW_SIDE + 30.0, y + row_height / 2.0, right - 8.0 - left - ROW_SIDE - 30.0);
                    self.targets.push((Rect { x: left, y, w: (right + 8.0 - left).max(0.0), h: row_height }, Target::StorageSelect(number)));
                }
            }
            if inside.is_some() {
                unsafe { frame.dc.PopAxisAlignedClip() };
            }
        }
        // The dragged row passes over the others.
        if let Some((id, row_height)) = dragged {
            let drag = self.drag.as_ref().unwrap();
            let y = drag.pointer - drag.grab - scroll;
            self.module_row(frame, palette, &id, left, y, width, row_height, row_height, now, &hovered, true);
        }
        // An open dropdown's list passes over the rows under it, its words each to be
        // pressed; what lay beneath gives its place to none of them.
        if let (Some(field), Some(at)) = (self.open_dropdown, self.dropdown_anchor) {
            let (_, options, chosen) = self.choices(field);
            let row_h = 34.0;
            let menu = Rect { x: at.x, y: at.y + at.h + 4.0, w: at.w, h: row_h * options.len() as f32 + 8.0 };
            self.targets.retain(|(_, t)| !matches!(t, Target::Choice(f, _) if *f == field));
            let _ = skins::shadows(frame, &[(menu, 6.0)], &[Shadow { y: 2.0, blur: 12.0, spread: 0.0, color: Color::hex(0x000000, 0.16) }]);
            fill(frame, palette.window, menu.x, menu.y, menu.w, menu.h, 6.0);
            stroke_inside(frame, menu, 6.0, palette.card_stroke);
            let font = Font::new(Family::Segoe, 13.0, 400.0);
            for (i, option) in options.iter().enumerate() {
                let r = Rect { x: menu.x + 4.0, y: menu.y + 4.0 + i as f32 * row_h, w: menu.w - 8.0, h: row_h };
                if chosen == Some(i) || hovered.as_ref() == Some(&Target::Choice(field, i)) {
                    fill(frame, palette.hover, r.x, r.y, r.w, r.h, 4.0);
                }
                text_centred(frame, option, font, palette.text, r.x + 12.0, r.y + row_h / 2.0, r.w - 24.0, Align::Start);
                self.targets.push((r, Target::Choice(field, i)));
            }
        }
        // The focus, shown once the keyboard is in use.
        if self.keyboard {
            if let Some(focus) = &self.focus {
                if let Some((r, target)) = self.targets.iter().find(|(_, target)| target == focus) {
                    // A skin's target holds its label flush with the card's
                    // edge, so its ring keeps some room; the rest hug theirs.
                    let room = if matches!(target, Target::Skin(_)) { 8.0 } else { -1.0 };
                    let ring = Rect { x: r.x - room, y: r.y - room, w: r.w + 2.0 * room, h: r.h + 2.0 * room };
                    let radius = 5.0 + room.max(0.0);
                    stroke_outside(frame, ring, radius, palette.text, 2.0);
                }
            }
        }
        // A hairline of a thumb beside the cards shows where in the list the view is, a little wider with the pointer
        // over the list.
        let content = self.content_height();
        if content > height {
            let (top, room) = (PAD_TOP, height - 2.0 * PAD_TOP);
            let thumb = (room * height / content).max(24.0);
            let at = top + (room - thumb) * self.scroll / (content - height);
            let over = self.pointer.is_some_and(|(x, _)| (NAV..NAV + self.pane()).contains(&x));
            let w = if over { 4.0 } else { 2.0 };
            fill(frame, palette.text3.alpha(if over { 0.7 } else { 0.45 }), NAV + PAD_SIDE + self.cards() + LIST_GAP / 3.0 - w / 2.0, at, w, thumb, w / 2.0);
        }
    }

    /// A module's row, and when its card is open (`reach` below the row's
    /// top), the card around the rows inside it: one rounded box.
    /// The fold of the modules few want: its name, what is in it, and a
    /// chevron that turns over as it opens.
    #[allow(clippy::too_many_arguments)]
    fn more_row(&mut self, frame: &Frame, palette: &Palette, count: usize, left: f32, y: f32, width: f32, height: f32, now: Instant) {
        let label = Font::new(Family::Segoe, 14.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        card(frame, palette, left, y, width, height, palette.card);
        let cy = y + height / 2.0;
        let turn = self.animate("chevron:more".into(), if self.more_open { 180.0 } else { 0.0 }, SLIDE, now, false);
        let cx = left + width - ROW_SIDE - 6.0;
        frame.place(Matrix3x2::rotation_around(turn, Vector2 { X: cx, Y: cy }));
        frame.icon(Icon::ChevronDown, Point { x: cx, y: cy }, CHEVRON, palette.text2);
        frame.origin(0.0, 0.0);
        let names: Vec<String> = self.prefs.modules.iter().filter(|entry| self.folded(&entry.id) && prefs::offered(&entry.id)).map(|entry| self.module(&entry.id).0).collect();
        let title = if self.lang == Lang::Zh { format!("开发工具（{count}）") } else { format!("Developer tools ({count})") };
        let text_left = left + ITEM_INSET;
        field_label_line(frame, palette, &title, Some(&names.join(pick(self.lang, "、", ", "))), label, hint, text_left, cy, cx - 24.0 - text_left);
        self.targets.push((Rect { x: left, y, w: width, h: height }, Target::More));
    }

    /// A row that leads somewhere else in this window: its name and a word under
    /// it, a chevron where a button would stand, and the whole card pressed — so
    /// its name is not said twice over. The 通用 hub's rows use it; the card lit
    /// with a ring is the sub-item whose preview the right pane shows.
    #[allow(clippy::too_many_arguments)]
    fn leads_row(
        &mut self,
        frame: &Frame,
        palette: &Palette,
        name: &str,
        under: Option<&str>,
        target: Target,
        here: bool,
        left: f32,
        y: f32,
        width: f32,
        row_height: f32,
        label: Font,
        hint: Font,
    ) {
        let lit = self.hovered_now.as_ref() == Some(&target);
        card(frame, palette, left, y, width, row_height, palette.card);
        if lit && !here {
            fill(frame, palette.hover, left, y, width, row_height, CARD_RADIUS);
        }
        if here {
            stroke_inside(frame, Rect { x: left, y, w: width, h: row_height }, CARD_RADIUS, palette.selection);
        }
        let cy = y + row_height / 2.0;
        let cx = left + width - ROW_SIDE - CHEVRON / 2.0;
        frame.place(Matrix3x2::rotation_around(-90.0, Vector2 { X: cx, Y: cy }));
        frame.icon(Icon::ChevronDown, Point { x: cx, y: cy }, CHEVRON, if here { palette.selection } else { palette.text3 });
        frame.origin(0.0, 0.0);
        field_label_in(frame, (if here { palette.selection } else { palette.text }, palette.text2), name, under, label, hint, left + ROW_SIDE, cy, width - 2.0 * ROW_SIDE - CHEVRON_ROOM);
        self.targets.push((Rect { x: left, y, w: width, h: row_height }, target));
    }

    #[allow(clippy::too_many_arguments)]
    fn module_row(&mut self, frame: &Frame, palette: &Palette, id: &str, left: f32, y: f32, width: f32, height: f32, reach: f32, now: Instant, hovered: &Option<Target>, held: bool) {
        let label = Font::new(Family::Segoe, 14.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        if held {
            let shape = Rect { x: left, y, w: width, h: height };
            let _ = skins::shadows(frame, &[(shape, CARD_RADIUS)], &[Shadow { y: 4.0, blur: 16.0, spread: 0.0, color: Color::hex(0, 0.12) }]);
            // Opaque while it passes over the other rows.
            fill(frame, palette.window, left, y, width, height, CARD_RADIUS);
            card(frame, palette, left, y, width, height, palette.card);
            stroke_inside(frame, shape, CARD_RADIUS, palette.selection);
        } else {
            // The whole card, its rows inside it in the same shade, as
            // Windows' own settings draw an open one.
            card(frame, palette, left, y, width, reach, palette.card);
        }
        let on = self.module_on(id);
        let (title, detail) = self.module(id);
        // The grip: six dots, to drag the row by (none in the fold, where a
        // module has no place among the others).
        let grip = Rect { x: left + 12.0, y: y + height / 2.0 - 16.0, w: 20.0, h: 32.0 };
        let draggable = !self.folded(id);
        if draggable && (*hovered == Some(Target::Grip(id.to_string())) || held) {
            fill(frame, palette.hover, grip.x, grip.y, grip.w, grip.h, 4.0);
        }
        for (dx, dy) in [(-2.0, -5.0), (2.0, -5.0), (-2.0, 0.0), (2.0, 0.0), (-2.0, 5.0), (2.0, 5.0)].into_iter().filter(|_| draggable) {
            Canvas::fill_circle(frame, palette.text3, Point { x: grip.x + 10.0 + dx, y: grip.y + 16.0 + dy }, 1.2);
        }
        let text_left = left + ITEM_INSET;
        // The switch, and right of it, if the module has a card to open, a
        // chevron that turns over as it opens.
        let opens = self.opens(id);
        let open = self.expanded.contains(id);
        let cy = y + height / 2.0;
        let switch_left = left + width - ROW_SIDE - CHEVRON_ROOM - 40.0;
        if opens {
            let turn = self.animate(format!("chevron:{id}"), if open { 180.0 } else { 0.0 }, SLIDE, now, false);
            let cx = left + width - ROW_SIDE - 6.0;
            frame.place(Matrix3x2::rotation_around(turn, Vector2 { X: cx, Y: cy }));
            frame.icon(Icon::ChevronDown, Point { x: cx, y: cy }, CHEVRON, palette.text2);
            frame.origin(0.0, 0.0);
        }
        field_label(frame, palette, &title, detail.as_deref(), label, hint, text_left, cy, switch_left - 12.0 - text_left);
        let pressed = self.pressed == Some(Target::Module(id.to_string()));
        self.toggle(frame, palette, format!("module:{id}"), on, true, switch_left, cy, now, pressed);
        if !held {
            // The grip drags, the switch switches, and the rest of the row
            // opens and closes the card (or, with none, switches too).
            if draggable {
                self.targets.push((grip, Target::Grip(id.to_string())));
            }
            let switch_area = Rect { x: switch_left - 12.0, y, w: 40.0 + 24.0, h: height };
            self.targets.push((switch_area, Target::Module(id.to_string())));
            let row = Rect { x: left, y, w: width, h: height };
            self.targets.push((row, if opens { Target::Expand(id.to_string()) } else { Target::Module(id.to_string()) }));
        }
    }

    /// A button right-aligned at `right` and centred on `cy`, pressed as
    /// `target`: outlined in the card's hairline, or, the one thing a row
    /// asks for, filled in the accent; returns where its left edge is.
    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, frame: &Frame, palette: &Palette, text: &str, right: f32, cy: f32, target: Target, kind: Button, hovered: &Option<Target>) -> f32 {
        let font = Font::new(Family::Segoe, 14.0, 400.0);
        let w = frame.gfx.measure(text, font) + 32.0;
        let rect = Rect { x: right - w, y: cy - 16.0, w, h: 32.0 };
        let hover = kind != Button::Quiet && hovered.as_ref() == Some(&target);
        let ink = match kind {
            Button::Accent => {
                fill(frame, if hover { palette.switch_on.alpha(0.9) } else { palette.switch_on }, rect.x, rect.y, rect.w, rect.h, 6.0);
                Color::hex(0xFFFFFF, 1.0)
            }
            _ => {
                if hover || kind == Button::Taking {
                    fill(frame, palette.hover, rect.x, rect.y, rect.w, rect.h, 6.0);
                }
                stroke_inside(frame, rect, 6.0, if kind == Button::Taking { palette.selection } else { palette.rule });
                if kind == Button::Quiet { palette.text2 } else { palette.text }
            }
        };
        text_centred(frame, text, font, ink, rect.x + 16.0, cy, w, Align::Start);
        if kind != Button::Quiet {
            self.targets.push((rect, target));
        }
        rect.x
    }

    /// A choice shown as Windows 11 shows it: its name, and on the right a box reading
    /// the chosen word, with a chevron that opens a list of them all (see `open_dropdown`).
    #[allow(clippy::too_many_arguments)]
    fn dropdown_row(&mut self, frame: &Frame, palette: &Palette, field: Field, name: &'static str, options: &[String], chosen: Option<usize>, left: f32, y: f32, width: f32, row_height: f32, label: Font, hint: Font, now: Instant, hovered: &Option<Target>) {
        let _ = (hint, now);
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let cy = y + row_height / 2.0;
        let widest = options.iter().map(|o| frame.gfx.measure(o, font)).fold(0.0f32, f32::max);
        let bw = widest + 44.0;
        let box_x = left + width - ROW_SIDE - bw;
        text_centred(frame, name, label, palette.text, left + ROW_SIDE, cy, (box_x - 16.0 - left - ROW_SIDE).max(80.0), Align::Start);
        let open = self.open_dropdown == Some(field);
        let rect = Rect { x: box_x, y: cy - 16.0, w: bw, h: 32.0 };
        let lit = open || *hovered == Some(Target::Choice(field, chosen.unwrap_or(0)));
        fill(frame, if open { palette.hover } else { palette.control }, rect.x, rect.y, rect.w, rect.h, 6.0);
        stroke_inside(frame, rect, 6.0, if lit { palette.selection } else { palette.card_stroke });
        if let Some(i) = chosen.filter(|i| *i < options.len()) {
            text_centred(frame, &options[i], font, palette.text, rect.x + 12.0, cy, rect.w - 36.0, Align::Start);
        }
        frame.icon(Icon::ChevronDown, Point { x: rect.x + rect.w - 16.0, y: cy }, CHEVRON, palette.text2);
        // (The box presses as a choice of the word it already reads: `activate` opens the list on it.)
        self.targets.push((rect, Target::Choice(field, chosen.unwrap_or(0))));
        if open {
            self.dropdown_anchor = Some(rect);
        }
    }

    /// The taskbar's edge as Windows 11 shows it: four little screens each wearing the bar
    /// along its edge, the chosen one ringed in the accent, all four to be pressed.
    #[allow(clippy::too_many_arguments)]
    fn position_cards(&mut self, frame: &Frame, palette: &Palette, field: Field, options: &[String], chosen: Option<usize>, x: f32, top: f32, room: f32, hovered: &Option<Target>) {
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let gap = 10.0;
        let card_w = ((room - 3.0 * gap) / 4.0).max(40.0);
        let screen_h = card_w * 0.6;
        let thick = (screen_h * 0.16).max(4.0);
        for (i, option) in options.iter().enumerate() {
            let cx = x + i as f32 * (card_w + gap);
            let target = Target::Choice(field, i);
            let lit = chosen == Some(i);
            let hover = hovered.as_ref() == Some(&target);
            // A little screen: the desktop washed in the accent, the bar along the edge it stands on.
            fill(frame, palette.window, cx, top, card_w, screen_h, 4.0);
            fill(frame, palette.selection.alpha(0.14), cx, top, card_w, screen_h * 0.55, 4.0);
            let bar = match i {
                1 => Rect { x: cx, y: top, w: card_w, h: thick },
                2 => Rect { x: cx, y: top, w: thick, h: screen_h },
                3 => Rect { x: cx + card_w - thick, y: top, w: thick, h: screen_h },
                _ => Rect { x: cx, y: top + screen_h - thick, w: card_w, h: thick },
            };
            fill(frame, if lit { palette.selection } else { palette.control_on }, bar.x, bar.y, bar.w, bar.h, 2.0);
            if hover && !lit {
                fill(frame, palette.hover, cx, top, card_w, screen_h, 4.0);
            }
            stroke_inside(frame, Rect { x: cx, y: top, w: card_w, h: screen_h }, 4.0, palette.card_stroke);
            if lit {
                stroke_outside(frame, Rect { x: cx, y: top, w: card_w, h: screen_h }, 4.0, palette.selection, 2.0);
            }
            text_middle(frame, option, font, if lit { palette.text } else { palette.text2 }, cx + card_w / 2.0, top + screen_h + 13.0);
            self.targets.push((Rect { x: cx, y: top, w: card_w, h: screen_h + 23.0 }, target));
        }
    }

    /// A row of options with one thumb that slides under the chosen one,
    /// right-aligned at `right` and centred on `cy`; unless `enabled`, dimmed
    /// and not to be pressed.
    #[allow(clippy::too_many_arguments)]
    /// Its first line centred on `cy`, each right-aligned at `right`, as many to a line as fit in `room` (see
    /// `choice_lines`), a line under another `CHOICE_LINE` apart.
    #[allow(clippy::too_many_arguments)]
    fn segmented(&mut self, frame: &Frame, palette: &Palette, field: Field, options: &[String], chosen: Option<usize>, enabled: bool, right: f32, cy: f32, room: f32, now: Instant, hovered: &Option<Target>) {
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let widths = self.choice_widths(options);
        // Each choice where it is: its left, its width, and the middle of its line.
        let mut spans = Vec::new();
        for (line, range) in lines_of(&widths, room).into_iter().enumerate() {
            let total = widths[range.clone()].iter().sum::<f32>() + 4.0;
            let (x0, line_cy) = (right - total, cy + line as f32 * CHOICE_LINE);
            fill(frame, palette.control, x0, line_cy - 16.0, total, 32.0, 6.0);
            let mut x = x0 + 2.0;
            for w in &widths[range] {
                spans.push((x, *w, line_cy));
                x += w;
            }
        }
        if let Some(index) = chosen {
            let (to_x, to_w, to_cy) = spans[index];
            let jump = self.settle_thumbs;
            let tx = self.animate(format!("thumb-x:{field:?}"), to_x - right, SLIDE, now, jump);
            let tw = self.animate(format!("thumb-w:{field:?}"), to_w, SLIDE, now, jump);
            let ty = self.animate(format!("thumb-y:{field:?}"), to_cy - cy, SLIDE, now, jump);
            let thumb = Rect { x: right + tx, y: cy + ty - 14.0, w: tw, h: 28.0 };
            if let Some(drop) = palette.control_drop {
                fill(frame, drop, thumb.x, thumb.y + 1.0, thumb.w, thumb.h, 4.0);
            }
            stroke_outside(frame, thumb, 4.0, palette.control_ring, 1.0);
            fill(frame, palette.control_on, thumb.x, thumb.y, thumb.w, thumb.h, 4.0);
        }
        for (i, ((sx, sw, line_cy), option)) in spans.iter().zip(options).enumerate() {
            let target = Target::Choice(field, i);
            let lit = chosen == Some(i) || *hovered == Some(target.clone());
            let color = match (enabled, lit) {
                (false, _) => palette.text3,
                (true, true) => palette.text,
                (true, false) => palette.text2,
            };
            let text_w = frame.gfx.measure(option, font);
            text_centred(frame, option, font, color, sx + (sw - text_w) / 2.0, *line_cy, *sw, Align::Start);
            if enabled {
                self.targets.push((Rect { x: *sx, y: line_cy - 16.0, w: *sw, h: 32.0 }, target));
            }
        }
    }

    /// A switch, 40 × 22, with its left edge at `x` and centred on `cy`;
    /// unless `enabled`, in the greys of one that does nothing for now.
    #[allow(clippy::too_many_arguments)]
    fn toggle(&mut self, frame: &Frame, palette: &Palette, key: String, on: bool, enabled: bool, x: f32, cy: f32, now: Instant, pressed: bool) {
        let dim = 1.0 - self.animate(format!("{key}:enabled"), if enabled { 1.0 } else { 0.0 }, FLIP, now, false).clamp(0.0, 1.0);
        let t = self.animate(key, if on { 1.0 } else { 0.0 }, FLIP, now, false).clamp(0.0, 1.0);
        let track = Rect { x, y: cy - 11.0, w: 40.0, h: 22.0 };
        // Off: an outlined track; on: a filled one. Between, the two
        // cross-fade. Disabled, the off track is outline alone, and both
        // take the disabled grey.
        if t < 1.0 {
            fill(frame, palette.switch_off.alpha((1.0 - t) * (1.0 - dim)), track.x, track.y, track.w, track.h, 11.0);
            stroke_inside(frame, track, 11.0, mix(palette.switch_stroke, palette.disabled, dim).alpha(1.0 - t));
        }
        if t > 0.0 {
            fill(frame, mix(palette.switch_on, palette.disabled, dim).alpha(t), track.x, track.y, track.w, track.h, 11.0);
        }
        // The knob grows as it crosses, and stretches while pressed.
        let size = 12.0 + 2.0 * t;
        let stretch = if pressed && enabled { 16.0 - size } else { 0.0 };
        let left = x + 4.0 + 17.0 * t - if on { stretch } else { 0.0 };
        let knob_off = mix(palette.switch_knob, palette.disabled, dim);
        let knob_on = mix(Color::hex(0xFFFFFF, 1.0), palette.disabled_knob, dim);
        fill(frame, mix(knob_off, knob_on, t), left, cy - size / 2.0, size + stretch, size, size / 2.0);
    }

    /// The overlay's preview: the whole screen, and the overlay where it was
    /// put, showing the readings chosen (an example game's frames, with none
    /// running).
    /// The phone app as a phone would show it for this PC now, in the
    /// middle of `area`: as tall as the room, in a phone's dark outline.
    /// Whether anything in it moves.
    fn paint_phone(&mut self, frame: &Frame, area: Rect) -> bool {
        let app = crate::app();
        let reads = app.controller.history.read().unwrap().back().map_or(serde_json::Value::Null, |sample| crate::remote::figures(sample, &app.info()));
        // The outline round it; the whole no taller or wider than the room, nor larger than a phone is.
        let (bezel, margin) = (10.0, 24.0);
        let (wide, tall) = super::phone_art::SCREEN;
        let k = ((area.h - 2.0 * margin - 2.0 * bezel) / tall).min((area.w - 2.0 * margin - 2.0 * bezel) / wide).min(1.0);
        if k <= 0.0 {
            return false;
        }
        let (w, h) = (wide * k, tall * k);
        let (x, y) = (area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0);
        fill(frame, Color::hex(0x15181D, 1.0), x - bezel, y - bezel, w + 2.0 * bezel, h + 2.0 * bezel, 26.0 * k + bezel);
        let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
        self.hear_phone();
        let held = self.phone.slid.as_ref().map(|(part, _, at)| (part, *at));
        let state = self.phone.state.as_ref().filter(|_| self.remote);
        // (The ring is shut with the lights' card: where they are off, or the remote does not answer.)
        if !state.is_some_and(|state| state["lights"].as_bool() == Some(true) && state["night"].as_bool() != Some(true)) {
            self.phone.making = None;
            self.phone.removing = false;
        }
        // A colour held a while: the colours kept can be taken away (and it is not chosen as it is let go).
        if matches!(self.phone.pressed, Some(super::phone_art::Part::Colour(_))) && self.phone.held_since.elapsed() >= super::buttons::LINGER {
            self.phone.pressed = None;
            self.phone.making = None;
            self.phone.removing = true;
        }
        let painted = super::phone_art::paint(frame, Rect { x, y, w, h }, self.lang, &name, &reads, state, !self.remote, self.phone_from.elapsed().as_secs_f32(), self.phone.scroll, held, self.phone.making.as_ref(), self.phone.removing);
        self.phone.at = Some((Rect { x, y, w, h }, k));
        self.phone.most = (painted.tall - tall).max(0.0);
        self.phone.scroll = self.phone.scroll.min(self.phone.most);
        self.phone.parts = painted.parts;
        // (Drawn again soon whether or not its fans turn: how the PC is, is asked meanwhile.)
        true
    }

    /// Asks the remote, on a thread of its own (it answers when it has done it: the window goes on meanwhile).
    fn ask_phone(&mut self, method: &str, path: &str, body: String) {
        let link = self.phone.link.get_or_insert_with(|| {
            let (ask, asked) = std::sync::mpsc::channel::<(String, String, String)>();
            let (answer, answered) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for (method, path, body) in asked {
                    if answer.send(crate::phone::said(&method, &path, &body)).is_err() {
                        break;
                    }
                }
            });
            (ask, answered)
        });
        if link.0.send((method.to_string(), path.to_string(), body)).is_ok() {
            self.phone.waiting += 1;
            self.phone.asked = Instant::now();
        }
    }

    /// Takes what the remote has answered, and asks it again how the PC is once that is due.
    fn hear_phone(&mut self) {
        if !self.remote {
            self.phone.state = None;
            return;
        }
        while let Some(answer) = self.phone.link.as_ref().and_then(|(_, answered)| answered.try_recv().ok()) {
            self.phone.waiting = self.phone.waiting.saturating_sub(1);
            // (What a look answers while something else was asked since is not how it is now: the last answer stands.)
            if self.phone.waiting == 0 && self.phone.slid.is_none() {
                if let Some(state) = answer {
                    self.phone.state = Some(state);
                }
            }
        }
        if self.phone.waiting == 0 && (self.phone.state.is_none() || self.phone.asked.elapsed() >= PHONE_ASKED_EVERY) {
            self.ask_phone("GET", "/v1/state", String::new());
        }
    }

    /// What of the phone's page is at a point of the window.
    fn phone_part(&self, x: f32, y: f32) -> Option<(Rect, super::phone_art::Part)> {
        self.phone.parts.iter().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).cloned()
    }

    /// A press on the phone's page: a slider follows the pointer from there; anything else is done when it is let go on it.
    fn press_phone(&mut self, x: f32, y: f32) -> bool {
        let Some((track, part)) = self.phone_part(x, y) else { return false };
        if part.slides() {
            let at = part.along(track, x, y);
            self.phone.slid = Some((part, track, at));
            unsafe { SetCapture(self.hwnd) };
        } else {
            self.phone.pressed = Some(part);
            self.phone.held_since = Instant::now();
        }
        self.next_frame = Instant::now();
        true
    }

    /// The phone's page let go of at a point: what was slid is asked for as it was left, what was pressed is done.
    fn release_phone(&mut self, x: f32, y: f32) -> bool {
        use super::phone_art::Part;
        use serde_json::json;
        let state = self.phone.state.clone().unwrap_or(serde_json::Value::Null);
        let flag = |name: &str| state.get(name).and_then(serde_json::Value::as_bool);
        if let Some((part, _, at)) = self.phone.slid.take() {
            super::window::let_go_of_capture(self.hwnd);
            // A colour being made: the lights take it as it is left.
            if let (Part::Hue | Part::Depth, Some(making)) = (&part, self.phone.making.as_mut()) {
                match part {
                    Part::Hue => making.hue = at * 360.0,
                    _ => making.depth = at,
                }
                let made = making.hex();
                if let Some(state) = self.phone.state.as_mut() {
                    state["light"]["color"] = json!(made);
                }
                self.ask_phone("POST", "/v1/lights", json!({ "on": true, "color": made }).to_string());
                self.next_frame = Instant::now();
                return true;
            }
            let (path, body) = match part {
                Part::Brightness => ("/v1/lights", json!({ "on": true, "brightness": (5.0 + at * 95.0).round() as u32 })),
                Part::MonitorBrightness(index) => ("/v1/monitor", json!({ "index": index, "brightness": (at * 100.0).round() as u32 })),
                _ => ("/v1/volume", json!({ "level": (at * 100.0).round() as u32 })),
            };
            // (Shown as asked for till the remote says how it is.)
            if let Some(state) = self.phone.state.as_mut() {
                match &part {
                    Part::Brightness => state["light"]["brightness"] = json!((5.0 + at * 95.0).round()),
                    Part::MonitorBrightness(index) => state["monitors"][*index]["brightness"] = json!((at * 100.0).round()),
                    _ => state["volume"] = json!((at * 100.0).round()),
                }
            }
            self.ask_phone("POST", path, body.to_string());
            self.next_frame = Instant::now();
            return true;
        }
        let Some(part) = self.phone.pressed.take() else { return false };
        if self.phone_part(x, y).map(|(_, part)| part) != Some(part.clone()) {
            return true;
        }
        let (path, body) = match &part {
            Part::Night => ("/v1/night", json!({ "on": flag("night") != Some(true) })),
            Part::Screen => ("/v1/screen", json!({ "on": flag("screen") == Some(false) })),
            Part::Lights => ("/v1/lights", json!({ "on": flag("lights") == Some(false) })),
            Part::Power(id) => ("/v1/power", json!({ "plan": id })),
            Part::Colour(colour) => {
                self.phone.making = None;
                ("/v1/lights", json!({ "on": true, "color": colour }))
            }
            // The ring opened from the colour in use, or shut as it is.
            Part::Make => {
                self.phone.making = match self.phone.making {
                    Some(_) => None,
                    None => Some(super::phone_art::Making::from(state["light"]["color"].as_str().unwrap_or("FFFFFF"))),
                };
                self.next_frame = Instant::now();
                return true;
            }
            // Kept with the others, for this PC's settings and every phone, where it is not one of them already.
            Part::Use => {
                let Some(making) = self.phone.making.take() else { return true };
                self.next_frame = Instant::now();
                let (made, mut kept) = (making.hex(), super::phone_art::kept_colours(&state));
                if kept.contains(&made) {
                    return true;
                }
                kept.push(made);
                ("/v1/colours", json!({ "colours": kept }))
            }
            Part::Remove(colour) => {
                let kept: Vec<String> = super::phone_art::kept_colours(&state).into_iter().filter(|one| one != colour).collect();
                ("/v1/colours", json!({ "colours": kept }))
            }
            Part::Done => {
                self.phone.removing = false;
                self.next_frame = Instant::now();
                return true;
            }
            // Given up: the lights go back to the colour they had.
            Part::GiveUp => {
                let Some(making) = self.phone.making.take() else { return true };
                self.next_frame = Instant::now();
                if state["light"]["color"].as_str().is_some_and(|now| now.eq_ignore_ascii_case(&making.was)) {
                    return true;
                }
                ("/v1/lights", json!({ "on": true, "color": making.was }))
            }
            Part::Effect(effect) => ("/v1/lights", json!({ "on": true, "effect": effect })),
            Part::Monitor(index) => ("/v1/monitor", json!({ "index": index, "on": state["monitors"][*index]["on"].as_bool() == Some(false) })),
            Part::Keys(level) => ("/v1/keys", json!({ "level": level })),
            Part::Mute => ("/v1/volume", json!({ "muted": flag("muted") != Some(true) })),
            Part::Media(what) => ("/v1/media", json!({ "do": what })),
            // Sleep is asked about first: everything on the PC stops.
            Part::Sleep => {
                let lang = self.lang;
                let asked = super::ask::Asked {
                    title: pick(lang, "让电脑睡眠？", "Put the PC to sleep?"),
                    intro: pick(
                        lang,
                        "电脑上的一切都会停下：下载、远程连接、正在运行的程序。只想关屏关灯、让电脑继续运行，请用夜间模式。",
                        "Everything on the PC stops: downloads, remote connections, programs at work. To turn off only the screens and the lights and leave the PC running, use night mode.",
                    ),
                    own: None,
                    option: None,
                    sent: None,
                    from_end: false,
                    yes: pick(lang, "睡眠", "Sleep"),
                    no: pick(lang, "取消", "Cancel"),
                    dark: self.dark,
                };
                let agreed = super::ask::show(self.hwnd, &asked).is_some();
                if !agreed {
                    return true;
                }
                ("/v1/sleep", json!({}))
            }
            Part::Brightness | Part::MonitorBrightness(_) | Part::Volume | Part::Hue | Part::Depth => return true,
        };
        // Shown as asked for at once, till the remote says how it is: it takes a moment to do it and answer.
        if let Some(state) = self.phone.state.as_mut() {
            match &part {
                Part::Night => state["night"] = body["on"].clone(),
                Part::Screen => state["screen"] = body["on"].clone(),
                Part::Lights => state["lights"] = body["on"].clone(),
                Part::Power(id) => {
                    for mode in state["power"].as_array_mut().into_iter().flatten() {
                        mode["on"] = json!(mode["id"].as_str() == Some(id));
                    }
                }
                Part::Colour(_) | Part::GiveUp => state["light"]["color"] = body["color"].clone(),
                Part::Use | Part::Remove(_) => state["colours"] = body["colours"].clone(),
                Part::Effect(effect) => state["light"]["effect"] = json!(effect),
                Part::Monitor(index) => state["monitors"][*index]["on"] = body["on"].clone(),
                Part::Keys(level) => state["keys"]["level"] = json!(level),
                Part::Mute => state["muted"] = body["muted"].clone(),
                Part::Media("toggle") => state["media"]["playing"] = json!(state["media"]["playing"].as_bool() == Some(false)),
                _ => {}
            }
        }
        self.ask_phone("POST", path, body.to_string());
        self.next_frame = Instant::now();
        true
    }

    fn paint_overlay_preview(&mut self, frame: &Frame, area: Rect) {
        let overlay = &self.settings.overlay;
        let readings = {
            let history = crate::app().controller.history.read().unwrap();
            let Some(sample) = history.back() else {
                fill(frame, self.palette.card, area.x, area.y, area.w, area.h, 8.0);
                stroke_inside(frame, area, 8.0, self.palette.card_stroke);
                return;
            };
            super::overlay::preview(sample, &overlay.chosen(), self.lang)
        };
        let frames = super::overlay::preview_frames();
        let (sw, sh) = self.stage.size;
        let size = overlay.size.clamp(SIZES.0, SIZES.1);
        let width = |text: &str, font| frame.gfx.measure(text, font);
        let line = |font| frame.gfx.baseline(font);
        let metrics = super::overlay::Metrics { width: &width, line: &line };
        let shape = overlay.shape(sw / size);
        let glass = |k: f32| {
            let px = self.scale * k * size;
            (px, super::overlay::size(&readings, shape, px, &metrics), super::overlay::radius(&readings, shape, px, &metrics))
        };
        // At its own size, as on screen: the part of the screen around it
        // that the preview holds (all of a screen smaller than that); smaller
        // only where the overlay, shadow and all, is wider or taller than
        // the preview.
        let k = (area.w / sw).min(area.h / sh).max(1.0);
        let (_, (gw, gh), _) = glass(k);
        let room = 2.0 * super::overlay::MARGIN;
        let k = k.min(area.w / (gw * size + room)).min(area.h / (gh * size + room));
        let (px, (gw, gh), _) = glass(k);
        let (w, h) = (gw * size, gh * size);
        // (Up to the screen's edges, as on the screen: see `overlay::room`.)
        let x = overlay.at.0.clamp(0.0, 1.0) * (sw - w).max(0.0);
        let y = overlay.at.1.clamp(0.0, 1.0) * (sh - h).max(0.0);
        let view = |at: f32, length: f32, room: f32, extent: f32| (at + length / 2.0 - room / 2.0 / k).clamp(0.0, (extent - room / k).max(0.0));
        let (vx, vy) = (view(x, w, area.w, sw), view(y, h, area.h, sh));
        let (ox, oy) = (area.x - vx * k + ((area.w - sw * k) / 2.0).max(0.0), area.y - vy * k + ((area.h - sh * k) / 2.0).max(0.0));
        // In the preview's frame, as the panel's preview is (see `framed`): what of the screen shows in the middle of it.
        let framed = area;
        let area = self.shown_of(area, k, ox - area.x, oy - area.y);
        self.framed(frame, framed);
        // (Inside the preview's rounded corners.)
        let Ok(mask) = (unsafe { frame.gfx.factory.CreateRoundedRectangleGeometry(&rounded(area, 8.0)) }) else { return };
        let layer = D2D1_LAYER_PARAMETERS1 {
            contentBounds: rect(area.x, area.y, area.w, area.h),
            geometricMask: std::mem::ManuallyDrop::new(windows::core::Interface::cast(&mask).ok()),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            ..Default::default()
        };
        unsafe { frame.dc.PushLayer(&layer, None) };
        if self.stage.bitmap.as_ref().is_none_or(|(key, _)| *key != 1.0f32.to_bits()) {
            self.stage.bitmap = self.stage.desktop.as_ref().and_then(|desktop| desktop.bitmap(&frame.dc, 1.0).ok()).map(|b| (1.0f32.to_bits(), b));
        }
        if let Some((_, bitmap)) = &self.stage.bitmap {
            unsafe {
                frame.dc.DrawBitmap(bitmap, Some(&rect(ox, oy, sw * k, sh * k)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        }
        // The glass, tinted for the screen around it, over the screen behind
        // it blurred as the compositor blurs it.
        let screen = Matrix3x2::scale(k, k) * Matrix3x2::translation(ox, oy);
        let placed = Matrix3x2::scale(size, size) * Matrix3x2::translation(x, y) * screen;
        let around = super::overlay::MARGIN + 16.0;
        let whole = |v: f32| v.round() as i32;
        let hole = RECT { left: whole(x), top: whole(y), right: whole(x + w), bottom: whole(y + h) };
        let ring = RECT { left: whole(x - around), top: whole(y - around), right: whole(x + w + around), bottom: whole(y + h + around) };
        let frost = 1.0 - overlay.clear.clamp(0.0, 1.0);
        let glass = super::overlay::Glass { presence: frost, ..self.stage.glass(ring, hole) };
        // (Its tint alone over the screen as it is: the overlay does not blur what is behind it.)
        frame.place(placed);
        frame.crisp_text();
        super::overlay::shadow(frame, &readings, shape, glass, false, px);
        super::overlay::paint(frame, &readings, &frames, shape, glass, super::overlay::Hand::Free, px);
        frame.origin(0.0, 0.0);
        unsafe { frame.dc.PopLayer() };
        drop(std::mem::ManuallyDrop::into_inner(layer.geometricMask));
        stroke_inside(frame, framed, 8.0, self.palette.card_stroke);
    }

    /// What plays, with the elements chosen, slid out from the edge that opens it on the screen the window opened on
    /// (as large as it was made there: see `MusicRooms`), as the readings' and the workbench's previews show theirs;
    /// the example while no music plays. Returns how soon it is to be drawn again (as the preview's charts' speed: see
    /// `paint`), while something plays.
    fn paint_music_preview(&mut self, frame: &Frame, area: Rect) -> Option<f32> {
        let edge = self.preview_edge();
        let (sw, sh) = self.stage.size;
        let shows = super::music::Shows::from(&self.settings.view);
        let size = super::music::rests_at(self.settings.music_rooms.at(edge == Edge::Top), &shows);
        let zoom = self.panel_size().clamp(SIZES.0, SIZES.1);
        let (pw, ph) = (size.0 * zoom, size.1 * zoom);
        let inset = Theme::new(Skin::named(&self.settings.skin), false).inset * zoom;
        let along = |at: f32, length: f32, extent: f32| (at - length / 2.0).min(extent - GAP - length).max(GAP);
        let pos = match edge {
            Edge::Left => (inset, along(sh / 2.0, ph, sh)),
            Edge::Right => (sw - inset - pw, along(sh / 2.0, ph, sh)),
            Edge::Top => (along(sw / 2.0, pw, sw), inset),
        };
        let (k, stage_x, stage_y) = self.staged(area, edge, pw, false);
        let (ox, oy) = (area.x + stage_x, area.y + stage_y);
        let framed = area;
        let area = self.shown_of(area, k, stage_x, stage_y);
        let palette = self.palette.clone();
        self.framed(frame, framed);
        self.stage_picture(frame, area, 8.0, (ox, oy), k, zoom);
        unsafe { frame.dc.PushAxisAlignedClip(&rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let (at, scale) = ((ox + pos.0 * k, oy + pos.1 * k), zoom * k);
        let pointer = self.pointer.map(|(x, y)| ((x - at.0) / scale, (y - at.1) / scale)).filter(|(x, y)| *x >= 0.0 && *y >= 0.0 && *x < size.0 && *y < size.1);
        self.follows_pointer.push(Rect { x: at.0, y: at.1, w: size.0 * scale, h: size.1 * scale });
        let local = Matrix3x2::scale(scale, scale) * Matrix3x2::translation(at.0, at.1);
        // In the theme the desktop under it gives, and on the skin, the skin's ground under it (the desktop behind it as
        // the stage's bitmap was made: `zoom` of its DIPs a DIP of what plays).
        let behind = RECT { left: pos.0 as i32, top: pos.1 as i32, right: (pos.0 + pw) as i32, bottom: (pos.1 + ph) as i32 };
        let (theme, frost) = self.stage.look(Skin::named(&self.settings.skin), self.prefs.theme, behind);
        let ground = super::music::Ground::of(&shows, theme.dark);
        if let super::music::Ground::Skin { .. } = ground {
            frame.origin(0.0, 0.0);
            self.stage.music_ground(frame, &mut self.music_ground, &theme, frost, size, Some((pos, zoom)), local, self.scale * scale);
        }
        frame.place(local);
        let family = theme.body.family;
        let now = Instant::now();
        let painted = if super::music::heard() {
            self.music.paint(frame, size, &shows, ground, family, self.scale * scale, pointer, now)
        } else {
            self.music.paint_example(frame, size, &shows, ground, family, self.scale * scale, pointer, now, self.example_from.elapsed().as_secs_f64())
        };
        frame.origin(0.0, 0.0);
        unsafe { frame.dc.PopAxisAlignedClip() };
        stroke_inside(frame, framed, 8.0, palette.card_stroke);
        (painted.busy || !super::music::heard()).then_some(MUSIC_PREVIEW_PACE)
    }

    /// The preview: the panel as it will look over the desktop, running live.
    fn paint_preview(&mut self, frame: &Frame, palette: &Palette, width: f32, height: f32) -> Option<f32> {
        let lang = self.lang;
        // On the 通用 hub, the sub-item whose preview the right pane shows; none
        // till one is picked (see `leads_row`).
        let hub = (self.page == Page::System).then(|| self.preview).flatten();
        let x0 = NAV + self.pane();
        let head = Font::new(Family::SegoeDisplay, 20.0, 600.0);
        let title = if self.page == Page::Layout {
            pick(lang, "屏幕", "Screens")
        } else if self.page == Page::System {
            match hub {
                Some(page) => page.hub_name(lang),
                None => pick(lang, "预览", "Preview"),
            }
        } else {
            pick(lang, "预览", "Preview")
        };
        text_centred(frame, title, head, palette.text, x0, PAD_TOP + 18.0, 200.0, Align::Start);
        let detail = match self.page {
            Page::System => match hub {
                Some(Page::About) => pick(lang, "硬件、系统版本与序列号", "Its hardware, system version and serial"),
                Some(Page::Update) => pick(lang, "检查、下载与安装更新", "Checking for and installing updates"),
                Some(Page::Storage) => pick(lang, "各磁盘的用量与清理", "What each drive holds, and clearing it"),
                Some(Page::Startup) => pick(lang, "登录时启动的应用", "What starts of its own accord at sign-in"),
                Some(Page::DateTime) => pick(lang, "时区、格式与校时", "The zone, the formats, the clock"),
                Some(Page::Access) => pick(lang, "高对比度与文字大小", "High contrast and the text's size"),
                Some(Page::Language) => pick(lang, "显示语言、地区格式与时区", "The display language, the formats and the zone"),
                Some(Page::Nearby) => pick(lang, "就近收发文件、跨机接续任务", "Files to a nearby PC, a task carried across"),
                Some(Page::Sharing) => pick(lang, "文件夹与打印机的共享", "The folders and printers shared out"),
                Some(Page::Backup) => pick(lang, "备份与还原", "Backup and restore"),
                Some(Page::Recovery) => pick(lang, "开机修复与恢复环境", "Startup repair and the recovery tools"),
                _ => pick(lang, "选一项看它的详情", "Pick one to see it here"),
            },
            Page::Layout => pick(lang, "屏幕的排列与 Windows 设置一致，桌面为当前壁纸", "The screens as Windows arranges them, over your wallpaper"),
            Page::Phone => pick(lang, "手机上的 Glance", "Glance on the phone"),
            Page::Music => pick(lang, "正在播放的内容", "What plays now"),
            Page::Network => pick(lang, "网卡与当前网速", "Adapters and the speed now"),
            Page::Display => pick(lang, "分辨率、刷新、旋转与色彩", "Resolution, refresh, rotation and color"),
            Page::Sound => pick(lang, "设备、音量与静音", "Devices, their volume, and muting"),
            Page::Notifications => pick(lang, "哪些应用可打扰你", "Which apps may reach you"),
            Page::Storage => pick(lang, "临时文件与回收站", "Temporary files and the recycle bin"),
            Page::Privacy => pick(lang, "摄像头、麦克风与位置的授权", "Camera, microphone, and location consent"),
            Page::Power => pick(lang, "电池与用电", "Battery and its power"),
            Page::Gaming => pick(lang, "游戏模式与 DVR", "Game Mode and the DVR"),
            Page::Appearance => pick(lang, "主题预览与确认", "Theme preview and confirm"),
            Page::Desktop => pick(lang, "任务栏在屏幕一角的模样", "The taskbar as it stands on your screen"),
            Page::Dock => pick(lang, "Dock 在它站的那条边上", "The dock along the edge it stands on"),
            Page::Launchpad => pick(lang, "启动台的那一页怎么排", "How the launchpad lays its page"),
            Page::Accessibility => pick(lang, "对比度、文字与光标穿上后的样子", "Contrast, the text and the pointer, worn"),
            _ => pick(lang, "实时数据", "Live readings"),
        };
        let title_w = frame.gfx.measure(title, head);
        let area = Rect { x: x0, y: PAD_TOP + 40.0, w: width - x0 - PAD_SIDE, h: height - PAD_TOP - 40.0 - PAD_TOP };
        // (No wider than the preview under it: where it is narrow, cut short with an ellipsis.)
        let detail_x = x0 + title_w + 12.0;
        text_centred(frame, detail, Font::new(Family::Segoe, 12.0, 400.0), palette.text2, detail_x, PAD_TOP + 20.0, (area.x + area.w - detail_x).max(0.0), Align::Start);
        if area.w <= 0.0 || area.h <= 0.0 {
            return None;
        }
        // The 通用 hub: its right pane shows one sub-item's preview, drawn as that
        // page's own preview is — the page pointed at for the while, taken back
        // after. With none picked, a card to say so.
        if self.page == Page::System {
            match hub {
                Some(page) => {
                    let before = self.page;
                    self.page = page;
                    // Its cards are laid out whole, however far past the pane's bottom they
                    // reach; the pane scrolls what does not fit. The scroll eased last frame
                    // is set as a translation before the cards are drawn, so what they reach
                    // moves with it; the clickables follow below.
                    let scroll = self.hub_scroll.min(self.hub_most());
                    self.hub_tall = 0.0;
                    let painted = self.targets.len();
                    self.hub_menu = None;
                    unsafe { frame.dc.PushAxisAlignedClip(&rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
                    frame.origin(0.0, -scroll);
                    match self.page {
                        Page::About => self.paint_about_preview(frame, palette, area),
                        Page::Update => self.paint_update_preview(frame, palette, area),
                        Page::Storage => self.paint_storage_preview(frame, palette, area),
                        Page::DateTime => self.paint_datetime_preview(frame, palette, area),
                        _ => self.paint_os_plain(frame, palette, area),
                    }
                    // The list one of the pane's pickers left standing: drawn over every card,
                    // in the pane's own scrolled room, as the cards were.
                    self.paint_hub_menu(frame, palette);
                    frame.origin(0.0, 0.0);
                    unsafe { frame.dc.PopAxisAlignedClip() };
                    self.page = before;
                    // How far the page's cards and the word under them reach past the pane's
                    // bottom: that is what the pane may be scrolled through. A page that only
                    // fills the pane is scrolled no further than its own size.
                    let past = (self.drawn + self.hanging - (area.y + area.h)).max(0.0);
                    self.hub_tall = area.h + past;
                    self.hub_shifted(frame, palette, area, painted, scroll);
                    return None;
                }
                // With none picked, the hub opens on the panel's components themselves: its cards laid
                // clean as a grid over the room, the readings running, with no desktop behind them.
                None => return self.panel_grid(frame, palette, area),
            }
        }
        // (The panel's and the music's previews frame what of the screen they show: see `shown_of`.)
        let overlay = self.page == Page::Monitor && self.tab == Tab::Overlay;
        if matches!(self.page, Page::Layout | Page::Phone) {
            fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
            stroke_inside(frame, area, 8.0, palette.card_stroke);
        }

        if overlay {
            self.paint_overlay_preview(frame, area);
            return None;
        }
        if self.page == Page::Music {
            return self.paint_music_preview(frame, area);
        }
        if self.page == Page::Layout {
            return self.paint_layout(frame, palette, area, Instant::now());
        }
        // The phone's page: the phone app as a phone shows it, in a phone's outline.
        if self.page == Page::Phone {
            // (Its fans turn: drawn again some thirty times a second while they do.)
            return self.paint_phone(frame, area).then_some(7.5);
        }
        // The system appearance page: theme gallery, preview, confirm.
        if self.page == Page::Appearance {
            self.paint_appearance_preview(frame, palette, area);
            return None;
        }
        // The OS pages whose right-hand pane is drawn from live readings, not the panel preview.
        match self.page {
            Page::Wifi => self.paint_wifi_preview(frame, palette, area),
            Page::Bluetooth => self.paint_bluetooth_preview(frame, palette, area),
            Page::Network => self.paint_network_preview(frame, palette, area),
            Page::Display => self.paint_display_preview(frame, palette, area),
            Page::Sound => self.paint_sound_preview(frame, palette, area),
            Page::Notifications => self.paint_notify_preview(frame, palette, area),
            Page::Storage => self.paint_storage_preview(frame, palette, area),
            Page::Privacy => self.paint_privacy_preview(frame, palette, area),
            Page::Gaming => self.paint_gaming_preview(frame, palette, area),
            Page::Power => self.paint_power_preview(frame, palette, area),
            Page::ScreenTime => self.paint_screen_time_preview(frame, palette, area),
            Page::Wallpaper => self.paint_wallpaper_preview(frame, palette, area),
            Page::Desktop => self.paint_desktop_preview(frame, palette, area),
            Page::Dock => self.paint_dock_preview(frame, palette, area),
            Page::Launchpad => self.paint_launchpad_preview(frame, palette, area),
            Page::Accessibility => self.paint_accessibility_preview(frame, palette, area),
            _ => {}
        }
        if matches!(self.page, Page::Wifi | Page::Bluetooth | Page::Network | Page::Display | Page::Sound | Page::Notifications | Page::Storage | Page::Privacy | Page::Gaming | Page::Power | Page::ScreenTime | Page::Wallpaper | Page::Desktop | Page::Dock | Page::Launchpad | Page::Accessibility) {
            return None;
        }

        // The readings (or the workbench), laid out for the screen the window opened on, centred along the edge they
        // open from.
        let part = self.previewed();
        let prefs = self.prefs.clone().holding(part);
        let edge = self.preview_edge();
        self.panel_on_stage(frame, area, Some((edge, &prefs, part)), false, 1.0).0
    }

    /// The system appearance page's preview: Windows-style theme thumbnail grid.
    fn paint_appearance_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        // Card background.
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 18.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let listed = super::system_settings::themes();
        let (_, worn) = super::system_settings::theme_choices();
        let picked = self.theme_picked.or(worn);

        // --- Heading ---
        let heading_font = Font::new(Family::Segoe, 15.0, 600.0);
        text_centred(frame, &p("选择主题", "Choose a theme"), heading_font, palette.text, inner.x, inner.y, inner.w, Align::Start);

        // --- Grid of theme cards (Windows-style: wallpaper thumbnail + name below) ---
        let grid_top = inner.y + 30.0;
        let gap = 10.0;
        let name_h = 22.0;
        // Card dimensions: adapt columns to available width.
        let card_w = 150.0_f32;
        let card_h = 84.0_f32; // 16:9-ish wallpaper preview
        let cell_w = card_w + gap;
        let cell_h = card_h + name_h + gap;
        let cols = ((inner.w + gap) / cell_w).floor().max(1.0) as usize;
        let total_grid_w = cols as f32 * cell_w - gap;
        let grid_x0 = inner.x + (inner.w - total_grid_w) / 2.0;

        for (i, (name, path)) in listed.iter().enumerate() {
            let row = i / cols;
            let col = i % cols;
            let cx = grid_x0 + col as f32 * cell_w;
            let cy = grid_top + row as f32 * cell_h;
            // Stop if beyond area.
            if cy + cell_h > inner.y + inner.h - 40.0 {
                break;
            }
            let is_picked = picked == Some(i);

            // Thumbnail card background.
            let radius = 6.0;
            fill(frame, palette.control, cx, cy, card_w, card_h, radius);

            // Try to load the wallpaper image.
            let wallpaper = super::system_settings::theme_wallpaper(path);
            if let Some(ref wp) = wallpaper {
                let side = (card_w * 2.0) as u32; // ask for physical px
                if let Some(pic) = crate::station::thumbs::of(std::path::Path::new(wp), side) {
                    frame.picture(&pic, cx, cy, card_w, card_h, radius, 1.0);
                } else {
                    // Still loading: gradient placeholder.
                    fill(frame, palette.hover, cx, cy, card_w, card_h, radius);
                }
            } else {
                fill(frame, palette.hover, cx, cy, card_w, card_h, radius);
            }

            // Selection border (accent ring).
            if is_picked {
                let sel = Rect { x: cx - 1.5, y: cy - 1.5, w: card_w + 3.0, h: card_h + 3.0 };
                Canvas::stroke_rounded(frame, crate::ui::canvas::Fill::Solid(palette.selection), sel.x, sel.y, sel.w, sel.h, radius + 1.5, 2.5);
            } else {
                stroke_inside(frame, Rect { x: cx, y: cy, w: card_w, h: card_h }, radius, palette.card_stroke);
            }

            // Theme name below the card.
            let name_font = Font::new(Family::Segoe, 11.5, if is_picked { 600.0 } else { 400.0 });
            let name_colour = if is_picked { palette.text } else { palette.text2 };
            let name_y = cy + card_h + 4.0;
            text_centred(frame, name, name_font, name_colour, cx, name_y + name_h / 2.0, card_w, Align::Start);

            // Click target covers the whole cell.
            let hit = Rect { x: cx, y: cy, w: card_w, h: card_h + name_h };
            self.targets.push((hit, Target::ThemePick(i)));
        }

        // --- Confirm button at bottom right ---
        let can_confirm = self.theme_picked.is_some();
        let btn_style = if can_confirm { Button::Accent } else { Button::Quiet };
        let btn_label = p("应用", "Apply");
        let btn_right = inner.x + inner.w;
        let btn_cy = inner.y + inner.h - 16.0;
        self.button(frame, palette, &btn_label, btn_right, btn_cy, Target::ThemeConfirm, btn_style, &None);
        // Record the changes count so we know when new thumbs arrive.
        self.theme_thumbs = crate::station::thumbs::changes();
    }

    /// The profile kept for the network shown in the preview: the picked one by name, else the joined one.
    fn wifi_chosen_profile(&self) -> Option<String> {
        let shot = crate::wifi::snapshot()?;
        if let Some(name) = &self.wifi_selected {
            if let Some(profile) = shot
                .nets
                .iter()
                .chain(shot.saved.iter())
                .find(|n| n.ssid.eq_ignore_ascii_case(name))
                .map(|n| n.profile.clone())
                .filter(|p| !p.is_empty())
            {
                return Some(profile);
            }
        }
        shot.details.map(|d| d.profile).filter(|p| !p.is_empty())
    }

    /// Wi-Fi's preview: the network picked at the left (or the one joined now, where none is picked) —
    /// its signal, and where joined its channel and rates, its security, and the key kept for it (shown once asked).
    fn paint_wifi_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let Some(shot) = crate::wifi::snapshot() else {
            say(&p("正在扫描无线网络…", "Scanning for networks…"));
            self.next_frame = Instant::now() + Duration::from_millis(1000);
            return;
        };
        if !shot.on {
            say(&p("WLAN 已关闭，请先在左侧打开", "Wi-Fi is off — switch it on at the left"));
            return;
        }
        // The network shown: the one picked by name, else the one joined now.
        let chosen = match &self.wifi_selected {
            Some(name) => shot.nets.iter().chain(shot.saved.iter()).find(|n| n.ssid.eq_ignore_ascii_case(name)).cloned(),
            None => shot.nets.iter().find(|n| n.connected).cloned(),
        };
        let Some(net) = chosen else {
            say(&p("点左侧的网络以查看其信息与密码", "Pick a network at the left to see its details and key"));
            return;
        };
        // The live readouts, only where this is the network joined now.
        let live = if net.connected { shot.details.clone() } else { None };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let name = Font::new(Family::Segoe, 13.5, 400.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        let mut y = inner.y;
        text_centred(frame, &net.ssid, head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 34.0;
        if net.connected && !shot.adapter.is_empty() {
            text_centred(frame, &shot.adapter, Font::new(Family::Segoe, 12.0, 400.0), palette.text3, inner.x, y + 6.0, inner.w, Align::Start);
            y += 22.0;
        }
        let mbps = |rate: u32| -> String {
            if rate == 0 {
                return p("无", "—");
            }
            let m = rate as f64 / 1_000_000.0;
            if m >= 1.0 { format!("{m:.0} {}", p("Mbps", "Mbps")) } else { format!("{:.0} {}", rate as f64 / 1000.0, p("Kbps", "Kbps")) }
        };
        let line = |label: String, shown: String, y: &mut f32| {
            text_centred(frame, &label, name, palette.text2, inner.x, *y + 15.0, inner.w * 0.5, Align::Start);
            text_centred(frame, &shown, value, palette.text, inner.x + inner.w * 0.5, *y + 15.0, inner.w * 0.5, Align::End);
            *y += 44.0;
        };
        let state = if net.connected {
            p("已连接", "Connected")
        } else if net.known {
            p("已保存", "Saved")
        } else {
            p("未连接", "Not connected")
        };
        line(p("状态", "Status"), state, &mut y);
        if net.signal > 0 {
            line(p("信号", "Signal"), format!("{}%", net.signal), &mut y);
            line(p("安全", "Security"), if net.secure { p("已加密", "Secured") } else { p("开放", "Open") }, &mut y);
        }
        if let Some(d) = &live {
            if d.channel > 0 {
                line(p("信道", "Channel"), format!("{}", d.channel), &mut y);
            }
            line(p("频段接收", "Download"), mbps(d.rx), &mut y);
            line(p("频段发送", "Upload"), mbps(d.tx), &mut y);
        }
        // The key kept for it, masked until asked; read from its profile (see `crate::wifi::password`).
        y += 6.0;
        if net.profile.is_empty() {
            text_centred(frame, &p("密码", "Password"), name, palette.text2, inner.x, y + 15.0, inner.w * 0.4, Align::Start);
            text_centred(frame, &p("连接后才会保存密码", "Its key is kept only once joined"), value, palette.text3, inner.x + inner.w * 0.4, y + 15.0, inner.w * 0.6, Align::Start);
        } else {
            let shown = self.wifi_key.clone();
            text_centred(frame, &p("密码", "Password"), name, palette.text2, inner.x, y + 15.0, inner.w * 0.4, Align::Start);
            let key_text = match &shown {
                Some(key) if !key.is_empty() => key.clone(),
                Some(_) => p("无法读取（需管理员权限）", "Cannot be read (needs administrator)"),
                None => "••••••••".to_string(),
            };
            text_centred(frame, &key_text, value, palette.text, inner.x + inner.w * 0.4, y + 15.0, inner.w * 0.5 - 8.0, Align::Start);
            let word = if shown.is_some() { p("隐藏", "Hide") } else { p("显示密码", "Show password") };
            self.button(frame, palette, &word, inner.x + inner.w, y + 15.0, Target::WifiKey, Button::Plain, &None);
            y += 44.0;
        }
        // The network joined now can be let go of here, without forgetting it.
        if net.connected {
            self.button(frame, palette, &p("断开连接", "Disconnect"), inner.x + inner.w, y + 6.0, Target::WifiDisconnect, Button::Quiet, &None);
        }
    }

    /// Bluetooth's preview: the devices kept, and those an inquiry found near; each press opens
    /// Windows' own page to be trusted there. (The snapshot is read off-thread.)
    fn paint_bluetooth_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let Some(shot) = crate::bluetooth::snapshot() else {
            say(&p("正在读取蓝牙设备…", "Reading Bluetooth devices…"));
            self.next_frame = Instant::now() + Duration::from_millis(1500);
            return;
        };
        if shot.radios == 0 {
            say(&p("本机没有蓝牙无线电", "This PC has no Bluetooth radio"));
            return;
        }
        // The device shown: the one picked by address, else the first kept.
        let chosen = match &self.bt_selected {
            Some(addr) => shot.paired.iter().chain(shot.nearby.iter()).find(|d| &d.addr == addr).cloned(),
            None => shot.paired.first().cloned(),
        };
        let Some(d) = chosen else {
            say(&p("点左侧的设备以查看其信息", "Pick a device at the left to see its details"));
            return;
        };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let name = Font::new(Family::Segoe, 13.5, 400.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        let mut y = inner.y;
        text_centred(frame, &d.name, head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 34.0;
        let line = |label: String, shown: String, y: &mut f32| {
            text_centred(frame, &label, name, palette.text2, inner.x, *y + 15.0, inner.w * 0.5, Align::Start);
            text_centred(frame, &shown, value, palette.text, inner.x + inner.w * 0.5, *y + 15.0, inner.w * 0.5, Align::End);
            *y += 44.0;
        };
        line(p("地址", "Address"), d.addr.clone(), &mut y);
        let state = if d.connected {
            p("已连接", "Connected")
        } else if d.remembered {
            p("已配对", "Paired")
        } else {
            p("未配对", "Not paired")
        };
        line(p("状态", "Status"), state, &mut y);
        // An unpaired device is asked of Windows; a kept one is opened there to be managed.
        let word = if d.remembered { p("在系统中管理", "Manage in Windows") } else { p("配对", "Pair") };
        self.button(frame, palette, &word, inner.x + inner.w, y + 12.0, Target::BtPair(d.name.clone()), Button::Plain, &None);
    }

    /// Display's preview, laid out as macOS' Displays pane: the screens as a row of
    /// wallpapered thumbnails to pick from, then what the picked one is used as, the
    /// resolutions as scaled desktops (larger text to more space), the refresh and the
    /// rotation as segmented choices that write back, and the colour profile and HDR read
    /// only — a press there leads to Windows' own page. The scale is set here too.
    fn paint_display_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        // The rows outgrow the pane on most screens, so the pane shows only what stands within
        // it and the wheel walks the rest: the content shifts up by the scroll and the
        // clickables with it, as in the hub's right pane.
        let scroll = self.hub_scroll.min(self.hub_most());
        let painted = self.targets.len();
        unsafe { frame.dc.PushAxisAlignedClip(&rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        frame.origin(0.0, -scroll);
        self.display_pane(frame, palette, area, scroll);
        frame.origin(0.0, 0.0);
        unsafe { frame.dc.PopAxisAlignedClip() };
        // (What the last row reaches past the pane's bottom is what the wheel may still turn.)
        self.hub_tall = area.h + self.hanging;
        self.hub_shifted(frame, palette, area, painted, scroll);
    }

    fn display_pane(&mut self, frame: &Frame, palette: &Palette, area: Rect, scroll: f32) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        // The card behind the rows, drawn as long as the scroll can lift, so its bottom keeps
        // the pane while the rows slide off its top.
        let veil = Rect { x: area.x, y: area.y - scroll, w: area.w, h: area.h + scroll };
        fill(frame, palette.card, veil.x, veil.y, veil.w, veil.h, 8.0);
        stroke_inside(frame, veil, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        self.hanging = 0.0;
        if self.screens.is_empty() {
            say(&p("没有读到显示器。", "No display was read."));
            return;
        }
        // Each screen gathered whole first: the preview leans on its own buttons between
        // reads, so none of the list is left borrowed across them.
        let mut cards: Vec<(String, bool, Option<crate::sys::Mode>)> = Vec::new();
        for s in &self.screens {
            let device = crate::sys::device_of(&s.monitor);
            let cur = crate::sys::current(&device);
            cards.push((device, s.primary, cur));
        }
        // The screen shown: the picked one by name, else the main, else the first.
        let idx = self
            .display_selected
            .as_ref()
            .and_then(|name| cards.iter().position(|(device, ..)| *device == *name))
            .unwrap_or_else(|| cards.iter().position(|(_, primary, _)| *primary).unwrap_or(0));
        let device = cards[idx].0.clone();
        let primary = cards[idx].1;
        let here = cards[idx].2;
        let (cw, ch) = here.map(|c| (c.wide, c.tall)).unwrap_or((0, 0));
        let now_hz = here.map(|c| c.hz).unwrap_or(0);
        let modes = crate::sys::modes(&device);
        let orient = crate::sys::orientation(&device);
        // The wallpaper every screen and every size card is filled with, taken once; while
        // it is still read a held colour stands in, so nothing is left blank.
        let wall = super::wallpaper::current_path().and_then(|path| crate::station::thumbs::of(std::path::Path::new(&path), 512));
        let label = Font::new(Family::Segoe, 13.0, 600.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        let small = Font::new(Family::Segoe, 12.0, 400.0);
        let mut y = inner.y;

        // The screens as macOS lays them out: a row of pictures, each kept to its own shape,
        // the one worn marked; a press here picks which screen the rest of the pane is about.
        text_centred(frame, &p("显示器", "Displays"), label, palette.text, inner.x, y + 8.0, inner.w, Align::Start);
        self.button(frame, palette, &p("排列…", "Arrange…"), inner.x + inner.w, y + 16.0, Target::Open("ms-settings:display".into()), Button::Plain, &None);
        y += 30.0;
        let n = cards.len();
        let gap = 16.0;
        let cell = ((inner.w - gap * (n as f32 - 1.0)) / n as f32).max(60.0);
        let box_h = 84.0;
        for (at, (name, is_primary, cur)) in cards.iter().enumerate() {
            let cx = inner.x + at as f32 * (cell + gap);
            let (pw, ph) = match cur {
                Some(m) => {
                    let (mut w, mut h) = (m.wide as f32, m.tall as f32);
                    if orient == 1 || orient == 3 {
                        std::mem::swap(&mut w, &mut h);
                    }
                    (w, h)
                }
                None => (16.0, 9.0),
            };
            let fit = (cell / pw).min(box_h / ph);
            let (mw, mh) = (pw * fit, ph * fit);
            let (mx, my) = (cx + (cell - mw) / 2.0, y + (box_h - mh) / 2.0);
            let chosen = at == idx;
            match &wall {
                Some(pic) => frame.picture(pic, mx, my, mw, mh, 4.0, 1.0),
                None => fill(frame, palette.hover, mx, my, mw, mh, 4.0),
            }
            stroke_inside(frame, Rect { x: mx, y: my, w: mw, h: mh }, 4.0, if chosen { palette.selection } else { palette.card_stroke });
            let said = crate::sys::friendly(name);
            let under = if *is_primary { format!("{} · {}", said, p("主", "Main")) } else { said };
            let tw = frame.gfx.measure(&under, small);
            text_centred(frame, &under, small, if chosen { palette.text } else { palette.text2 }, cx + (cell - tw) / 2.0, y + box_h + 12.0, tw, Align::Start);
            self.targets.push((Rect { x: cx, y, w: cell, h: box_h + 20.0 }, Target::ScreenSelect(name.clone())));
        }
        y += box_h + 30.0;

        // What this screen is for; another may be crowned the one things start on.
        text_centred(frame, &p("用作", "Use as"), label, palette.text, inner.x, y + 15.0, inner.w * 0.4, Align::Start);
        if primary {
            text_centred(frame, &p("主显示器", "Main display"), value, palette.text2, inner.x + inner.w * 0.4, y + 15.0, inner.w * 0.6, Align::End);
        } else {
            self.button(frame, palette, &p("设为主显示器", "Make main"), inner.x + inner.w, y + 15.0, Target::SetPrimary(device.clone()), Button::Plain, &None);
        }
        y += 44.0;

        // The sizes as macOS shows them: not a list of figures but the desktop itself at each,
        // its word growing as the pixels thin — the left end larger text, the right more space.
        // Each card is named with the pixels it stands for; a reading told in halves is no reading.
        text_centred(frame, &p("分辨率", "Resolution"), label, palette.text, inner.x, y + 8.0, inner.w, Align::Start);
        y += 24.0;
        if modes.is_empty() {
            text_centred(frame, &p("该显示器未回报可选的分辨率", "This display reports no modes to choose"), small, palette.text3, inner.x, y + 6.0, inner.w, Align::Start);
            y += 24.0;
        } else {
            let total = modes.len();
            let picks: Vec<usize> = if total <= 5 {
                (0..total).collect()
            } else {
                (0..5).map(|k| (k as f32 * (total - 1) as f32 / 4.0).round() as usize).collect()
            };
            let maxpix = modes.first().map(|m| m.wide as f32 * m.tall as f32).unwrap_or(1.0).max(1.0);
            let rgap = 10.0;
            let rcw = ((inner.w - rgap * (picks.len() as f32 - 1.0)) / picks.len() as f32).max(48.0);
            let rch = 58.0;
            for (k, &mi) in picks.iter().enumerate() {
                let m = modes[mi];
                let cx = inner.x + k as f32 * (rcw + rgap);
                match &wall {
                    Some(pic) => frame.picture(pic, cx, y, rcw, rch, 5.0, 1.0),
                    None => fill(frame, palette.hover, cx, y, rcw, rch, 5.0),
                }
                // A mock window whose bulk grows as the pixels thin, so a smaller size reads
                // as bigger text before its numbers are read at all.
                let grow = (maxpix / (m.wide as f32 * m.tall as f32).max(1.0)).clamp(0.6, 1.8);
                let (ww, wh) = ((rcw * 0.5 * grow).min(rcw - 8.0), (rch * 0.42 * grow).min(rch - 8.0));
                let (wx, wy) = (cx + (rcw - ww) / 2.0, y + (rch - wh) / 2.0);
                fill(frame, Color::hex(0x1c1c1c, 0.55), wx, wy, ww, wh, 3.0);
                fill(frame, Color::hex(0xffffff, 0.7), wx + 4.0, wy + 4.0, (ww * 0.3).max(3.0), (2.0 * grow).min(wh - 8.0).max(1.0), 1.0);
                let worn = here.is_some_and(|c| c.wide == m.wide && c.tall == m.tall);
                stroke_inside(frame, Rect { x: cx, y, w: rcw, h: rch }, 5.0, if worn { palette.selection } else { palette.card_stroke });
                self.targets.push((Rect { x: cx, y, w: rcw, h: rch }, Target::ModeApply { device: device.clone(), wide: m.wide, tall: m.tall, hz: now_hz }));
                let figure = format!("{}×{}", m.wide, m.tall);
                let fw = frame.gfx.measure(&figure, small);
                text_centred(frame, &figure, small, if worn { palette.text } else { palette.text3 }, cx + (rcw - fw) / 2.0, y + rch + 10.0, fw, Align::Start);
            }
            y += rch + 26.0;
            text_centred(frame, &p("更大字体", "Larger Text"), small, palette.text3, inner.x, y + 8.0, inner.w * 0.5, Align::Start);
            text_centred(frame, &p("更多空间", "More Space"), small, palette.text3, inner.x + inner.w * 0.5, y + 8.0, inner.w * 0.5, Align::End);
            y += 22.0;
        }

        // The rates this size will draw at, the worn one marked; a press keeps the size and
        // takes the rate. Where only one is offered, it is said rather than chosen.
        let rates = here.map(|c| crate::sys::refreshes(&device, c.wide, c.tall)).unwrap_or_default();
        text_centred(frame, &p("刷新率", "Refresh"), label, palette.text, inner.x, y + 15.0, inner.w * 0.4, Align::Start);
        if rates.len() <= 1 {
            let said = here.map(|c| format!("{} Hz", c.hz)).unwrap_or_else(|| p("未知", "Unknown"));
            text_centred(frame, &said, value, palette.text2, inner.x + inner.w * 0.4, y + 15.0, inner.w * 0.6, Align::End);
        } else {
            let mut right = inner.x + inner.w;
            for hz in rates.iter().rev() {
                let kind = if *hz == now_hz { Button::Accent } else { Button::Plain };
                right = self.button(frame, palette, &format!("{hz} Hz"), right, y + 15.0, Target::ModeApply { device: device.clone(), wide: cw, tall: ch, hz: *hz }, kind, &None) - 8.0;
            }
        }
        y += 44.0;

        // The way round the screen is drawn, four turns; a driver that will not take one keeps
        // the screen as it was.
        text_centred(frame, &p("旋转", "Rotation"), label, palette.text, inner.x, y + 15.0, inner.w * 0.3, Align::Start);
        let mut right = inner.x + inner.w;
        for (word, want) in [(p("倒置", "Upside down"), 3u32), (p("翻转", "Flipped"), 2), (p("竖屏", "Portrait"), 1), (p("标准", "Standard"), 0)] {
            let kind = if orient == want { Button::Accent } else { Button::Plain };
            right = self.button(frame, palette, &word, right, y + 15.0, Target::RotateSet { device: device.clone(), orient: want }, kind, &None) - 8.0;
        }
        y += 44.0;

        // The colour profile and the high dynamic range are the system's to set; here they are
        // only read (a profile by its file) and pressed to Windows' own page.
        let prof = crate::sys::color_profile(&device);
        let said = if prof.is_empty() { p("未关联", "None") } else { prof };
        text_centred(frame, &p("颜色描述文件", "Colour profile"), label, palette.text, inner.x, y + 15.0, inner.w * 0.45, Align::Start);
        text_centred(frame, &said, value, palette.text2, inner.x + inner.w * 0.45, y + 15.0, inner.w * 0.55, Align::End);
        self.targets.push((Rect { x: inner.x, y, w: inner.w, h: 34.0 }, Target::Open("ms-settings:display".into())));
        y += 44.0;
        text_centred(frame, &p("高动态范围", "High dynamic range"), label, palette.text, inner.x, y + 15.0, inner.w * 0.55, Align::Start);
        text_centred(frame, &p("在系统中设置", "Set in Windows"), value, palette.text2, inner.x + inner.w * 0.55, y + 15.0, inner.w * 0.45, Align::End);
        self.targets.push((Rect { x: inner.x, y, w: inner.w, h: 34.0 }, Target::Open("ms-settings:display".into())));
        y += 44.0;

        // The desktop's growth, set here as Windows' own panel does: newly opened windows take
        // it at once, the rest wait to be shown again — said under the row rather than hidden.
        let scale = crate::sys::scale();
        text_centred(frame, &p("缩放", "Scale"), label, palette.text, inner.x, y + 15.0, inner.w * 0.3, Align::Start);
        let mut right = inner.x + inner.w;
        for step in [200u32, 175, 150, 125, 100] {
            let kind = if scale == step { Button::Accent } else { Button::Plain };
            right = self.button(frame, palette, &format!("{step}%"), right, y + 15.0, Target::ScaleSet(step), kind, &None) - 8.0;
        }
        y += 34.0;
        text_centred(frame, &p("新开的窗口即刻采用，已开着的等重新显示才跟上。", "Windows opened after the change take it at once; those already open follow when they are next shown."), small, palette.text3, inner.x, y + 6.0, inner.w, Align::Start);
        y += 30.0;
        text_centred(frame, &p("夜间光照", "Night light"), label, palette.text, inner.x, y + 8.0, inner.w, Align::Start);
        y += 24.0;
        let now_night = crate::sys::night();
        let mut right = inner.x + inner.w;
        for (level, word) in [(3u32, p("浓", "Strong")), (2, p("中", "Medium")), (1, p("淡", "Faint")), (0, p("关", "Off"))] {
            let kind = if now_night == level { Button::Accent } else { Button::Plain };
            right = self.button(frame, palette, &word, right, y + 12.0, Target::NightSet(level), kind, &None) - 8.0;
        }
        // (The buttons stand at the middle of `y + 12`; the pane wants its padding under them.)
        self.hanging = (y + 28.0 + margin - (area.y + area.h)).max(0.0);
    }

    /// Sound's preview: the device picked at the left (or the default one things play through), its
    /// volume as a track to set by the twenty steps along it, and its mute. Formats and enhancements
    /// stay Windows' own, so the row's button leads there.
    fn paint_sound_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let Some(shot) = crate::audio::snapshot() else {
            say(&p("正在读取音频设备…", "Reading the sound devices…"));
            self.next_frame = Instant::now() + Duration::from_millis(1500);
            return;
        };
        if shot.devices.is_empty() {
            say(&p("没有发现音频设备。", "No sound devices were found."));
            return;
        }
        // The device shown: the picked one by id, else the default that plays, else the first that plays, else the first.
        let chosen = match &self.audio_selected {
            Some(id) => shot.devices.iter().find(|d| &d.id == id).cloned(),
            None => None,
        }
        .or_else(|| shot.devices.iter().find(|d| d.render && d.default).cloned())
        .or_else(|| shot.devices.iter().find(|d| d.render).cloned())
        .or_else(|| shot.devices.first().cloned());
        let Some(d) = chosen else { return };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let small = Font::new(Family::Segoe, 12.0, 400.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        let mut y = inner.y;
        text_centred(frame, &d.name, head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 40.0;
        let mut where_ = if d.render { p("输出", "Output") } else { p("输入", "Input") };
        if d.default {
            where_ = format!("{where_} · {}", p("默认", "Default"));
        }
        text_centred(frame, &where_, small, palette.text3, inner.x, y + 4.0, inner.w, Align::Start);
        y += 44.0;
        // The volume, as a track filled to where it stands, and set by the twenty steps along it.
        text_centred(frame, &p("音量", "Volume"), value, palette.text2, inner.x, y + 10.0, inner.w * 0.4, Align::Start);
        let shown = if d.muted { p("已静音", "Muted") } else { format!("{}%", d.volume) };
        text_centred(frame, &shown, value, if d.muted { palette.text3 } else { palette.text }, inner.x + inner.w * 0.4, y + 10.0, inner.w * 0.6, Align::End);
        y += 28.0;
        let track_h = 8.0;
        fill(frame, palette.control, inner.x, y, inner.w, track_h, 4.0);
        let fill_w = inner.w * d.volume.min(100) as f32 / 100.0;
        if fill_w > 0.0 {
            fill(frame, if d.muted { palette.text3 } else { palette.selection }, inner.x, y, fill_w, track_h, 4.0);
        }
        let knob_x = (inner.x + fill_w - 7.0).max(inner.x);
        fill(frame, if d.muted { palette.text3 } else { palette.selection }, knob_x, y - 3.0, 14.0, 14.0, 7.0);
        let steps = 20u32;
        let seg = inner.w / steps as f32;
        for j in 0..steps {
            let percent = (j + 1) * 5;
            let r = Rect { x: inner.x + j as f32 * seg, y: y - 8.0, w: seg, h: track_h + 16.0 };
            self.targets.push((r, Target::AudioVolume { id: d.id.clone(), percent }));
        }
        y += 34.0;
        // Muting, or lifting the mute — the one thing besides volume the mixer keeps here.
        let word = if d.muted { p("取消静音", "Unmute") } else { p("静音", "Mute") };
        let kind = if d.muted { Button::Accent } else { Button::Plain };
        self.button(frame, palette, &word, inner.x + inner.w, y + 12.0, Target::AudioMute { id: d.id.clone(), muted: !d.muted }, kind, &None);
    }

    /// Notifications' preview: the master switch, then the app picked at the left and whether its
    /// banners are let. The banner and lock-screen styles are Windows' own, so the row's button leads there.
    fn paint_notify_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let mut y = inner.y;
        // The master switch: notices let at all, or held from every app at once.
        text_centred(frame, &p("通知", "Notifications"), Font::new(Family::Segoe, 13.0, 600.0), palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 30.0;
        let master = crate::sys::notify_master();
        let mut right = inner.x + inner.w;
        for (on, word) in [(true, p("开", "On")), (false, p("关", "Off"))] {
            let kind = if master == on { Button::Accent } else { Button::Plain };
            right = self.button(frame, palette, &word, right, y + 12.0, Target::NotifyMaster(on), kind, &None) - 8.0;
        }
        y += 44.0;
        let Some(apps) = crate::sys::notify_snapshot() else {
            say(&p("正在读取通知设置…", "Reading the notification settings…"));
            self.next_frame = Instant::now() + Duration::from_millis(1500);
            return;
        };
        if apps.is_empty() {
            say(&p("没有应用留下通知设置。", "No app has left a notification setting."));
            return;
        }
        // The app shown: the picked one by id, else the first.
        let chosen = match &self.notify_selected {
            Some(id) => apps.iter().find(|a| &a.aumid == id).cloned(),
            None => None,
        }
        .or_else(|| apps.first().cloned());
        let Some(a) = chosen else { return };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let small = Font::new(Family::Segoe, 12.0, 400.0);
        text_centred(frame, &a.display, head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 40.0;
        let state = if a.enabled { p("允许通知", "Allowed") } else { p("已禁止", "Off") };
        text_centred(frame, &state, small, if a.enabled { palette.selection } else { palette.text3 }, inner.x, y + 4.0, inner.w, Align::Start);
        y += 44.0;
        // Letting the app's banners, or holding them.
        let word = if a.enabled { p("禁止通知", "Turn off") } else { p("允许通知", "Turn on") };
        self.button(frame, palette, &word, inner.x + inner.w, y + 12.0, Target::NotifyToggle { aumid: a.aumid.clone(), on: !a.enabled }, Button::Plain, &None);
    }

    /// Storage's preview: the room the temporary files take and the recycle bin's, each with the
    /// button that frees it (the walk and the emptying done off-thread), and storage sense — the
    /// tidying Windows does of its own — put on or off. Per-drive totals and sense's own rules stay
    /// Windows', so the row's button leads there.
    fn paint_storage_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        // A name at the left of a row and its figures at the right, the two columns kept
        // the width apart that the words of the left need.
        let line = |label: String, shown: String, y: &mut f32| {
            text_centred(frame, &label, value, palette.text2, inner.x, *y + 15.0, inner.w * 0.4, Align::Start);
            text_centred(frame, &shown, value, palette.text, inner.x + inner.w * 0.4, *y + 15.0, inner.w * 0.6, Align::End);
            *y += 44.0;
        };
        let mut y = inner.y;
        // The disk picked at the left (the first when none is yet), and each letter mounted on it: the
        // room taken and the room held, asked of the volume's root, which answers at once — nothing is
        // walked, so the frame may wait on it.
        text_centred(frame, &p("磁盘", "Drives"), head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 34.0;
        let disks = super::system_settings::disks();
        let chosen = self
            .storage_selected
            .and_then(|n| disks.iter().find(|d| d.number == n))
            .or(disks.first());
        match chosen {
            None => {
                text_centred(frame, &p("没读到可用的磁盘。", "No drive could be read."), Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, y + 5.0, inner.w, Align::Start);
                y += 44.0;
            }
            Some(disk) => {
                let word = if disk.model.is_empty() { format!("{} {}", p("磁盘", "Disk"), disk.number) } else { disk.model.clone() };
                text_centred(frame, &format!("{word} · {}", human_bytes(disk.size)), Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, y + 4.0, inner.w, Align::Start);
                y += 40.0;
                for (name, used, total) in &disk.volumes {
                    line(name.clone(), super::text::usage(*used, *total), &mut y);
                }
            }
        }
        y += 14.0;
        match crate::sys::storage_snapshot() {
            Some(shot) => {
                // The temporary files, and the button that frees what is not locked.
                text_centred(frame, &p("临时文件", "Temporary files"), head, palette.text, inner.x, y + 15.0, inner.w, Align::Start);
                text_centred(frame, &human_bytes(shot.temp), value, palette.text, inner.x + inner.w * 0.5, y + 15.0, inner.w * 0.5, Align::End);
                y += 44.0;
                self.button(frame, palette, &p("清理临时文件", "Clear temporary files"), inner.x + inner.w, y + 15.0, Target::CleanTemp, Button::Plain, &None);
                y += 44.0;
                // The recycle bin, and the button that empties it.
                text_centred(frame, &p("回收站", "Recycle bin"), head, palette.text, inner.x, y + 15.0, inner.w, Align::Start);
                text_centred(frame, &format!("{} · {} {}", human_bytes(shot.recycle_bytes), shot.recycle_count, p("项", "items")), value, palette.text, inner.x + inner.w * 0.4, y + 15.0, inner.w * 0.6, Align::End);
                y += 44.0;
                self.button(frame, palette, &p("清空回收站", "Empty recycle bin"), inner.x + inner.w, y + 15.0, Target::EmptyRecycle, Button::Plain, &None);
            }
            None => {
                say(&p("正在统计磁盘占用…", "Tallying what the disk holds…"));
                self.next_frame = Instant::now() + Duration::from_millis(1500);
            }
        }
        y += 44.0;
        // Storage sense: the tidying Windows does of its own.
        text_centred(frame, &p("存储感知", "Storage Sense"), head, palette.text, inner.x, y + 15.0, inner.w, Align::Start);
        y += 44.0;
        let sense = crate::sys::storage_sense();
        let mut right = inner.x + inner.w;
        for (on, word) in [(true, p("开", "On")), (false, p("关", "Off"))] {
            let kind = if sense == on { Button::Accent } else { Button::Plain };
            right = self.button(frame, palette, &word, right, y + 15.0, Target::StorageSenseApply(on), kind, &None) - 8.0;
        }
    }

    /// Privacy's preview: the apps that have asked for the capability its rows sit under, each on a
    /// line of its own with the switch that lets or holds it back. The capabilities are split in the
    /// middle column; the one picked there names the head. The reading runs PowerShell, so it is
    /// taken off-thread. The other categories stay Windows' own.
    fn paint_privacy_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        // The list of apps outgrows the pane on a machine with many of them, so the pane is a
        // window onto it: the content shifts up by the scroll, clipped to the pane, and the
        // clickables along with it — the same machinery the hub's right pane runs on.
        let scroll = self.hub_scroll.min(self.hub_most());
        let painted = self.targets.len();
        unsafe { frame.dc.PushAxisAlignedClip(&rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        frame.origin(0.0, -scroll);
        self.privacy_pane(frame, palette, area, scroll);
        frame.origin(0.0, 0.0);
        unsafe { frame.dc.PopAxisAlignedClip() };
        // (How far past the pane's bottom the lines reach is how far the wheel may still turn.)
        self.hub_tall = area.h + self.hanging;
        self.hub_shifted(frame, palette, area, painted, scroll);
    }

    fn privacy_pane(&mut self, frame: &Frame, palette: &Palette, area: Rect, scroll: f32) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        // The card behind the content, drawn as long as the scroll can lift: its bottom stays
        // with the pane while the lines above it slide away.
        let veil = Rect { x: area.x, y: area.y - scroll, w: area.w, h: area.h + scroll };
        fill(frame, palette.card, veil.x, veil.y, veil.w, veil.h, 8.0);
        stroke_inside(frame, veil, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        self.hanging = 0.0;
        let mut y = inner.y;
        let capability = self.privacy_capability.clone();
        // The reach the picked app's row sits under, named at the head — the middle column has split by it.
        let reach = match capability.as_str() {
            "Location" => p("位置", "Location"),
            "Microphone" => p("麦克风", "Microphone"),
            _ => p("摄像头", "Camera"),
        };
        text_centred(frame, &reach, Font::new(Family::Segoe, 13.0, 600.0), palette.text, inner.x, y + 12.0, inner.w, Align::Start);
        y += 44.0;
        let Some(apps) = crate::sys::consent_snapshot(&capability) else {
            say(&p("正在读取授权…", "Reading the permissions…"));
            self.next_frame = Instant::now() + Duration::from_millis(1500);
            return;
        };
        if apps.is_empty() {
            say(&p("没有应用就这项权限留下记录。", "No app has left a mark for this permission."));
            return;
        }
        // One line per app, kept together under hairlines: its name, whether it is let now, and
        // the switch that lets or holds it back. What does not fit the pane is scrolled
        // through, as the hub's cards are; the reach past its bottom is told for the scroll.
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let small = Font::new(Family::Segoe, HINT, 400.0);
        let row = 56.0;
        for a in apps {
            if y > inner.y {
                fill(frame, palette.rule, inner.x, y, inner.w, 1.0, 0.0);
            }
            text_centred(frame, &a.display, font, palette.text, inner.x, y + row / 2.0 - 11.0, inner.w * 0.55, Align::Start);
            let said = if a.allowed { p("已允许", "Allowed") } else { p("已拒绝", "Blocked") };
            text_centred(frame, &said, small, if a.allowed { palette.selection } else { palette.text3 }, inner.x, y + row / 2.0 + 11.0, inner.w * 0.4, Align::Start);
            // The one switch that lets the app or holds it back, as every other card's row wears it.
            let target = Target::ConsentSet { capability: capability.clone(), keys: a.keys.clone(), packaged: a.packaged, allow: !a.allowed };
            let switch_left = inner.x + inner.w - 40.0;
            let pressed = self.pressed == Some(target.clone());
            self.toggle(frame, palette, format!("toggle:consent:{capability}:{}", a.display), a.allowed, true, switch_left, y + row / 2.0, Instant::now(), pressed);
            self.targets.push((Rect { x: inner.x, y, w: inner.w, h: row }, target));
            y += row;
        }
        // (The last line's bottom, less the pane's: what the scroll has left to give.)
        self.hanging = (y + margin - (area.y + area.h)).max(0.0);
    }

    /// Gaming's preview: whether Game Mode takes over, and whether the DVR records. A quick
    /// registry read, so it is taken here rather than off-thread; the buttons flip the same
    /// switches the left-hand page shows.
    fn paint_gaming_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let mut y = inner.y;
        for (label, on, switch) in [
            (p("游戏模式", "Game Mode"), super::system_settings::game_mode(), Switch::GameMode),
            (p("游戏 DVR", "Game DVR"), super::system_settings::game_flag("AppCaptureEnabled"), Switch::GameDvr),
        ] {
            text_centred(frame, &label, head, palette.text, inner.x, y + 15.0, inner.w, Align::Start);
            let mut right = inner.x + inner.w;
            for (want, word) in [(true, p("开", "On")), (false, p("关", "Off"))] {
                let kind = if on == want { Button::Accent } else { Button::Plain };
                right = self.button(frame, palette, &word, right, y + 15.0, Target::Switch(switch), kind, &None) - 8.0;
            }
            y += 56.0;
        }
    }

    /// Windows Update as it stands: the edition it runs, how many fixes are in and the newest of
    /// them, the list of what went in, then how it fetches them and where its own page is. The
    /// list lives in WMI's store and is slow to answer, so it is read in the background — the pane
    /// says so while it comes rather than standing blank.
    fn paint_update_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh: &'static str, en: &'static str| pick(lang, zh, en).to_string();
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        let margin = 20.0;
        let col = hub_column(area.w, 560.0, margin);
        let x = area.x + (area.w - col) / 2.0;
        // The fixes as last read, newest first; nothing landed yet while the background read runs.
        let read = crate::sys::patches_snapshot();
        let reading = read.is_none();
        let read = read.unwrap_or_default();
        let latest = read.first().map(|x| x.when.clone()).unwrap_or_default();
        // Windows itself: its edition, feature update and build, how many fixes went in, the newest
        // of them, and the day the system came.
        let product = super::system_settings::os_product();
        let (release, build) = super::system_settings::os_release();
        let install = super::system_settings::os_install_date();
        let said = if reading {
            p("正在读取…", "Reading…")
        } else if read.is_empty() {
            p("没读到", "None read")
        } else {
            format!("{}", read.len())
        };
        let mut facts = Vec::new();
        if !product.is_empty() {
            facts.push((p("操作系统", "Operating system"), product));
        }
        if !release.is_empty() {
            facts.push((p("功能版本", "Feature update"), release));
        }
        if !build.is_empty() {
            facts.push((p("OS 内部版本", "OS build"), build));
        }
        facts.push((p("已装更新", "Installed"), said));
        if !latest.is_empty() {
            facts.push((p("最近更新", "Latest"), latest));
        }
        if !install.is_empty() {
            facts.push((p("安装于", "Installed on"), install));
        }
        let mut y = self.grouped_card(frame, palette, x, area.y + margin, col, Some(&p("Windows 更新", "Windows Update")), &facts, None, font, hint);
        // The fixes themselves, newest first. Only the newest few are shown here — the pane is not
        // scrolled — and the count above keeps the rest honest.
        const SHOWN: usize = 8;
        let lines: Vec<(String, String)> = if reading || read.is_empty() {
            // One sentence where a value would stand, the name column left empty for it.
            vec![(String::new(), if reading { p("正在读取…", "Reading…") } else { p("没读到的更新", "None were read") })]
        } else {
            let mut told: Vec<(String, String)> = read
                .iter()
                .take(SHOWN)
                .map(|x| {
                    let what = if x.what.is_empty() || x.what == "Update" { x.id.clone() } else { format!("{} · {}", x.id, x.what) };
                    (x.when.clone(), what)
                })
                .collect();
            if read.len() > SHOWN {
                let more = read.len() - SHOWN;
                let (zh, en) = (format!("…其余 {more} 项见更新历史"), format!("…{more} more in the history"));
                told.push((String::new(), lang.pick(&zh, &en).to_string()));
            }
            told
        };
        y = self.grouped_card(frame, palette, x, y, col, Some(&p("已安装更新", "Installed updates")), &lines, None, font, hint);
        // How it fetches fixes, and when it last looked; the installing itself is Windows' to do,
        // so the one door leads to its own page.
        let auto = match super::system_settings::update_auto_option() {
            Some(1) => p("关闭", "Off"),
            Some(2) => p("下载与安装前通知", "Notify before download and install"),
            Some(3) => p("自动下载", "Download automatically"),
            Some(4) => p("自动下载并安装", "Download and install automatically"),
            Some(5) => p("由本地管理员决定", "Let the local admin choose"),
            _ => p("由 Windows 默认管理", "Left to Windows' default"),
        };
        let last = super::system_settings::update_last_check();
        let last_said = if last.is_empty() { p("尚未记录", "Not recorded") } else { last };
        y = self.grouped_card(
            frame,
            palette,
            x,
            y,
            col,
            Some(&p("自动更新", "Automatic updates")),
            &[(p("获取方式", "Fetching"), auto), (p("上次检查", "Last checked"), last_said)],
            Some((p("Windows 更新…", "Windows Update…"), "ms-settings:windowsupdate")),
            font,
            hint,
        );
        // The one thing the hub cannot do itself is start a check; the door leads to Windows' page.
        y += 6.0;
        self.button(frame, palette, &p("检查更新", "Check for updates"), x + col, y, Target::Open("ms-settings:windowsupdate-action".into()), Button::Plain, &None);
        y += 34.0;
        frame.text_wrapped(
            &p("更新的下载与安装由 Windows 负责，Glance 只把它们读给你看，并带你到它的页面。", "Windows itself downloads and installs the fixes; Glance only reads them off the machine and leads you to its page."),
            hint,
            palette.text2,
            x,
            y + 16.0,
            col,
        );
        self.hanging = frame.gfx.wrapped_height(&p("更新的下载与安装由 Windows 负责，Glance 只把它们读给你看，并带你到它的页面。", "Windows itself downloads and installs the fixes; Glance only reads them off the machine and leads you to its page."), hint, col) + 16.0;
        // Woken again while the list is still coming in.
        if reading {
            self.next_frame = Instant::now() + Duration::from_millis(1500);
        }
    }

    /// A card of the hub's shape, its rows stood `line` apart: the usual spacing is 32, wider where
    /// a page wants the facts to breathe (see 关于本机).
    fn grouped_card(
        &mut self,
        frame: &Frame,
        palette: &Palette,
        x: f32,
        y: f32,
        width: f32,
        heading: Option<&str>,
        lines: &[(String, String)],
        door: Option<(String, &str)>,
        font: Font,
        hint: Font,
    ) -> f32 {
        // (The room between lines is the About card's own: every card in the hub breathes the same.)
        self.grouped_card_spaced(frame, palette, x, y, width, heading, lines, door, font, hint, 44.0)
    }

    /// A card of the hub's shape: a heading, its lines of name and value kept
    /// together under hairlines, and one door where Windows keeps the thing.
    /// Returns where the next card begins.
    fn grouped_card_spaced(
        &mut self,
        frame: &Frame,
        palette: &Palette,
        x: f32,
        y: f32,
        width: f32,
        heading: Option<&str>,
        lines: &[(String, String)],
        door: Option<(String, &str)>,
        font: Font,
        hint: Font,
        line: f32,
    ) -> f32 {
        let head = 34.0;
        let fall = 18.0;
        let pad = 14.0;
        let door_room = if door.is_some() { 44.0 } else { 0.0 };
        // Each line measured first: what the name leaves the value is its room, and a
        // value longer than that goes on a line of its own under the name rather than
        // be cut short — a reading told in halves is no reading at all.
        let rooms: Vec<(f32, bool)> = lines
            .iter()
            .map(|(name, value)| {
                let room = (width - 2.0 * pad - frame.gfx.measure(name, font) - 16.0).max(80.0);
                (room, frame.gfx.measure(value, font) > room)
            })
            .collect();
        let mut h = if heading.is_some() { head } else { 0.0 };
        h += rooms.iter().map(|(_, wrapped)| line + if *wrapped { fall } else { 0.0 }).sum::<f32>() + door_room + pad;
        card(frame, palette, x, y, width, h, palette.card);
        stroke_inside(frame, Rect { x, y, w: width, h }, 10.0, palette.card_stroke);
        let mut cy = y + pad;
        if let Some(said) = heading {
            text_centred(frame, said, Font::new(Family::Segoe, 13.0, 600.0), palette.text2, x + pad, y + head / 2.0, width - 2.0 * pad, Align::Start);
            cy = y + head;
        }
        for (at, (name, value)) in lines.iter().enumerate() {
            let (room, wrapped) = rooms[at];
            if at > 0 {
                fill(frame, palette.rule, x + pad, cy, width - 2.0 * pad, 1.0, 0.0);
            }
            // The value measured and set against the card's right edge, where it can
            // only ever be cut short at the room it is given; trusted to a trailing
            // alignment it would reach out over the edge instead.
            let said = frame.gfx.measure(value, font);
            if wrapped {
                text_centred(frame, name, font, palette.text2, x + pad, cy + line / 2.0 - fall / 2.0, width * 0.42, Align::Start);
                let shown = said.min(width - 2.0 * pad);
                text_centred(frame, value, font, palette.text, x + width - pad - shown, cy + line / 2.0 + fall / 2.0, shown, Align::Start);
                cy += line + fall;
            } else {
                text_centred(frame, name, font, palette.text2, x + pad, cy + line / 2.0, width * 0.42, Align::Start);
                let shown = said.min(room);
                text_centred(frame, value, font, palette.text, x + width - pad - shown, cy + line / 2.0, shown, Align::Start);
                cy += line;
            }
        }
        if let Some((word, uri)) = door {
            if !lines.is_empty() {
                fill(frame, palette.rule, x + pad, cy, width - 2.0 * pad, 1.0, 0.0);
            }
            text_centred(frame, &word, hint, palette.selection, x + pad, cy + 22.0, width - 2.0 * pad, Align::Start);
            self.targets.push((
                Rect { x, y: cy, w: width, h: door_room },
                Target::Open(uri.to_string()),
            ));
        }
        // (Where the card ends is kept for the pane's scroll: see `hub_tall`.)
        self.drawn = y + h + 14.0;
        y + h + 14.0
    }

    /// A card of rows each ending at its right in a switch or in a box of choices. A heading
    /// and a line said under it where the card has them. Returns where the next card begins.
    fn switch_card(
        &mut self,
        frame: &Frame,
        palette: &Palette,
        x: f32,
        y: f32,
        width: f32,
        heading: Option<&str>,
        said: Option<&str>,
        rows: &[(String, CardEnd)],
        font: Font,
        hint: Font,
    ) -> f32 {
        let pad = 14.0;
        let head = 34.0;
        let row = 44.0;
        let switch = 40.0;
        let mut h = if heading.is_some() { head } else { pad };
        if said.is_some() {
            h += 24.0;
        }
        h += rows.len() as f32 * row + pad;
        card(frame, palette, x, y, width, h, palette.card);
        stroke_inside(frame, Rect { x, y, w: width, h }, 10.0, palette.card_stroke);
        let mut cy = y + pad;
        if let Some(text) = heading {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 600.0), palette.text2, x + pad, y + head / 2.0, width - 2.0 * pad, Align::Start);
            cy = y + head;
        }
        if let Some(text) = said {
            text_centred(frame, text, hint, palette.text3, x + pad, cy + 12.0, width - 2.0 * pad, Align::Start);
            cy += 24.0;
        }
        let switch_left = x + width - pad - switch;
        let now = Instant::now();
        // How low the lowest open list reaches: a long one (the zones are a hundred and
        // forty four) is let into the height the pane may be scrolled through, so its last
        // word is reachable rather than cut off below the pane.
        let mut hung = 0.0f32;
        for (at, (name, end)) in rows.iter().enumerate() {
            if at > 0 {
                fill(frame, palette.rule, x + pad, cy, width - 2.0 * pad, 1.0, 0.0);
            }
            let middle = cy + row / 2.0;
            match end {
                CardEnd::Switch(aside, on, enabled, target) => {
                    text_centred(frame, name, font, palette.text, x + pad, middle, (switch_left - 32.0 - x - pad).max(40.0), Align::Start);
                    let aw = frame.gfx.measure(aside, hint);
                    text_centred(frame, aside, hint, palette.text2, switch_left - 14.0 - aw, middle, aw, Align::Start);
                    let pressed = self.pressed == Some(target.clone());
                    self.toggle(frame, palette, format!("toggle:{name}"), *on, *enabled, switch_left, middle, now, pressed);
                    self.targets.push((Rect { x, y: cy, w: width, h: row }, target.clone()));
                }
                CardEnd::Picker(which) => {
                    let field = *which;
                    let letter = Font::new(Family::Segoe, 13.0, 400.0);
                    let (_, words, chosen) = self.choices(field);
                    let widest = words.iter().map(|word| frame.gfx.measure(word, letter)).fold(0.0f32, f32::max);
                    let bw = (widest + 44.0).clamp(96.0, (width - 2.0 * pad - 130.0).max(96.0));
                    let box_ = Rect { x: x + width - pad - bw, y: middle - 16.0, w: bw, h: 32.0 };
                    text_centred(frame, name, font, palette.text, x + pad, middle, (box_.x - 12.0 - x - pad).max(40.0), Align::Start);
                    let open = self.open_dropdown == Some(field);
                    fill(frame, if open { palette.hover } else { palette.control }, box_.x, box_.y, box_.w, box_.h, 6.0);
                    stroke_inside(frame, box_, 6.0, if open { palette.selection } else { palette.card_stroke });
                    if let Some(at) = chosen.filter(|at| *at < words.len()) {
                        text_centred(frame, &words[at], letter, palette.text, box_.x + 12.0, middle, box_.w - 36.0, Align::Start);
                    }
                    frame.icon(Icon::ChevronDown, Point { x: box_.x + box_.w - 16.0, y: middle }, CHEVRON, palette.text2);
                    self.targets.push((box_, Target::Choice(field, chosen.unwrap_or(0))));
                    if open {
                        // The list is kept back to be drawn over the whole pane once every card is
                        // laid (see `paint_hub_menu`): drawn here it would sit under the rows that
                        // come after it. Its room is counted all the same, capped to what shows.
                        self.hub_menu = Some((field, box_));
                        let full = DROP_LINE * words.len() as f32 + 8.0;
                        hung = hung.max(box_.y + box_.h + 4.0 + full.min(DROP_CAP));
                    }
                }
            }
            cy += row;
        }
        // (Where the card ends is kept for the pane's scroll: see `hub_tall`.)
        self.drawn = (y + h + 14.0).max(hung + 14.0);
        y + h + 14.0
    }

    /// The list an open picker has left standing: drawn after every card of the pane, over them
    /// all, so no row laid later covers a word of it. A list longer than the pane shows is kept
    /// within its own height and turned by the wheel (see `wheel`).
    fn paint_hub_menu(&mut self, frame: &Frame, palette: &Palette) {
        let Some((field, at)) = self.hub_menu else { return };
        let (_, words, chosen) = self.choices(field);
        let full = DROP_LINE * words.len() as f32 + 8.0;
        let cap = full.min(DROP_CAP);
        let most = (full - cap).max(0.0);
        self.dropdown_scroll = self.dropdown_scroll.clamp(0.0, most);
        let menu = Rect { x: at.x, y: at.y + at.h + 4.0, w: at.w, h: cap };
        // The box gives up its own place while the list stands, so a press elsewhere shuts it.
        self.targets.retain(|(_, target)| !matches!(target, Target::Choice(f, _) if *f == field));
        let _ = skins::shadows(frame, &[(menu, 6.0)], &[Shadow { y: 2.0, blur: 12.0, spread: 0.0, color: Color::hex(0x000000, 0.16) }]);
        fill(frame, palette.window, menu.x, menu.y, menu.w, menu.h, 6.0);
        stroke_inside(frame, menu, 6.0, palette.card_stroke);
        let letter = Font::new(Family::Segoe, 13.0, 400.0);
        unsafe { frame.dc.PushAxisAlignedClip(&rect(menu.x, menu.y, menu.w, menu.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        for (i, word) in words.iter().enumerate() {
            let r = Rect { x: menu.x + 4.0, y: menu.y + 4.0 + i as f32 * DROP_LINE - self.dropdown_scroll, w: menu.w - 8.0, h: DROP_LINE };
            // (What is turned clean out of the list is neither drawn nor pressed.)
            if r.y + r.h <= menu.y || r.y >= menu.y + menu.h {
                continue;
            }
            if chosen == Some(i) {
                fill(frame, palette.hover, r.x, r.y, r.w, r.h, 4.0);
            }
            text_centred(frame, word, letter, palette.text, r.x + 12.0, r.y + DROP_LINE / 2.0, r.w - 24.0, Align::Start);
            self.targets.push((r, Target::Choice(field, i)));
        }
        unsafe { frame.dc.PopAxisAlignedClip() };
        // A hairline of a thumb, when the list is longer than the pane shows.
        if most > 0.0 {
            let thumb = (cap * cap / full).max(20.0);
            let at = menu.y + 4.0 + (cap - 8.0 - thumb) * (self.dropdown_scroll / most);
            fill(frame, palette.text3.alpha(0.6), menu.x + menu.w - 7.0, at, 3.0, thumb, 1.5);
        }
    }

    /// The hub's sentence under its last card: told in lines `col` wide, and kept
    /// counted in the height the pane may be scrolled through.
    fn hub_note(&mut self, frame: &Frame, palette: &Palette, text: &str, x: f32, y: f32, col: f32, hint: Font) {
        frame.text_wrapped(text, hint, palette.text2, x, y, col);
        self.hanging = frame.gfx.wrapped_height(text, hint, col) + 16.0;
    }

    /// The hub's plain card: the page's own mark and word, its readings told as
    /// the machine has them — what is the system's to keep is left to its page.
    fn paint_os_plain(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        let margin = 20.0;
        let col = area.w - 2.0 * margin;
        let x = area.x + margin;
        let y = area.y + margin;
        use super::system_settings as sys;
        match self.page {
            // (Windows Update is told by `paint_update_preview`, not here.)
            Page::Startup => {
                // Glance's own place at the sign-in, then every app the machine and this
                // user keep there — each with the switch the shell itself holds. The list
                // spans two hives and a file's version resource, so it is read in the
                // background; the card says so while it comes rather than standing short.
                let read = crate::sys::startup_snapshot();
                let reading = read.is_none();
                let apps = read.unwrap_or_default();
                let named = |app: &crate::sys::Startup| app.display.eq_ignore_ascii_case("glance") || app.name.eq_ignore_ascii_case("glance");
                // Glance's own entry is the switch at the top, never a second one below.
                let kept: Vec<&crate::sys::Startup> = apps.iter().filter(|app| !named(app)).collect();
                let rows: Vec<(String, CardEnd)> = kept
                    .iter()
                    .take(16)
                    .map(|app| {
                        let hive = if app.machine { p("本机", "Machine") } else { p("本用户", "User") };
                        (app.display.clone(), CardEnd::Switch(hive, app.enabled, true, Target::Startup(app.machine, app.name.clone(), !app.enabled)))
                    })
                    .collect();
                let said = if reading {
                    p("正在读取启动项…", "Reading the start-up apps…")
                } else {
                    let on = kept.iter().filter(|app| app.enabled).count() + usize::from(self.autostart);
                    format!("{} {} · {}", p("允许自启", "Allowed"), on, kept.len() + 1)
                };
                let mut to = vec![(p("Glance", "Glance"), CardEnd::Switch(p("本应用", "This app"), self.autostart, self.may_autostart, Target::Switch(Switch::Startup)))];
                to.extend(rows);
                let after = self.switch_card(frame, palette, x, y, col, Some(&p("登录时启动", "At sign-in")), Some(&said), &to, font, hint);
                self.grouped_card(frame, palette, x, after, col, None, &[], Some((p("Windows 登录项设置…", "Windows' login items…"), "ms-settings:startupapps")), font, hint);
                // Woken again while the list is still coming in.
                if reading {
                    self.next_frame = Instant::now() + Duration::from_millis(1500);
                }
            }
            Page::Access => {
                // What the machine is worn for reading: the three effects it keeps as
                // switches of its own, and the two sizes it keeps as numbers — the
                // sliders that set those live in the page's own list, so here they are
                // only told as they stand, with the way on to the rest.
                let said = vec![
                    (
                        p("高对比度", "High contrast"),
                        CardEnd::Switch(if sys::high_contrast() { p("开", "On") } else { p("关", "Off") }, sys::high_contrast(), true, Target::Switch(Switch::HighContrast)),
                    ),
                    (
                        p("透明效果", "Transparent effects"),
                        CardEnd::Switch(if sys::system_transparent() { p("开", "On") } else { p("关", "Off") }, sys::system_transparent(), true, Target::Switch(Switch::Glass)),
                    ),
                    (
                        p("动画效果", "Animation effects"),
                        CardEnd::Switch(if sys::system_animation() { p("开", "On") } else { p("关", "Off") }, sys::system_animation(), true, Target::Switch(Switch::AnimOn)),
                    ),
                ];
                let after = self.switch_card(frame, palette, x, y, col, Some(&p("显示", "Display")), None, &said, font, hint);
                // The pointer's scheme is told by the name it gives itself; where it has
                // none, it is worn as Windows came with it.
                let scheme = sys::cursor_scheme();
                let scheme = if scheme.is_empty() || scheme == "(default)" { p("默认", "default") } else { scheme };
                let pointer = format!("{} · {}", sys::cursor_size(), scheme);
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    after,
                    col,
                    Some(&p("大小", "Sizes")),
                    &[
                        (p("文字", "Text"), format!("{}%", sys::text_scale())),
                        (p("光标", "Pointer"), pointer),
                    ],
                    Some((p("辅助功能设置…", "Accessibility settings…"), "ms-settings:easeofaccess")),
                    font,
                    hint,
                );
                self.hub_note(frame, palette, &p("文字缩放要注销并重新登录才完全生效；上面三个开关即时采用。", "A change to the text size takes full hold after signing out and back in; the three switches above are taken at once."), x, after + 16.0, col, hint);
            }
            Page::Language => {
                // The tongue the display is worn in, the shapes the locale keeps and the
                // zone the clock stands in are all Glance's to change: the language list is
                // put in order, the locale's own shapes are copied over the user's, the
                // country is written and told round, and the clock is moved. What is read
                // but not kept here — the machine's own name for its locale — is told under
                // the boxes, and the way on to Windows' page stays open at the foot.
                let picks = vec![
                    (p("显示语言", "Display language"), CardEnd::Picker(Field::DisplayLanguage)),
                    (p("短日期", "Short date"), CardEnd::Picker(Field::ShortDate)),
                    (p("长日期", "Long date"), CardEnd::Picker(Field::LongDate)),
                ];
                let after = self.switch_card(frame, palette, x, y, col, Some(&p("语言与日期格式", "Language & date formats")), None, &picks, font, hint);
                let shapes = vec![
                    (p("地区格式", "Regional format"), CardEnd::Picker(Field::RegionFormat)),
                    (p("国家或地区", "Country or region"), CardEnd::Picker(Field::Country)),
                    (p("时间", "Time"), CardEnd::Picker(Field::TimePattern)),
                    (p("时区", "Time zone"), CardEnd::Picker(Field::TimeZone)),
                ];
                let after = self.switch_card(frame, palette, x, after, col, Some(&p("地区、数字与时区", "Region, numbers and the zone")), None, &shapes, font, hint);
                // What the machine says of itself, told rather than chosen: it is the read-
                // out of the shapes above, not a setting of its own.
                let geo = sys::country_name(&sys::geo_name(), lang);
                let zone = sys::current_zone();
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    after,
                    col,
                    Some(&p("现在读到的", "As it reads now")),
                    &[
                        (p("地区格式", "Regional format"), sys::locale_name()),
                        (p("国家或地区", "Country or region"), if geo.is_empty() { p("未知", "Unknown") } else { geo }),
                        (p("时间", "Time"), sys::time_format()),
                        (p("时区", "Time zone"), if zone.is_empty() { p("未知", "Unknown") } else { zone }),
                    ],
                    Some((p("语言与地区设置…", "Language & Region settings…"), "ms-settings:regionlanguage")),
                    font,
                    hint,
                );
                self.hub_note(frame, palette, &p("日期、时间与数字格式即刻换上，时区即刻挪钟。显示语言要新开的程序才穿上，资源管理器与已开着的得注销一次；国家或地区即刻写进系统，程序多在启动时才读它。没有装语言包的语言不在列出的几个里。", "The date, time and number shapes are taken at once, and the zone moves the clock at once. A display language is worn by whatever opens after it; the shell and what already runs wait for a sign-out. The country is written at once, though most programs read it when they start. A tongue without its language pack is not among those listed."), x, after + 16.0, col, hint);
            }
            Page::Sharing => {
                // The machine as the network knows it: its name and the workgroup or domain
                // it stands in, the cards it carries that on, what the carrying one holds and
                // the services that answer on it, and the folders it holds out. What is shared
                // out is the system's to arrange, so the way on is its page; the reading is
                // quick, save for the services, which are asked in the background.
                let read = crate::network::snapshot();
                let reading = read.is_none();
                let nics = read.map(|shot| shot.adapters).unwrap_or_default();
                let named = sys::computer_name();
                let group = sys::workgroup();
                let mut card = |y: f32, title: &str, lines: &[(String, String)], door: Option<(String, &str)>| {
                    self.grouped_card(frame, palette, x, y, col, Some(title), lines, door, font, hint)
                };
                // Each card names what it holds; an empty one says so rather than standing blank.
                let asked = |said: Vec<(String, String)>, none: &str| {
                    if said.is_empty() {
                        vec![(none.to_string(), String::new())]
                    } else {
                        said
                    }
                };
                let mut lines = vec![
                    (p("计算机名", "Name"), if named.is_empty() { p("未知", "Unknown") } else { named }),
                    (p("工作组或域", "Workgroup or domain"), if group.is_empty() { p("未加入", "Not joined") } else { group }),
                    (
                        p("网络适配器", "Adapters"),
                        if reading {
                            p("正在读取…", "Reading…")
                        } else {
                            let count = nics.len();
                            lang.pick(&format!("{count} 块"), &format!("{count} adapters")).to_string()
                        },
                    ),
                ];
                for nic in nics.iter() {
                    let said = if nic.current { p("在用", "Carrying") } else if nic.up { p("已连接", "Connected") } else { p("未连接", "Down") };
                    let where_ = if nic.ipv4.is_empty() { String::new() } else { format!(" · {}", nic.ipv4) };
                    lines.push((nic.name.clone(), format!("{said}{where_}")));
                }
                let after = card(
                    y,
                    &p("本机在网络里", "This PC on the network"),
                    &lines,
                    Some((p("网络和共享中心…", "Network and Sharing Centre…"), "ms-settings:network-sharing")),
                );
                // The one the internet leaves by, told whole: the address it holds, the way
                // out it was given, the names it asks, its hardware address and its rate.
                let carried = nics.iter().find(|nic| nic.current).or_else(|| nics.iter().find(|nic| nic.up));
                let mut lines = match carried {
                    Some(nic) => vec![
                        (p("走的这条", "The one carried"), format!("{} · {}", nic.name, if crate::network::is_wire(nic.kind) { p("无线", "wireless") } else { p("有线", "wired") })),
                        (p("IPv4 地址", "IPv4 address"), if nic.ipv4.is_empty() { p("没有", "None") } else { nic.ipv4.clone() }),
                        (p("网关", "Gateway"), if nic.gateway.is_empty() { p("没有", "None") } else { nic.gateway.clone() }),
                        (p("DNS 服务器", "DNS servers"), if nic.dns.is_empty() { p("没有记下的", "None listed") } else { nic.dns.clone() }),
                        (p("物理地址", "Physical address"), if nic.mac.is_empty() { p("读不到", "Not read") } else { nic.mac.clone() }),
                        (p("链路速率", "Link speed"), net_rate(nic.link_bps)),
                    ],
                    None => vec![(p("网络适配器", "Adapters"), if reading { p("正在读取…", "Reading…") } else { p("没读到连着的网卡", "None up") })],
                };
                // The network the wireless one is joined to, where it says.
                if let Some(ssid) = crate::wifi::snapshot().and_then(|shot| shot.details).map(|one| one.ssid).filter(|said| !said.is_empty()) {
                    lines.push((p("连着的网络", "Joined to"), ssid));
                }
                let after = card(after, &p("走的那条", "The one it goes by"), &asked(lines, &p("走的那条", "The way out")), None);
                // What answers on the machine for the sharing to work at all: the file and
                // printer services, and the discovery that lets it be seen.
                let services = crate::sys::service_snapshot(&["LanmanServer", "LanmanWorkstation", "FDResPub"]);
                let told = services.clone().unwrap_or_default();
                let mut lines: Vec<(String, String)> = ["LanmanServer", "LanmanWorkstation", "FDResPub"]
                    .iter()
                    .filter_map(|want| {
                        told.iter().find(|one| one.name == *want).map(|one| {
                            let state = if one.running { p("在跑", "Running") } else { p("没在跑", "Not running") };
                            let start = match one.auto {
                                true => p("自动起", "Starts of its own"),
                                false => p("手动起", "Started when asked"),
                            };
                            (one.display.clone(), format!("{state} · {start}"))
                        })
                    })
                    .collect();
                if services.is_none() {
                    lines.push((p("共享要靠的服务", "The services behind it"), p("正在读取…", "Reading…")));
                }
                let after = card(after, &p("它靠的服务", "What it leans on"), &asked(lines, &p("它靠的服务", "What it leans on")), None);
                // The folders held out, read off the file service's own configuration; the
                // administrative ones it keeps to itself are told apart from the chosen.
                let shares = sys::share_list();
                let mut lines: Vec<(String, String)> = shares
                    .iter()
                    .filter(|share| !share.special)
                    .take(8)
                    .map(|share| (share.name.clone(), if share.path.is_empty() { p("读不到位置", "No path read") } else { share.path.clone() }))
                    .collect();
                let special = shares.iter().filter(|share| share.special).count();
                if special > 0 {
                    lines.push((p("系统自留的共享", "Its own shares"), lang.pick(&format!("{special} 个"), &format!("{special} shares")).to_string()));
                }
                if lines.is_empty() {
                    lines.push((p("共享出去的文件夹", "Folders shared out"), p("一个也没有", "Not one")));
                }
                let after = card(
                    after,
                    &p("共享出去", "Held out to the network"),
                    &lines,
                    Some((p("共享设置…", "Sharing settings…"), "ms-settings:network-sharing")),
                );
                self.hub_note(
                    frame,
                    palette,
                    &p("这里只把共享的实情读给你看；要开、要改，都在 Windows 的那一页。", "Only the truth of the sharing is read here; the opening and the changing stay on Windows' own page."),
                    x,
                    after + 16.0,
                    col,
                    hint,
                );
                // Woken again while the adapters and the services are still coming in.
                if reading || services.is_none() {
                    self.next_frame = Instant::now() + Duration::from_millis(1500);
                }
            }
            Page::Nearby => {
                // The switch Windows keeps and whom it lets the machine reach, the two
                // radios the sending stands on, and the way on to where Windows arranges
                // it. The sending itself stays the system's doing.
                let level = sys::nearby_share_level();
                let on = level != 0;
                let said = vec![(
                    p("就近共享", "Nearby sharing"),
                    CardEnd::Switch(
                        match level {
                            1 => p("所有人", "Anyone near"),
                            2 => p("我的设备", "My own devices"),
                            _ => p("关", "Off"),
                        },
                        on,
                        true,
                        Target::Nearby(!on),
                    ),
                )];
                let after = self.switch_card(
                    frame,
                    palette,
                    x,
                    y,
                    col,
                    Some(&p("就近共享", "Nearby sharing")),
                    Some(&p("用蓝牙和无线局域网把文件发给近处的设备", "Sends files to a machine near by over Bluetooth and Wi-Fi")),
                    &said,
                    font,
                    hint,
                );
                let lines = vec![
                    (
                        p("无线局域网", "Wi-Fi"),
                        match crate::wifi::state() {
                            Some(true) => p("开着", "On"),
                            Some(false) => p("关着", "Off"),
                            None => p("读不到", "Not read"),
                        },
                    ),
                    (
                        p("蓝牙", "Bluetooth"),
                        match crate::bluetooth::snapshot() {
                            Some(shot) if shot.radios == 0 => p("没有蓝牙网卡", "No radio in this PC"),
                            Some(shot) => lang.pick(&format!("{} 块网卡 · {} 个已配对", shot.radios, shot.paired.len()), &format!("{} radios, {} paired", shot.radios, shot.paired.len())).to_string(),
                            None => p("正在读取…", "Reading…"),
                        },
                    ),
                    (
                        p("可共享给", "Shared with"),
                        match level {
                            1 => p("近处的任何人", "Anyone near by"),
                            2 => p("自己的设备", "This user's own devices"),
                            _ => p("关着", "Off"),
                        },
                    ),
                ];
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    after,
                    col,
                    Some(&p("它要靠的", "What it stands on")),
                    &lines,
                    Some((p("就近共享设置…", "Nearby sharing settings…"), "ms-settings:nearby-sharing-settings")),
                    font,
                    hint,
                );
                self.hub_note(frame, palette, &p("接力与跨设备接续由 Windows 的跨设备体验承担；这里只读与开关就近共享。", "Picking a task up on another PC is the cross-device experience's own; only nearby sharing is read and set here."), x, after + 16.0, col, hint);
            }
            Page::Backup => {
                // What Windows keeps of the files and whether the points a bad change is
                // turned back to are kept, then the room each drive has left for those
                // copies — the history's own arrangement is the system's to do.
                let drives = sys::drive_space();
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    y,
                    col,
                    Some(&p("备份", "Backup")),
                    &[
                        (p("文件历史记录", "File History"), if sys::file_history_configured() { p("已设置", "Set up") } else { p("未设置", "Not set up") }),
                        (p("系统还原点", "Restore points"), match sys::restore_disabled() {
                            Some(true) => p("已关闭", "Turned off"),
                            Some(false) => p("开着", "Kept"),
                            // (The key is only there once the protection has been turned on by hand.)
                            None => p("没开启", "Not turned on"),
                        }),
                    ],
                    Some((p("备份设置…", "Backup settings…"), "ms-settings:backup")),
                    font,
                    hint,
                );
                // Where the copies could go: each drive read, with the room it has left.
                let lines: Vec<(String, String)> = drives
                    .iter()
                    .take(5)
                    .map(|(name, used, total)| (name.clone(), format!("{} / {}", super::text::bytes((*total - *used) as f64), super::text::bytes(*total as f64))))
                    .collect();
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    after,
                    col,
                    Some(&p("各盘的余量", "Room on the drives")),
                    &lines,
                    None,
                    font,
                    hint,
                );
                self.hub_note(frame, palette, &p("文件历史记录把文档、图片等处的副本存到另一块盘或网络位置；这里只读它的状态。", "File History keeps copies of the document shelves on another drive or a network place; only its state is read here."), x, after + 16.0, col, hint);
            }
            Page::Recovery => {
                // What the firmware keeps and what the machine writes when it stops, read
                // and told; where a restart into the recovery environment or the firmware
                // setup is asked, the machine does it of its own accord once agreed to.
                let disk = sys::startup_disk();
                let after = self.grouped_card(
                    frame,
                    palette,
                    x,
                    y,
                    col,
                    Some(&p("启动磁盘与恢复", "Startup disk and recovery")),
                    &[
                        (p("启动磁盘", "Startup disk"), if disk.is_empty() { p("未知", "Unknown") } else { disk }),
                        (p("固件", "Firmware"), match sys::firmware_uefi() {
                            Some(true) => p("UEFI", "UEFI"),
                            Some(false) => p("传统 BIOS", "Legacy BIOS"),
                            None => p("未知", "Unknown"),
                        }),
                        (p("安全启动", "Secure boot"), match sys::secure_boot() {
                            Some(true) => p("开着", "Worn"),
                            Some(false) => p("未开", "Not worn"),
                            // (The firmware's own keep is only read with the administrator's leave.)
                            None => p("读不到", "Not read"),
                        }),
                        (p("故障转储", "Crash dump"), match sys::crash_dump() {
                            Some(0) => p("不写", "Written nothing"),
                            Some(1) => p("完整内存转储", "A whole memory dump"),
                            Some(2) => p("内核内存转储", "The kernel's own"),
                            Some(3) => p("小内存转储", "A small dump"),
                            Some(4) => p("自动", "Automatic"),
                            _ => p("未知", "Unknown"),
                        }),
                    ],
                    Some((p("恢复设置…", "Recovery settings…"), "ms-settings:recovery")),
                    font,
                    hint,
                );
                let mut by = after;
                for (label, recovery) in [
                    (p("重新启动到固件…", "Restart into the firmware…"), false),
                    (p("重新启动到恢复环境…", "Restart into the recovery environment…"), true),
                ] {
                    text_centred(frame, &label, hint, palette.selection, x + 14.0, by + 18.0, col - 28.0, Align::Start);
                    self.targets.push((Rect { x, y: by, w: col, h: 36.0 }, Target::RebootTo(recovery)));
                    by += 36.0;
                }
            }
            // A page kept elsewhere in this window: its mark and its word, and the
            // readings its own preview tells.
            page => {
                frame.icon(page.icon(), Point { x: x + col / 2.0, y: y + 44.0 }, 44.0, palette.text2);
                text_middle(frame, page.name(lang), Font::new(Family::Segoe, 16.0, 600.0), palette.text, x + col / 2.0, y + 92.0);
            }
        }
    }

    /// 关于本机 as the hub shows it: the machine named over its own facts.
    fn paint_about_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let zh = lang == Lang::Zh;
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        // The facts are read whole rather than cut short, so the column is let run wide; and the rows
        // are stood further apart than the hub's usual, to give the machine's telling room to breathe.
        let col = hub_column(area.w, 560.0, margin);
        let line = 44.0;
        let x = area.x + (area.w - col) / 2.0;
        super::about::draw_machine(frame, area.x + area.w / 2.0, area.y + margin + 34.0, palette.text2, palette.card, crate::smbios::chassis_laptop());
        let named = super::system_settings::computer_name();
        text_middle(frame, &named, Font::new(Family::Segoe, 17.0, 600.0), palette.text, x + col / 2.0, area.y + margin + 84.0);
        text_middle(frame, pick(lang, "本机", "This PC"), Font::new(Family::Segoe, HINT, 400.0), palette.text2, x + col / 2.0, area.y + margin + 104.0);
        let facts = super::system_settings::about_facts(zh);
        // macOS keeps the machine's hardware and the system it runs apart, under their own heads; so
        // does this page. The name stands at the top already, so its fact is left out of both cards.
        const HARDWARE: [&str; 14] = [
            "制造商", "机型", "处理器", "机载内存", "显卡", "启动磁盘", "序列号",
            "Manufacturer", "Model", "Processor", "Installed RAM", "Graphics", "Startup disk", "Serial number",
        ];
        const DEVICE: [&str; 2] = ["设备名称", "Device name"];
        let (hardware, system): (Vec<_>, Vec<_>) = facts
            .into_iter()
            .filter(|(label, _)| !DEVICE.contains(&label.as_str()))
            .partition(|(label, _)| HARDWARE.contains(&label.as_str()));
        let top = (area.y + margin + 126.0).min(area.y + area.h - 120.0);
        let after = self.grouped_card_spaced(frame, palette, x, top, col, Some(pick(lang, "硬件", "Hardware")), &hardware, None, font, hint, line);
        self.grouped_card_spaced(frame, palette, x, after, col, Some(pick(lang, "系统", "System")), &system, Some((pick(lang, "详细信息…", "More info…").to_string(), "ms-settings:about")), font, hint, line);
    }

    /// 日期与时间 as the hub shows it: the clock as it reads now and the zone it
    /// keeps, the two switches that can be turned here, the formats behind them.
    fn paint_datetime_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        use super::system_settings as sys;
        let font = Font::new(Family::Segoe, 13.0, 400.0);
        let hint = Font::new(Family::Segoe, HINT, 400.0);
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let col = hub_column(area.w, 420.0, margin);
        let x = area.x + (area.w - col) / 2.0;

        // The hour the machine stands at, its zone named, the shift it keeps from
        // UTC told whole, and whether summer time has hold of it.
        let named = sys::current_zone();
        let after = self.grouped_card(
            frame,
            palette,
            x,
            area.y + margin,
            col,
            Some(&p("日期与时间", "Date & Time")),
            &[
                (p("现在", "Now"), sys::wall_clock()),
                (p("时区", "Time zone"), if named.is_empty() { p("未知", "Unknown") } else { named }),
                (p("与 UTC 的差", "Shift from UTC"), sys::zone_shift().unwrap_or_else(|| p("未知", "Unknown"))),
                (p("夏令时", "Summer time"), if sys::summer_time() { p("在跑", "Running") } else { p("不在跑", "Not running") }),
            ],
            None,
            font,
            hint,
        );

        // The three things about the clock that Glance changes in place: the hours it
        // counts on, whether the seconds are shown, and the zone it stands in. The hours
        // and the zone are chosen from lists, as Windows itself puts them; the seconds are
        // one switch.
        let rows = vec![
            (p("小时制", "The hours"), CardEnd::Picker(Field::ClockHour)),
            (
                p("显示秒数", "Seconds on the clock"),
                CardEnd::Switch(if sys::clock_seconds() { p("显示", "Shown") } else { p("不显示", "Not shown") }, sys::clock_seconds(), true, Target::Switch(Switch::Seconds)),
            ),
            (p("时区", "Time zone"), CardEnd::Picker(Field::TimeZone)),
        ];
        let after = self.switch_card(frame, palette, x, after, col, Some(&p("时钟的样子", "The clock's face")), None, &rows, font, hint);

        let blank = |said: String| if said.is_empty() { p("未记录", "Not recorded") } else { said };
        let after = self.grouped_card(
            frame,
            palette,
            x,
            after,
            col,
            Some(&p("格式", "The formats")),
            &[
                (p("短日期", "Short date"), blank(sys::short_date())),
                (p("长日期", "Long date"), blank(sys::long_date())),
                (p("时间", "Time"), blank(sys::time_format())),
            ],
            Some((p("日期与时间设置…", "Date & Time settings…"), "ms-settings:dateandtime")),
            font,
            hint,
        );
        // The formats' other half — the tongue they are worn in — is chosen on the
        // language page, which is Glance's own; so the way on leads there, not to Windows.
        text_centred(frame, &p("语言与地区：显示语言与格式…", "Language & Region: the display language and the formats…"), hint, palette.selection, x, after + 18.0, col, Align::Start);
        self.targets.push((Rect { x, y: after + 4.0, w: col, h: 32.0 }, Target::Preview(Page::Language)));
        // The clock is kept running to the second, so the card is drawn again to
        // show the one it has reached.
        self.next_frame = Instant::now() + Duration::from_millis(1000);
    }

    /// Network's preview: the adapter picked at the left (or the one the internet leaves by), what
    /// it holds — addresses, gateway, DNS, its rate — and the machine's upload and download running
    /// along the bottom, from the live history.
    fn paint_network_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let say = |text: &str| {
            text_centred(frame, text, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, inner.y + 10.0, inner.w, Align::Start);
        };
        let Some(shot) = crate::network::snapshot() else {
            say(&p("正在读取网卡…", "Reading the adapters…"));
            self.next_frame = Instant::now() + Duration::from_millis(1000);
            return;
        };
        // The adapter shown: the one picked by name, else the one the internet leaves by, else the first up.
        let chosen = match &self.net_selected {
            Some(name) => shot.adapters.iter().find(|a| &a.name == name).cloned(),
            None => shot.adapters.iter().find(|a| a.current).or_else(|| shot.adapters.iter().find(|a| a.up)).cloned(),
        };
        let Some(a) = chosen else {
            say(&p("点左侧的网卡以查看其信息", "Pick an adapter at the left to see its details"));
            return;
        };
        let head = Font::new(Family::Segoe, 16.0, 600.0);
        let name = Font::new(Family::Segoe, 13.5, 400.0);
        let value = Font::new(Family::Segoe, 13.5, 600.0);
        let mut y = inner.y;
        text_centred(frame, &a.name, head, palette.text, inner.x, y + 10.0, inner.w, Align::Start);
        y += 34.0;
        let kind = if crate::network::is_wire(a.kind) {
            p("无线网卡", "Wireless adapter")
        } else if crate::network::is_wired(a.kind) {
            p("有线网卡", "Wired adapter")
        } else {
            p("网络适配器", "Network adapter")
        };
        text_centred(frame, &kind, Font::new(Family::Segoe, 12.0, 400.0), palette.text3, inner.x, y + 6.0, inner.w, Align::Start);
        y += 24.0;
        let dash = |v: String| if v.is_empty() { p("无", "—") } else { v };
        let line = |label: String, shown: String, y: &mut f32| {
            text_centred(frame, &label, name, palette.text2, inner.x, *y + 12.0, inner.w * 0.42, Align::Start);
            text_centred(frame, &shown, value, palette.text, inner.x + inner.w * 0.42, *y + 12.0, inner.w * 0.58, Align::End);
            *y += 28.0;
        };
        let state = if a.current {
            p("使用中", "In use")
        } else if a.up {
            p("已连接", "Connected")
        } else {
            p("未连接", "Not connected")
        };
        line(p("状态", "Status"), state, &mut y);
        line(p("描述", "Description"), dash(a.description.clone()), &mut y);
        line(p("IPv4 地址", "IPv4 address"), dash(a.ipv4.clone()), &mut y);
        line(p("网关", "Gateway"), dash(a.gateway.clone()), &mut y);
        line(p("DNS 服务器", "DNS servers"), dash(a.dns.clone()), &mut y);
        line(p("物理地址", "Physical address"), dash(a.mac.clone()), &mut y);
        let link = net_rate(a.link_bps);
        line(p("链路速率", "Link speed"), link, &mut y);
        // --- The current network's running speed, from the live history ---
        y += 10.0;
        let (down, up) = net_series(120);
        let rate = |v: f64| {
            if v >= 1_000_000.0 {
                format!("{:.1} MB/s", v / 1_000_000.0)
            } else if v >= 1000.0 {
                format!("{:.0} KB/s", v / 1000.0)
            } else {
                format!("{:.0} B/s", v)
            }
        };
        text_centred(frame, &p("当前网络的上传与下载", "Upload and download now"), Font::new(Family::Segoe, 13.0, 600.0), palette.text, inner.x, y + 8.0, inner.w, Align::Start);
        let latest = |v: &[f64]| v.last().copied().unwrap_or(0.0);
        let figures = format!("↓ {}   ↑ {}", rate(latest(&down)), rate(latest(&up)));
        text_centred(frame, &figures, value, palette.text2, inner.x + inner.w * 0.45, y + 8.0, inner.w * 0.55, Align::End);
        y += 26.0;
        let box_h = (inner.y + inner.h - y - 26.0).clamp(0.0, 150.0);
        if box_h >= 30.0 && down.len() > 1 {
            let (cx, cy, cw) = (inner.x, y, inner.w);
            let most = down.iter().chain(up.iter()).fold(1024.0f64, |m, v| m.max(*v));
            let at = |v: &[f64]| -> Vec<Point> {
                let step = cw / v.len().saturating_sub(1).max(1) as f32;
                v.iter().enumerate().map(|(i, x)| Point { x: cx + i as f32 * step, y: cy + box_h - box_h * (*x / most).min(1.0) as f32 }).collect()
            };
            fill(frame, palette.rule, cx, cy + box_h, cw, 1.0, 0.5);
            let down_points = at(&down);
            let mut wash = down_points.clone();
            wash.push(Point { x: cx + cw, y: cy + box_h });
            wash.push(Point { x: cx, y: cy + box_h });
            frame.fill_shape(&wash, crate::ui::canvas::Fill::Solid(palette.selection.alpha(0.16)));
            frame.stroke(&down_points, palette.selection, 1.5);
            frame.stroke_dashed(&at(&up), palette.text2.alpha(0.8), 1.2);
            // Which line is which, under it.
            text_centred(frame, &p("— 下载   – – 上传", "— download   – – upload"), Font::new(Family::Segoe, 11.5, 400.0), palette.text3, cx, cy + box_h + 12.0, cw, Align::Start);
        }
        // The curve moves: drawn again each second.
        self.next_frame = Instant::now() + Duration::from_millis(1000);
    }

    /// Power's preview: the battery large and what it says now — its estimate left, its health —
    /// the charge of late running along the bottom from the live history, and the plan and the
    /// delays the left sets.
    fn paint_power_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let zh = lang == Lang::Zh;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 22.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let mut y = inner.y + 6.0;
        let live = battery_latest();
        match super::system_settings::battery() {
            Some((pct, charging)) => {
                // A battery drawn by hand: an outline, its knob, and the fill.
                let (bw, bh) = (92.0_f32, 44.0_f32);
                let (bx, by) = (inner.x, y);
                fill(frame, palette.control, bx, by, bw, bh, 8.0);
                Canvas::stroke_rounded(frame, crate::ui::canvas::Fill::Solid(palette.text3), bx, by, bw, bh, 8.0, 1.5);
                fill(frame, palette.text2, bx + bw + 2.0, by + bh / 2.0 - 6.0, 4.0, 12.0, 2.0);
                let pad = 4.0;
                let in_w = (bw - 2.0 * pad) * pct as f32 / 100.0;
                fill(frame, palette.selection, bx + pad, by + pad, in_w, bh - 2.0 * pad, 4.0);
                let big = format!("{pct}%");
                text_centred(frame, &big, Font::new(Family::SegoeDisplay, 30.0, 600.0), palette.text, bx + bw + 18.0, by + bh / 2.0 - 4.0, 160.0, Align::Start);
                let word = if charging { p("正在充电", "charging") } else { p("使用电池", "on battery") };
                text_centred(frame, &word, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, bx + bw + 18.0, by + bh / 2.0 + 22.0, 200.0, Align::Start);
                y = by + bh + 18.0;
                // What the live sample adds: the estimate left, the watts, and the health.
                let mut words: Vec<String> = Vec::new();
                if let Some(b) = &live {
                    if let Some(secs) = b.seconds_left {
                        let (h, m) = (secs / 3600, (secs % 3600) / 60);
                        let said = match (zh, h > 0) {
                            (true, true) => format!("{h} 小时 {m} 分"),
                            (true, false) => format!("{m} 分钟"),
                            (false, true) => format!("{h} h {m} min"),
                            (false, false) => format!("{m} min"),
                        };
                        words.push(format!("{} {said}", p("剩余", "Left")));
                    }
                    if let Some(w) = b.watts.map(|w| w.abs()) {
                        if w >= 0.05 {
                            words.push(format!("{:.1} W", w));
                        }
                    }
                }
                if let Some(health) = live.as_ref().and_then(|b| b.health).map(|h| h as u32).or(super::system_settings::battery_health()) {
                    words.push(format!("{} {health}%", p("健康度", "Health")));
                }
                if !words.is_empty() {
                    text_centred(frame, &words.join(" · "), Font::new(Family::Segoe, 12.5, 400.0), palette.text2, inner.x, y + 4.0, inner.w, Align::Start);
                    y += 24.0;
                }
            }
            None => {
                text_centred(frame, &p("本机没有电池，接交流电运行", "No battery — running on mains power"), Font::new(Family::Segoe, 14.0, 400.0), palette.text2, inner.x, y + 10.0, inner.w, Align::Start);
                y += 40.0;
            }
        }
        // The plan on now, and the two delays the left sets.
        let (plans, on) = super::system_settings::plan_choices(zh);
        if let Some(i) = on {
            if let Some(name) = plans.get(i) {
                text_centred(frame, &format!("{} · {}", p("电源方案", "Power plan"), name), Font::new(Family::Segoe, 13.5, 400.0), palette.text, inner.x, y + 8.0, inner.w, Align::Start);
                y += 26.0;
            }
        }
        let line = format!(
            "{} {} · {} {}",
            p("关屏", "Screen off"),
            super::system_settings::screen_timeout(),
            p("睡眠", "Sleep"),
            super::system_settings::sleep_timeout(),
        );
        text_centred(frame, &line, Font::new(Family::Segoe, 13.5, 400.0), palette.text2, inner.x, y + 8.0, inner.w, Align::Start);
        y += 34.0;
        // --- The charge of late, from the live history: one line, 0 to 100% ---
        if super::system_settings::battery().is_some() {
            let series = battery_series(140);
            if series.len() > 1 {
                text_centred(frame, &p("近期电量", "Charge of late"), Font::new(Family::Segoe, 13.0, 600.0), palette.text, inner.x, y + 6.0, inner.w, Align::Start);
                y += 24.0;
                let box_h = (inner.y + inner.h - y - 6.0).clamp(0.0, 130.0);
                if box_h >= 40.0 {
                    let (cx, cy, cw) = (inner.x, y, inner.w);
                    fill(frame, palette.rule, cx, cy + box_h, cw, 1.0, 0.5);
                    for share in [0.25, 0.5, 0.75] {
                        fill(frame, palette.rule.alpha(0.5), cx, cy + box_h - box_h * share, cw, 1.0, 0.5);
                    }
                    let step = cw / series.len().saturating_sub(1).max(1) as f32;
                    let points: Vec<Point> = series
                        .iter()
                        .enumerate()
                        .map(|(i, x)| Point { x: cx + i as f32 * step, y: cy + box_h - box_h * (x / 100.0).clamp(0.0, 1.0) as f32 })
                        .collect();
                    let mut wash = points.clone();
                    wash.push(Point { x: cx + cw, y: cy + box_h });
                    wash.push(Point { x: cx, y: cy + box_h });
                    frame.fill_shape(&wash, crate::ui::canvas::Fill::Solid(palette.selection.alpha(0.14)));
                    frame.stroke(&points, palette.selection, 1.5);
                }
            }
            // The charge creeps: drawn again each second.
            self.next_frame = Instant::now() + Duration::from_millis(1000);
        }
    }

    /// Screen time's preview: seven days of bars, today's busiest programs, and what is
    /// in front now.
    fn paint_screen_time_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let zh = lang == Lang::Zh;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let week = super::screen_time::week();
        // --- Seven bars ---
        let bars_h = (inner.h * 0.42).min(150.0);
        let top = inner.y + 10.0;
        let base = top + bars_h;
        let gap = 10.0;
        let bw = ((inner.w - gap * 6.0) / 7.0).max(6.0);
        let max = week.iter().map(|(_, s)| *s).max().unwrap_or(0).max(60);
        for (i, (day, secs)) in week.iter().enumerate() {
            let x = inner.x + i as f32 * (bw + gap);
            let h = bars_h * (*secs as f32 / max as f32);
            fill(frame, palette.control, x, base - bars_h, bw, bars_h, 4.0);
            fill(frame, if i + 1 == week.len() { palette.selection } else { palette.hover }, x, base - h, bw, h.max(2.0), 4.0);
            // The day of the month under each bar.
            let label = day.rsplit('-').next().unwrap_or("").to_string();
            text_centred(frame, &label, Font::new(Family::Segoe, 10.5, 400.0), palette.text3, x - 4.0, base + 12.0, bw + 8.0, Align::Start);
        }
        // --- Today's busiest programs ---
        let mut y = base + 30.0;
        text_centred(frame, &p("今日最常用", "Busiest today"), Font::new(Family::Segoe, 12.5, 600.0), palette.text3, inner.x, y, inner.w, Align::Start);
        y += 20.0;
        let today = week.last().map(|(d, _)| d.clone()).unwrap_or_default();
        let apps = super::screen_time::usage(&today);
        let app_max = apps.first().map(|(_, s)| *s).unwrap_or(1).max(1);
        for (name, secs) in apps.iter().take(5) {
            if y + 22.0 > inner.y + inner.h {
                break;
            }
            text_centred(frame, name, Font::new(Family::Segoe, 13.0, 400.0), palette.text, inner.x, y + 8.0, 150.0, Align::Start);
            let track = Rect { x: inner.x + 158.0, y, w: inner.w - 158.0 - 70.0, h: 12.0 };
            fill(frame, palette.control, track.x, track.y, track.w, track.h, 6.0);
            fill(frame, palette.selection, track.x, track.y, track.w * (*secs as f32 / app_max as f32), track.h, 6.0);
            let (h, m) = (*secs / 3600, (*secs % 3600) / 60);
            let dur = if zh { format!("{h}时{m:02}分") } else { format!("{h}h {m:02}m") };
            text_centred(frame, &dur, Font::new(Family::Segoe, 12.0, 400.0), palette.text2, inner.x + inner.w - 70.0, y + 6.0, 70.0, Align::End);
            y += 24.0;
        }
        if apps.is_empty() {
            text_centred(frame, &p("还没有记录", "Nothing kept yet"), Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, y + 6.0, inner.w, Align::Start);
        }
        // What is in front now.
        let fg = super::screen_time::foreground();
        if !fg.is_empty() {
            text_centred(frame, &format!("{} · {}", p("当前", "In front now"), fg), Font::new(Family::Segoe, 12.0, 400.0), palette.text3, inner.x, inner.y + inner.h - 6.0, inner.w, Align::Start);
        }
    }

    /// Accessibility's preview: a window as the desktop would wear it under the settings —
    /// the contrast's own colours, the words at the size slid, the pointer at the size
    /// drawn — so what the rows do is seen rather than read back.
    fn paint_accessibility_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 20.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        let contrast = super::system_settings::high_contrast();
        let scale = super::system_settings::text_scale().clamp(100, 225) as f32 / 100.0;
        let cursor = super::system_settings::cursor_size();
        // (High contrast wears its own face rather than the theme's: a black ground, white
        // words, and the amber Windows keeps for what may be followed.)
        let face = if contrast { Color::hex(0x000000, 1.0) } else { palette.window };
        let ink = if contrast { Color::hex(0xFFFFFF, 1.0) } else { palette.text };
        let faint = if contrast { Color::hex(0xFFFFFF, 0.82) } else { palette.text2 };
        let mark = if contrast { Color::hex(0xFFD700, 1.0) } else { palette.selection };
        let edge = if contrast { Color::hex(0xFFFFFF, 1.0) } else { palette.card_stroke };
        text_centred(frame, &p("桌面会穿上这样", "As the desktop would wear it"), Font::new(Family::Segoe, 15.0, 600.0), palette.text, inner.x, inner.y, inner.w, Align::Start);
        // The sample window, between the heading and the three readings under it.
        let win = Rect { x: inner.x, y: inner.y + 26.0, w: inner.w, h: (inner.h - 26.0 - 118.0).clamp(110.0, 240.0) };
        fill(frame, face, win.x, win.y, win.w, win.h, 8.0);
        stroke_inside(frame, win, 8.0, edge);
        let bar = 30.0;
        fill(frame, edge, win.x + 1.0, win.y + bar, win.w - 2.0, 1.0, 0.0);
        unsafe { frame.dc.PushAxisAlignedClip(&rect(win.x, win.y, win.w, win.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        text_centred(frame, &p("示例窗口", "Sample window"), Font::new(Family::Segoe, 13.0 * scale, 600.0), ink, win.x + 14.0, win.y + bar / 2.0, win.w - 28.0, Align::Start);
        let mut y = win.y + bar + 22.0;
        for line in [p("文字随这里的设置一同放大。", "Text grows with the size you slide here."), p("高对比度换上黑底、白字与粗线。", "High contrast takes a black ground, white words, firm lines.")] {
            text_centred(frame, &line, Font::new(Family::Segoe, 12.5 * scale, 400.0), faint, win.x + 14.0, y, win.w - 28.0, Align::Start);
            y += 20.0 * scale;
        }
        text_centred(frame, &p("了解详情", "Learn more"), Font::new(Family::Segoe, 12.0 * scale, 400.0), mark, win.x + 14.0, y + 4.0, win.w - 28.0, Align::Start);
        // The button, and the pointer beside it at the size the machine is set to show.
        let label = p("确定", "OK");
        let letter = Font::new(Family::Segoe, 12.5 * scale, 600.0);
        let bw = frame.gfx.measure(&label, letter) + 26.0;
        let bh = 28.0_f32.min(win.h * 0.3);
        let by = win.y + win.h - 16.0 - bh;
        fill(frame, mark, win.x + win.w - 16.0 - bw, by, bw, bh, 6.0);
        text_middle(frame, &label, letter, if contrast { Color::hex(0x000000, 1.0) } else { palette.card }, win.x + win.w - 16.0 - bw / 2.0, by + bh / 2.0);
        frame.icon(Icon::Pointer, Point { x: win.x + win.w - 16.0 - bw - 34.0, y: by + bh / 2.0 - 6.0 }, 14.0 + cursor as f32 * 2.2, ink);
        unsafe { frame.dc.PopAxisAlignedClip() };
        // The three settings as they stand, under the sample.
        let mut y = win.y + win.h + 20.0;
        let rows = [
            (p("高对比度", "High contrast"), if contrast { p("开", "On") } else { p("关", "Off") }),
            (p("文字大小", "Text size"), format!("{}%", (scale * 100.0).round() as u32)),
            (p("光标大小", "Pointer size"), format!("{cursor}")),
        ];
        for (i, (name, value)) in rows.iter().enumerate() {
            if i > 0 {
                fill(frame, palette.rule, inner.x, y, inner.w, 1.0, 0.0);
            }
            text_centred(frame, name, Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, y + 15.0, inner.w - 90.0, Align::Start);
            text_centred(frame, value, Font::new(Family::Segoe, 13.0, 600.0), palette.text, inner.x + inner.w - 90.0, y + 15.0, 90.0, Align::End);
            y += 30.0;
        }
    }

    /// Wallpaper's preview: a grid of the pictures Windows ships and the themes use; pressing
    /// one puts it on the desktop.
    fn paint_wallpaper_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let margin = 18.0;
        let inner = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin };
        text_centred(frame, &p("选择壁纸", "Choose a wallpaper"), Font::new(Family::Segoe, 15.0, 600.0), palette.text, inner.x, inner.y, inner.w, Align::Start);
        let listed = super::system_settings::wallpapers();
        let current = super::system_settings::current_wallpaper();
        let grid_top = inner.y + 30.0;
        let gap = 10.0;
        let name_h = 20.0;
        let card_w = 150.0_f32;
        let card_h = 84.0_f32;
        let cell_w = card_w + gap;
        let cell_h = card_h + name_h + gap;
        let cols = ((inner.w + gap) / cell_w).floor().max(1.0) as usize;
        let total_w = cols as f32 * cell_w - gap;
        let grid_x0 = inner.x + (inner.w - total_w) / 2.0;
        for (i, (name, path)) in listed.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            let (cx, cy) = (grid_x0 + col as f32 * cell_w, grid_top + row as f32 * cell_h);
            if cy + cell_h > inner.y + inner.h {
                break;
            }
            let radius = 6.0;
            fill(frame, palette.control, cx, cy, card_w, card_h, radius);
            if let Some(pic) = crate::station::thumbs::of(std::path::Path::new(path), (card_w * 2.0) as u32) {
                frame.picture(&pic, cx, cy, card_w, card_h, radius, 1.0);
            }
            let on = !current.is_empty() && current.eq_ignore_ascii_case(path);
            if on {
                let sel = Rect { x: cx - 1.5, y: cy - 1.5, w: card_w + 3.0, h: card_h + 3.0 };
                Canvas::stroke_rounded(frame, crate::ui::canvas::Fill::Solid(palette.selection), sel.x, sel.y, sel.w, sel.h, radius + 1.5, 2.5);
            } else {
                stroke_inside(frame, Rect { x: cx, y: cy, w: card_w, h: card_h }, radius, palette.card_stroke);
            }
            text_centred(frame, name, Font::new(Family::Segoe, 11.5, if on { 600.0 } else { 400.0 }), if on { palette.text } else { palette.text2 }, cx, cy + card_h + name_h / 2.0 + 2.0, card_w, Align::Start);
            self.targets.push((Rect { x: cx, y: cy, w: card_w, h: card_h + name_h }, Target::SetWallpaper(path.clone())));
        }
        if listed.is_empty() {
            text_centred(frame, &p("未找到墙纸文件", "No wallpaper files found"), Font::new(Family::Segoe, 13.0, 400.0), palette.text2, inner.x, grid_top + 10.0, inner.w, Align::Start);
        }
    }

    /// The desktop and taskbar page's preview: a screen as Windows 11 wears it, the
    /// taskbar along its chosen edge with the start, a row of icons, a tray and the
    /// clock — centred or to the corner, grouped or apart, at the chosen size, and
    /// reduced to a thin strip when it hides.
    fn paint_desktop_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);

        use super::system_settings as sys;
        let position = sys::taskbar_position(); // 0 left, 1 top, 2 right, 3 bottom
        let size = sys::taskbar_size(); // 0 small, 1 medium, 2 large
        let combine = sys::combine_icons(); // 0 always, 1 when full, 2 never
        let autohide = sys::taskbar_autohide();
        let align = sys::taskbar_align(); // 0 to the corner, 1 in the middle
        let vertical = matches!(position, 0 | 2);

        // The screen, kept near 16:10 and centred, with room at the bottom for the caption.
        let (margin, caption) = (22.0, 26.0);
        let mut screen = Rect { x: area.x + margin, y: area.y + margin, w: area.w - 2.0 * margin, h: area.h - 2.0 * margin - caption };
        let want = 16.0 / 10.0;
        if screen.w / screen.h.max(1.0) > want {
            let w = screen.h * want;
            screen.x += (screen.w - w) / 2.0;
            screen.w = w;
        } else {
            let h = screen.w / want;
            screen.y += (screen.h - h) / 2.0;
            screen.h = h;
        }
        if screen.w <= 0.0 || screen.h <= 0.0 {
            return;
        }

        // The desktop: a plain surface washed with a soft accent glow up top.
        fill(frame, palette.window, screen.x, screen.y, screen.w, screen.h, 6.0);
        fill(frame, palette.selection.alpha(0.16), screen.x, screen.y, screen.w, screen.h * 0.6, 6.0);
        stroke_inside(frame, screen, 6.0, palette.card_stroke);

        // The bar's thickness follows the chosen size; its icons fill most of it.
        let bar = if autohide { 4.0 } else { screen.h * match size { 0 => 0.062, 2 => 0.1, _ => 0.08 } };
        let icon = bar * 0.52;
        let desk = match position {
            1 => Rect { x: screen.x, y: screen.y + bar, w: screen.w, h: screen.h - bar },
            0 => Rect { x: screen.x + bar, y: screen.y, w: screen.w - bar, h: screen.h },
            2 => Rect { x: screen.x, y: screen.y, w: screen.w - bar, h: screen.h },
            _ => Rect { x: screen.x, y: screen.y, w: screen.w, h: screen.h - bar },
        };
        // Two idle windows resting on the desktop.
        let pad = (desk.w.min(desk.h) * 0.08).max(6.0);
        let half = (desk.w - 3.0 * pad) / 2.0;
        let tall = desk.h - 2.0 * pad;
        for (i, at) in [desk.x + pad, desk.x + 2.0 * pad + half].into_iter().enumerate() {
            let top = desk.y + pad + if i == 0 { 0.0 } else { tall * 0.12 };
            let h = (tall - if i == 0 { 0.0 } else { tall * 0.12 }).min(tall);
            fill(frame, palette.control, at, top, half, h, 4.0);
            stroke_inside(frame, Rect { x: at, y: top, w: half, h }, 4.0, palette.card_stroke);
            fill(frame, palette.selection.alpha(0.5), at + 6.0, top + 6.0, half * 0.4, 4.0, 2.0);
        }

        // The bar: an acrylic sheet along its edge, a hairline toward the desktop.
        let tb = match position {
            1 => Rect { x: screen.x, y: screen.y, w: screen.w, h: bar },
            0 => Rect { x: screen.x, y: screen.y, w: bar, h: screen.h },
            2 => Rect { x: screen.x + screen.w - bar, y: screen.y, w: bar, h: screen.h },
            _ => Rect { x: screen.x, y: screen.y + screen.h - bar, w: screen.w, h: bar },
        };
        fill(frame, mix(palette.window, palette.card, 0.45).alpha(0.92), tb.x, tb.y, tb.w, tb.h, if autohide { 0.0 } else { 4.0 });
        stroke_inside(frame, tb, if autohide { 0.0 } else { 4.0 }, palette.card_stroke);

        if !autohide {
            // The clock the machine keeps, and the apps resting on the bar: the explorer's
            // folder, a browser, a player, two others; all but the first are running.
            let clock = sys::wall_clock();
            let mut bits = clock.split_whitespace();
            let day: String = bits.next().unwrap_or("").replace('-', "/");
            let hhmm: String = bits.next().unwrap_or("").chars().take(5).collect();
            let apps: [(u32, bool); 5] = [(0xE3B04B, false), (0x3E8FD0, true), (0x8F6CD0, true), (0x53B582, true), (0xD06060, true)];
            // The start's four panes, in the accent, as the logo Windows wears.
            let logo = |frame: &Frame, cx: f32, cy: f32, q: f32, ink: Color| {
                let (side, gap) = (q * 0.44, q * 0.12);
                for (dx, dy) in [(0.0f32, 0.0f32), (side + gap, 0.0), (0.0, side + gap), (side + gap, side + gap)] {
                    fill(frame, ink, cx - q / 2.0 + dx, cy - q / 2.0 + dy, side, side, 1.0);
                }
            };
            let gap = icon * 0.5;
            if !vertical {
                let cy = tb.y + tb.h / 2.0;
                let bw = if combine == 2 { icon * 2.4 } else { icon };
                let group_w = (icon + gap) * 2.0 + bw * apps.len() as f32 + gap * (apps.len() as f32 - 1.0);
                // The tray, at the far end: the clock in two lines, its glyphs before it.
                let cfont = Font::new(Family::Segoe, (bar * 0.25).max(6.0), 400.0);
                let dfone = Font::new(Family::Segoe, (bar * 0.21).max(5.5), 400.0);
                let tw = frame.gfx.measure(&hhmm, cfont).max(frame.gfx.measure(&day, dfone));
                let tray_w = tw + bar * 2.4 + 10.0;
                let mut gx = if align == 1 { tb.x + (tb.w - tray_w) / 2.0 - group_w / 2.0 } else { tb.x + bar * 0.22 };
                gx = gx.max(tb.x + 3.0).min(tb.x + tb.w - tray_w - group_w - 3.0).max(tb.x + 3.0);
                logo(frame, gx + icon / 2.0, cy, icon * 0.72, palette.selection);
                // A search glass: a ring and its handle.
                let sx = gx + icon + gap + icon / 2.0;
                Canvas::fill_circle(frame, palette.text2, Point { x: sx, y: cy - 1.0 }, icon * 0.24);
                Canvas::fill_circle(frame, mix(palette.window, palette.card, 0.45), Point { x: sx, y: cy - 1.0 }, icon * 0.14);
                fill(frame, palette.text2, sx + icon * 0.16, cy + icon * 0.12, icon * 0.2, 1.5, 0.8);
                let mut ax = gx + (icon + gap) * 2.0;
                for (i, (hue, running)) in apps.into_iter().enumerate() {
                    let ink = Color::hex(hue, 1.0);
                    let (bx, s) = (ax + (bw - icon) / 2.0, icon);
                    // The explorer as a folder, the browser as a circle, the rest as plates.
                    if i == 0 {
                        fill(frame, ink, ax + (bw - s) / 2.0, cy - s * 0.3, s, s * 0.66, 1.5);
                        fill(frame, ink.alpha(0.85), ax + (bw - s) / 2.0, cy - s * 0.48, s * 0.45, s * 0.22, 1.0);
                    } else if i == 1 {
                        Canvas::fill_circle(frame, ink, Point { x: bx + s / 2.0, y: cy }, s * 0.5);
                    } else {
                        fill(frame, ink, bx, cy - s / 2.0, s, s, s * 0.22);
                        fill(frame, Color::hex(0xFFFFFF, 0.85), bx + s * 0.3, cy + s * 0.16, s * 0.4, 1.5, 0.8);
                    }
                    if combine == 2 && i > 0 {
                        // Buttons not combined wear a hairline between them.
                        fill(frame, palette.text3.alpha(0.5), ax - gap / 2.0, cy - s * 0.55, 1.0, s * 1.1, 0.0);
                    }
                    if running {
                        // The running mark: a pill of the accent under its button.
                        let pw = if combine == 2 { bw * 0.66 } else { icon * 0.55 };
                        fill(frame, palette.selection, ax + (bw - pw) / 2.0, tb.y + tb.h - 4.0, pw, 2.0, 1.0);
                    }
                    ax += bw + gap;
                }
                // The tray's glyphs, then the clock to the corner.
                let mut tx = tb.x + tb.w - 8.0 - tw;
                text_middle(frame, &hhmm, cfont, palette.text, tx + tw / 2.0, cy - bar * 0.13);
                text_middle(frame, &day, dfone, palette.text, tx + tw / 2.0, cy + bar * 0.16);
                tx -= bar * 0.9;
                // The battery: a shell, its charge, and a nub.
                let (bwh, bhh) = (bar * 0.46, bar * 0.26);
                stroke_inside(frame, Rect { x: tx - bwh, y: cy - bhh / 2.0, w: bwh, h: bhh }, 2.0, palette.text2);
                fill(frame, palette.text2, tx - bwh + 1.5, cy - bhh / 2.0 + 1.5, (bwh - 3.0) * 0.62, bhh - 3.0, 1.0);
                fill(frame, palette.text2, tx + 0.5, cy - 1.0, 1.5, 2.0, 0.8);
                tx -= bar * 0.7;
                // The speaker: a body and a wave.
                fill(frame, palette.text2, tx - bar * 0.2, cy - bar * 0.1, bar * 0.14, bar * 0.2, 1.0);
                Canvas::fill_circle(frame, palette.text2, Point { x: tx - bar * 0.05, y: cy }, bar * 0.09);
                tx -= bar * 0.7;
                // The signal: three bars, taller as they go.
                for (i, h) in [0.22, 0.36, 0.5].into_iter().enumerate() {
                    fill(frame, palette.text2, tx - (3.0 - i as f32) * (bar * 0.12), cy + bar * 0.25 - bar * h, bar * 0.08, bar * h, 1.0);
                }
            } else {
                // A bar stood on end: the start at its head, the tray at its foot, the
                // icons in a file between, kept together or centred as the bar is told.
                let cx = tb.x + tb.w / 2.0;
                let tray_h = bar * 2.6;
                let group_h = (icon + gap) * 2.0 + (icon + gap) * apps.len() as f32;
                let mut ay = if align == 1 { tb.y + (tb.h - tray_h) / 2.0 - group_h / 2.0 } else { tb.y + bar * 0.25 };
                ay = ay.max(tb.y + 3.0).min((tb.y + tb.h - tray_h - group_h - 3.0).max(tb.y + 3.0));
                logo(frame, cx, ay + icon / 2.0, icon * 0.72, palette.selection);
                ay += icon + gap;
                let sx = cx;
                Canvas::fill_circle(frame, palette.text2, Point { x: sx, y: ay + icon / 2.0 - 1.0 }, icon * 0.24);
                Canvas::fill_circle(frame, mix(palette.window, palette.card, 0.45), Point { x: sx, y: ay + icon / 2.0 - 1.0 }, icon * 0.14);
                ay += icon + gap;
                let mark = if position == 0 { tb.x + tb.w - 3.0 } else { tb.x + 1.0 };
                for (_i, (hue, running)) in apps.into_iter().enumerate() {
                    let ink = Color::hex(hue, 1.0);
                    let s = icon;
                    fill(frame, ink, cx - s / 2.0, ay, s, s, s * 0.22);
                    if running {
                        // The running mark stood on end against the desktop's side of the bar.
                        fill(frame, palette.selection, mark, ay + (s - icon * 0.55) / 2.0, 2.0, icon * 0.55, 1.0);
                    }
                    ay += s + gap;
                }
                // The tray at the foot: the clock in two lines under a battery dot.
                let cfont = Font::new(Family::Segoe, (bar * 0.24).max(5.5), 400.0);
                let dfone = Font::new(Family::Segoe, (bar * 0.2).max(5.0), 400.0);
                let foot = tb.y + tb.h - 6.0;
                text_middle(frame, &hhmm, cfont, palette.text, cx, foot - bar * 0.3);
                text_middle(frame, &day, dfone, palette.text, cx, foot - bar * 0.04);
                fill(frame, palette.text2, cx - bar * 0.2, foot - bar * 1.05, bar * 0.4, bar * 0.2, 1.5);
            }
        }

        // The caption: what the bar is set to, spelled out.
        let edge = match position {
            1 => p("顶部", "top"),
            0 => p("左侧", "left"),
            2 => p("右侧", "right"),
            _ => p("底部", "bottom"),
        };
        let sizing = match size {
            0 => p("小号", "small"),
            2 => p("大号", "large"),
            _ => p("标准", "medium"),
        };
        let alignment = if align == 0 { p("图标靠左", "icons to the left") } else { p("图标居中", "icons centred") };
        let hiding = if autohide { p(" · 自动隐藏", " · hidden until reached") } else { String::new() };
        let line = if lang == Lang::Zh {
            format!("任务栏居{edge} · {sizing} · {alignment}{hiding}")
        } else {
            format!("The taskbar sits {edge}, {sizing}, {alignment}{hiding}")
        };
        text_centred(frame, &line, Font::new(Family::Segoe, 12.0, 400.0), palette.text2, area.x + margin, screen.y + screen.h + 8.0, area.w - 2.0 * margin, Align::Start);
    }

    /// The dock page's preview: the desktop the stage shows, with the dock standing on the edge it is told to — its
    /// own tiles, at the size, spacing, glass and hiding chosen, drawn by the same hand that draws the dock itself.
    fn paint_dock_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        let dock = self.settings.dock.clone();
        // (Room for the caption under the screen, which the stage is centred in the space left.)
        let caption = 24.0;
        let stage = Rect { x: area.x, y: area.y, w: area.w, h: (area.h - caption).max(0.0) };
        if stage.w <= 0.0 || stage.h <= 0.0 {
            return;
        }
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        let (_, screen) = self.panel_on_stage(frame, stage, None, true, 1.0);
        stroke_inside(frame, screen, 8.0, palette.card_stroke);
        if screen.w <= 0.0 || self.stage.size.0 <= 0.0 || !dock.on {
            return;
        }
        // One stage DIP drawn as this many of the window's: the dock is scaled by the same, so it keeps the size it
        // has against the screen it stands on rather than being drawn large on a small one.
        let k = screen.w / self.stage.size.0;
        let (size, spacing) = dock.measures();
        let side = (size * k * self.scale).round().max(0.0) as u32;
        // Its tiles as the dock makes them: what is kept, Glance's doors, the Bin, and the room between.
        let mut entries: Vec<super::dock::Entry> = Vec::new();
        for item in &dock.items {
            let (act, name, picture) = match item {
                crate::settings::DockItem::Keep { target, name } => (super::dock::Act::Launch(target.clone()), name.clone(), crate::dock::pictured(target, side.clamp(16, 256))),
                crate::settings::DockItem::Door { door } => (super::dock::Act::Door(*door), super::dock::door_says(*door, lang).to_string(), None),
                crate::settings::DockItem::Trash => (super::dock::Act::Trash, super::dock::trash_says(lang).to_string(), None),
                crate::settings::DockItem::Gap => (super::dock::Act::Gap, String::new(), None),
            };
            entries.push(super::dock::Entry { act, name, picture, running: false });
        }
        let row = super::dock::row(&entries, None, size, spacing, dock.peak());
        let glass = super::overlay::Glass { presence: 1.0 - dock.clear.clamp(0.0, 1.0), ..super::overlay::glass(&mut [], None).0 };
        // Where its glass stands on the stage, as `Dock::glass_at` puts it: inset from the edge it is on, slid along
        // that edge as a drag left it; hidden, it is pushed back out of the edge till only its tiles are left.
        let inset = super::dock::INSET;
        let at_top = dock.edge == DockEdge::Top;
        let room = (self.stage.size.0 - 2.0 * inset - row.length).max(0.0);
        let x = inset + room * dock.along.clamp(0.0, 1.0);
        let y = if at_top { inset } else { self.stage.size.1 - inset - row.thickness };
        let away = if dock.auto_hide {
            (inset + row.thickness + super::dock::overhang(size, dock.peak()).1) * 0.66
        } else {
            0.0
        };
        let y = if at_top { y - away } else { y + away };
        // Drawn in the dock's own DIPs, scaled onto the screen and clipped by it: what is pushed past an edge goes
        // out of the screen there, as it does on the real one.
        unsafe { frame.dc.PushAxisAlignedClip(&rect(screen.x, screen.y, screen.w, screen.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let mut parent = Matrix3x2::default();
        unsafe { frame.dc.GetTransform(&mut parent) };
        let placed = Matrix3x2::translation(x, y) * Matrix3x2::scale(k, k) * Matrix3x2::translation(screen.x, screen.y) * parent;
        unsafe {
            frame.dc.SetTransform(&placed);
            super::dock::shadow(frame, &row, glass, false);
            super::dock::paint(frame, &entries, &row, glass, size, at_top, None, false, self.scale * k);
            frame.dc.SetTransform(&parent);
            frame.dc.PopAxisAlignedClip();
        }
        // The caption: what the dock is set to, spelled out.
        let edge = match dock.edge {
            DockEdge::Top => p("屏幕顶部", "along the top"),
            DockEdge::Bottom => p("屏幕底部", "along the bottom"),
        };
        let swell = if dock.magnify {
            format!("{} {}×", p("悬停放大到", "swells to"), dock.peak())
        } else {
            p("不悬停放大", "no swell")
        };
        let mut bits = vec![edge, p("图标", "icons") + &format!(" {}", size.round() as i32), p("间距", "spacing") + &format!(" {}", spacing.round() as i32), swell];
        if dock.clear > 0.0 {
            bits.push(format!("{} {:.0}%", p("玻璃透明", "glass clear"), dock.clear * 100.0));
        }
        if dock.auto_hide {
            bits.push(p("自动隐藏", "hidden until reached"));
        }
        let line = bits.join(" · ");
        text_centred(frame, &line, Font::new(Family::Segoe, 12.0, 400.0), palette.text2, area.x + 18.0, screen.y + screen.h + 6.0, area.w - 36.0, Align::Start);
    }

    /// The launchpad page's preview: the desktop the stage shows washed as the launchpad washes it, and its tiles laid
    /// in the grid the settings ask for — drawn by the same hand that draws the launchpad, on the programs the shell
    /// has been found to list (see `super::launchpad`).
    fn paint_launchpad_preview(&mut self, frame: &Frame, palette: &Palette, area: Rect) {
        let lang = self.lang;
        let p = |zh, en| pick(lang, zh, en).to_string();
        // (Room for the caption under the screen, which the stage is centred in the space left.)
        let caption = 24.0;
        let stage = Rect { x: area.x, y: area.y, w: area.w, h: (area.h - caption).max(0.0) };
        if stage.w <= 0.0 || stage.h <= 0.0 {
            return;
        }
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        let (_, screen) = self.panel_on_stage(frame, stage, None, true, 1.0);
        stroke_inside(frame, screen, 8.0, palette.card_stroke);
        if screen.w <= 0.0 || self.stage.size.0 <= 0.0 {
            return;
        }
        // One stage DIP drawn as this many of the window's: the page is laid in the stage's own DIPs and scaled onto
        // the screen it stands on, so its tiles keep the room they have against a screen of their own.
        let k = screen.w / self.stage.size.0;
        let (wide, tall) = self.stage.size;
        // The programs the Start menu lists, read on a thread of their own: asked for while they are still coming,
        // and the page drawn again when they land.
        let Some(all) = crate::station::found::all() else {
            crate::station::found::look(|| {});
            self.next_frame = Instant::now() + Duration::from_millis(1500);
            return;
        };
        let dock = self.settings.dock.clone();
        let stood = |target: &str| {
            dock.items
                .iter()
                .any(|one| matches!(one, crate::settings::DockItem::Keep { target: here, .. } if crate::station::apps::same(here, target)))
        };
        let grid = super::launchpad::Grid::asked(wide, tall, all.len(), dock.pad_cols, dock.pad_rows);
        let side = (grid.side * k * self.scale).round().max(0.0) as u32;
        // The first page of them, as the launchpad makes its tiles: the shell's picture where it has given one, the
        // name's first letter till it does — which is asked for again a moment later.
        let mut tiles: Vec<super::launchpad::Tile> = Vec::new();
        let mut waiting = false;
        for one in all.iter().take(grid.per.min(all.len())) {
            let picture = crate::dock::plated(&one.target, side.clamp(16, 256));
            waiting |= picture.is_none();
            tiles.push(super::launchpad::Tile {
                name: one.name.clone(),
                target: one.target.clone(),
                picture,
                kept: stood(&one.target),
                group: Vec::new(),
            });
        }
        if waiting {
            self.next_frame = Instant::now() + Duration::from_millis(400);
        }
        // Drawn inside the screen: washed over it, its search's place at the head, its page of tiles, the pages' dots
        // at their feet — what is pushed past a screen edge goes out of it there, as it does on the real one.
        unsafe { frame.dc.PushAxisAlignedClip(&rect(screen.x, screen.y, screen.w, screen.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let mut parent = Matrix3x2::default();
        unsafe { frame.dc.GetTransform(&mut parent) };
        let placed = Matrix3x2::scale(k, k) * Matrix3x2::translation(screen.x, screen.y) * parent;
        unsafe { frame.dc.SetTransform(&placed) };
        let wash = if self.dark {
            Color::hex(0x0A0D12, 0.72)
        } else {
            Color::hex(0xEAEFF6, 0.72)
        };
        frame.fill(wash, 0.0, 0.0, wide, tall);
        // (The search's box: only its place shown, since nothing is written here.)
        let (bx, by, bw, bh) = super::launchpad::field(wide);
        Canvas::fill_rounded(frame, Color::hex(if self.dark { 0xFFFFFF } else { 0x1C1C1C }, 0.1), bx, by, bw, bh, bh / 2.0);
        super::launchpad::paint(frame, &tiles, &grid, 0, tiles.len(), super::launchpad::Hand { come: 1.0, ..Default::default() }, self.dark);
        super::launchpad::dots(frame, grid.pages(all.len()), 0, wide, super::launchpad::feet(tall), self.dark);
        unsafe {
            frame.dc.SetTransform(&parent);
            frame.dc.PopAxisAlignedClip();
        }
        // The caption: how the page lies, and how many pages the programs make in it.
        let lying = if dock.pad_cols == 0 || dock.pad_rows == 0 {
            p("按屏幕自定", "laid for the screen")
        } else {
            format!("{} × {}", grid.cols, grid.rows)
        };
        let pages = grid.pages(all.len());
        let line = if pages > 1 {
            format!("{} {} · {} · {} {}", p("每页", "a page"), lying, p("共", "of"), pages, p("页", "pages"))
        } else {
            format!("{} {}", p("一页排满", "one page holds"), lying)
        };
        text_centred(frame, &line, Font::new(Family::Segoe, 12.0, 400.0), palette.text2, area.x + 18.0, screen.y + screen.h + 6.0, area.w - 36.0, Align::Start);
    }

    /// How the stage is shown in `area` with a panel `pw` wide (stage DIPs) slid out from `edge`: how large a stage
    /// DIP is drawn, and where the stage's corner is in the area. The whole screen (`whole`), else the whole height of
    /// it with the whole panel and a strip of desktop beside it (along the top, either side of it). In the middle of
    /// the area, what of it is left plain (the preview's frame is the whole area: see `framed`).
    fn staged(&self, area: Rect, edge: Edge, pw: f32, whole: bool) -> (f32, f32, f32) {
        let (sw, sh) = self.stage.size;
        let fit = (area.w / sw).min(area.h / sh).min(1.0);
        let middle = |room: f32, length: f32| ((room - length) / 2.0).max(0.0);
        match (whole, edge) {
            (true, _) => (fit, (area.w - sw * fit) / 2.0, (area.h - sh * fit) / 2.0),
            (false, Edge::Left | Edge::Right) => {
                let k = (area.h / sh).min(area.w / (pw + PREVIEW_MARGIN).min(sw)).min(1.0);
                // A screen that fits whole, in the middle; one that does not, its side the panel is on.
                let x = if sw * k <= area.w { middle(area.w, sw * k) } else if edge == Edge::Left { 0.0 } else { area.w - sw * k };
                (k, x, middle(area.h, sh * k))
            }
            // The panel in the middle of the top, with as much desktop either side of it as the sides' previews have
            // beside theirs, and the whole height of the screen under it. (Its whole width shown, a wide screen made
            // the panel small.)
            (false, Edge::Top) => {
                let k = (area.h / sh).min(area.w / (pw + 2.0 * PREVIEW_MARGIN).min(sw)).min(1.0);
                (k, (area.w - sw * k) / 2.0, middle(area.h, sh * k))
            }
        }
    }

    /// The preview's frame, the whole of `area`: plain where the screen drawn in it leaves room (see `staged`).
    fn framed(&self, frame: &Frame, area: Rect) {
        fill(frame, self.palette.card, area.x, area.y, area.w, area.h, 8.0);
    }

    /// Where the stage's whole screen is drawn in `area` (as `panel_on_stage` draws it whole).
    fn whole_stage(&self, area: Rect) -> Rect {
        let (sw, sh) = self.stage.size;
        let (k, x, y) = self.staged(area, Edge::Left, 0.0, true);
        Rect { x: area.x + x, y: area.y + y, w: sw * k, h: sh * k }
    }

    /// What of the stage shows in `area`, drawn `k` DIPs to one of its own with its corner at `(x, y)` in the area
    /// (as `staged` places it): what is drawn of it is clipped to that, in the preview's frame (see `framed`).
    fn shown_of(&self, area: Rect, k: f32, x: f32, y: f32) -> Rect {
        let (sw, sh) = self.stage.size;
        let (left, top) = (area.x + x.max(0.0), area.y + y.max(0.0));
        let (right, bottom) = ((area.x + x + sw * k).min(area.x + area.w), (area.y + y + sh * k).min(area.y + area.h));
        Rect { x: left, y: top, w: right - left, h: bottom - top }
    }

    /// The stage's desktop in `area` and, where `panel` is one, that part's panel showing its prefs slid out from its edge
    /// (as large, and in as many columns, as that part's are, whichever page shows it; as far as `out`: 1 all the way, 0
    /// just past the edge), running live: the whole screen (`whole`), else the whole height of it
    /// with the whole panel and a strip of desktop beside it. Returns how fast the panel's charts move (physical pixels
    /// a second), and where the screen was drawn.
    fn panel_on_stage(&mut self, frame: &Frame, area: Rect, panel: Option<(Edge, &Prefs, Part)>, whole: bool, out: f32) -> (Option<f32>, Rect) {
        let staged = self.stage_panel(frame, area, panel, Staging { whole, out, ..Staging::default() });
        (staged.plot, staged.screen)
    }

    /// The stage's desktop in `area` and on it the panel `panel` (its edge, what it shows, what it is), as `staging` has
    /// it (see `Staging`): as `panel_on_stage` draws it, or as large as the guide's plays make it, or put down on the
    /// screen as a widget pulled off it, whole or one lane of it. What was drawn, where (see `Staged`).
    pub(super) fn stage_panel(&mut self, frame: &Frame, area: Rect, panel: Option<(Edge, &Prefs, Part)>, staging: Staging) -> Staged {
        let Staging { whole, out, size, at, lane: only, opacity, desktop, draw, shown: staged_as } = staging;
        let lang = self.lang;
        let (sw, sh) = self.stage.size;
        let fit = (area.w / sw).min(area.h / sh).min(1.0);
        let Some((edge, prefs, part)) = panel else {
            let (ox, oy) = (area.x + (area.w - sw * fit) / 2.0, area.y + (area.h - sh * fit) / 2.0);
            let screen = Rect { x: ox, y: oy, w: sw * fit, h: sh * fit };
            if draw && desktop {
                let (clip, radius) = screen_shown(screen);
                self.stage_picture(frame, clip, radius, (ox, oy), fit, 1.0);
            }
            return Staged { plot: None, screen, panel: None, lanes: Vec::new(), bar: None, head: 0.0 };
        };
        let app = crate::app();
        let skin = Skin::named(&self.settings.skin);
        let interval = self.settings.interval_ms as f64;
        // Held while the preview is drawn; the sampler waits that long.
        let history = app.controller.history.read().unwrap();
        let samples = crate::panel::contiguous(&history);
        if samples.is_empty() {
            if !whole && draw && desktop {
                fill(frame, self.palette.card, area.x, area.y, area.w, area.h, 8.0);
                stroke_inside(frame, area, 8.0, self.palette.card_stroke);
            }
            return Staged { plot: None, screen: Rect { x: area.x, y: area.y, w: area.w, h: area.h }, panel: None, lanes: Vec::new(), bar: None, head: 0.0 };
        }
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0;
        let pen = wall - interval - PEN_LAG_MS;
        let measure = Theme::new(skin, false);
        // The preview shows what the panel would hold if it opened now.
        let seen = app.controller.seen.lock().unwrap().clone();
        let probe = Scene { music_apart: false, hint: None, thumbs: &crate::ui::view::RESTING, info: &app.info(), prefs, theme: &measure, lang, history: samples, seen: &seen, pen_ms: pen, scrolls: crate::ui::view::Scrolls::default(), tab: crate::ui::view::Tab::Shelf, hover: None, pinned: false, overlay: crate::app().controller.overlay_wanted(), buttons: true, newer: update::available().is_some(), settings_lit: None };
        let lanes = view::lanes(&probe);
        let heights: Vec<f32> = lanes.iter().map(|lane| lane.height(&measure)).collect();
        // (Quick launch as its rail, as the panel lays it out.)
        let rail = lanes.iter().position(|lane| lane.id() == "apps");
        let columns = self.settings.columns_of(part);
        // What it is (another drawn in its place is not turned into: see `render::Turn::follow`).
        let what = {
            let mut key = DefaultHasher::new();
            (part, edge, whole, size.is_some(), at.is_some(), only).hash(&mut key);
            key.finish()
        };
        // No larger than the screen holds it, as the panel is made (see `Panel::dragged`).
        let size = size.unwrap_or_else(|| self.settings.panel_size_on(part, self.stage.screen.as_deref())).clamp(SIZES.0, SIZES.1);
        let size = size.min(arrange::largest(&measure, edge, &heights, (sw, sh), columns, size, rail).max(SIZES.0));
        let (layout, zoom) = arrange::arrange(&measure, edge, heights, (sw, sh), columns, size, rail);
        let (pw, ph) = (layout.width() * zoom, layout.height() * zoom);
        let along = |at: f32, length: f32, extent: f32| (at - length / 2.0).min(extent - GAP - length).max(GAP);
        // (Slid in, as far past the edge as its shadow reaches.)
        let away = 1.0 - out;
        let boxes = layout.lanes();
        // Its corner on the screen, laid out as `layout` at `zoom`: against its edge, or where it is put down (where
        // one lane of it is, that lane's).
        let corner = |layout: &view::Layout, zoom: f32| {
            let (pw, ph, inset) = (layout.width() * zoom, layout.height() * zoom, measure.inset * zoom);
            match (at, only.and_then(|lane| layout.lanes().get(lane).copied())) {
                (Some(at), Some(lane)) => (at.0 - lane.x * zoom, at.1 - lane.y * zoom),
                (Some(at), None) => at,
                (None, _) => match edge {
                    Edge::Left => (inset - away * (pw + inset + GAP), along(sh / 2.0, ph, sh)),
                    Edge::Right => (sw - inset - pw + away * (pw + inset + GAP), along(sh / 2.0, ph, sh)),
                    Edge::Top => (along(sw / 2.0, pw, sw), inset - away * (ph + inset + GAP)),
                },
            }
        };
        let pos = corner(&layout, zoom);
        let (k, stage_x, stage_y) = self.staged(area, edge, pw, whole);
        let (ox, oy) = (area.x + stage_x, area.y + stage_y);
        let shown = |x: f32, y: f32, w: f32, h: f32| Rect { x: ox + x * k, y: oy + y * k, w: w * k, h: h * k };
        let screen = Rect { x: ox, y: oy, w: sw * k, h: sh * k };
        let staged = |plot| Staged {
            plot,
            screen,
            panel: Some(shown(pos.0, pos.1, pw, ph)),
            lanes: boxes.iter().map(|lane| shown(pos.0 + lane.x * zoom, pos.1 + lane.y * zoom, lane.w * zoom, lane.h * zoom)).collect(),
            bar: Some(layout.bar()).map(|bar| shown(pos.0 + bar.x * zoom, pos.1 + bar.y * zoom, bar.w * zoom, bar.h * zoom)),
            head: (measure.pad_top + view::HEAD_BAND) * zoom * k,
        };
        if !draw {
            return staged(None);
        }
        // The desktop behind the panel decides its theme when it follows the
        // backdrop, and how frosted glass is.
        let behind = RECT { left: pos.0 as i32, top: pos.1 as i32, right: (pos.0 + pw) as i32, bottom: (pos.1 + ph) as i32 };
        let (theme, frost) = self.stage.look(skin, prefs.theme, behind);
        let scene = Scene { music_apart: false, hint: None, thumbs: &crate::ui::view::RESTING, info: &app.info(), prefs, theme: &theme, lang, history: samples, seen: &seen, pen_ms: pen, scrolls: crate::ui::view::Scrolls::default(), tab: crate::ui::view::Tab::Shelf, hover: None, pinned: false, overlay: crate::app().controller.overlay_wanted(), buttons: true, newer: update::available().is_some(), settings_lit: None };
        let lanes = view::lanes(&scene);

        let framed = area;
        // The whole screen inside its own rounded corners, a strip of it inside the preview's frame's.
        let (area, radius) = if whole { screen_shown(screen) } else { (self.shown_of(area, k, stage_x, stage_y), 8.0) };
        if !whole && desktop {
            self.framed(frame, framed);
        }
        // Clipped to the screen, and where one lane of it is drawn, to that lane inside its corners; as opaque as `opacity`.
        let lane = only.and_then(|lane| boxes.get(lane)).map(|lane| shown(pos.0 + lane.x * zoom, pos.1 + lane.y * zoom, lane.w * zoom, lane.h * zoom));
        let clips: Vec<(Rect, f32)> = std::iter::once((area, radius)).chain(lane.map(|lane| (lane, theme.radius * zoom * k))).collect();
        let last = clips.len() - 1;
        let layers: Vec<D2D1_LAYER_PARAMETERS1> = clips
            .iter()
            .enumerate()
            .map(|(at, (clip, radius))| {
                let mask = unsafe { frame.gfx.factory.CreateRoundedRectangleGeometry(&rounded(*clip, *radius)) };
                D2D1_LAYER_PARAMETERS1 {
                    contentBounds: rect(clip.x, clip.y, clip.w, clip.h),
                    geometricMask: std::mem::ManuallyDrop::new(mask.ok().map(|m| windows::core::Interface::cast(&m).unwrap())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: if at == last { opacity } else { 1.0 },
                    ..Default::default()
                }
            })
            .collect();
        for layer in &layers {
            unsafe { frame.dc.PushLayer(layer, None) };
        }
        // The desktop in the panel's units, which a zoomed panel shrinks (made even where it is drawn already: the
        // panel's glass is frosted from it).
        if desktop {
            self.stage_picture(frame, area, radius, (ox, oy), k, zoom);
        } else {
            self.stage_bitmap(frame, zoom);
        }
        let local = Matrix3x2::scale(zoom, zoom) * Matrix3x2::translation(pos.0, pos.1) * Matrix3x2::scale(k, k) * Matrix3x2::translation(ox, oy);
        // The stage's desktop is a pixel a DIP: a panel DIP covers `zoom` of them.
        let backdrop = self.stage.bitmap.as_ref().map(|(_, bitmap)| (bitmap, Vector2 { X: pos.0 / zoom, Y: pos.1 / zoom }, self.stage.desktop.as_ref().map_or(0, |desktop| desktop.digest), zoom));
        let picture = render::Picture { scene: &scene, lanes: &lanes, layout: &layout, small: None, size: (layout.width(), layout.height()), at: (0.0, 0.0), edge: at.is_none().then_some(edge), backdrop, frost, rough: false };
        let scale = self.scale * zoom * k;
        let cache = match staged_as {
            Shown::Preview => &mut self.layers,
            Shown::Guide => &mut self.guide_layers,
            Shown::Torn(at) => &mut self.torn_layers[at],
        };
        // Laid out in other columns than it was drawn in last, it turns into the new layout as the panel does, what
        // it turns from where it was on the screen.
        let drawn = match cache.turn.follow(what, &lanes, &layout, zoom, Instant::now(), !crate::panel::reduced_motion()) {
            Some(t) => cache.turn.draw(frame, &picture, &mut cache.layers, local, scale, t, |old, old_zoom| {
                let was = corner(old, old_zoom);
                ((was.0 - pos.0) / zoom, (was.1 - pos.1) / zoom)
            }),
            None => cache.layers.draw(frame, &picture, local, scale),
        };
        for layer in layers.into_iter().rev() {
            unsafe { frame.dc.PopLayer() };
            drop(std::mem::ManuallyDrop::into_inner(layer.geometricMask));
        }
        if !whole && desktop {
            stroke_inside(frame, framed, 8.0, self.palette.card_stroke);
        }
        match drawn {
            Ok(drawn) if drawn.grounded => self.gfx.clear_caches(),
            Ok(_) => {}
            // (Not shown, the frame: see `Frame::fail`.)
            Err(error) => frame.fail(error),
        }
        staged(layout.plot_width(&theme, &lanes).map(|width| width * self.scale * zoom * k / prefs.chart_seconds as f32))
    }

    /// The hub's landing: the panel's own components — its lanes, the readings' cards — drawn clean as a grid filling
    /// `area`, with no desktop behind them and none slid to a screen edge. The same cards the live panel shows, laid
    /// out to the room the preview has and drawn on its skin as that skin draws its cards.
    fn panel_grid(&mut self, frame: &Frame, palette: &Palette, area: Rect) -> Option<f32> {
        fill(frame, palette.card, area.x, area.y, area.w, area.h, 8.0);
        stroke_inside(frame, area, 8.0, palette.card_stroke);
        let lang = self.lang;
        let app = crate::app();
        // Held while the cards are drawn; the sampler waits that long.
        let history = app.controller.history.read().unwrap();
        let samples = crate::panel::contiguous(&history);
        if samples.is_empty() {
            return None;
        }
        let skin = Skin::named(&self.settings.skin);
        let prefs = self.prefs.clone().holding(self.previewed());
        // The skin drawn on no desktop: light or dark as the setting says, not as a backdrop behind it.
        let dark = theme::is_dark(prefs.theme, None);
        let look = Theme::new(skin, dark);
        let interval = self.settings.interval_ms as f64;
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0;
        let pen = wall - interval - PEN_LAG_MS;
        let seen = app.controller.seen.lock().unwrap().clone();
        let scene = Scene { music_apart: false, hint: None, thumbs: &crate::ui::view::RESTING, info: &app.info(), prefs: &prefs, theme: &look, lang, history: samples, seen: &seen, pen_ms: pen, scrolls: crate::ui::view::Scrolls::default(), tab: crate::ui::view::Tab::Shelf, hover: None, pinned: false, overlay: app.controller.overlay_wanted(), buttons: true, newer: update::available().is_some(), settings_lit: None };
        let lanes = view::lanes(&scene);
        let heights: Vec<f32> = lanes.iter().map(|lane| lane.height(&look)).collect();
        if heights.is_empty() {
            return None;
        }
        // One card to a row, each as wide as the room. The column of cards is as tall as it needs to be; what
        // reaches past the pane's bottom is scrolled through, as the hub's other panes are.
        let base = view::Layout::with_columns(heights, 1, &look);
        let extra = (area.w - base.width()).max(0.0);
        let layout = base.stretched((extra, 0.0));
        let scroll = self.hub_scroll.min(self.hub_most());
        let picture = render::Picture { scene: &scene, lanes: &lanes, layout: &layout, small: None, size: (layout.width(), layout.height()), at: (0.0, 0.0), edge: None, backdrop: None, frost: 0.0, rough: false };
        let local = Matrix3x2::translation(area.x, area.y - scroll);
        let painted = self.targets.len();
        unsafe { frame.dc.PushAxisAlignedClip(&rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let drawn = self.layers.layers.draw(frame, &picture, local, self.scale);
        unsafe { frame.dc.PopAxisAlignedClip() };
        match drawn {
            Ok(drawn) if drawn.grounded => self.gfx.clear_caches(),
            Ok(_) => {}
            Err(error) => frame.fail(error),
        }
        // How far the row of cards reaches past the pane's bottom: that is what the pane may be scrolled through.
        self.hub_tall = area.h + (layout.height() - area.h).max(0.0);
        self.hub_shifted(frame, palette, area, painted, scroll);
        layout.plot_width(&look, &lanes).map(|width| width * self.scale / prefs.chart_seconds as f32)
    }

    /// The stage's desktop made a bitmap for a panel's `zoom` (a zoomed panel's units shrink it), where it is not yet.
    fn stage_bitmap(&mut self, frame: &Frame, zoom: f32) {
        let zoom_key = zoom.to_bits();
        if self.stage.bitmap.as_ref().is_none_or(|(key, _)| *key != zoom_key) {
            self.stage.bitmap = self.stage.desktop.as_ref().and_then(|desktop| desktop.bitmap(&frame.dc, zoom).ok()).map(|b| (zoom_key, b));
        }
    }

    /// The stage's desktop at `at`, `k` DIPs to one of its own, clipped to `area` inside its corners rounded by `radius`:
    /// made a bitmap for the panel's `zoom` (a zoomed panel's units shrink it), as it is drawn under it.
    fn stage_picture(&mut self, frame: &Frame, area: Rect, radius: f32, at: (f32, f32), k: f32, zoom: f32) {
        let (sw, sh) = self.stage.size;
        self.stage_bitmap(frame, zoom);
        let bitmap = self.stage.bitmap.as_ref().map(|(_, bitmap)| bitmap);
        desktop_at(frame, bitmap, self.palette.switch_off, area, radius, Rect { x: at.0, y: at.1, w: sw * k, h: sh * k });
    }
}

/// Where a screen drawn whole at `screen` is clipped to and its corners' radius: itself, its corners rounded as the
/// layout's map rounds each screen it draws (the same in DIPs however large it is drawn), so that its desktop, a panel
/// on it and its outline all keep inside them.
fn screen_shown(screen: Rect) -> (Rect, f32) {
    (screen, MAP_RADIUS * 2.0)
}

/// A desktop drawn at `at`, clipped to `area` inside its corners rounded by `radius`: `bitmap`, or where there is none
/// (not read yet, or given up: see `reads`), `none` all over, as the layout's map shows a screen whose desktop it has
/// not.
fn desktop_at(frame: &Frame, bitmap: Option<&ID2D1Bitmap1>, none: Color, area: Rect, radius: f32, at: Rect) {
    let Ok(mask) = (unsafe { frame.gfx.factory.CreateRoundedRectangleGeometry(&rounded(area, radius)) }) else { return };
    let layer = D2D1_LAYER_PARAMETERS1 {
        contentBounds: rect(area.x, area.y, area.w, area.h),
        geometricMask: std::mem::ManuallyDrop::new(windows::core::Interface::cast(&mask).ok()),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity: 1.0,
        ..Default::default()
    };
    unsafe { frame.dc.PushLayer(&layer, None) };
    match bitmap {
        Some(bitmap) => unsafe { frame.dc.DrawBitmap(bitmap, Some(&rect(at.x, at.y, at.w, at.h)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None) },
        None => fill(frame, none, at.x, at.y, at.w, at.h, 0.0),
    }
    unsafe { frame.dc.PopLayer() };
    drop(std::mem::ManuallyDrop::into_inner(layer.geometricMask));
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(r.x, r.y, r.w, r.h), radiusX: radius, radiusY: radius }
}

/// (Through the frame's own fill, put on whole pixels as everything Glance draws is: drawn straight, the settings'
/// rails and cards were spread over pixels, softer than the rest, and a slider's knob stood half a pixel off its rail.)
pub(crate) fn fill(frame: &Frame, color: Color, x: f32, y: f32, w: f32, h: f32, radius: f32) {
    Canvas::fill_rounded(frame, color, x, y, w, h, radius.min(w / 2.0).min(h / 2.0));
}

/// The picture `bitmap` filling the box `r`, its corners rounded, as
/// opaque as `opacity`.
fn picture_rounded(frame: &Frame, bitmap: &ID2D1Bitmap1, r: Rect, radius: f32, opacity: f32) {
    let (x, y, w, h) = frame.on_pixels(r.x, r.y, r.w, r.h);
    let r = Rect { x, y, w, h };
    let Ok(mask) = (unsafe { frame.gfx.factory.CreateRoundedRectangleGeometry(&rounded(r, radius)) }) else { return };
    let layer = D2D1_LAYER_PARAMETERS1 {
        contentBounds: rect(r.x, r.y, r.w, r.h),
        geometricMask: std::mem::ManuallyDrop::new(windows::core::Interface::cast(&mask).ok()),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity,
        ..Default::default()
    };
    unsafe {
        frame.dc.PushLayer(&layer, None);
        frame.dc.DrawBitmap(bitmap, Some(&rect(r.x, r.y, r.w, r.h)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        frame.dc.PopLayer();
    }
    drop(std::mem::ManuallyDrop::into_inner(layer.geometricMask));
}

/// A one-DIP border drawn inside the box, as CSS's `border`.
fn stroke_inside(frame: &Frame, r: Rect, radius: f32, color: Color) {
    Canvas::stroke_rounded(frame, crate::ui::canvas::Fill::Solid(color), r.x, r.y, r.w, r.h, radius, 1.0);
}

/// A ring `width` DIPs wide just outside the box, as CSS's `box-shadow: 0 0 0 width`.
fn stroke_outside(frame: &Frame, r: Rect, radius: f32, color: Color, width: f32) {
    Canvas::stroke_rounded(frame, crate::ui::canvas::Fill::Solid(color), r.x - width, r.y - width, r.w + 2.0 * width, r.h + 2.0 * width, radius + width, width);
}

/// A row's card: filled, with a hairline border.
fn card(frame: &Frame, palette: &Palette, x: f32, y: f32, w: f32, h: f32, color: Color) {
    fill(frame, color, x, y, w, h, CARD_RADIUS);
    stroke_inside(frame, Rect { x, y, w, h }, CARD_RADIUS, palette.card_stroke);
}

/// One line of text centred on `cy`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_centred(frame: &Frame, text: &str, font: Font, color: Color, x: f32, cy: f32, width: f32, align: Align) {
    let (ascent, descent) = frame.gfx.baseline(font);
    frame.text(text, font, color, x, cy - (ascent + descent) / 2.0, width, align);
}

/// Text drawn whole and horizontally centred on `cx`, its middle on the line `cy`.
fn text_middle(frame: &Frame, text: &str, font: Font, color: Color, cx: f32, cy: f32) {
    let width = frame.gfx.measure(text, font);
    text_centred(frame, text, font, color, cx - width / 2.0, cy, width, Align::Start);
}

/// Choices `widths` wide, line by line: each line as many whole as fit in `room` (the track's 4 DIPs of rim
/// included), one at least.
fn lines_of(widths: &[f32], room: f32) -> Vec<std::ops::Range<usize>> {
    let mut lines = Vec::new();
    let (mut start, mut taken) = (0, 4.0);
    for (at, width) in widths.iter().enumerate() {
        if at > start && taken + width > room {
            lines.push(start..at);
            (start, taken) = (at, 4.0);
        }
        taken += width;
    }
    if start < widths.len() {
        lines.push(start..widths.len());
    }
    lines
}

/// A row's label, and below it, if there is one, its hint, centred together on `cy`.
#[allow(clippy::too_many_arguments)]
fn field_label(frame: &Frame, palette: &Palette, label: &str, hint: Option<&str>, label_font: Font, hint_font: Font, x: f32, cy: f32, width: f32) {
    field_label_in(frame, (palette.text, palette.text2), label, hint, label_font, hint_font, x, cy, width);
}

/// A row's label and hint in the colours given (label, hint), the hint
/// broken into as many lines as `width` makes it.
#[allow(clippy::too_many_arguments)]
fn field_label_in(frame: &Frame, colors: (Color, Color), label: &str, hint: Option<&str>, label_font: Font, hint_font: Font, x: f32, cy: f32, width: f32) {
    match hint {
        None => text_centred(frame, label, label_font, colors.0, x, cy, width, Align::Start),
        Some(hint) => {
            // A 20 DIP line, 2 apart, then the hint's lines.
            let width = width.max(40.0);
            let top = cy - (22.0 + frame.gfx.wrapped_height(hint, hint_font, width)) / 2.0;
            text_centred(frame, label, label_font, colors.0, x, top + 10.0, width, Align::Start);
            frame.text_wrapped(hint, hint_font, colors.1, x, top + 22.0, width);
        }
    }
}

/// What is said beside a code: what to do with it, and what comes of that.
fn code_words(lang: Lang, what: CodeFor, page: Option<&str>) -> (&'static str, Cow<'static, str>) {
    let (title, words) = match what {
        // (The first of each on one line: short enough for the room beside the code.)
        CodeFor::App => (pick(lang, "扫码下载 Glance", "Scan to get Glance"), crate::phone::APP_PAGE.trim_start_matches("https://")),
        CodeFor::Pairing => (pick(lang, "用 Glance APP 扫描", "Scan with the Glance app"), pick(lang, "打开手机上的 Glance，扫描此配对码。10 分钟内有效，仅供一部手机使用", "Open Glance on the phone and scan this code. Good for ten minutes, for one phone")),
        // (The address itself too, to be typed where the code cannot be scanned.)
        CodeFor::WebPage => {
            let words = pick(lang, "请在此前配对时使用的浏览器中打开。电脑的网络地址变更后，原配对随之失效，请通过“用网页”重新配对", "Open it in the browser the phone was paired with. Once this PC's address on the network changes, that pairing no longer holds. Pair again with “With the web”");
            return (pick(lang, "用手机相机扫描", "Scan with the camera"), Cow::Owned(format!("{}
{words}", page.unwrap_or_default())));
        }
        CodeFor::WebPairing => (pick(lang, "用手机相机扫描", "Scan with the camera"), pick(lang, "扫码后将在浏览器中打开，之后可添加到主屏幕。网页未加密，请仅在可信任的网络（如家庭网络）中使用。10 分钟内有效，仅供一部手机使用", "It opens in the browser, and can then be added to the home screen. The page is not encrypted. Use it only on a network you trust, such as your home's. Good for ten minutes, for one phone")),
    };
    (title, Cow::Borrowed(words))
}

/// A row's label and hint, each on one line, cut short if they do not fit.
#[allow(clippy::too_many_arguments)]
fn field_label_line(frame: &Frame, palette: &Palette, label: &str, hint: Option<&str>, label_font: Font, hint_font: Font, x: f32, cy: f32, width: f32) {
    match hint {
        None => text_centred(frame, label, label_font, palette.text, x, cy, width, Align::Start),
        Some(hint) => {
            // A 20 DIP line, 2 apart, then a 16 DIP one.
            let top = cy - 19.0;
            text_centred(frame, label, label_font, palette.text, x, top + 10.0, width, Align::Start);
            text_centred(frame, hint, hint_font, palette.text2, x, top + 22.0 + 8.0, width, Align::Start);
        }
    }
}

/// A miniature of each skin, drawn the same whichever is active.
fn skin_miniature(frame: &Frame, skin: Skin, r: Rect) {
    let dc = &frame.dc;
    let inner = (r.x + 10.0, r.w - 20.0);
    let cy = r.y + r.h / 2.0;
    match skin {
        Skin::Paper => {
            fill(frame, Color::hex(0xEEF2EC, 1.0), r.x, r.y, r.w, r.h, 6.0);
            // Three ruled lines, the middle one in the signal colour.
            for (i, color) in [0x16221E, 0xD23F1C, 0x16221E].into_iter().enumerate() {
                let y = cy + (i as f32 - 1.0) * 13.5;
                fill(frame, Color::hex(color, 1.0), inner.0, y - 0.75, inner.1, 1.5, 0.0);
            }
        }
        Skin::Glass => {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: Color::hex(0x7AA7FF, 1.0).d2d() },
                D2D1_GRADIENT_STOP { position: 0.55, color: Color::hex(0xC79BFF, 1.0).d2d() },
                D2D1_GRADIENT_STOP { position: 1.0, color: Color::hex(0xFFB38A, 1.0).d2d() },
            ];
            let span = (r.w + r.h) * std::f32::consts::FRAC_1_SQRT_2 / 2.0 * std::f32::consts::FRAC_1_SQRT_2;
            let (cx, my) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            unsafe {
                if let Ok(stops) = ID2D1RenderTarget::CreateGradientStopCollection(dc, &stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                    let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: Vector2 { X: cx - span, Y: my - span },
                        endPoint: Vector2 { X: cx + span, Y: my + span },
                    };
                    if let Ok(brush) = dc.CreateLinearGradientBrush(&line, None, &stops) {
                        let (x, y, w, h) = frame.on_pixels(r.x, r.y, r.w, r.h);
                        dc.FillRoundedRectangle(&rounded(Rect { x, y, w, h }, 6.0), &brush);
                    }
                }
            }
            for i in 0..3 {
                let y = cy - 19.0 + i as f32 * 14.0;
                let pill = Rect { x: inner.0, y, w: inner.1, h: 10.0 };
                fill(frame, Color::hex(0xFFFFFF, 0.35), pill.x, pill.y, pill.w, pill.h, 5.0);
                stroke_inside(frame, pill, 5.0, Color::hex(0xFFFFFF, 0.4));
                fill(frame, Color::hex(0xFFFFFF, 0.9), pill.x + 4.0, pill.y, pill.w - 8.0, 1.0, 0.0);
            }
        }
        Skin::Fluent => {
            fill(frame, Color::hex(0xECEEF2, 1.0), r.x, r.y, r.w, r.h, 6.0);
            for i in 0..3 {
                let y = cy - 8.5 + i as f32 * 7.0;
                let (w, color) = if i == 0 { (inner.1 * 0.6, theme::accent(false)) } else { (inner.1, Color::hex(0xC8CCD2, 1.0)) };
                fill(frame, color, inner.0, y, w, 3.0, 1.5);
            }
        }
    }
}

/// Pictures of every page (and each of the monitor's tabs) at the least, the usual and a wide size of the window,
/// drawn off screen (the window made, not shown), to see that nothing reaches past what holds it. Into
/// `target/settings`: `cargo test --release --lib --features studio draws_every_page -- --ignored`.
#[cfg(all(test, feature = "studio"))]
mod pictures {
    use super::*;

    #[test]
    #[ignore = "draws on this PC's graphics card, to pictures to be looked at"]
    fn draws_every_page() {
        let _pretending = crate::pretending();
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        // This machine's names, and two minutes of made-up readings up to now.
        let info = crate::metrics::Sampler::new().info.clone();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0;
        let history = (0..120).map(|i| crate::studio::made_up(&info, now_ms - (120 - i) as f64 * 1000.0, i as u64, 0.4, None)).collect();
        crate::pictured(info, history);
        // Reports sent from here, followed (see `Page::Reports`).
        crate::reports::pretend(now_ms as u64, &[]);
        // The station holding a few things (files of this repository, pictured as Explorer shows them).
        {
            use crate::station::shelf::{Item, What};
            let file = |name: &str| std::path::PathBuf::from(format!("{}/{name}", env!("CARGO_MANIFEST_DIR"))).components().collect::<std::path::PathBuf>();
            let item = |id: u64, what: What, copied: bool| Item { id, what, added_ms: 0, pinned: id == 2, copied, stays_here: false, own_bytes: 0 };
            crate::station::thumbs::start(|| {});
            crate::station::run::pretend(vec![
                item(1, What::Files { paths: vec![file("icons/128x128@2x.png")], kept: Vec::new() }, false),
                item(2, What::Text { text: "会议改到周四下午三点
地点：二楼小会议室
记得带上季度报表".into() }, false),
                item(3, What::Link { url: "https://github.com/lulu-loopp/glance/releases".into() }, false),
                item(4, What::Files { paths: vec![file("Cargo.toml"), file("README.md"), file("LICENSE")], kept: Vec::new() }, false),
                item(5, What::Text { text: "cargo build --profile fast".into() }, true),
                item(6, What::Link { url: "https://glancepc.com/app".into() }, true),
                item(7, What::Files { paths: vec![file("README.zh-CN.md")], kept: Vec::new() }, true),
            ], crate::settings::ShelfView::Cards);
        }
        let gfx = gfx::held().unwrap();
        let hwnd = make(gfx.clone(), None, None).unwrap();
        let out = std::path::PathBuf::from(format!("{}/target/settings", env!("CARGO_MANIFEST_DIR")));
        std::fs::create_dir_all(&out).unwrap();
        let shown = [
            (Page::Layout, Tab::Shown, "layout"),
            (Page::Look, Tab::Shown, "look"),
            (Page::Monitor, Tab::Shown, "monitor-shown"),
            (Page::Monitor, Tab::Data, "monitor-data"),
            (Page::Monitor, Tab::Overlay, "monitor-overlay"),
            (Page::Workbench, Tab::Shown, "workbench"),
            (Page::Music, Tab::Shown, "music"),
            (Page::Phone, Tab::Shown, "phone"),
            (Page::General, Tab::Shown, "general"),
            (Page::Reports, Tab::Shown, "reports"),
        ];
        let scale = with_ui(|ui| ui.scale).unwrap();
        for (w, h) in [MIN_SIZE, SIZE, (1700.0, 900.0)] {
            unsafe {
                let _ = SetWindowPos(hwnd, None, 0, 0, (w * scale) as i32, (h * scale) as i32, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            for (page, tab, name) in shown {
                with_ui(|ui| {
                    ui.tab = tab;
                    ui.turn_to(page);
                });
                // Drawn a while (the desktops read meanwhile, what moves come to rest), then pictured.
                for _ in 0..12 {
                    with_ui(|ui| ui.draw(Instant::now()));
                    std::thread::sleep(Duration::from_millis(80));
                }
                let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
                let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
                let pixels = gfx.draw_offscreen(size, scale, |frame| {
                    with_ui(|ui| {
                        // (On the window's own ground: Mica is the system's, not drawn.)
                        fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                        ui.paint(frame, Instant::now())
                    });
                })
                .unwrap();
                crate::studio::save_png(&out.join(format!("{name}-{}.png", w as u32)), size.0, size.1, &pixels).unwrap();
            }
            // A release found whose installer did not pass its check, and one not downloaded: each with the site's
            // download page beside trying again.
            for (state, name) in [(update::State::Rejected, "general-update-rejected"), (update::State::Failed, "general-update-failed")] {
                update::pretend(Some(("0.4.4", state)));
                with_ui(|ui| ui.turn_to(Page::General));
                for _ in 0..12 {
                    with_ui(|ui| ui.draw(Instant::now()));
                    std::thread::sleep(Duration::from_millis(80));
                }
                let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
                let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
                let pixels = gfx.draw_offscreen(size, scale, |frame| {
                    with_ui(|ui| {
                        fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                        ui.paint(frame, Instant::now())
                    });
                })
                .unwrap();
                crate::studio::save_png(&out.join(format!("{name}-{}.png", w as u32)), size.0, size.1, &pixels).unwrap();
            }
            update::pretend(None);
            // A notice of Glance's site, at the top of the updates, with its link and without.
            for (name, link) in [("general-notice", Some("https://glancepc.com/#download")), ("general-notice-plain", None)] {
                crate::notice::pretend(Some((
                    "pictured",
                    ("更新方式变更", "Glance 0.5.0 改用新的签名证书，当前版本无法自动更新到该版本。请从官网下载安装一次，之后的更新将恢复自动进行。"),
                    ("A change to updating", "Glance 0.5.0 is signed with a new certificate, which this version cannot check. Download it once from the website. Later updates will be automatic again."),
                    link,
                )));
                with_ui(|ui| ui.turn_to(Page::General));
                for _ in 0..12 {
                    with_ui(|ui| ui.draw(Instant::now()));
                    std::thread::sleep(Duration::from_millis(80));
                }
                let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
                let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
                let pixels = gfx.draw_offscreen(size, scale, |frame| {
                    with_ui(|ui| {
                        fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                        ui.paint(frame, Instant::now())
                    });
                })
                .unwrap();
                crate::studio::save_png(&out.join(format!("{name}-{}.png", w as u32)), size.0, size.1, &pixels).unwrap();
            }
            crate::notice::pretend(None);
            // Recent files' group, scrolled to: only some folders (one chosen), a drive left out and away, a folder
            // not listed (the window's own settings, not kept).
            with_ui(|ui| {
                ui.turn_to(Page::Workbench);
                ui.settings.recent_only = true;
                ui.settings.recent_folders = vec![std::path::PathBuf::from(r"D:\Photos")];
                ui.settings.recent_hidden = vec![std::path::PathBuf::from(r"D:\Photos\Private")];
                ui.settings.recent_hidden_drives = vec![crate::settings::HiddenDrive { volume: r"\\?\Volume{0}\".into(), label: "Phone backup".into(), letter: 'Z' }];
            });
            for _ in 0..12 {
                with_ui(|ui| {
                    ui.scroll_target = (ui.content_height() - ui.client().1).max(0.0);
                    ui.draw(Instant::now());
                });
                std::thread::sleep(Duration::from_millis(80));
            }
            let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
            let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
            let pixels = gfx.draw_offscreen(size, scale, |frame| {
                with_ui(|ui| {
                    fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                    ui.paint(frame, Instant::now())
                });
            })
            .unwrap();
            crate::studio::save_png(&out.join(format!("workbench-recent-{}.png", w as u32)), size.0, size.1, &pixels).unwrap();
            with_ui(|ui| {
                ui.settings.recent_only = false;
                ui.settings.recent_folders.clear();
                ui.settings.recent_hidden.clear();
                ui.settings.recent_hidden_drives.clear();
                ui.scroll_target = 0.0;
            });
            // The workbench carried from the library over an edge: of every screen small (the first screen's left),
            // and of one screen large (its right); and so again with the workbench in two columns, smaller, the readings in
            // three (what the layout page draws is the workbench's size and columns, not theirs).
            for (large, name, sized) in [(false, "layout-carry", false), (true, "layout-carry-one", false), (true, "layout-carry-sized", true)] {
                // (Painted to learn where things are: `draw` paints nothing of a window not shown.)
                let painted = || {
                    let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
                    gfx.draw_offscreen(((cw * scale).round() as u32, (ch * scale).round() as u32), scale, |frame| {
                        with_ui(|ui| ui.paint(frame, Instant::now()));
                    })
                    .unwrap();
                };
                with_ui(|ui| {
                    ui.turn_to(Page::Layout);
                    ui.view = None;
                    if sized {
                        ui.settings.set_columns(crate::settings::Part::Monitor, Some(3));
                        ui.settings.set_columns(crate::settings::Part::Workbench, Some(2));
                        let screen = ui.stage.screen.clone();
                        ui.settings.size_panel(crate::settings::Part::Workbench, screen.as_deref(), 0.85);
                    }
                });
                painted();
                with_ui(|ui| {
                    // (A screen's picture, the largest of what shows one: the buttons above them show one too.)
                    let screen = ui.targets.iter().filter(|(_, target)| matches!(target, Target::View(Some(_)))).max_by(|a, b| (a.0.w * a.0.h).total_cmp(&(b.0.w * b.0.h))).map(|(r, target)| (*r, target.clone())).unwrap();
                    if large {
                        ui.press_editor(&screen.1);
                    }
                    ui.carry = Some(layout_page::Carry { part: crate::settings::Part::Workbench, from: None, start: (0.0, 0.0), moved: true });
                    ui.pointer = Some((screen.0.x + 8.0, screen.0.y + screen.0.h / 2.0));
                });
                painted();
                if large {
                    with_ui(|ui| {
                        let zone = ui.targets.iter().find_map(|(r, target)| matches!(target, Target::Zone(_, Edge::Right)).then_some(*r)).unwrap();
                        ui.pointer = Some((zone.x + zone.w - 20.0, zone.y + zone.h / 2.0));
                    });
                }
                // (What moves comes to rest as it is painted.)
                for _ in 0..12 {
                    painted();
                    std::thread::sleep(Duration::from_millis(80));
                }
                let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
                let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
                let pixels = gfx
                    .draw_offscreen(size, scale, |frame| {
                        with_ui(|ui| {
                            fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                            ui.paint(frame, Instant::now())
                        });
                    })
                    .unwrap();
                crate::studio::save_png(&out.join(format!("{name}-{}.png", w as u32)), size.0, size.1, &pixels).unwrap();
                with_ui(|ui| {
                    ui.carry = None;
                    ui.pointer = None;
                    ui.view = None;
                    ui.shown_edge = None;
                });
            }
        }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    /// How soon the window, shown cloaked to pour out, can be pictured as it is: pictured at once, after a frame of the
    /// compositor and after a while (pictures in target/genie-wait, times said), never seen meanwhile.
    #[test]
    #[ignore = "makes a window of this session's (cloaked) and writes pictures to look at"]
    fn how_soon_it_is_pictured() {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
        let (_device, hwnd) = pretended();
        let out = std::path::PathBuf::from(format!("{}/target/genie-wait", env!("CARGO_MANIFEST_DIR")));
        std::fs::create_dir_all(&out).unwrap();
        for round in 0..2 {
            super::super::genie::hide_to_open(hwnd);
            let began = Instant::now();
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            let shown = began.elapsed();
            let mut said = format!("round {round}: shown in {:.1} ms", shown.as_secs_f32() * 1000.0);
            for (name, wait) in [("at-once", 0u32), ("flushed", 1), ("later", 2)] {
                match wait {
                    1 => unsafe {
                        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
                    },
                    2 => std::thread::sleep(Duration::from_millis(400)),
                    _ => {}
                }
                let at = began.elapsed();
                let (picture, _) = super::super::genie::picture_of(hwnd).unwrap();
                said += &format!(", {name} at {:.1} ms", at.as_secs_f32() * 1000.0);
                crate::studio::save_png(&out.join(format!("{round}-{name}.png")), picture.size.0, picture.size.1, &picture.pixels).unwrap();
            }
            println!("{said}");
            unsafe {
                let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
            }
        }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    /// Asked for at a page (the overlay's, the workbench's, the reports', the updates'), the window is at it as it is
    /// made, before it is first drawn: it pours out showing that page, not the one shown last. (Made unseen here, it is
    /// not drawn till shown: see `draw`.)
    #[test]
    fn the_settings_are_at_the_page_asked_for_from_their_first_frame() {
        let (gfx, hwnd) = pretended();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
        for (to, page) in [(To::Overlay, Page::Monitor), (To::Workbench, Page::Workbench), (To::Reports, Page::Reports), (To::Updates, Page::General)] {
            let hwnd = make(gfx.clone(), None, Some(to)).unwrap();
            let at = with_ui(|ui| ui.page).unwrap();
            // (Closed before what is asserted: a window left open as the test thread ends gives its device back as the
            // thread's own data goes, under the loader's lock, where the graphics driver may wait for ever.)
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            assert_eq!(at, page);
        }
    }

    /// A notice of Glance's site is shown at the top of the updates, a dot beside the general page's name, till it is
    /// read: told in a notification once, and not again once read. Its link, where it has one, offered beside reading
    /// it. Opened at from its notification, the window is at the updates, the notice marked.
    #[test]
    fn a_notice_is_shown_till_read() {
        let (_gfx, hwnd) = pretended();
        // (The window closed before what failed is told: see `the_settings_are_at_the_page_asked_for_from_their_first_frame`.)
        let outcome = std::panic::catch_unwind(|| {
            let app = crate::app();
            app.change(|settings| {
                settings.notices_told.clear();
                settings.notices_read.clear();
            });
            // No other news on the page: the dot the notice's alone.
            with_ui(|ui| {
                ui.followed.clear();
                ui.turn_to(Page::General);
            });
            crate::notice::pretend(None);
            assert!(!with_ui(|ui| ui.rows().iter().any(|row| matches!(row, Row::Notice)) || ui.general_has_news()).unwrap());
            assert!(!crate::notice::tell(Lang::Zh));
            crate::notice::pretend(Some(("test-notice", ("更新方式变更", "请从官网下载安装一次。"), ("A change to updating", "Download it once."), Some("https://glancepc.com/#download"))));
            with_ui(|ui| {
                let rows = ui.rows();
                let at = rows.iter().position(|row| matches!(row, Row::Notice)).expect("the notice shown");
                assert!(matches!(rows[at - 1], Row::Group(_)) && matches!(rows[at + 1], Row::Version), "first of the updates");
                assert!(ui.general_has_news());
                let (title, text, buttons) = ui.button_row(&Row::Notice, Instant::now()).unwrap();
                assert_eq!((title.as_str(), text.as_str()), ("更新方式变更", "请从官网下载安装一次。"));
                assert_eq!(buttons.into_iter().map(|(_, target, _)| target).collect::<Vec<_>>(), vec![Target::NoticeRead, Target::NoticeLink]);
            })
            .unwrap();
            // Told once.
            assert!(crate::notice::tell(Lang::Zh));
            assert!(!crate::notice::tell(Lang::Zh));
            // Opened at from it: the general page, the notice marked.
            with_ui(|ui| {
                ui.turn_to(Page::Look);
                ui.turn_as_asked(To::Updates);
                assert_eq!(ui.page, Page::General);
                assert!(ui.motion.get("flash:notice").is_some_and(|flash| flash.value(Instant::now()) > 0.5));
            })
            .unwrap();
            // Read: gone, and the dot with it, and not told again.
            with_ui(|ui| ui.activate(Target::NoticeRead));
            assert!(!with_ui(|ui| ui.rows().iter().any(|row| matches!(row, Row::Notice)) || ui.general_has_news()).unwrap());
            assert!(app.settings.lock().unwrap().notices_read.contains(&"test-notice".to_string()));
            assert!(!crate::notice::tell(Lang::Zh));
            // Without a link, reading it alone is offered.
            crate::notice::pretend(Some(("test-notice-2", ("维护", "官网维护。"), ("Maintenance", "The site is down."), None)));
            let targets = with_ui(|ui| ui.button_row(&Row::Notice, Instant::now()).unwrap().2.into_iter().map(|(_, target, _)| target).collect::<Vec<_>>()).unwrap();
            assert_eq!(targets, vec![Target::NoticeRead]);
        });
        crate::notice::pretend(None);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
        if let Err(failed) = outcome {
            std::panic::resume_unwind(failed);
        }
    }

    /// The preview's charts set how soon the window is drawn again: the readings' creep along, and a workbench with
    /// none (quick launch alone, on its rail, the rest turned off) has none to set it. Its lane's width was taken for a
    /// chart's, too narrow for one, and the time till the next frame came out below nothing: Glance stopped.
    #[test]
    fn a_workbench_without_charts_sets_no_pace() {
        let (gfx, hwnd) = pretended();
        let creep = |page: Page| {
            with_ui(|ui| ui.turn_to(page));
            with_ui(|ui| {
                let (cw, ch) = ui.client();
                let mut creep = None;
                gfx.draw_offscreen(((cw * ui.scale) as u32, (ch * ui.scale) as u32), ui.scale, |frame| creep = ui.paint(frame, Instant::now())).unwrap();
                ui.draw(Instant::now());
                creep
            })
            .unwrap()
        };
        assert!(creep(Page::Monitor).is_some_and(|speed| speed > 0.0));
        // (Turned off as their switches do, kept to the window alone: these settings are this PC's own.)
        with_ui(|ui| {
            for entry in &mut ui.prefs.workbench {
                entry.on = entry.id == "apps";
            }
            ui.settings.view = crate::ui::prefs::kept(&ui.prefs, &ui.settings.view);
            let settings = ui.settings.clone();
            ui.take(settings);
            assert_eq!(ui.prefs.workbench.iter().filter(|entry| entry.on).map(|entry| entry.id.as_str()).collect::<Vec<_>>(), ["apps"]);
        });
        assert_eq!(creep(Page::Workbench), None);
        assert_eq!(super::creeping(None), Duration::from_millis(250));
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    /// Glance pictured with made-up readings, the settings window made (not shown), and reports sent from here
    /// pretended (see `reports::pretend`), the first carrying pictures taken of that window: the device, the window.
    fn pretended() -> (gfx::Held, HWND) {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let info = crate::metrics::Sampler::new().info.clone();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let history = (0..120).map(|i| crate::studio::made_up(&info, now_ms as f64 - (120 - i) as f64 * 1000.0, i as u64, 0.4, None)).collect();
        crate::pictured(info, history);
        crate::reports::pretend(now_ms, &[]);
        let gfx = gfx::held().unwrap();
        let hwnd = make(gfx.clone(), None, None).unwrap();
        // Pictures taken of the window's pages, as a user takes them of what is wrong.
        let scale = with_ui(|ui| ui.scale).unwrap();
        let mut taken = Vec::new();
        for (at, page) in [Page::Monitor, Page::General, Page::Look].into_iter().enumerate() {
            with_ui(|ui| ui.turn_to(page));
            for _ in 0..6 {
                with_ui(|ui| ui.draw(Instant::now()));
                std::thread::sleep(Duration::from_millis(80));
            }
            let (cw, ch) = with_ui(|ui| ui.client()).unwrap();
            let size = ((cw * scale).round() as u32, (ch * scale).round() as u32);
            let pixels = gfx
                .draw_offscreen(size, scale, |frame| {
                    with_ui(|ui| {
                        fill(frame, ui.palette.window, 0.0, 0.0, cw, ch, 0.0);
                        ui.paint(frame, Instant::now())
                    });
                })
                .unwrap();
            let file = std::env::temp_dir().join(format!("glance-thread-picture-{}-{at}.png", std::process::id()));
            crate::studio::save_png(&file, size.0, size.1, &pixels).unwrap();
            taken.push(std::mem::take(&mut super::super::picture::from_file(&file.to_string_lossy(), (1, 1)).unwrap().jpeg));
            std::fs::remove_file(&file).unwrap();
        }
        crate::reports::pretend(now_ms, &taken);
        with_ui(|ui| {
            ui.turn_to(Page::Reports);
            ui.followed = crate::reports::list();
        });
        (gfx, hwnd)
    }

    /// A report's thread as its window draws it (see `ask::thread`), in Chinese and in English, light and dark,
    /// scrolled to its newest: into `target/settings` (`thread-zh-light.png` and the rest), with
    /// `cargo test --release --lib --features studio draws_a_report_s_thread -- --ignored`.
    #[test]
    #[ignore = "draws on this PC's graphics card, to pictures to be looked at"]
    fn draws_a_report_s_thread() {
        let (gfx, hwnd) = pretended();
        let out = std::path::PathBuf::from(format!("{}/target/settings", env!("CARGO_MANIFEST_DIR")));
        std::fs::create_dir_all(&out).unwrap();
        let report = crate::reports::list().into_iter().find(|report| report.id == "KQRH9F").unwrap();
        let scale = with_ui(|ui| ui.scale).unwrap();
        for (lang, said) in [(Lang::Zh, "zh"), (Lang::En, "en")] {
            for (dark, shade) in [(false, "light"), (true, "dark")] {
                let ((w, h), pixels) = super::super::ask::thread::pictured(&gfx, thread_of(lang, &report), &thread_talk(lang, dark), scale);
                crate::studio::save_png(&out.join(format!("thread-{said}-{shade}.png")), w, h, &pixels).unwrap();
            }
            // Opened again by the user after the fix: its button closes it.
            let mut reopened = report.clone();
            let fixed = reopened.thread.last().unwrap().clone();
            reopened.thread.push(crate::reports::Message { n: fixed.n + 1, ts: fixed.ts + 1_800_000, kind: crate::reports::Kind::Open, version: String::new(), mine: true, ..fixed });
            let ((w, h), pixels) = super::super::ask::thread::pictured(&gfx, thread_of(lang, &reopened), &thread_talk(lang, false), scale);
            crate::studio::save_png(&out.join(format!("thread-{said}-reopened.png")), w, h, &pixels).unwrap();
        }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }

    /// Opens a report's thread (its latest reply and fix not yet read) in its window on this PC's desktop, for `FOR`
    /// seconds (8 if not set) or till it is closed, to be looked at and tried:
    /// `cargo test --lib --features studio shows_a_report_s_thread -- --ignored` (`DARK` set: dark, `EN` set: in
    /// English).
    #[test]
    #[ignore = "puts a window up on this PC's desktop"]
    fn shows_a_report_s_thread() {
        let (_gfx, hwnd) = pretended();
        with_ui(|ui| {
            ui.dark = std::env::var("DARK").is_ok();
            if std::env::var("EN").is_ok() {
                ui.lang = Lang::En;
            }
            ui.open_thread("KQRH9F");
        });
        // The settings' thread's own turn at the messages, as `up` gives it, while the thread is open.
        let until = Instant::now() + Duration::from_secs(std::env::var("FOR").ok().and_then(|s| s.parse().ok()).unwrap_or(8));
        let mut msg = MSG::default();
        while Instant::now() < until && with_ui(|ui| ui.thread.is_some()).unwrap_or(false) {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            tick(Instant::now());
            std::thread::sleep(Duration::from_millis(15));
        }
        with_ui(|ui| ui.close_thread());
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An update that cannot be taken here, its installer not downloaded or not passing its check, is offered from
    /// the site's download page as well (a release Glance cannot check, whatever its signature, is still had so):
    /// first where trying again will not help. One that can be taken, or is being, is not.
    #[test]
    fn an_update_not_taken_here_is_offered_from_the_site() {
        for lang in [Lang::Zh, Lang::En] {
            let targets = |state| update_offer(state, lang).1.into_iter().map(|(_, target, _)| target).collect::<Vec<_>>();
            assert_eq!(targets(update::State::Rejected), vec![Target::Download, Target::Update]);
            assert_eq!(targets(update::State::Failed), vec![Target::Update, Target::Download]);
            assert_eq!(targets(update::State::Ready), vec![Target::Update]);
            assert_eq!(targets(update::State::Downloading), vec![Target::Update]);
        }
        assert_eq!(update::download_page(Lang::Zh), "https://glancepc.com/#download");
        assert_eq!(update::download_page(Lang::En), "https://glancepc.com/en/#download");
    }

    /// The memory's lights are offered once the lights' program is there, on no laptop: there the sensors run no
    /// OpenRGB with an administrator's rights, and the switch could only fail.
    #[test]
    fn offers_the_memorys_lights_on_no_laptop() {
        use Switch::{RemoteLights, RemoteRaised, RemoteScreens};
        assert_eq!(remote_switches(true, false), [RemoteLights, RemoteRaised, RemoteScreens]);
        assert_eq!(remote_switches(false, false), [RemoteLights, RemoteScreens]);
        assert_eq!(remote_switches(true, true), [RemoteLights, RemoteScreens]);
        assert_eq!(remote_switches(false, true), [RemoteLights, RemoteScreens]);
    }

    /// Recent files' drives: those it reads now and those left out that are away (as last seen), by their letters,
    /// none twice. One left out by its volume, kept with its label and letter, and listed again (a drive left out
    /// and away listed again too, one neither read nor left out not made up).
    #[test]
    fn offers_and_turns_drives() {
        use crate::settings::HiddenDrive;
        use crate::station::recent::Volume;
        let volume = |letter: char, volume: &str, label: &str| Volume { letter, volume: volume.into(), label: label.into() };
        let drives = [volume('D', "vol-d", "Data"), volume('C', "vol-c", "")];
        let mut hidden = vec![HiddenDrive { volume: "vol-e".into(), label: "Backup".into(), letter: 'E' }];
        let letters = |offered: Vec<Volume>| offered.into_iter().map(|drive| (drive.letter, drive.volume)).collect::<Vec<_>>();
        assert_eq!(letters(drives_offered(&drives, &hidden)), [('C', "vol-c".to_string()), ('D', "vol-d".into()), ('E', "vol-e".into())]);
        turn_drive(&mut hidden, &drives, "vol-d");
        assert_eq!(hidden[1], HiddenDrive { volume: "vol-d".into(), label: "Data".into(), letter: 'D' });
        assert_eq!(drives_offered(&drives, &hidden).len(), 3);
        turn_drive(&mut hidden, &drives, "vol-d");
        turn_drive(&mut hidden, &drives, "vol-e");
        turn_drive(&mut hidden, &drives, "vol-x");
        assert!(hidden.is_empty());
    }

    /// What the preview measures of its desktop is what the desktop is where it is asked: kept while it is asked of
    /// the same place, measured again for another.
    #[test]
    fn the_preview_measures_its_desktop_where_asked() {
        // Black on the left half, white on the right.
        let whole = RECT { left: 0, top: 0, right: 64, bottom: 32 };
        let pixels: Vec<u8> = (0..32).flat_map(|_| (0..64).flat_map(|x| if x < 32 { [0, 0, 0, 255] } else { [255, 255, 255, 255] })).collect();
        let desktop = Capture::from_pixels(whole, pixels);
        let mut stage = Stage::new((64.0, 32.0), None, whole, desktop.copied());
        let (left, right) = (RECT { right: 32, ..whole }, RECT { left: 32, ..whole });
        assert_eq!(stage.tone(left), desktop.luminance(left, 1.0));
        assert_eq!(stage.tone(left), (0.0, 0.0));
        assert_eq!(stage.tone(right), desktop.luminance(right, 1.0));
        assert!(stage.tone(right).0 > 0.99);
        // (Both kept: each frame may ask of several.)
        assert_eq!(stage.measured.tones.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [right, left]);
        let (ring, hole) = (whole, RECT { left: 8, top: 8, right: 56, bottom: 24 });
        assert_eq!(stage.glass(ring, hole), super::super::overlay::glass(&mut desktop.luminances(ring, hole, 1), None).0);
        assert_eq!(stage.glass(ring, hole), stage.glass(ring, hole));
        // Not read yet: grey.
        assert_eq!(Stage::new((64.0, 32.0), None, whole, None).tone(left), (0.5, 0.0));
    }

    /// The map keeps each screen's picture by its name and where it is, another screen's left as it is: two screens of
    /// one name (as two alike displays were once known, see `screens::told_apart`), drawn frame after frame, have each
    /// desktop read once and each picture holding its own. A picture sized afresh beyond what it holds is read again
    /// at the new size, and again at the old one when it is sized back (it no longer holds that).
    #[test]
    fn the_map_reads_each_screen_s_desktop_once() {
        let (mut arts, mut reads) = (Vec::new(), reads::Reads::default());
        let wide = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let tall = RECT { left: -2160, top: -1071, right: 0, bottom: 2769 };
        let now = Instant::now();
        let mut read = Vec::new();
        let mut frames = |arts: &mut Vec<MapArt>, reads: &mut reads::Reads, wanted: &[(RECT, (u32, u32))]| {
            for _ in 0..10 {
                for &(monitor, size) in wanted {
                    map_art_of(arts, reads, "same", monitor, size, false);
                }
                // (A lot read at once, as it is between two frames.)
                let Some((lot, asks)) = reads.lot(now) else { continue };
                for ask in &asks {
                    assert!(reads.came(lot, ask));
                    let reads::Ask::Map { id, monitor, size } = ask.clone() else { panic!("the map asks for its own") };
                    let pixels = [40u8, 80, 120, 255].repeat((size.0 * size.1) as usize);
                    map_art_came(arts, &id, monitor, size, Capture::from_pixels(RECT { left: 0, top: 0, right: size.0 as i32, bottom: size.1 as i32 }, pixels));
                }
                reads.done(lot);
                read.extend(asks);
            }
        };
        frames(&mut arts, &mut reads, &[(wide, (384, 216)), (tall, (216, 384))]);
        frames(&mut arts, &mut reads, &[(wide, (100, 56)), (tall, (216, 384))]);
        frames(&mut arts, &mut reads, &[(wide, (384, 216)), (tall, (216, 384))]);
        let map = |monitor: RECT, size: (u32, u32)| reads::Ask::Map { id: "same".into(), monitor, size };
        assert_eq!(read, [map(wide, (384, 216)), map(tall, (216, 384)), map(wide, (100, 56)), map(wide, (384, 216))]);
        assert_eq!(arts.iter().map(|art| (art.monitor, art.read_at)).collect::<Vec<_>>(), [(wide, Some((384, 216))), (tall, Some((216, 384)))]);
        assert!(arts.iter().all(|art| art.desktop.is_some()));
    }

    /// A screen whose desktop has not been read (or was given up) is drawn plain, as the layout's map draws it, in the
    /// preview and the layout's one screen alike, inside its frame's rounded corners and nowhere else.
    #[test]
    fn a_desktop_not_read_is_drawn_plain() {
        let _device = crate::ui::gfx::held().unwrap();
        let gfx = Gfx::new().unwrap();
        let pixels = gfx.draw_offscreen((100, 60), 1.0, |frame| desktop_at(frame, None, Color::hex(0xFF0000, 1.0), Rect { x: 10.0, y: 10.0, w: 80.0, h: 40.0 }, 8.0, Rect { x: 0.0, y: 0.0, w: 100.0, h: 60.0 })).unwrap();
        let at = |x: usize, y: usize| <[u8; 4]>::try_from(&pixels[(y * 100 + x) * 4..][..4]).unwrap();
        assert_eq!(at(50, 30), [0, 0, 255, 255]);
        assert_eq!(at(12, 30), [0, 0, 255, 255]);
        // (Outside its frame, and in its rounded corner.)
        assert_eq!(at(5, 30), [0, 0, 0, 0]);
        assert_eq!(at(10, 10), [0, 0, 0, 0]);
    }

    /// A desktop the preview holds is read again once the wallpaper changes, and is still wanted however often the
    /// screens change while it is read (what was being read then is not taken): it is wanted till it holds the
    /// wallpaper as it is now, not only while it holds none.
    #[test]
    fn a_wallpaper_refresh_outlives_the_screens_changing() {
        let whole = RECT { left: 0, top: 0, right: 64, bottom: 32 };
        let desktop = Capture::from_pixels(whole, [40u8, 80, 120, 255].repeat(64 * 32));
        let mut stage = Stage { current: true, ..Stage::new((64.0, 32.0), Some("a".into()), whole, Some(desktop)) };
        assert_eq!(stage.wanted(), None);
        // The wallpaper changed (see `Ui::desktops_changed`), then the screens as it was read.
        stage.current = false;
        let (mut reads, now) = (reads::Reads::default(), Instant::now());
        let ask = reads::Ask::Stage { screen: "a".into(), work: whole, size: (64.0, 32.0) };
        reads.want(stage.wanted().expect("read again"));
        assert_eq!(reads.lot(now), Some((0, vec![ask.clone()])));
        reads.forget();
        assert!(!reads.came(0, &ask));
        reads.done(0);
        reads.want(stage.wanted().expect("still read again"));
        assert_eq!(reads.lot(now), Some((1, vec![ask])));
    }

    /// A screen drawn whole, as the layout's one screen view and the guide draw it, has its corners rounded as the
    /// map of every screen rounds each: its desktop (or the plain fill where it has none) is not drawn in its corner,
    /// as the map's is not.
    #[test]
    fn a_screen_drawn_whole_is_rounded_as_the_map_draws_it() {
        let _device = crate::ui::gfx::held().unwrap();
        let gfx = Gfx::new().unwrap();
        let screen = Rect { x: 10.0, y: 10.0, w: 80.0, h: 40.0 };
        let red = Color::hex(0xFF0000, 1.0);
        let drawn = |paint: &dyn Fn(&Frame)| gfx.draw_offscreen((100, 60), 1.0, paint).unwrap();
        let whole = drawn(&|frame| {
            let (clip, radius) = screen_shown(screen);
            desktop_at(frame, None, red, clip, radius, screen);
        });
        let map = drawn(&|frame| fill(frame, red, screen.x, screen.y, screen.w, screen.h, MAP_RADIUS * 2.0));
        let at = |pixels: &[u8], x: usize, y: usize| <[u8; 4]>::try_from(&pixels[(y * 100 + x) * 4..][..4]).unwrap();
        for pixels in [&whole, &map] {
            assert_eq!(at(pixels, 10, 10), [0, 0, 0, 0]);
            assert_eq!(at(pixels, 89, 49), [0, 0, 0, 0]);
            assert_eq!(at(pixels, 50, 30), [0, 0, 255, 255]);
            assert_eq!(at(pixels, 10, 30), [0, 0, 255, 255]);
        }
    }

    /// Choices wider than their room go on as many lines as they take, each whole, in order.
    #[test]
    fn choices_too_wide_go_on_lines() {
        assert_eq!(super::lines_of(&[50.0, 60.0, 70.0], f32::INFINITY), vec![0..3]);
        assert_eq!(super::lines_of(&[50.0, 60.0, 70.0], 120.0), vec![0..2, 2..3]);
        assert_eq!(super::lines_of(&[50.0, 60.0, 70.0], 100.0), vec![0..1, 1..2, 2..3]);
        // (One wider than all the room still has a line.)
        assert_eq!(super::lines_of(&[500.0, 20.0], 100.0), vec![0..1, 1..2]);
        assert!(super::lines_of(&[], 100.0).is_empty());
    }
}
