//! The dock's window: a row of tiles along the foot of a screen, or its top, never taking the focus. Each opens a
//! thing kept at hand, one of Glance's own doors, or the Recycle Bin; a dot under a tile says its program is
//! running, and pressing such a tile goes to its window rather than opening another copy of it. The pointer swells
//! the tile it passes (see `crate::ui::dock`), locked it stays where it was put, and auto-hidden it goes off the
//! edge till the pointer comes back to it.
//!
//! Like the overlay it is two windows: one as large as the glass and the room its tiles swell into, which takes the
//! pointer and shows nothing, and one larger still by the shadow's room, which shows it all and lets every click
//! through. The glass itself is never resized by what the pointer does to the tiles, so nothing moves out from under
//! the hand while they swell.
//!
//! It is drawn by the panel's thread, as the overlay is: at every sample, as the pointer moves over it, and as its
//! swell eases out or it slides in.

use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsZoomed,
    KillTimer, SetTimer, SetWindowPos, HWND_TOPMOST, IDC_ARROW, IDC_HAND, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE,
};

use crate::settings::{DockDoor, DockEdge, DockItem, DockSettings};
use crate::ui::backdrop::Capture;
use crate::ui::dock::{self, Act, Entry, Row};
use crate::ui::gfx::{self, Gfx};
use crate::ui::glass::{self, Frosted};
use crate::ui::icons::Icon;
use crate::ui::life::{Lives, Showing};
use crate::ui::menu::{Item, Look, Menu};
use crate::ui::motion::{Easing, Transition};
use crate::ui::overlay::{self, Glass};
use crate::ui::text::Lang;
use crate::ui::window::Window;
use crate::{dock_running, station};

/// What its menu offers: each row, as it is chosen.
#[derive(Clone, PartialEq, Debug)]
pub enum Choice {
    Lock(bool),
    Through(bool),
    Hide(bool),
    Magnify(bool),
    Dots(bool),
    ShowOpen(bool),
    Edge(DockEdge),
    /// One more gap at the end of the row.
    Divider,
    /// The launchpad's tile put on the row: its door, and the screen of every program with it.
    Launchpad,
    /// A tile pressed, or asked to be opened from the menu.
    Act(Act),
    /// A tile taken off the dock (the setting it stands for).
    Remove(DockItem),
    /// A program run with the administrator's leave.
    Admin(String),
    /// A window's program put on the dock, and kept there (the tile it stands for).
    Add(DockItem),
    /// A thing kept at hand, shown where it is.
    Where(String),
    Settings,
    /// The dock turned off, and kept off: its own menu is only reached while it shows.
    Close,
}

/// What the dock's own tiles tell is running: which of Glance's doors stand open.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Lights {
    pub launchpad: bool,
    pub monitor: bool,
    pub workbench: bool,
    pub music: bool,
    pub overlay: bool,
}

impl Lights {
    /// Whether `door` stands open.
    fn lit(&self, door: DockDoor) -> bool {
        match door {
            DockDoor::Launchpad => self.launchpad,
            DockDoor::Monitor => self.monitor,
            DockDoor::Workbench => self.workbench,
            DockDoor::Music => self.music,
            DockDoor::Overlay => self.overlay,
            DockDoor::Settings => false,
        }
    }
}

/// Where the dock was left by a drag: along its edge, at which edge, on which screen.
pub struct Placed {
    pub along: f32,
    pub edge: DockEdge,
    /// A point on the screen it stands on (physical px).
    pub point: (i32, i32),
}

/// What letting go of it brings: where it was moved to, and what was pressed on it.
pub struct Let {
    pub placed: Option<Placed>,
    pub act: Option<Act>,
    /// The row's order, as it was dragged into: some only where a tile was rearranged rather than the dock slid.
    pub items: Option<Vec<DockItem>>,
}

/// The tile the hand is on: where it went down, what it was on, how far along the edge the dock stood then, and
/// whether it has moved enough to be a drag rather than a press.
struct Hand {
    from: POINT,
    tile: Option<usize>,
    along: f32,
    moved: bool,
    /// Whether this drag rearranges the row rather than slides the dock: begun on a tile the settings keep.
    reorder: bool,
    /// The kept tile begun on, which is the one carried about the row.
    item: Option<DockItem>,
}

/// How far the pointer has to go from where it went down for the dock to be dragged rather than pressed (DIPs).
const DRAG: f32 = 4.0;
/// How near the screen's edge the pointer has to be to call a hidden dock back (DIPs).
const PROBE: f32 = 3.0;
/// How long the pointer can leave it before an auto-hidden dock goes back (ms).
const TUCK_AFTER: Duration = Duration::from_millis(400);
/// How the swell comes up and goes back.
const REACH: (Duration, Easing) = (Duration::from_millis(120), Easing(0.16, 1.0, 0.3, 1.0));
/// How the dock slides in and out of the screen's edge.
const SLIDE: (Duration, Easing) = (Duration::from_millis(220), Easing(0.16, 1.0, 0.3, 1.0));
const SLIDE_OUT: (Duration, Easing) = (Duration::from_millis(160), Easing(0.4, 0.0, 1.0, 1.0));
/// How often it is drawn while anything is moving (ms), on this timer.
pub const TICK_TIMER: usize = 21;
const TICK_MS: u32 = 16;
/// How often the pointer, what runs, and what is behind it are looked at (ms), on this timer.
pub const PEEK_TIMER: usize = 22;
const PEEK_MS: u32 = 80;
/// How often the screen behind it is looked at again, for how to tint the glass (see `look`).
const LOOK_EVERY: Duration = Duration::from_millis(400);
/// How often the row is put together again: what runs, and what is pictured.
const AGAIN_EVERY: Duration = Duration::from_millis(300);
/// How far around it the screen is looked at (DIPs).
const AROUND: f32 = 16.0;
/// How clear the picture of a tile is asked for, at most and least (physical px).
const PICTURE: (u32, u32) = (16, 256);

fn cursor() -> Option<POINT> {
    let mut at = POINT::default();
    unsafe { GetCursorPos(&mut at) }.ok().map(|_| at)
}

/// A screen's work area, its whole area, and its scale.
fn screen_of(monitor: HMONITOR) -> (RECT, RECT, f32) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    let (mut dpi, mut unused) = (96u32, 96u32);
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused) };
    (info.rcWork, info.rcMonitor, dpi as f32 / 96.0)
}

impl Lives for Dock {
    fn device(&self) -> &Rc<Gfx> {
        &self.gfx
    }

    fn showing(&mut self) -> &mut Option<Showing> {
        &mut self.showing
    }

    fn let_go_of_shown(&mut self) {
        self.surface.release();
    }

    fn let_go_of_kept(&mut self) {}
}

pub struct Dock {
    gfx: Rc<Gfx>,
    /// Counted as shown on its device while it is (see `life`).
    showing: Option<Showing>,
    /// What takes the pointer: the glass, and the room its tiles swell into. Shows nothing.
    pub window: Window,
    /// What shows it all (see the top): the window above, and the shadow's room round it. Clicks pass through.
    stage: Window,
    surface: Frosted,
    /// Whether its windows are up.
    out: bool,
    /// Where the glass, the pointer's window and the stage are (physical px).
    rect: RECT,
    window_at: RECT,
    stage_at: RECT,
    /// The panel's window while the panel is up: kept above the dock.
    beneath: Option<HWND>,
    /// A menu's window while one is up: the dock kept under it (see `under_menu`).
    menu: Option<HWND>,
    /// The settings it is drawn under, as last taken up.
    was: DockSettings,
    lang: Lang,
    lights: Lights,
    /// Whether a game has the screen: its glass is then not frosted (see `Overlay::draw`).
    playing: bool,
    locked: bool,
    through: bool,
    auto_hide: bool,
    dots: bool,
    magnify: bool,
    show_open: bool,
    clear: f32,
    /// Which edge it stands at, how far along it, and the screen it stands on.
    edge: DockEdge,
    along: f32,
    screen: Option<(i32, i32)>,
    /// Its screen's work area, its whole area, and its scale.
    work: RECT,
    monitor: RECT,
    dpi: f32,
    /// A tile's side and the room between tiles, DIPs, as they are drawn.
    size: f32,
    spacing: f32,
    /// How far the tile under the pointer swells.
    peak: f32,
    /// The tiles, and the row they make as they lie.
    entries: Vec<Entry>,
    /// Which setting each tile stands for, where one does (a program running has a tile of its own).
    sources: Vec<Option<DockItem>>,
    row: Row,
    /// The row as it was last drawn: what is seen is what a press is judged against.
    live: Row,
    /// Where the pointer is along the row (DIPs from the glass's left), while it is on it.
    focus: Option<f32>,
    /// How far the swell has come up, and how far out of the screen's edge the dock has come.
    reach: Transition,
    slide: Transition,
    /// Gone back into the edge for want of the pointer.
    tucked: bool,
    /// Whether the pointer is on it, and since when it left.
    inside: bool,
    away: Option<Instant>,
    /// The tile the pointer is on, and whether its name is said over it.
    over: Option<usize>,
    said: Option<usize>,
    /// How its glass is tinted, as the screen around it last asked.
    glass: Option<Glass>,
    needed: Option<f32>,
    looked: Instant,
    /// Since when the row was last put together.
    asked: Instant,
    /// In hand: dragged along the edge, or pressed.
    hand: Option<Hand>,
    /// Its timers are armed.
    ticking: bool,
    peeking: bool,
}

impl Dock {
    pub fn new() -> Result<Self> {
        let gfx = gfx::for_main_screen()?;
        let window = Window::see_through()?;
        let mut stage = match Window::see_through() {
            Ok(stage) => stage,
            Err(error) => {
                window.destroy();
                return Err(error);
            }
        };
        stage.set_click_through(true);
        let surface = match Frosted::new(&gfx, stage.hwnd, (glass::BLUR, glass::SATURATION)) {
            Ok(surface) => surface,
            Err(error) => {
                // Tried again at the next sample, with windows of its own.
                window.destroy();
                stage.destroy();
                return Err(error);
            }
        };
        let now = Instant::now();
        Ok(Dock {
            gfx,
            window,
            stage,
            surface,
            showing: None,
            out: false,
            rect: RECT::default(),
            window_at: RECT::default(),
            stage_at: RECT::default(),
            beneath: None,
            menu: None,
            was: DockSettings::default(),
            lang: Lang::En,
            lights: Lights::default(),
            playing: false,
            locked: false,
            through: false,
            auto_hide: false,
            dots: true,
            magnify: true,
            show_open: false,
            clear: 0.0,
            edge: DockEdge::Bottom,
            along: 0.5,
            screen: None,
            work: RECT::default(),
            monitor: RECT::default(),
            dpi: 1.0,
            size: 46.0,
            spacing: 8.0,
            peak: 1.5,
            entries: Vec::new(),
            sources: Vec::new(),
            row: dock::row(&[], None, 46.0, 8.0, 1.0),
            live: dock::row(&[], None, 46.0, 8.0, 1.0),
            focus: None,
            reach: Transition::settled(0.0),
            slide: Transition::settled(1.0),
            tucked: false,
            inside: false,
            away: None,
            over: None,
            said: None,
            glass: None,
            needed: None,
            looked: now,
            asked: now,
            hand: None,
            ticking: false,
            peeking: false,
        })
    }

    /// Whether it is being dragged.
    pub fn held(&self) -> bool {
        self.hand.as_ref().is_some_and(|hand| hand.moved)
    }

    /// Whether the left button is down on it, dragged or barely pressed.
    pub fn in_hand(&self) -> bool {
        self.hand.is_some()
    }

    /// Where its glass stands on the screen (physical px), while it is up and out of the edge it tucks into: none
    /// where there is nothing to put a tile on (see `crate::launchpad`, which drops what is carried onto it).
    pub fn glass(&self) -> Option<RECT> {
        (self.out && !self.tucked).then(|| self.glass_at(1.0))
    }

    /// Puts the dock up as `settings` ask, on the screen they name (the pointer's, where none), above all but
    /// `beneath` — the window it gives way to, an open panel's or the music's full screen, whose picture its glass
    /// would otherwise lie across. Drawn again only where something of it changed.
    pub fn show(
        &mut self,
        settings: &DockSettings,
        lang: Lang,
        lights: Lights,
        playing: bool,
        beneath: Option<HWND>,
    ) {
        if !settings.on {
            return self.hide();
        }
        let before = (self.entries.clone(), self.was.clone());
        // (Rearranged in hand: the order being dragged is the settings' own as it is being changed, so the settings
        // taken up mid-drag keep it rather than snapping the row back; they catch up as the drag is let go.)
        let reordering = self.hand.as_ref().is_some_and(|hand| hand.reorder);
        let kept = if reordering {
            Some(std::mem::take(&mut self.was.items))
        } else {
            None
        };
        self.was = settings.clone();
        if let Some(items) = kept {
            self.was.items = items;
        }
        self.lang = lang;
        self.lights = lights;
        self.playing = playing;
        self.beneath = beneath;
        self.locked = settings.locked;
        self.auto_hide = settings.auto_hide;
        self.dots = settings.running;
        self.magnify = settings.magnify;
        self.show_open = settings.show_open;
        self.clear = settings.clear.clamp(0.0, 1.0);
        self.edge = settings.edge;
        self.through = settings.click_through;
        self.window.set_click_through(self.through);
        // Dragged, the pointer has it: the settings only catch up as the drag is let go.
        if self.hand.is_none() {
            self.along = settings.along.clamp(0.0, 1.0);
        }
        let zoom = settings
            .scale
            .clamp(crate::panel::SIZES.0, crate::panel::SIZES.1);
        let (size, spacing) = settings.measures();
        self.size = size * zoom;
        self.spacing = spacing * zoom;
        self.peak = settings.peak();
        self.screen = settings.screen;
        let point = settings
            .screen
            .map(|(x, y)| POINT { x, y })
            .or_else(cursor)
            .unwrap_or_default();
        (self.work, self.monitor, self.dpi) =
            screen_of(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) });
        self.gather();
        // Asked to be hidden, and the pointer is not at the edge it stands at: gone back into it. (An app enlarged
        // over this screen asks for the same, whatever the settings say.)
        if !self.auto_hide && !self.enlarged() {
            self.tucked = false;
        } else {
            let at = cursor();
            if self.tucked {
                self.tucked = !at.is_some_and(|at| self.calls(at));
            } else {
                self.tucked = !self.has(at);
            }
        }
        self.settle(false);
        if self.tucked && !self.out {
            // It never came out: set at the edge it hides at, rather than slid through the screen to it.
            self.slide.jump(0.0);
        }
        if !self.peeking {
            self.peeking = true;
            unsafe { SetTimer(Some(self.window.hwnd), PEEK_TIMER, PEEK_MS, None) };
        }
        if self.entries != before.0 || self.was != before.1 || !self.out {
            self.settle(true);
            self.draw();
        }
    }

    /// The tiles: what the settings keep, in their order, and where they ask for them, the programs with a window a
    /// person could go to that no tile of its own stands for. Told whether they came out other than they were.
    fn gather(&mut self) -> bool {
        let side = ((self.size * self.peak * self.dpi).round() as u32).clamp(PICTURE.0, PICTURE.1);
        let listed = self.dots || self.show_open;
        let found = listed.then(dock_running::listed).unwrap_or_default();
        let mut entries = Vec::new();
        let mut sources = Vec::new();
        let mut opened: Vec<String> = Vec::new();
        for item in &self.was.items.clone() {
            let entry = match item {
                DockItem::Keep { target, name } => {
                    let path = station::apps::opens(target).path;
                    let running = self.dots
                        && found
                            .iter()
                            .any(|one| one.path.as_deref() == Some(path.as_str()));
                    opened.push(path);
                    Entry {
                        act: Act::Launch(target.clone()),
                        name: name.clone(),
                        picture: pictured(target, side),
                        running,
                    }
                }
                DockItem::Door { door } => Entry {
                    act: Act::Door(*door),
                    name: dock::door_says(*door, self.lang).to_string(),
                    picture: None,
                    running: self.lights.lit(*door),
                },
                DockItem::Trash => Entry {
                    act: Act::Trash,
                    name: dock::trash_says(self.lang).to_string(),
                    picture: None,
                    running: false,
                },
                DockItem::Gap => Entry {
                    act: Act::Gap,
                    name: String::new(),
                    picture: None,
                    running: false,
                },
            };
            entries.push(entry);
            sources.push(Some(item.clone()));
        }
        if self.show_open {
            // (What runs but is kept nowhere goes in after the last gap — the divider between the row's own tiles and
            // the windows that only run here — so the Bin and Settings stay pinned at the row's end, as macOS keeps
            // its trash. No gap: they join the end, as they did.)
            let divider = entries.iter().rposition(|entry| entry.gap());
            let mut at = divider.map_or(entries.len(), |gap| gap + 1);
            let mut runs = false;
            // (One tile per program: the same executable seen from different paths (a multi-process app's children)
            // is only shown once, keyed by its file stem.)
            let mut stems: Vec<String> = Vec::new();
            // (Pre-seed stems from pinned tiles so a running window whose exe matches a
            // pinned tile's name is filtered even if the full path differs slightly.)
            for path in &opened {
                let stem = std::path::Path::new(path.as_str())
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if !stem.is_empty() {
                    stems.push(stem);
                }
            }
            for one in found.iter().filter(|one| {
                !opened
                    .iter()
                    .any(|path| one.path.as_deref() == Some(path.as_str()))
            }) {
                let Some(path) = &one.path else { continue };
                let stem = std::path::Path::new(path.as_str())
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if stems.iter().any(|s| s == &stem) {
                    continue;
                }
                stems.push(stem);
                entries.insert(
                    at,
                    Entry {
                        act: Act::GoTo(path.clone()),
                        name: one.title.clone(),
                        picture: pictured(path, side),
                        running: true,
                    },
                );
                sources.insert(at, None);
                at += 1;
                runs = true;
            }
            // (A second divider to close the running section, so what runs stands between two lines and the Bin and
            // Settings stay pinned beyond them — as macOS brackets its windows. Only where a gap opened the section
            // and windows came out into it; with none, they simply join the end.)
            if runs && divider.is_some() {
                entries.insert(
                    at,
                    Entry {
                        act: Act::Gap,
                        name: String::new(),
                        picture: None,
                        running: false,
                    },
                );
                sources.insert(at, None);
            }
        }
        if entries == self.entries {
            self.asked = Instant::now();
            return false;
        }
        self.entries = entries;
        self.sources = sources;
        self.row = dock::row(&self.entries, None, self.size, self.spacing, self.peak);
        self.asked = Instant::now();
        true
    }

    /// How far out of the screen's edge it is at `share` (1: where it stands, 0: gone back in).
    fn glass_at(&self, share: f32) -> RECT {
        let (wide, tall) = (self.px(self.row.length), self.px(self.row.thickness));
        let inset = self.px(dock::INSET);
        let room = (self.work.right - self.work.left - 2 * inset - wide).max(0);
        let left =
            self.work.left + inset + (self.along.clamp(0.0, 1.0) * room as f32).round() as i32;
        let away = self.tuck(tall) * (1.0 - share.clamp(0.0, 1.0));
        let top = match self.edge {
            DockEdge::Bottom => self.work.bottom - inset - tall + away.round() as i32,
            DockEdge::Top => self.work.top + inset + away.round() as i32,
        };
        RECT {
            left,
            top,
            right: left + wide,
            bottom: top + tall,
        }
    }

    /// How far it has to move to be wholly clear of its screen's edge: gone back that far, the whole of it is off the
    /// screen, its tiles' swelling room and all (which its window reaches past the glass for, on the side away from the
    /// screen's edge), so that the pointer is never taken off what is under where it was.
    fn tuck(&self, tall: i32) -> f32 {
        let rest = match self.edge {
            DockEdge::Bottom => self.monitor.bottom - self.work.bottom,
            DockEdge::Top => self.work.top - self.monitor.top,
        };
        (rest + self.px(dock::INSET) + tall + self.px(dock::overhang(self.size, self.peak).1))
            as f32
    }

    /// The window that takes the pointer: the glass, and the room its tiles swell into.
    fn window_of(&self, glass: RECT) -> RECT {
        let (across, band) = dock::overhang(self.size, self.peak);
        let (side, past) = (self.px(across), self.px(band));
        let (top, bottom) = match self.edge {
            DockEdge::Bottom => (glass.top - past, glass.bottom),
            DockEdge::Top => (glass.top, glass.bottom + past),
        };
        RECT {
            left: glass.left - side,
            top,
            right: glass.right + side,
            bottom,
        }
    }

    /// The window that shows it: that one, and the shadow's room round it.
    fn stage_of(&self, window: RECT) -> RECT {
        let m = self.px(dock::MARGIN);
        RECT {
            left: window.left - m,
            top: window.top - m,
            right: window.right + m,
            bottom: window.bottom + m,
        }
    }

    fn px(&self, dips: f32) -> i32 {
        (dips * self.dpi).round() as i32
    }

    /// A dock along the top of the screen hangs from it; one along the foot stands on it. What swells leaves the
    /// glass by the edge further from the screen's.
    fn at_top(&self) -> bool {
        self.edge == DockEdge::Top
    }

    /// Where the three windows are now, as the swell and the slide have come to.
    fn place_them(&mut self, now: Instant) {
        let share = if crate::panel::reduced_motion() {
            self.slide.target()
        } else {
            self.slide.value(now)
        };
        self.rect = self.glass_at(share);
        self.window_at = self.window_of(self.rect);
        self.stage_at = self.stage_of(self.window_at);
    }

    /// The tile a point of the screen is on, as it was drawn: the one that is seen there (see `crate::ui::dock`).
    pub fn tile_at(&self, at: POINT) -> Option<usize> {
        let (x, y) = (
            (at.x - self.rect.left) as f32 / self.dpi,
            (at.y - self.rect.top) as f32 / self.dpi,
        );
        dock::hit(
            &self.live.tiles,
            self.size,
            self.row.thickness,
            self.at_top(),
            (x, y),
        )
    }

    /// Where the pointer is along the row (DIPs from the glass's left).
    fn along_at(&self, at: POINT) -> f32 {
        (at.x - self.rect.left) as f32 / self.dpi
    }

    /// Whether the pointer is on it, as it is drawn.
    fn has(&self, at: Option<POINT>) -> bool {
        at.is_some_and(|at| {
            (self.window_at.left..self.window_at.right).contains(&at.x)
                && (self.window_at.top..self.window_at.bottom).contains(&at.y)
        })
    }

    /// Whether the pointer calls a hidden dock back: on the screen's edge, over where it stands.
    fn calls(&self, at: POINT) -> bool {
        let stand = self.glass_at(1.0);
        let window = self.window_of(stand);
        let near = self.px(PROBE);
        let along = (window.left..window.right).contains(&at.x);
        along && matches!(self.edge, DockEdge::Bottom if at.y >= self.monitor.bottom - near)
            || matches!(self.edge, DockEdge::Top if at.y <= self.monitor.top + near)
    }

    /// Whether the app in front has been enlarged over this screen: maximized to it (Glance's own windows are no
    /// app to give way to). Where one has, the dock keeps out of its way and tucks as `auto_hide` does, the pointer
    /// at the edge calling it out again (see `peek`).
    fn enlarged(&self) -> bool {
        unsafe {
            let front = GetForegroundWindow();
            if front.is_invalid() || !IsZoomed(front).as_bool() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(front, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return false;
            }
            let mut place = RECT::default();
            if GetWindowRect(front, &mut place).is_err() {
                return false;
            }
            let middle = POINT {
                x: (place.left + place.right) / 2,
                y: (place.top + place.bottom) / 2,
            };
            screen_of(MonitorFromPoint(middle, MONITOR_DEFAULTTONEAREST)).1 == self.monitor
        }
    }

    /// Brings it out, or puts it back, as it should be: `now` for a change that asks to be seen.
    fn settle(&mut self, now: bool) {
        let out = !self.tucked || self.hand.is_some();
        let wanted = if out { 1.0 } else { 0.0 };
        if self.slide.target() == wanted {
            return;
        }
        let (time, easing) = if out { SLIDE } else { SLIDE_OUT };
        if crate::panel::reduced_motion() {
            self.slide.jump(wanted);
            return;
        }
        self.slide.retarget(
            wanted,
            if now { time } else { Duration::ZERO },
            easing,
            Instant::now(),
        );
        self.arm();
    }

    /// Draws it where it is: the row as the pointer and the swell have it, on glass as the screen behind asks.
    fn draw(&mut self) {
        let Ok(gfx) = gfx::for_window(self.stage.hwnd) else { return };
        if !Rc::ptr_eq(&gfx, &self.gfx) {
            self.gone();
            self.gfx = gfx;
        }
        let now = Instant::now();
        self.place_them(now);
        if self.slide.value(now) <= 0.001 && self.tucked {
            return self.put_away();
        }
        self.shown();
        let share = if crate::panel::reduced_motion() {
            self.reach.target()
        } else {
            self.reach.value(now)
        };
        let swollen = dock::row(
            &self.entries,
            self.focus,
            self.size,
            self.spacing,
            self.peak,
        );
        self.live = dock::eased(&swollen, share);
        let dragged = self.hand.as_ref().is_some_and(|hand| hand.moved);
        let (stage, glass) = (self.stage_at, self.rect);
        let (wide, tall) = (glass.right - glass.left, glass.bottom - glass.top);
        let (at_x, at_y) = (glass.left - stage.left, glass.top - stage.top);
        let sheet = (
            (stage.right - stage.left) as u32,
            (stage.bottom - stage.top) as u32,
        );
        let corner = dock::radius(self.row.thickness) * self.dpi;
        let pane = (at_x as f32, at_y as f32, wide as f32, tall as f32, corner);
        let presence = 1.0 - self.clear;
        let tint = self
            .glass
            .unwrap_or_else(|| overlay::glass(&mut [], None).0);
        let glass = Glass { presence, ..tint };
        // Its glass is frosted over the desktop, but never over a game: the compositor would then draw the game
        // behind into it every frame, and cost it frames (see `Overlay::draw`).
        self.surface
            .set_frost(if self.playing { 0.0 } else { presence });
        let entries = &self.entries;
        let live = &self.live;
        let rest = &self.row;
        let (side, at_top, over) = (self.size, self.at_top(), self.over);
        let dpi = self.dpi;
        let painted = self.surface.draw(&self.gfx, sheet, dpi, pane, |frame| {
            frame.origin(at_x as f32 / dpi, at_y as f32 / dpi);
            frame.crisp_text();
            dock::shadow(frame, rest, glass, dragged);
            dock::paint(
                frame, entries, live, glass, side, at_top, over, dragged, dpi,
            );
        });
        if let Err(error) = &painted {
            gfx::failed("dock", &self.gfx, Some(error));
            return;
        }
        self.gfx.sweep();
        self.place();
        if !self.out {
            self.out = true;
            self.window.show();
            self.stage.show();
            self.place();
        }
        crate::ui::tip::above();
    }

    /// Places its windows: the one that takes the pointer above all but the open panel, its stage just beneath it.
    fn place(&self) {
        self.window.place_under(self.window_at, self.beneath);
        self.menu_above();
        self.stage
            .place_under(self.stage_at, Some(self.window.hwnd));
    }

    /// Looks at the screen around it, and tints the glass for it (see `crate::ui::overlay::glass`).
    fn look(&mut self) {
        let around = self.px(AROUND);
        let (r, s) = (self.stage_at, self.monitor);
        let area = RECT {
            left: (r.left - around).max(s.left),
            top: (r.top - around).max(s.top),
            right: (r.right + around).min(s.right),
            bottom: (r.bottom + around).min(s.bottom),
        };
        let seen = Capture::take(area);
        let mut behind = seen
            .map(|capture| capture.luminances(area, self.rect, self.dpi.round().max(1.0) as i32))
            .unwrap_or_default();
        let (glass, needed) = overlay::glass(&mut behind, self.glass.zip(self.needed));
        (self.glass, self.needed, self.looked) = (Some(glass), Some(needed), Instant::now());
    }

    /// What the pointer does to it while it is on it, and what leaves it: its swell, the tile it is on, and its name
    /// said over that. Told whether the row has to be drawn again.
    fn hover(&mut self, at: Option<POINT>) -> bool {
        let inside = self.out && self.has(at);
        let over = inside
            .then(|| self.tile_at(at.unwrap_or_default()))
            .flatten();
        let mut again = false;
        if inside != self.inside {
            self.inside = inside;
            let (time, easing) = REACH;
            if crate::panel::reduced_motion() {
                self.reach.jump(if inside { 1.0 } else { 0.0 });
            } else {
                self.reach
                    .retarget(if inside { 1.0 } else { 0.0 }, time, easing, Instant::now());
                self.arm();
            }
            self.away = if inside { None } else { Some(Instant::now()) };
            again = true;
        }
        if inside {
            let focus = self.along_at(at.unwrap_or_default());
            if Some(focus) != self.focus {
                self.focus = Some(focus);
                again = true;
            }
        } else if self.focus.take().is_some() {
            again = true;
        }
        if over != self.over {
            self.over = over;
            again = true;
        }
        self.say(over);
        again
    }

    /// The tile the pointer is on, said over it; none, the saying gone.
    fn say(&mut self, over: Option<usize>) {
        if over == self.said && over.is_some() {
            return;
        }
        self.said = over;
        match over.filter(|_| self.out) {
            Some(index) => {
                let entry = &self.entries[index];
                if entry.gap() || entry.name.is_empty() {
                    crate::ui::tip::hide();
                    return;
                }
                let (x, y, wide, tall) = dock::box_of(
                    self.live.tiles[index],
                    self.size,
                    self.row.thickness,
                    self.at_top(),
                );
                let about = RECT {
                    left: self.rect.left + self.px(x),
                    top: self.rect.top + self.px(y),
                    right: self.rect.left + self.px(x + wide),
                    bottom: self.rect.top + self.px(y + tall),
                };
                crate::ui::tip::show(self.window.hwnd, &entry.name, about, crate::os::apps_dark());
            }
            None => crate::ui::tip::hide(),
        }
    }

    /// Its timers are armed while something is moving, and the pointer is followed while it is in hand.
    fn arm(&mut self) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        unsafe { SetTimer(Some(self.window.hwnd), TICK_TIMER, TICK_MS, None) };
    }

    fn still(&mut self) {
        self.ticking = false;
        unsafe {
            let _ = KillTimer(Some(self.window.hwnd), TICK_TIMER);
        }
    }

    /// Every frame of what is moving: drawn again, and the timer let go of when nothing is.
    pub fn step(&mut self) {
        let now = Instant::now();
        if self.hand.as_ref().is_some_and(|hand| hand.moved) {
            self.drag(cursor());
        }
        let moving = !self.reach.done(now) || !self.slide.done(now);
        self.draw();
        if !moving && !self.held() {
            self.still();
        }
    }

    /// The pointer, what runs, and what is behind it, looked at while nothing else says to look: the dock's own
    /// slow watch (see `PEEK_TIMER`).
    pub fn peek(&mut self) {
        let now = Instant::now();
        let at = cursor();
        // Where the pointer is, when no message said: followed at this pace, the swell easing to it.
        let again = self.hover(at);
        let hiding = self.auto_hide || self.enlarged();
        if hiding && !self.held() {
            if self.tucked {
                // Out again: as soon as the pointer is at the edge it stands at.
                if at.is_some_and(|at| self.calls(at)) {
                    self.tucked = false;
                    self.settle(true);
                }
            } else if self.inside {
                self.away = None;
            } else if self
                .away
                .is_some_and(|since| now.duration_since(since) >= TUCK_AFTER)
            {
                self.tucked = true;
                self.settle(true);
            }
        } else if !hiding && self.tucked && !self.held() {
            // (The app gave its size back: out of the edge of its own accord again, where it was only the
            // enlarging that put it away.)
            self.tucked = false;
            self.settle(true);
        }
        if again && self.out {
            self.draw();
        }
        if !self.out {
            return;
        }
        // What runs, and what is pictured of it, taken again: drawn only where the row came out other than it was.
        if now.duration_since(self.asked) >= AGAIN_EVERY && self.gather() {
            self.draw();
        }
        if now.duration_since(self.looked) >= LOOK_EVERY {
            let was = self.glass;
            self.look();
            if self.glass != was {
                self.draw();
            }
        }
    }

    /// The left button went down on it: pressed, or taken in hand to be moved along its edge.
    pub fn press(&mut self) {
        let Some(from) = cursor() else { return };
        let tile = self.tile_at(from);
        // (A drag begun on a tile the settings keep — a door, a kept thing, the Bin, a gap — rearranges the row; one
        // begun on empty glass, or on a window that only runs here, slides the whole dock along its edge, as it did.)
        let item = tile
            .and_then(|index| self.sources.get(index).cloned())
            .flatten();
        let reorder = !self.locked && item.is_some();
        self.hand = Some(Hand {
            from,
            tile,
            along: self.along,
            moved: false,
            reorder,
            item,
        });
        if !self.locked {
            unsafe { SetCapture(self.window.hwnd) };
            self.arm();
        }
        self.draw();
    }

    /// The pointer moved over it, or, in hand, wherever it goes.
    pub fn moved(&mut self) {
        let Some(at) = cursor() else { return };
        match &self.hand {
            Some(hand) if !self.locked => {
                if !hand.moved && (at.x - hand.from.x).abs() as f32 / self.dpi > DRAG {
                    if let Some(hand) = &mut self.hand {
                        hand.moved = true;
                        // In hand: out of the edge it comes, whatever asked for it, and deep blue till it is let go.
                        self.tucked = false;
                        self.slide.jump(1.0);
                    }
                }
                if self.held() {
                    if self.hand.as_ref().is_some_and(|hand| hand.reorder) {
                        self.reorder(Some(at));
                    } else {
                        self.drag(Some(at));
                    }
                    return;
                }
            }
            _ => {}
        }
        self.window.point(if self.over.is_some() && !self.locked {
            IDC_HAND
        } else {
            IDC_ARROW
        });
        if self.hover(Some(at)) {
            self.draw();
        }
    }

    /// Dragged: along its edge with the pointer, and to the other edge of its screen when it is carried there.
    fn drag(&mut self, at: Option<POINT>) {
        let (Some(at), Some(hand)) = (at, self.hand.as_ref()) else {
            return;
        };
        let inset = dock::INSET * self.dpi;
        let room =
            (self.work.right - self.work.left) as f32 - 2.0 * inset - self.row.length * self.dpi;
        if room > 0.0 {
            let from = hand.along * room + (at.x - hand.from.x) as f32;
            self.along = (from / room).clamp(0.0, 1.0);
        }
        // Carried past the middle of its screen: it goes to the other edge, and stands on it from there.
        let tall = self.px(self.row.thickness);
        let past = self.px(40.0);
        match self.edge {
            DockEdge::Bottom
                if at.y < self.work.top + tall + past + self.px(self.peak * self.size) =>
            {
                self.edge = DockEdge::Top
            }
            DockEdge::Top
                if at.y > self.work.bottom - tall - past - self.px(self.peak * self.size) =>
            {
                self.edge = DockEdge::Bottom
            }
            _ => {}
        }
        let stand = self.glass_at(1.0);
        // (The pointer kept over the glass it moves: its own row follows along with it.)
        let across = (at.x - stand.left) as f32 / self.dpi;
        self.focus = Some(across.clamp(0.0, self.row.length));
        self.rect = stand;
        self.over = self.tile_at(at);
        self.say(self.over);
        self.draw();
    }

    /// Dragged a tile the settings keep: the row rearranged under the pointer. The tile carried is traded with
    /// whichever kept tile it is brought over, one swap at a time, so the row follows the hand; the dock itself
    /// stands where it did. A window that only runs here (kept nowhere) is nothing to trade with.
    fn reorder(&mut self, at: Option<POINT>) {
        let Some(at) = at else { return };
        let Some(dragged) = self.hand.as_ref().and_then(|hand| hand.item.clone()) else {
            return;
        };
        let Some(index) = self.tile_at(at) else {
            return;
        };
        let Some(over) = self.sources.get(index).cloned().flatten() else {
            return;
        };
        if over == dragged {
            return;
        }
        let from = self.was.items.iter().position(|one| *one == dragged);
        let to = self.was.items.iter().position(|one| *one == over);
        if let (Some(from), Some(to)) = (from, to) {
            self.was.items.swap(from, to);
            let along = self.along_at(at);
            self.focus = Some(along);
            self.gather();
            self.draw();
        }
    }

    /// Let go: where it was moved to, what was pressed on it, and, rearranged, the order the row was dragged into
    /// (each none where it was not the doing).
    pub fn release(&mut self) -> Let {
        let Some(hand) = self.hand.take() else {
            return Let {
                placed: None,
                act: None,
                items: None,
            };
        };
        unsafe {
            let _ = ReleaseCapture();
        }
        if !hand.moved {
            let at = cursor().unwrap_or(hand.from);
            let act = self
                .tile_at(at)
                .filter(|&index| Some(index) == hand.tile)
                .and_then(|index| match &self.entries[index].act {
                    Act::Gap => None,
                    act => Some(act.clone()),
                });
            self.still();
            return Let {
                placed: None,
                act,
                items: None,
            };
        }
        if hand.reorder {
            // (Rearranged rather than slid: the dock stays where it stands, the swell is let go, and the order the row
            // was dragged into is handed over as the settings' own — written on another thread, as with every other
            // change, so the disk is not waited on here.)
            self.focus = None;
            self.over = None;
            self.still();
            self.draw();
            return Let {
                placed: None,
                act: None,
                items: Some(self.was.items.clone()),
            };
        }
        // Where it was left: along its edge, at which edge, on the screen it stands over.
        let stand = self.glass_at(1.0);
        let centre = POINT {
            x: (stand.left + stand.right) / 2,
            y: (stand.top + stand.bottom) / 2,
        };
        (self.work, self.monitor, self.dpi) =
            screen_of(unsafe { MonitorFromPoint(centre, MONITOR_DEFAULTTONEAREST) });
        self.screen = Some((centre.x, centre.y));
        self.settle(true);
        self.draw();
        Let {
            placed: Some(Placed {
                along: self.along,
                edge: self.edge,
                point: (centre.x, centre.y),
            }),
            act: None,
            items: None,
        }
    }

    /// The capture taken from it by something else (a UAC prompt, the lock screen): nothing was done.
    pub fn lost(&mut self) {
        if self.hand.take().is_some() {
            self.still();
            self.settle(false);
            self.draw();
        }
    }

    /// Its menu: the tile under `at`'s, where there is one, and the dock's own over the glass between them. With
    /// what each of its rows asks for.
    pub fn menu(&self, at: POINT, over_the_panel: bool) -> Option<(Menu, Vec<Choice>)> {
        let tile = self.tile_at(at);
        let zh = self.lang == Lang::Zh;
        let say = |zh_text: &'static str, en: &'static str| if zh { zh_text } else { en };
        let mut rows: Vec<(Choice, &'static str, Icon, bool, bool)> = Vec::new();
        let entry = tile
            .filter(|index| *index < self.entries.len())
            .map(|index| &self.entries[index]);
        if let Some(entry) = entry {
            match &entry.act {
                Act::Gap => {}
                Act::GoTo(path) => {
                    rows.push((
                        Choice::Act(Act::GoTo(path.clone())),
                        say("转到窗口", "Go to its window"),
                        Icon::Panel,
                        false,
                        false,
                    ));
                    // (A window of a program the row does not keep: it can be kept, so that it stays after it is closed.)
                    if tile.is_some_and(|index| {
                        self.sources.get(index).and_then(Option::as_ref).is_none()
                    }) {
                        let name = entry.name.clone();
                        rows.push((
                            Choice::Add(DockItem::Keep {
                                target: path.clone(),
                                name,
                            }),
                            say("固定到 Dock", "Keep on the dock"),
                            Icon::Pin,
                            false,
                            false,
                        ));
                    }
                }
                Act::Launch(target) => {
                    let running = self.was.running && dock_running::for_target(target).is_some();
                    rows.push((
                        Choice::Act(Act::Launch(target.clone())),
                        if running {
                            say("转到窗口", "Go to its window")
                        } else {
                            say("打开", "Open")
                        },
                        Icon::Panel,
                        false,
                        false,
                    ));
                    // (The administrator's leave is asked of what runs by itself; a Store app's package is not.)
                    if !target.contains('!') {
                        rows.push((
                            Choice::Admin(target.clone()),
                            say("以管理员身份运行", "Run as administrator"),
                            Icon::Shield,
                            false,
                            false,
                        ));
                    }
                    let here = station::apps::App {
                        target: target.clone(),
                        name: entry.name.clone(),
                    }
                    .location();
                    if here.is_some() {
                        rows.push((
                            Choice::Where(target.clone()),
                            say("打开所在位置", "Open its location"),
                            Icon::Folder,
                            false,
                            false,
                        ));
                    }
                }
                Act::Door(door) => rows.push((
                    Choice::Act(Act::Door(*door)),
                    say("打开", "Open"),
                    Icon::Panel,
                    false,
                    false,
                )),
                Act::Trash => rows.push((
                    Choice::Act(Act::Trash),
                    say("打开", "Open"),
                    Icon::Trash,
                    false,
                    false,
                )),
            }
            let rule = rows.len() == 1;
            if let Some(item) = tile
                .and_then(|index| self.sources.get(index).cloned())
                .flatten()
            {
                rows.push((
                    Choice::Remove(item),
                    say("从 Dock 移除", "Take off the dock"),
                    Icon::PinOff,
                    false,
                    rule,
                ));
            }
        }
        if entry.is_none() {
            rows.push((
                Choice::Lock(!self.locked),
                say("锁定位置", "Lock position"),
                Icon::Pin,
                self.locked,
                false,
            ));
            rows.push((
                Choice::Through(!self.through),
                say("鼠标穿透", "Let clicks through"),
                Icon::Pointer,
                self.through,
                false,
            ));
            rows.push((
                Choice::Hide(!self.auto_hide),
                say("自动隐藏", "Hide until needed"),
                Icon::EyeOff,
                self.auto_hide,
                false,
            ));
            rows.push((
                Choice::Magnify(!self.magnify),
                say("悬停放大", "Grow under the pointer"),
                Icon::Search,
                self.magnify,
                false,
            ));
            rows.push((
                Choice::Dots(!self.dots),
                say("显示运行指示", "Mark what is running"),
                Icon::CircleDot,
                self.dots,
                false,
            ));
            rows.push((
                Choice::ShowOpen(!self.show_open),
                say("显示打开的窗口", "Show open windows"),
                Icon::Layers,
                self.show_open,
                false,
            ));
            let to_top = match self.edge {
                DockEdge::Bottom => DockEdge::Top,
                DockEdge::Top => DockEdge::Bottom,
            };
            rows.push((
                Choice::Edge(to_top),
                if to_top == DockEdge::Top {
                    say("移到屏幕顶部", "Move to the top edge")
                } else {
                    say("移到屏幕底部", "Move to the bottom edge")
                },
                Icon::ArrowUpToLine,
                false,
                false,
            ));
            // (Its tile only offered where the row has none: the screen of every program, reached from the dock.)
            let keeps = self.was.items.iter().any(|item| {
                *item
                    == DockItem::Door {
                        door: DockDoor::Launchpad,
                    }
            });
            if !keeps {
                rows.push((
                    Choice::Launchpad,
                    say("添加启动台", "Add the launchpad"),
                    Icon::LayoutGrid,
                    false,
                    false,
                ));
            }
            rows.push((
                Choice::Divider,
                say("添加空位", "Add a gap"),
                Icon::Minus,
                false,
                false,
            ));
            rows.push((
                Choice::Settings,
                say("设置…", "Settings…"),
                Icon::Settings,
                false,
                true,
            ));
            rows.push((
                Choice::Close,
                say("关闭 Dock", "Close the dock"),
                Icon::Close,
                false,
                false,
            ));
        }
        if rows.is_empty() {
            return None;
        }
        let items = rows
            .iter()
            .map(|(_, label, icon, checked, rule_before)| Item {
                label,
                mark: Some(crate::ui::menu::Mark::Icon(*icon)),
                checked: *checked,
                rule_before: *rule_before,
            })
            .collect();
        let menu = Menu::open(items, Look::system(), over_the_panel)?;
        Some((menu, rows.into_iter().map(|(choice, ..)| choice).collect()))
    }

    /// Kept under `menu`'s window while one is up (none: no menu is): placed above all as it is drawn, it covered the
    /// menu, and a click on it went to the menu underneath.
    pub fn under_menu(&mut self, menu: Option<HWND>) {
        self.menu = menu;
        self.menu_above();
    }

    fn menu_above(&self) {
        if let Some(menu) = self.menu {
            unsafe {
                let _ = SetWindowPos(
                    menu,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Hidden for good (its settings turned off): its windows away, its timers gone.
    pub fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.window.hwnd), PEEK_TIMER);
            let _ = KillTimer(Some(self.window.hwnd), TICK_TIMER);
        }
        (self.peeking, self.ticking) = (false, false);
        if self.hand.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.inside = false;
        self.focus = None;
        self.reach.jump(0.0);
        self.slide.jump(1.0);
        self.tucked = false;
        self.put_away();
    }

    /// Its windows away, what it draws with let go of: it may be brought back, and will be drawn again when.
    fn put_away(&mut self) {
        if !self.out {
            return;
        }
        self.out = false;
        self.over = None;
        self.said = None;
        self.window.hide();
        self.stage.hide();
        crate::ui::tip::hide();
        crate::widget::own_window_changed(self.stage_at);
        self.hidden();
    }
}

/// A tile's picture: the shell's own for what it stands, where it stands for a file.
pub(crate) fn pictured(target: &str, side: u32) -> Option<crate::ui::canvas::Picture> {
    (!target.starts_with("https://") && !target.starts_with("http://"))
        .then(|| crate::station::thumbs::icon(std::path::Path::new(target), side))
        .flatten()
}

/// A launchpad tile's picture: the shell's icon set on a plate of its own colours where it is a thin glyph, as on a
/// Mac's (see `crate::station::thumbs::plated`); a whole tile's picture, a modern app's, as it is.
pub(crate) fn plated(target: &str, side: u32) -> Option<crate::ui::canvas::Picture> {
    (!target.starts_with("https://") && !target.starts_with("http://"))
        .then(|| crate::station::thumbs::plated(std::path::Path::new(target), side))
        .flatten()
}
