//! The launchpad: every program the Start menu lists, a screen of tiles at a time, and the search written in at its
//! head.
//!
//! It is Glance's own window, as the dock is (see `crate::dock`): one window over the screen it opens on, its glass
//! frosted over what is behind it, drawn by the panel's thread. It never holds the focus but while it is up, and gives
//! the keys back as Esc is pressed, or as one of its tiles is opened (see `crate::ui::keys`).
//!
//! What it shows is read as it comes: the shell's list of programs, and the picture of each as the shell gives it,
//! a thin glyph set on a plate of its own colours as on a Mac's (see `crate::station::thumbs::plated`), a tile of
//! its name's first letter till then. Pressing a tile opens what it is; pressing
//! its right button opens it, opens it with the administrator's leave, fixes it to the dock, or shows where it is.
//! Carrying one tile onto another merges them into a folder, which is pressed open and stood in again, named, and
//! taken apart (see `crate::settings::LaunchFolder`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LRESULT, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, KillTimer, SetTimer, IDC_ARROW, IDC_HAND, IDC_IBEAM, WM_CHAR, WM_KEYDOWN,
};

use crate::dock::plated;
use crate::settings::{DockItem, DockSettings, LaunchFolder};
use crate::station::{apps, found};
use crate::ui::canvas::{Canvas, Color};
use crate::ui::dock::overhang;
use crate::ui::gfx::{self, Gfx};
use crate::ui::glass::{self, Frosted};
use crate::ui::icons::Icon;
use crate::ui::keys::{self, Answer};
use crate::ui::launchpad::{self as shape, Grid, Hand, Tile};
use crate::ui::life::{Lives, Showing};
use crate::ui::menu::{Item, Look, Menu};
use crate::ui::motion::{Easing, Transition};
use crate::ui::text::Lang;
use crate::ui::typing::Typing;
use crate::ui::window::Window;

/// What its menu offers, for the tile the pointer is on: each row, as the panel is told it was chosen. A folder is
/// named by its place in the settings' list, which is where it is taken apart and renamed.
#[derive(Clone, PartialEq, Debug)]
pub enum Choice {
    /// Opened, or gone to where it is running.
    Open(String),
    /// Opened with the administrator's leave (the shell asked, and the person with it).
    Admin(String),
    /// Fixed to the dock, or taken off it: whether it stands there now decides which.
    Keep(String),
    /// Shown where it is.
    Where(String),
    /// Taken off this machine: the word its installer left in the registry, run as it was left (see
    /// `apps::uninstaller`). Only offered where that word is there to be had, and never for a program's own door out,
    /// which is no tile at all (see `apps::is_uninstaller`).
    Uninstall(String),
    /// Taken out of the folder it stands in, and left among the tiles on its own.
    Out(String),
    /// A folder stood in: its tiles are the page.
    Enter(usize),
    /// A folder's name written again in the place at the head.
    Name(usize),
    /// A folder taken apart, its tiles back among the rest.
    Break(usize),
    /// The dock's page in the settings.
    Settings,
}

/// What the panel is asked to do after a menu row was chosen: open a program, open one with the administrator's leave,
/// or show one where it is. (Glance's own doors and the Recycle Bin are the dock's business; these are said here in the
/// same words.)
#[derive(Clone, PartialEq, Debug)]
pub enum Follow {
    Launch(String),
    LaunchAs(String),
    Reveal(String),
    /// An uninstaller's own words, as the registry gave them (see `apps::command_line`).
    Uninstall(String),
}

/// What letting go of a tile asks of the panel: the program opened, or kept on the dock under the name it goes by
/// there — which is what a tile carried to the strip at the foot asks.
#[derive(Clone, PartialEq, Debug)]
pub enum Carry {
    Open(String),
    Pin { target: String, name: String },
}

/// What one of the page's tiles stands for: a program, of the list the shell gave, or a folder of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cell {
    App(usize),
    Group(usize),
}

/// How often it is drawn while it is up (ms): its screen coming up, its caret's blink, and the programs and their
/// pictures coming in.
pub const TICK_TIMER: usize = 23;
const TICK_MS: u32 = 60;
/// How often the shell's list, and the pictures of what it lists, are looked at again.
const AGAIN: Duration = Duration::from_millis(AGAIN_MS);
const AGAIN_MS: u64 = 300;
/// How the screen comes up under it, and how a page of tiles comes in after it.
const WASH: (Duration, Easing) = (Duration::from_millis(180), Easing(0.16, 1.0, 0.3, 1.0));
const TURN: (Duration, Easing) = (Duration::from_millis(150), Easing(0.2, 0.9, 0.3, 1.0));
/// How long the caret is shown for, and how it blinks after that (ms).
const SHOWN: Duration = Duration::from_millis(1060);
const BLINK: Duration = Duration::from_millis(530);
/// How long a search may be.
const MOST: usize = 120;
/// How clear a tile's picture is asked for, at least (physical px).
const LEAST: u32 = 32;
/// How far the pointer has to go from where its button went down before a tile is taken up to be carried (DIPs). A
/// press and a let-go open a program, so the take-up has to be a deliberate one: the system's own distance for a drag
/// is half a box, barely a tremor of the hand.
const PICK: f32 = 8.0;

fn cursor() -> Option<POINT> {
    let mut at = POINT::default();
    unsafe { GetCursorPos(&mut at) }.ok().map(|_| at)
}

/// What the keys write into. Out on the whole screen that is the search; standing in a folder there is nothing to
/// search for — its tiles are the page whatever is written — so what is typed there is either the folder's name being
/// written again, or nothing at all (see `relayout`, which settles it, and `write_name`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Writing {
    Search,
    Name,
    Nothing,
}

/// What is written in its search, and what the keys asked of it: shared with the answer the keys are given (see
/// `keys::take`), which is called from inside a message, where none of the window's own work may be done.
struct Words {
    text: Typing,
    /// A folder's name being written again, kept whole apart from the search: what is typed at a folder's head never
    /// reaches the search, and the search is the same words again as soon as the writing is done with.
    name: Typing,
    /// Where what is typed now goes: the search, a folder's name being written again, or nowhere.
    writing: Writing,
    /// Enter was pressed: the picked tile — or the first of what is shown, where none is picked — is to be opened, or
    /// a folder's name written again.
    enter: bool,
    /// Something was written since it was last told: the caret's blink begins again.
    edited: bool,
    /// The page it shows, and how many there are.
    page: usize,
    pages: usize,
    /// The keys' pick: the tile ringed for opening with Enter, as the arrows walk the page (none till they are
    /// pressed, or a tile is stood on).
    picked: Option<usize>,
    /// How the tiles lie, and how many there are: kept here because the keys are answered where no window is at hand
    /// to be asked (see `relayout`).
    cols: usize,
    rows: usize,
    count: usize,
    /// The pick walked onto another page: its number, to be turned to when the page is next told (a message answers
    /// the keys, but turns no page itself).
    crossed: Option<usize>,
}

impl Words {
    /// The words what is typed now goes into — the search, or a folder's name at the head of its page — and none
    /// where there is nothing to write into (see `Writing`).
    fn pen(&self) -> Option<&Typing> {
        match self.writing {
            Writing::Search => Some(&self.text),
            Writing::Name => Some(&self.name),
            Writing::Nothing => None,
        }
    }

    /// `pen`, for writing into.
    fn pen_mut(&mut self) -> Option<&mut Typing> {
        match self.writing {
            Writing::Search => Some(&mut self.text),
            Writing::Name => Some(&mut self.name),
            Writing::Nothing => None,
        }
    }

    /// What is typed, put in place of the selection.
    fn put(&mut self, units: &[u16]) {
        let Some(pen) = self.pen_mut() else { return };
        pen.put(units);
        self.edited = true;
    }

    /// The selection, or where there is none one character by the caret, taken away.
    fn erase(&mut self, forward: bool) {
        let Some(pen) = self.pen_mut() else { return };
        pen.erase(forward);
        self.edited = true;
    }

    /// The caret at `to` in what is typed.
    fn go(&mut self, to: usize, extend: bool) {
        if let Some(pen) = self.pen_mut() {
            pen.go(to, extend);
        }
    }

    /// Where the caret stands, and how far what is typed reaches.
    fn caret(&self) -> usize {
        self.pen().map(|pen| pen.caret).unwrap_or(0)
    }

    fn reach(&self) -> usize {
        self.pen().map(|pen| pen.text.len()).unwrap_or(0)
    }

    /// One character on (or back) from `at`: nowhere to step where there is nothing to type into.
    fn step(&self, at: usize, forward: bool) -> usize {
        self.pen().map(|pen| pen.step(at, forward)).unwrap_or(at)
    }

    /// Nothing typed at all — which is also what it is where there is nothing to type into, so that the keys which
    /// walk the tiles at the ends of what is written keep walking them there.
    fn blank(&self) -> bool {
        self.pen().map(|pen| pen.text.is_empty()).unwrap_or(true)
    }
}

impl Lives for Launchpad {
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

pub struct Launchpad {
    gfx: Rc<Gfx>,
    /// Counted as shown on its device while it is (see `life`).
    showing: Option<Showing>,
    window: Window,
    surface: Frosted,
    /// Whether it is up.
    out: bool,
    /// The screen it covers, and its scale.
    rect: RECT,
    dpi: f32,
    lang: Lang,
    dark: bool,
    /// Every program the shell lists: its tiles, and the shell's own read of it, for the search to match names and
    /// pinyin against.
    apps: Vec<Tile>,
    sources: Arc<Vec<found::Found>>,
    /// The folders its tiles are merged into, as the settings keep them.
    folders: Vec<LaunchFolder>,
    /// The tiles the page shows, and what each of them stands for.
    tiles: Vec<Tile>,
    what: Vec<Cell>,
    /// The folder stood in, where the page is its tiles rather than the whole screen's; and the folder whose name is
    /// being written again.
    inside: Option<usize>,
    renaming: Option<usize>,
    grid: Grid,
    /// The side the whole screen's page gave its tiles last it was laid out: a folder stood in keeps that side, so its
    /// tiles are the same tiles laid for themselves and the pictures already fetched are the ones wanted there (see
    /// `Grid::inside`).
    page_side: f32,
    /// How the settings ask the page to lie: (across, down), either 0 leaving it to the screen (see `Grid::asked`).
    pad: (usize, usize),
    /// How far the dock's tiles reach past its glass at full peak: the dock's window takes the pointer over all of
    /// that band (the glass no bigger for it), so the dots must clear the band whole to be clickable (see `overhang`).
    dock_room: f32,
    /// The tile the pointer is on, of the page it shows, the one its left button went down on, and the one it has
    /// taken up to carry onto another.
    over: Option<usize>,
    pressed: Option<usize>,
    held: Option<usize>,
    from: POINT,
    /// Whether the button went down on a folder's name at its head: written there and then, it is not written and done
    /// with by a click on the name itself.
    on_title: bool,
    /// Where the dock's glass stands (physical px, of the screen it is on): told it by the panel as the pointer moves,
    /// and where a carried tile is let go to be kept there.
    dock: Option<RECT>,
    /// How far the screen has come up, and how far the page's tiles have.
    wash: Transition,
    fade: Transition,
    words: Rc<RefCell<Words>>,
    /// Its window takes the keys, and had them from `before`.
    taking: bool,
    before: HWND,
    /// Since when the caret has been where it is, and when the programs were last looked at.
    blink: Instant,
    asked: Instant,
    /// The side a tile's picture was last asked for (physical px).
    asked_side: u32,
    ticking: bool,
}

impl Launchpad {
    pub fn new() -> Result<Self> {
        let gfx = gfx::for_main_screen()?;
        let window = Window::see_through()?;
        let surface = match Frosted::new(&gfx, window.hwnd, (glass::BLUR, glass::SATURATION)) {
            Ok(surface) => surface,
            Err(error) => {
                window.destroy();
                return Err(error);
            }
        };
        let now = Instant::now();
        Ok(Launchpad {
            gfx,
            window,
            surface,
            showing: None,
            out: false,
            rect: RECT::default(),
            dpi: 1.0,
            lang: Lang::En,
            dark: false,
            apps: Vec::new(),
            sources: Arc::new(Vec::new()),
            folders: Vec::new(),
            tiles: Vec::new(),
            what: Vec::new(),
            inside: None,
            renaming: None,
            grid: Grid::of(1000.0, 700.0, 0),
            page_side: Grid::of(1000.0, 700.0, 0).side,
            pad: (0, 0),
            dock_room: 0.0,
            over: None,
            pressed: None,
            held: None,
            from: POINT::default(),
            on_title: false,
            dock: None,
            wash: Transition::settled(0.0),
            fade: Transition::settled(1.0),
            words: Rc::new(RefCell::new(Words {
                text: Typing::most(MOST),
                name: Typing::most(MOST),
                writing: Writing::Search,
                enter: false,
                edited: false,
                page: 0,
                pages: 1,
                picked: None,
                cols: 1,
                rows: 1,
                count: 0,
                crossed: None,
            })),
            taking: false,
            before: HWND::default(),
            blink: now,
            asked: now,
            asked_side: 0,
            ticking: false,
        })
    }

    /// Its window's own, for the panel to tell its messages from another's.
    pub fn owns(&self, hwnd: HWND) -> bool {
        self.window.hwnd == hwnd
    }

    /// Whether it is up.
    pub fn is_up(&self) -> bool {
        self.out
    }

    /// The screen the pointer is on: the one it opens on, and the one it is drawn to.
    fn look_at(&mut self) {
        let at = cursor().unwrap_or_default();
        let monitor = unsafe { MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
        let (mut dpi, mut unused) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused) };
        self.rect = info.rcMonitor;
        self.dpi = dpi as f32 / 96.0;
    }

    /// Its screen in DIPs: how wide, how tall.
    fn size(&self) -> (f32, f32) {
        (
            (self.rect.right - self.rect.left) as f32 / self.dpi,
            (self.rect.bottom - self.rect.top) as f32 / self.dpi,
        )
    }

    /// Up on the screen the pointer is on: the programs taken, the grid laid out, the keys taken, the screen drawn up
    /// under them. `beneath` (the open panel's window) is kept over it.
    pub fn open(&mut self, settings: &DockSettings, lang: Lang, dark: bool, beneath: Option<HWND>) {
        self.lang = lang;
        self.dark = dark;
        self.pad = (settings.pad_cols, settings.pad_rows);
        self.dock_room = overhang(settings.measures().0, settings.peak()).1;
        self.look_at();
        // (The shell's read taken again in the background: a program installed since the last look comes in when it
        //  lands, the tick folding it in. Never waited for, so the page still comes up at once on what was read before.)
        found::look(|| {});
        self.names(settings);
        {
            let mut words = self.words.borrow_mut();
            (words.page, words.enter, words.picked, words.crossed) = (0, false, None, None);
            words.text.put(&[]);
        }
        self.clear();
        (
            self.inside,
            self.renaming,
            self.held,
            self.pressed,
            self.over,
        ) = (None, None, None, None, None);
        self.wash.jump(0.0);
        self.fade.jump(0.0);
        self.relayout();
        self.pictures();
        self.window.place(self.rect);
        self.window.show();
        self.out = true;
        if let Some(panel) = beneath {
            // (The panel, open, keeps over it: where Glance's own windows are together.)
            self.window.place_under(self.rect, Some(panel));
        }
        self.take_keys();
        self.arm(true);
        self.draw();
    }

    /// Down: the keys given back to whoever had them, and its screen left.
    pub fn close(&mut self) {
        self.give_keys();
        if !self.out {
            return;
        }
        self.out = false;
        self.arm(false);
        self.window.hide();
        self.surface.release();
        let holding = self.pressed.is_some() || self.held.is_some();
        (self.over, self.pressed, self.held) = (None, None, None);
        if holding {
            // (Only its own capture, and only while it holds a tile: another window's drag is left to it.)
            unsafe {
                ReleaseCapture().ok();
            }
        }
    }

    /// Told that one of its own messages was answered (see `keys`): its tick, and what the keys wrote. The
    /// programs taken again, the tiles laid out, the screen drawn; and the program to open, where Enter asked for
    /// the tile the keys had picked — or the first of what is shown, where they had picked none.
    pub fn told(&mut self, hwnd: HWND, settings: &DockSettings) -> Option<String> {
        if !self.owns(hwnd) {
            return None;
        }
        let enter = std::mem::take(&mut self.words.borrow_mut().enter);
        if enter && self.renaming.is_some() {
            // (Enter in a folder's name: written again, and the writing done with.)
            self.name_it();
            return None;
        }
        // (The pick walked onto another page between one telling and this: turn to it, and keep it ringed there —
        // the page's own turning would else drop the pick with it.)
        let crossed = std::mem::take(&mut self.words.borrow_mut().crossed);
        if let Some(page) = crossed {
            let kept = self.words.borrow().picked;
            self.turn(page);
            self.words.borrow_mut().picked = kept;
        }
        let mut wanted = None;
        if enter {
            // (Enter opens the tile the keys picked, or the first of the page shown where they picked no
            // one: a folder's tile, either way, is stood in rather than opened.)
            let (from, on) = self.page_of();
            let picked = self
                .words
                .borrow()
                .picked
                .filter(|at| *at >= from && *at < from + on)
                .map(|at| at - from)
                .unwrap_or(0);
            if on > 0 {
                if let Some(cell) = self.what.get(from + picked).copied() {
                    match cell {
                        Cell::App(at) => wanted = self.apps.get(at).map(|tile| tile.target.clone()),
                        Cell::Group(at) => self.enter(at),
                    }
                }
            }
        }
        self.step(settings);
        wanted
    }

    /// A point of the screen, in DIPs from its corner: its window covers the screen whole, and everything on it is
    /// drawn from that corner.
    fn on(&self, at: POINT) -> (f32, f32) {
        (
            (at.x - self.rect.left) as f32 / self.dpi,
            (at.y - self.rect.top) as f32 / self.dpi,
        )
    }

    /// The pointer, in the same DIPs: none where there is no pointer to read.
    fn place(&self) -> Option<(f32, f32)> {
        cursor().map(|at| self.on(at))
    }

    /// The tile a point of the screen is on, of the page it shows: none off the tiles, or past the page's last one.
    fn tile_on(&self, at: POINT) -> Option<usize> {
        let (from, on) = self.page_of();
        if on == 0 || from >= self.tiles.len() {
            return None;
        }
        let (x, y) = self.on(at);
        self.grid.hit(on, x, y)
    }

    /// The tile the pointer is on.
    fn tile_at(&self) -> Option<usize> {
        cursor().and_then(|at| self.tile_on(at))
    }

    /// Its window taking the keys (see `keys`): its own answer says what they do. The front it takes is given back as
    /// they are let go.
    fn take_keys(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        if self.taking {
            return;
        }
        self.taking = true;
        self.before = unsafe { GetForegroundWindow() };
        // (Brought to the front with the keyboard's focus, as `Window::take_keys` does: the search is to be written in.)
        self.window.take_keys(true);
        self.blink = Instant::now();
        let words = self.words.clone();
        let answer: Answer = Rc::new(move |_hwnd, message, wparam, _lparam| {
            let mut words = words.borrow_mut();
            match message {
                WM_CHAR => match wparam.0 as u16 {
                    // (Backspace and Enter have their say in their keydowns, where the caret and the tiles are walked:
                    // the characters Windows makes from them are swallowed here rather than asked a second time — two
                    // sayings of one Enter would name a folder and then open a tile of the folder's own.)
                    0x08 | 0x0D => Some(Some(LRESULT(0))),
                    // (Esc is `keys` own: it gives the keys back.)
                    0x1B => None,
                    below if below < 0x20 => None,
                    // (What is typed goes into the words in hand: the search, or a folder's name being written again.
                    // Standing in a folder whose name is left be there are no such words, and what is typed is
                    // swallowed rather than left to fill a search no one is shown.)
                    written => {
                        words.put(&[written]);
                        Some(Some(LRESULT(0)))
                    }
                },
                WM_KEYDOWN => {
                    let answered = written_key(&mut words, VIRTUAL_KEY(wparam.0 as u16));
                    words.edited |= answered.is_some();
                    answered.map(Some)
                }
                // Its own tick, told as its own: its screen drawn again, and the programs looked at. (Of the keys'
                // messages, which `keys::queued` hands on here rather than to the panel's own dealing with them.)
                windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == TICK_TIMER => {
                    Some(Some(LRESULT(0)))
                }
                // The rest to the system: an input method's own, and the keys' up.
                _ => None,
            }
        });
        keys::take(self.window.hwnd, answer);
    }

    /// The keys given back, and the front to whoever had it.
    fn give_keys(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};
        if !self.taking {
            return;
        }
        self.taking = false;
        keys::give_back(self.window.hwnd);
        self.window.take_keys(false);
        let before = self.before;
        if !before.is_invalid() && unsafe { IsWindow(Some(before)) }.as_bool() {
            let _ = unsafe { SetForegroundWindow(before) };
        }
    }

    /// The keys taken again, and the front: after a menu of its own went down, its search writes on.
    pub fn resume(&mut self) {
        if self.out {
            self.window.take_keys(true);
        }
    }

    /// The way out of what it stands in: a folder's name left unwritten, or the folder stepped out of. Whether that is
    /// what went, rather than the whole screen being asked to go down.
    pub fn back(&mut self) -> bool {
        if let Some(at) = self.renaming.take() {
            // (Its name was being written again: what was written goes, and the folder keeps the name it had.)
            let _ = at;
            self.clear();
            self.relayout();
            self.draw();
            return true;
        }
        if self.inside.take().is_some() {
            self.turn_in();
            self.relayout();
            self.draw();
            return true;
        }
        false
    }

    /// What is written gone, both in the search and in a folder's name being written again: the carets and the
    /// selections with the words, so that stepping out of a folder, or naming one, leaves nothing written behind.
    fn clear(&mut self) {
        // (One box of words written, and the caret and the selection in them, gone.)
        fn wipe(pen: &mut Typing) {
            pen.text.clear();
            pen.go(0, false);
        }
        let mut words = self.words.borrow_mut();
        wipe(&mut words.text);
        wipe(&mut words.name);
    }

    /// The programs taken again, as the shell lists them: their names, and whether the dock keeps them. Whether what
    /// is shown came out other than it was.
    fn names(&mut self, settings: &DockSettings) -> bool {
        let Some(all) = found::all() else {
            // (Not read yet: asked for, and looked at again at the next tick.)
            found::look(|| {});
            return false;
        };
        let stood = |target: &str| {
            settings.items.iter().any(|one| matches!(one, DockItem::Keep { target: here, .. } if apps::same(here, target)))
        };
        let apps: Vec<Tile> = all
            .iter()
            .map(|one| Tile {
                name: one.name.clone(),
                target: one.target.clone(),
                picture: self
                    .apps
                    .iter()
                    .find(|was| was.target == one.target)
                    .and_then(|was| was.picture.clone()),
                kept: stood(&one.target),
                group: Vec::new(),
            })
            .collect();
        let came = apps != self.apps || all != self.sources || settings.folders != self.folders;
        if came {
            self.apps = apps;
            self.sources = all;
            self.folders = settings.folders.clone();
        }
        came
    }

    /// The picture of every program shown, as the shell has given it: a tile of its first letter while it has not.
    /// Whether any of them came in. (Asked for at the side the grid gives: a bigger tile is a sharper picture, so
    /// all of them are looked for again when the grid is laid out other than it was.)
    fn pictures(&mut self) -> bool {
        let side = ((self.grid.side * self.dpi) as u32).max(LEAST);
        if side != self.asked_side {
            self.asked_side = side;
            for tile in self.apps.iter_mut() {
                tile.picture = None;
            }
        }
        let mut came = false;
        for tile in self.apps.iter_mut() {
            if tile.picture.is_none() {
                tile.picture = plated(&tile.target, side);
                came |= tile.picture.is_some();
            }
        }
        came
    }

    /// Which folder holds each program, of the tiles as the shell listed them: none where it stands on its own.
    fn owners(&self) -> Vec<Option<usize>> {
        let mut owner: Vec<Option<usize>> = vec![None; self.apps.len()];
        for (at, folder) in self.folders.iter().enumerate() {
            for target in &folder.items {
                if let Some(app) = self.apps.iter().position(|one| &one.target == target) {
                    owner[app] = Some(at);
                }
            }
        }
        owner
    }

    /// The programs that stand in the `at`th folder, as they are listed.
    fn members(&self, at: usize) -> Vec<usize> {
        self.folders
            .get(at)
            .map(|folder| {
                folder
                    .items
                    .iter()
                    .filter_map(|target| self.apps.iter().position(|one| &one.target == target))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A folder's tile: its name, and what stands in it drawn small within it.
    fn folder_tile(&self, at: usize) -> Tile {
        let Some(folder) = self.folders.get(at) else {
            return Tile {
                name: String::new(),
                target: String::new(),
                picture: None,
                kept: false,
                group: Vec::new(),
            };
        };
        Tile {
            name: folder.name.clone(),
            target: String::new(),
            picture: None,
            kept: false,
            group: self
                .members(at)
                .into_iter()
                .filter_map(|app| self.apps.get(app))
                .cloned()
                .collect(),
        }
    }

    /// The tiles laid out for what is shown now: the folder stood in, or the programs and folders the search leaves;
    /// the grid as its screen takes it, the pages, and the pointer's tile where it stands now that they are laid out
    /// again.
    fn relayout(&mut self) {
        let renaming = self.renaming.is_some();
        let query = self.query();
        let head = query.trim().to_lowercase();
        let stood_in = self.inside.filter(|at| !self.members(*at).is_empty());
        let (tiles, what): (Vec<Tile>, Vec<Cell>) =
            match stood_in {
                // (Standing in a folder: its tiles are the page, and the search leaves them be.)
                Some(at) => self
                    .members(at)
                    .into_iter()
                    .filter_map(|app| {
                        self.apps
                            .get(app)
                            .cloned()
                            .map(|tile| (tile, Cell::App(app)))
                    })
                    .unzip(),
                None if head.is_empty() => {
                    let owner = self.owners();
                    let mut made: Vec<(Tile, Cell)> = Vec::with_capacity(self.apps.len());
                    let mut stood: Vec<bool> = vec![false; self.folders.len()];
                    for (at, tile) in self.apps.iter().enumerate() {
                        match owner[at] {
                            None => made.push((tile.clone(), Cell::App(at))),
                            Some(group) if !stood[group] => {
                                stood[group] = true;
                                made.push((self.folder_tile(group), Cell::Group(group)));
                            }
                            Some(_) => {}
                        }
                    }
                    made.into_iter().unzip()
                }
                // (Searching: every program on its own, in a folder or not, as a Mac's launchpad leaves its folders out of
                // what a search brings.)
                None => self
                    .apps
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| {
                        self.sources
                            .get(*at)
                            .is_some_and(|one| found::matches(one, &head))
                    })
                    .map(|(at, tile)| (tile.clone(), Cell::App(at)))
                    .unzip(),
            };
        // (A folder whose tiles are all gone is no tile at all.)
        let pairs = tiles
            .into_iter()
            .zip(what)
            .filter(|(tile, _)| !tile.is_group() || !tile.group.is_empty())
            .collect::<Vec<_>>();
        let (tiles, what) = (
            pairs
                .iter()
                .map(|(tile, _)| tile.clone())
                .collect::<Vec<Tile>>(),
            pairs.iter().map(|(_, cell)| *cell).collect::<Vec<Cell>>(),
        );
        let turned = {
            let mut words = self.words.borrow_mut();
            let was = words.page;
            // (The list as it lies apart from its pictures: after a start the programs and their tiles' faces come in
            // one after another, and a picture arriving under a tile already shown is not a page to be washed again.)
            let turned =
                !Self::faces_match(&tiles, &self.tiles) || what.as_slice() != self.what.as_slice();
            self.tiles = tiles;
            self.what = what;
            let (wide, tall) = self.size();
            // (The grid keeps the room between the search and the dots' row: where the dock's glass stands at the
            // foot and lifts the dots, the grid gives that room up with them — the screen lies in its bands then,
            // the search at its head, the tiles between, the dots over the dock.)
            let keeps = shape::holds(tall, self.dots_line());
            self.grid = match stood_in {
                // (Standing in a folder: its tiles are laid for themselves — the page the settings ask for is the whole
                // screen's page, and a folder of four is four tiles in the room's middle rather than four of a
                // screen's fifty stretched out to its corners. Their side is the page's own, fetched and kept here for
                // the moment the pointer steps in.)
                Some(_) => Grid::inside(wide, keeps, self.tiles.len(), self.page_side),
                None => {
                    let page = Grid::asked(wide, keeps, self.tiles.len(), self.pad.0, self.pad.1);
                    self.page_side = page.side;
                    page
                }
            };
            words.pages = self.grid.pages(self.tiles.len()).max(1);
            words.page = words.page.min(words.pages - 1);
            // (The keys are answered away from the window but keep their own read of how the tiles lie: and the pick,
            // made of the list as it stood, is gone where the list itself came out other than it was.)
            (words.cols, words.rows, words.count) =
                (self.grid.cols, self.grid.rows, self.tiles.len());
            // (What is typed goes where the page shown can use it: the search out on the whole screen, the folder's
            // name while it is being written again, and nothing at all standing in a folder.)
            words.writing = match (renaming, stood_in) {
                (true, _) => Writing::Name,
                (false, Some(_)) => Writing::Nothing,
                (false, None) => Writing::Search,
            };
            if turned {
                words.picked = None;
                words.crossed = None;
            }
            turned || words.page != was
        };
        if turned {
            self.turn_in();
        }
        // (The tiles moved under the pointer, or the page turned: it is on another of them, or on none.)
        self.over = if turned {
            None
        } else {
            cursor().and_then(|at| self.tile_on(at))
        };
    }

    /// A page come in: its tiles washed up from nothing.
    fn turn_in(&mut self) {
        self.fade.jump(0.0);
    }

    /// Where two lists of tiles lie the same, pictures aside: the same programs, kept the same, foldered the same.
    fn faces_match(one: &[Tile], other: &[Tile]) -> bool {
        one.len() == other.len()
            && one
                .iter()
                .zip(other)
                .all(|(one, other)| Self::face_match(one, other))
    }

    /// Where one tile is the other, picture aside: called the same, opening the same, kept the same, and holding
    /// the same where it is a folder.
    fn face_match(one: &Tile, other: &Tile) -> bool {
        one.name == other.name
            && one.target == other.target
            && one.kept == other.kept
            && Self::faces_match(&one.group, &other.group)
    }

    /// What is written in its search.
    fn query(&self) -> String {
        self.words.borrow().text.string()
    }

    /// What the page shows of its tiles: from where it begins, as many as there are on it.
    fn page_of(&self) -> (usize, usize) {
        let from = self.words.borrow().page * self.grid.per;
        (
            from,
            self.tiles.len().saturating_sub(from).min(self.grid.per),
        )
    }

    /// Whether the pointer has gone far enough from where its button went down for the tile to be taken up: screen px,
    /// as the pointer is read in them, over a distance kept in DIPs.
    fn carried(&self) -> bool {
        let room = (PICK * self.dpi) as i32;
        cursor().is_some_and(|at| {
            (at.x - self.from.x).abs() > room || (at.y - self.from.y).abs() > room
        })
    }

    /// The program the tile being carried stands for, with the name it goes by: none where what is carried is a
    /// folder's tile, which the dock has no place for of its own.
    fn carrying(&self) -> Option<(String, String)> {
        let index = self.held?;
        let Cell::App(app) = self.what.get(self.page_of().0 + index).copied()? else {
            return None;
        };
        self.apps
            .get(app)
            .map(|one| (one.target.clone(), one.name.clone()))
    }

    /// Where the dock's glass is on this screen (DIPs from its corner): none where the dock is not up, or stands on
    /// another screen than the one it covers.
    fn glass_box(&self) -> Option<(f32, f32, f32, f32)> {
        let glass = self.dock?;
        let (wide, tall) = self.size();
        let (x, y) = (
            (glass.left - self.rect.left) as f32 / self.dpi,
            (glass.top - self.rect.top) as f32 / self.dpi,
        );
        let (across, down) = (
            (glass.right - glass.left) as f32 / self.dpi,
            (glass.bottom - glass.top) as f32 / self.dpi,
        );
        (x >= 0.0 && y >= 0.0 && x + across <= wide && y + down <= tall)
            .then_some((x, y, across, down))
    }

    /// Where a carried tile is let go to be kept on the dock: over its glass where the dock shows on this screen, and
    /// along the foot where it does not.
    fn strip(&self) -> (f32, f32, f32, f32) {
        let (wide, tall) = self.size();
        shape::drop(wide, tall, self.glass_box())
    }

    /// Whether the pointer stands over that strip.
    fn over_strip(&self) -> bool {
        let box_ = self.strip();
        self.place()
            .is_some_and(|(x, y)| shape::in_drop(box_, x, y))
    }

    /// Where the pages' dots stand: at the screen's foot, and lifted above the dock's glass where the dock shows
    /// along that foot — the dock keeps its place over the launchpad (a tile is carried onto it to be fixed), and
    /// would else wash over the dots altogether.
    fn dots_line(&self) -> f32 {
        let tall = self.size().1;
        let foot = shape::feet(tall);
        match self.dock {
            Some(glass) => {
                // (Measured against the glass itself: its top and bottom in the screen's DIPs. The old nearness to
                // the screen's edge asked the glass to stand within eight DIPs of it, when the glass keeps ten off
                // that edge by its own design — the lift could never happen, and the dots stood under the glass
                // still. And lifted clear of the whole band the dock's tiles swell into: its window takes the
                // pointer over that, however tall the glass itself, and a dot under it is a dot no click reaches.)
                let (top, bottom) = (
                    (glass.top - self.rect.top) as f32 / self.dpi,
                    (glass.bottom - self.rect.top) as f32 / self.dpi,
                );
                if bottom > tall - 40.0 && top < foot + self.dock_room + 12.0 {
                    (top - self.dock_room - 10.0).clamp(foot - 320.0, foot)
                } else {
                    foot
                }
            }
            None => foot,
        }
    }

    /// The left button went down: on which tile, on one of the pages' dots, on the way back out of a folder, or on its
    /// search. What it went down on may be carried onto another tile, and so made a folder of the two.
    pub fn press(&mut self) {
        (self.pressed, self.held, self.on_title) = (None, None, false);
        self.from = cursor().unwrap_or_default();
        match self.place() {
            Some((x, y)) => {
                let wide = self.size().0;
                let pages = self.words.borrow().pages;
                if shape::in_back(wide, x, y) && self.inside.is_some() {
                    self.back();
                } else if let Some(page) = shape::dot_at(pages, wide, self.dots_line(), x, y) {
                    self.turn(page);
                } else if self.inside.is_some() && shape::in_title(wide, x, y) {
                    // (A folder's name at its head: pressed where it is shown, it is written there and then; pressed
                    // while it is being written, the writing goes on and the caret is left where it stands.)
                    self.on_title = true;
                    self.blink = Instant::now();
                    if self.renaming.is_none() {
                        if let Some(at) = self.inside {
                            self.write_name(at);
                        }
                    }
                } else if shape::in_field(wide, x, y) && self.inside.is_none() {
                    // (Its caret begins its blink again: written in where it is, as it is everywhere else in Glance.)
                    self.blink = Instant::now();
                } else {
                    self.pressed = self.tile_at();
                }
            }
            None => self.pressed = self.tile_at(),
        }
        self.over = self.pressed;
        // (The pick follows the pointer as the pointer follows it: a tile stood on is the tile the keys would walk
        // from next.)
        if let Some(index) = self.pressed {
            self.words.borrow_mut().picked = Some(index + self.page_of().0);
        }
        self.window.point(if self.pressed.is_some() {
            IDC_HAND
        } else if self.on_title {
            IDC_IBEAM
        } else {
            IDC_ARROW
        });
        if self.pressed.is_some() {
            // (The pointer is held till it is let go, wherever it goes: a tile is carried over the glass of the dock,
            // which stands over this screen, and off the screen's edge altogether.)
            unsafe {
                SetCapture(self.window.hwnd);
            }
        }
        self.draw();
    }

    /// The left button let go: the program to open, where the same tile was pressed and let go and neither was carried
    /// anywhere; the folder made, where it was carried onto another; the program taken out of its folder, where it was
    /// carried from one and let go off its tiles; and one kept on the dock, where it was carried to the strip at the
    /// foot.
    pub fn release(&mut self) -> Option<Carry> {
        let went = self.pressed.take();
        let carried = self.held.take();
        if went.is_some() || carried.is_some() {
            unsafe {
                ReleaseCapture().ok();
            }
        }
        self.window.point(IDC_ARROW);
        if self.renaming.is_some() {
            // (A folder's name being written: let go anywhere but on the way back and but on the name itself, and it
            // is written and done with.)
            if !self.on_title {
                self.name_it();
            }
            return None;
        }
        if let Some(from) = carried {
            // (Let go over the strip at the foot: the program is kept on the dock, and the screen goes down with it.
            // A folder's own tile is not kept, only what it stands for.)
            if self.over_strip() {
                if let Some((target, name)) = self.carrying() {
                    // (Carried out of a folder onto the dock as well: out of the folder it goes with it, and the screen
                    // steps out to show it among the rest.)
                    if self.inside.is_some() {
                        self.eject(target.clone(), true);
                    }
                    return Some(Carry::Pin { target, name });
                }
            }
            let into = self.over;
            self.draw();
            match (self.inside, into) {
                // (Carried onto another of the page's tiles: the two made into a folder.)
                (None, Some(into)) => self.merge(from, into),
                // (Standing in a folder: wherever but back on the place it came from a tile is let go, another of its
                // tiles or nothing at all, it is carried out of the folder — the screen stepping out with it.)
                (Some(_), place) if place != Some(from) => self.take_out(from),
                // (Let go where it came from, or off the tiles of the whole screen's page: nothing done.)
                _ => {}
            }
            return None;
        }
        let tile = went.filter(|index| self.tile_at() == Some(*index))?;
        match self.what.get(tile + self.page_of().0)? {
            // (A folder pressed: stood in, rather than opened.)
            Cell::Group(at) => {
                self.enter(*at);
                None
            }
            Cell::App(app) => Some(Carry::Open(
                self.apps.get(*app).map(|tile| tile.target.clone())?,
            )),
        }
    }

    /// Whether the left button is down on one of its tiles, carried or barely pressed.
    pub fn in_hand(&self) -> bool {
        self.pressed.is_some() || self.held.is_some()
    }

    /// The pointer moved: the tile it stands on, the hand over it, and the tile it has taken one up to carry. `glass`
    /// is where the dock's glass stands, which the panel knows and it does not (none where the dock is not up).
    pub fn moved(&mut self, glass: Option<RECT>) {
        // (The glass came, went or moved: the dots' row lifts with it, and the grid takes its room back or gives it
        // up — laid out again, short of a page turned, for the tiles' faces are the same.)
        let mut laid = false;
        if self.dock != glass {
            self.dock = glass;
            if self.out {
                self.relayout();
                laid = true;
            }
        }
        let over = self.tile_at();
        // (A tile may be carried over the whole screen's tiles only while nothing is searched — what a search brought is
        // not what stands on the page — and its name is not being written; within a folder, its own tiles may be
        // carried out of it.)
        let may = self.renaming.is_none() && (self.inside.is_some() || self.query().is_empty());
        let held = match (self.pressed, may) {
            (Some(index), true) if self.carried() => Some(index),
            (Some(_), true) => self.held,
            _ => None,
        };
        let came = held != self.held || over != self.over;
        self.held = held;
        self.over = over;
        let titled = self.inside.is_some()
            && self
                .place()
                .is_some_and(|(x, y)| shape::in_title(self.size().0, x, y));
        self.window.point(match (over.is_some(), titled) {
            (true, _) => IDC_HAND,
            // (The name at a folder's head is written there and then: the caret's own pointer says so.)
            (false, true) => IDC_IBEAM,
            (false, false) => IDC_ARROW,
        });
        if came || laid {
            self.draw();
        }
    }

    /// The capture taken from it by something else (a UAC prompt, the lock screen): what was in hand is put back where
    /// it came from, nothing done.
    pub fn lost(&mut self) {
        if self.pressed.is_none() && self.held.is_none() {
            return;
        }
        (self.pressed, self.held) = (None, None);
        unsafe {
            ReleaseCapture().ok();
        }
        self.window.point(IDC_ARROW);
        self.draw();
    }

    /// A press of its right button: the menu of the tile it went down on. None where there is no tile to say anything
    /// of.
    pub fn menu(&mut self, at: POINT) -> Option<(Menu, Vec<Choice>)> {
        let index = self.tile_on(at)?;
        let (from, _) = self.page_of();
        let (cell, tile) = (
            self.what.get(from + index)?.to_owned(),
            self.tiles.get(from + index)?.clone(),
        );
        let zh = self.lang == Lang::Zh;
        let say = |zh_text: &'static str, en: &'static str| if zh { zh_text } else { en };
        let mut rows: Vec<Choice> = Vec::new();
        match cell {
            Cell::App(app) => {
                let target = self.apps.get(app)?.target.clone();
                let here = apps::App {
                    target: target.clone(),
                    name: tile.name.clone(),
                }
                .location();
                rows.push(Choice::Open(target.clone()));
                // (The administrator's leave is asked of what runs by itself; a Store app's package is not.)
                if !target.contains('!') {
                    rows.push(Choice::Admin(target.clone()));
                }
                rows.push(Choice::Keep(target.clone()));
                if self.in_group(app) {
                    rows.push(Choice::Out(target.clone()));
                }
                if here.is_some() {
                    rows.push(Choice::Where(target.clone()));
                }
                // (A Store app's package is taken off by Windows itself; of the rest, the one asked for is the
                // installer's own word for it, where the registry still has it — no row where it has not.)
                if !target.contains('!') {
                    if let Some(word) = apps::uninstaller(&tile.name) {
                        rows.push(Choice::Uninstall(word));
                    }
                }
            }
            Cell::Group(at) => {
                rows.push(Choice::Enter(at));
                rows.push(Choice::Name(at));
                rows.push(Choice::Break(at));
            }
        }
        rows.push(Choice::Settings);
        let labels: Vec<(&'static str, Icon, bool, bool)> = rows
            .iter()
            .map(|row| match row {
                Choice::Open(_) => (say("打开", "Open"), Icon::Panel, false, false),
                Choice::Admin(_) => (
                    say("以管理员身份运行", "Run as administrator"),
                    Icon::Shield,
                    false,
                    false,
                ),
                Choice::Keep(target) => {
                    let kept = self
                        .apps
                        .iter()
                        .find(|one| &one.target == target)
                        .is_some_and(|one| one.kept);
                    (
                        if kept {
                            say("从 Dock 移除", "Take off the dock")
                        } else {
                            say("固定到 Dock", "Fix to the dock")
                        },
                        if kept { Icon::PinOff } else { Icon::Pin },
                        false,
                        false,
                    )
                }
                Choice::Out(_) => (
                    say("移出文件夹", "Take out of its folder"),
                    Icon::ArrowRightToLine,
                    false,
                    false,
                ),
                Choice::Where(_) => (
                    say("打开所在位置", "Open its location"),
                    Icon::Folder,
                    false,
                    false,
                ),
                Choice::Uninstall(_) => (say("卸载", "Uninstall"), Icon::CircleX, false, false),
                Choice::Enter(_) => (
                    say("打开文件夹", "Open the folder"),
                    Icon::Folder,
                    false,
                    false,
                ),
                Choice::Name(_) => (
                    say("重命名文件夹", "Rename the folder"),
                    Icon::SquarePen,
                    false,
                    false,
                ),
                Choice::Break(_) => (
                    say("解散文件夹", "Take the folder apart"),
                    Icon::Unlink,
                    false,
                    false,
                ),
                Choice::Settings => (
                    say("Dock 设置…", "Dock settings…"),
                    Icon::Settings,
                    false,
                    true,
                ),
            })
            .collect();
        let items = labels
            .iter()
            .map(|(label, icon, checked, rule)| Item {
                label,
                mark: Some(crate::ui::menu::Mark::Icon(*icon)),
                checked: *checked,
                rule_before: *rule,
            })
            .collect();
        let menu = Menu::open(items, Look::system(), false)?;
        Some((menu, rows))
    }

    /// Whether the `at`th program stands in one of the folders.
    fn in_group(&self, at: usize) -> bool {
        self.owners().get(at).is_some_and(|one| one.is_some())
    }

    /// Stand in a folder: its tiles the page, the search left empty for it.
    fn enter(&mut self, at: usize) {
        (self.inside, self.renaming) = (Some(at), None);
        self.clear();
        {
            self.words.borrow_mut().page = 0;
        }
        self.turn_in();
        self.relayout();
        self.draw();
    }

    /// A folder's name being written again: what it is called now stands at the head of its own page to be written
    /// over, its tiles stayed where they are. Reached by a click on the name, or by the menu of its tile — which
    /// steps in first, since a name is written where the folder is stood in.
    fn write_name(&mut self, at: usize) {
        (self.inside, self.renaming) = (Some(at), Some(at));
        // (The search left empty for the folder, as stepping into it leaves it: what is written at its head goes into
        // the name's own words, and the search is never the worse for a folder being named.)
        self.clear();
        let units: Vec<u16> = self
            .folders
            .get(at)
            .map(|one| one.name.clone())
            .unwrap_or_default()
            .encode_utf16()
            .collect();
        self.words.borrow_mut().name.put(&units);
        self.blink = Instant::now();
        self.relayout();
        self.draw();
    }

    /// A folder's name written again is put back: what stands at the head of its page becomes what it is called, and
    /// the writing is done with. Nothing written is no name: the folder keeps the one it had.
    fn name_it(&mut self) {
        let Some(at) = self.renaming else { return };
        let written = self.words.borrow().name.string();
        self.renaming = None;
        self.clear();
        let name = written.trim().to_string();
        if !name.is_empty() {
            std::thread::spawn(move || {
                crate::app().change_dock(move |dock| {
                    // (The folder by the number it stands at, not by the list it holds: two folders with the same
                    // tiles in them are named apart, and a list looked for by what it held a moment ago has often
                    // moved since.)
                    if let Some(here) = dock.folders.get_mut(at) {
                        here.name = name;
                    }
                });
            });
        }
        self.relayout();
        self.draw();
    }

    /// Two tiles carried into one: the programs they stand for merged into a folder named for the one they were
    /// dropped on, and kept in the settings (which is where the tiles come back from, a moment after).
    fn merge(&mut self, from: usize, into: usize) {
        // (The tiles are numbered of the page they show: what stands under them is the whole screen's list.)
        let off = self.page_of().0;
        let (Some(from), Some(into)) = (
            self.what.get(off + from).copied(),
            self.what.get(off + into).copied(),
        ) else {
            return;
        };
        if from == into {
            return;
        }
        let held = |cell: Cell| match cell {
            Cell::App(at) => self
                .apps
                .get(at)
                .map(|one| vec![one.target.clone()])
                .unwrap_or_default(),
            Cell::Group(at) => self
                .members(at)
                .into_iter()
                .filter_map(|app| self.apps.get(app))
                .map(|one| one.target.clone())
                .collect(),
        };
        let (added, moved_whole) = (held(from), matches!(from, Cell::Group(_)));
        let stood = held(into);
        if added.is_empty() || stood.is_empty() {
            return;
        }
        let name = match into {
            Cell::App(at) => self
                .apps
                .get(at)
                .map(|one| one.name.clone())
                .unwrap_or_default(),
            Cell::Group(at) => self
                .folders
                .get(at)
                .map(|one| one.name.clone())
                .unwrap_or_default(),
        };
        // (Which folder a tile stands for, by the number it stands at in the settings: none where it is a program's
        // own tile. The join is made at this number rather than by matching lists of targets — a folder still names
        // what the scan has gone off, and a join looked for by the live tiles alone would answer to no folder at all
        // and make a second one beside the first.)
        let group = |cell: Cell| match cell {
            Cell::Group(at) => Some(at),
            Cell::App(_) => None,
        };
        let joined: Vec<String> = stood
            .iter()
            .chain(added.iter())
            .filter(|target| !stood.contains(target))
            .cloned()
            .collect();
        let (one_out, into_at) = (
            (!moved_whole).then(|| added.first().cloned()).flatten(),
            group(into),
        );
        // (A whole folder carried onto another: the list of what it held, gone by what it is rather than by the number
        // it stood at — between the drag and the writing the numbers may have moved, and a folder removed at the wrong
        // number is another folder's tiles thrown away.)
        let gone = if moved_whole {
            group(from)
                .and_then(|at| self.folders.get(at))
                .map(|one| one.items.clone())
        } else {
            None
        };
        self.inside = None;
        std::thread::spawn(move || {
            crate::app().change_dock(move |dock| {
                if let Some(one) = &one_out {
                    // (One program carried: taken out of where it stood before it stands again.)
                    for here in dock.folders.iter_mut() {
                        here.items.retain(|target| target != one);
                    }
                }
                if let Some(at) = into_at {
                    // (The folder it is joined into takes what was carried onto it, and keeps the rest of what it
                    // named — including a target the scan has gone off, which is its own to let go of in its own
                    // time rather than in a drag's.)
                    if let Some(here) = dock.folders.get_mut(at) {
                        for target in &added {
                            if !here.items.contains(target) {
                                here.items.push(target.clone());
                            }
                        }
                    }
                } else {
                    dock.folders.push(LaunchFolder { name, items: joined });
                }
                if let Some(gone) = &gone {
                    // (A whole folder carried: it is gone now that the other has taken what it held.)
                    if let Some(at) = dock.folders.iter().position(|one| &one.items == gone) {
                        dock.folders.remove(at);
                    }
                }
            });
        });
    }

    /// A tile carried out of the folder it stands in and let go off the place it came from: the program it is a tile of
    /// its own again, and the screen steps out of the folder to show it among the rest.
    fn take_out(&mut self, index: usize) {
        // (The tiles are numbered of the page they show: what stands under them is the whole screen's list.)
        let off = self.page_of().0;
        let Some(Cell::App(app)) = self.what.get(off + index).copied() else {
            return;
        };
        let Some(target) = self.apps.get(app).map(|one| one.target.clone()) else {
            return;
        };
        self.eject(target, true);
        self.draw();
    }

    /// A program taken out of every folder it stands in, and kept nowhere else: it is a tile of its own again. A folder
    /// left with one program is no folder, and the number it stood at means another's tiles by then — so where a tile
    /// was carried out of the folder stood in, or the folder is to dissolve, the screen steps out of it first.
    fn eject(&mut self, target: String, out: bool) {
        let Some(at) = self
            .folders
            .iter()
            .position(|one| one.items.contains(&target))
        else {
            return;
        };
        let Some(items) = self.folders.get(at).map(|one| one.items.clone()) else {
            return;
        };
        if out || (items.len() <= 2 && self.inside == Some(at)) {
            self.inside = None;
            self.turn_in();
            // (The screen is the whole page's again at once: what is kept comes back a moment after, and the folder
            // would else be shown standing in empty tiles.)
            self.relayout();
        }
        std::thread::spawn(move || {
            crate::app().change_dock(move |dock| {
                if let Some(here) = dock.folders.get_mut(at) {
                    here.items.retain(|one| *one != target);
                }
                dock.folders.retain(|one| one.items.len() > 1);
            });
        });
    }

    /// What was chosen in its menu: the tile fixed to the dock, or taken off it, the folder stood in, named, or taken
    /// apart, drawn again for it; and what the panel is asked to do for the rest.
    pub fn chosen(&mut self, choice: Choice) -> Option<Follow> {
        match choice {
            Choice::Open(target) => Some(Follow::Launch(target)),
            Choice::Admin(target) => Some(Follow::LaunchAs(target)),
            Choice::Where(target) => {
                let here = apps::App {
                    target,
                    name: String::new(),
                }
                .location();
                let path = here?;
                Some(Follow::Reveal(path.to_string_lossy().to_string()))
            }
            Choice::Uninstall(word) => Some(Follow::Uninstall(word)),
            Choice::Settings => None,
            Choice::Enter(at) => {
                self.enter(at);
                None
            }
            Choice::Name(at) => {
                // (Its name written at the head of its own page: the folder is stood in first, its tiles staying
                // where they are while it is written.)
                self.write_name(at);
                None
            }
            Choice::Break(at) => {
                let Some(folder) = self.folders.get(at).cloned() else {
                    return None;
                };
                if self.inside == Some(at) {
                    self.inside = None;
                }
                std::thread::spawn(move || {
                    crate::app().change_dock(move |dock| {
                        if let Some(here) = dock.folders.iter().position(|one| *one == folder) {
                            dock.folders.remove(here);
                        }
                    });
                });
                None
            }
            Choice::Out(target) => {
                self.eject(target, false);
                self.draw();
                None
            }
            Choice::Keep(target) => {
                let index = self.apps.iter().position(|one| one.target == target);
                let (name, kept) = index
                    .map(|at| (self.apps[at].name.clone(), self.apps[at].kept))
                    .unwrap_or_default();
                let app = apps::App { target, name };
                std::thread::spawn(move || {
                    crate::app().change_dock(move |dock| {
                        let stood = dock.items.iter().position(|one| matches!(one, DockItem::Keep { target: here, .. } if apps::same(here, &app.target)));
                        match (stood, kept) {
                            (Some(at), true) => {
                                dock.items.remove(at);
                            }
                            (None, false) => dock.items.insert(0, DockItem::Keep { target: app.target, name: app.name }),
                            _ => {}
                        }
                    });
                });
                None
            }
        }
    }

    /// The wheel, or the keys: the pages about.
    pub fn wheel(&mut self, notches: i32) {
        let was = self.words.borrow().page;
        let pages = self.words.borrow().pages;
        self.turn((was as i32 - notches).clamp(0, pages.saturating_sub(1) as i32) as usize);
    }

    /// The page shown, and nothing the pointer is on: the dots, the wheel, and the keys all turn it the same.
    fn turn(&mut self, page: usize) {
        let moved = {
            let mut words = self.words.borrow_mut();
            let was = words.page;
            words.page = page.min(words.pages.saturating_sub(1));
            // (The pick stands on the page it was walked upon: another page shows none of it.)
            (words.picked, words.crossed) = (None, None);
            words.page != was
        };
        if moved {
            self.over = None;
            self.turn_in();
            self.draw();
        }
    }

    /// The screen it covers gone elsewhere, or changed: placed again, and laid out again.
    pub fn screens_changed(&mut self, settings: &DockSettings) {
        if !self.out {
            return;
        }
        self.look_at();
        self.window.place(self.rect);
        self.names(settings);
        self.relayout();
        self.draw();
    }

    /// Its timers armed, or no longer: while it is up it is drawn by itself.
    fn arm(&mut self, on: bool) {
        if on == self.ticking {
            return;
        }
        self.ticking = on;
        let hwnd = self.window.hwnd;
        unsafe {
            if on {
                SetTimer(Some(hwnd), TICK_TIMER, TICK_MS, None);
            } else {
                KillTimer(Some(hwnd), TICK_TIMER).ok();
            }
        }
    }

    /// A tick, or a thing written: its screen coming up, its caret's blink, the programs and their pictures
    /// coming in, and the tiles laid out for what the search leaves.
    pub fn step(&mut self, settings: &DockSettings) {
        if !self.out {
            return;
        }
        let now = Instant::now();
        // (The settings asked for another lie of the page while it was up: laid out again at once, the tiles
        // growing or shrinking with the grid set over them.)
        let pad = (settings.pad_cols, settings.pad_rows);
        let room = overhang(settings.measures().0, settings.peak()).1;
        if pad != self.pad || room != self.dock_room {
            self.pad = pad;
            self.dock_room = room;
            self.relayout();
        }
        let fading = !self.wash.done(now) || !self.fade.done(now);
        let again = now.duration_since(self.asked) >= AGAIN;
        let wrote = std::mem::take(&mut self.words.borrow_mut().edited);
        if again {
            self.asked = now;
            let came = self.names(settings);
            self.pictures();
            if came {
                self.relayout();
            }
        }
        // (Written in while standing in a folder: the folder stepped out of, and the search has the screen again.)
        if self.inside.is_some() && self.renaming.is_none() && !self.query().is_empty() {
            self.back();
        }
        if again || wrote {
            self.relayout();
        }
        if wrote {
            self.blink = now;
        }
        if fading || again || wrote || self.blinking(now) {
            self.draw();
        }
    }

    /// Whether its caret is to be shown now: kept lit a while after anything was written, and blinking
    /// after that.
    fn caret(&self, now: Instant) -> bool {
        if !self.taking {
            return false;
        }
        let since = now.duration_since(self.blink).as_millis() as i64;
        let shown = SHOWN.as_millis() as i64;
        let blink = BLINK.as_millis() as i64;
        since < shown || (since - shown) % (2 * blink) < blink
    }

    /// Whether the blink turns over by now: drawn again as it does, and not otherwise.
    fn blinking(&self, now: Instant) -> bool {
        self.caret(now) != self.caret(now - Duration::from_millis(TICK_MS as u64))
    }

    /// Draws it: its screen washed over what is behind it, its search, its page of tiles, and the pages' dots.
    fn draw(&mut self) {
        let Ok(gfx) = gfx::for_window(self.window.hwnd) else { return };
        if !Rc::ptr_eq(&gfx, &self.gfx) {
            self.gone();
            self.gfx = gfx;
        }
        let now = Instant::now();
        self.shown();
        let reduced = crate::panel::reduced_motion();
        let share = if reduced { 1.0 } else { self.wash.value(now) };
        if self.wash.target() < 1.0 {
            self.wash.retarget(1.0, WASH.0, WASH.1, now);
        }
        if reduced {
            self.fade.jump(1.0);
        } else if self.fade.target() < 1.0 || self.fade.done(now) {
            self.fade.retarget(1.0, TURN.0, TURN.1, now);
        }
        let come = if reduced { 1.0 } else { self.fade.value(now) };
        let (from, on) = self.page_of();
        let (wide, tall) = self.size();
        let sheet = (
            (self.rect.right - self.rect.left) as u32,
            (self.rect.bottom - self.rect.top) as u32,
        );
        let pane = (0.0, 0.0, sheet.0 as f32, sheet.1 as f32, 0.0);
        // Its glass is frosted over the desktop, as the dock's is: washed deeper as it comes up.
        self.surface.set_frost(share);
        let wash = if self.dark {
            Color::hex(0x0A0D12, 0.72 * share)
        } else {
            Color::hex(0xEAEFF6, 0.72 * share)
        };
        let tiles = &self.tiles;
        let grid = &self.grid;
        let words = self.query();
        let caret = self.caret(now);
        let (page, pages, picked) = {
            let words = self.words.borrow();
            // (The pick is kept of the whole list: the page's tiles are numbered from their own beginning.)
            (
                words.page,
                words.pages,
                words
                    .picked
                    .filter(|at| *at >= from && *at < from + on)
                    .map(|at| at - from),
            )
        };
        let (over, dark, dpi, lang) = (self.over, self.dark, self.dpi, self.lang);
        let zh = lang == Lang::Zh;
        // (The tile another would be merged into is lit only over the whole screen's tiles: within a folder, what is
        // carried is carried out of it, not onto a neighbour.)
        let into = if self.held.is_some() && self.inside.is_none() {
            over.filter(|one| Some(*one) != self.held)
        } else {
            None
        };
        let hand = Hand {
            over,
            held: self.held,
            at: self.place().unwrap_or_default(),
            into,
            come,
            picked,
        };
        // (The strip at the foot a carried program is let go on to be kept on the dock: shown only while one is
        // carried, so the screen is not cluttered by a place that answers to nothing.)
        let strip = self.carrying().is_some().then(|| self.strip());
        let hot = strip.is_some() && self.over_strip();
        let folder: Option<(String, bool)> = match (self.inside, self.renaming) {
            // (A folder's name at the head of its page, and whether it is being written again there and then: shown,
            // it is the name the folder has; written again, what stands in the name's own words, which the page leaves
            // out of its tiles and the search never sees.)
            (Some(_), Some(_)) => Some((self.words.borrow().name.string(), true)),
            (Some(at), None) => self.folders.get(at).map(|one| (one.name.clone(), false)),
            _ => None,
        };
        // (The dots' row: at the foot, or lifted clear of the dock's glass where that stands there.)
        let dots_at = self.dots_line();
        let painted = self
            .surface
            .draw(&self.gfx, sheet, dpi, pane, move |frame| {
                frame.crisp_text();
                frame.fill(wash, 0.0, 0.0, wide, tall);
                shape::search(
                    frame,
                    wide,
                    &words,
                    "",
                    caret,
                    dark,
                    if zh {
                        "搜索程序，可以用拼音"
                    } else {
                        "Search programs"
                    },
                    dpi,
                    folder
                        .as_ref()
                        .map(|(one, editing)| (one.as_str(), *editing)),
                );
                if let Some(box_) = strip {
                    shape::drop_plate(
                        frame,
                        box_,
                        hot,
                        dark,
                        if zh {
                            "固定到 Dock"
                        } else {
                            "Keep on the dock"
                        },
                        1.0,
                    );
                }
                shape::paint(frame, tiles, grid, from, on, hand, dark);
                shape::dots(frame, pages, page, wide, dots_at, dark);
            });
        if let Err(error) = &painted {
            gfx::failed("launchpad", &self.gfx, Some(error));
            return;
        }
        self.gfx.sweep();
    }
}

/// What a key the search answers to does: `Some` what it answered, `None` where the key is not the search's own (Esc,
/// which `keys` sees to, and the rest).
fn written_key(words: &mut Words, key: VIRTUAL_KEY) -> Option<LRESULT> {
    // (Walking the whole list's tiles with the keys: from where the pick stands, or from the page's first tile where
    // it stands nowhere yet. A step past the list's ends is kept at them; a step onto another page turns it when the
    // message is next told, for a message answers the keys but turns no page itself.)
    let per = (words.cols * words.rows).max(1);
    fn walk(words: &mut Words, step: isize) -> Option<LRESULT> {
        let per = (words.cols * words.rows).max(1) as isize;
        if words.count == 0 {
            return None;
        }
        let page = words.page as isize;
        let from = words.picked.unwrap_or(0) as isize + page * per;
        let last = words.count as isize - 1;
        let to = (from + step).clamp(0, last);
        words.picked = Some(to as usize);
        let onto = to / per;
        if onto != page {
            words.crossed = Some(onto as usize);
        }
        Some(LRESULT(0))
    }
    fn edge(words: &Words, forward: bool) -> bool {
        words.step(words.caret(), forward) != words.caret()
    }
    match key {
        // (Standing in a folder whose name is not being written again there is nothing to write into: these keys take
        // letters off words no one is shown there, and are left unanswered with the characters they make.)
        VK_BACK | VK_DELETE if words.pen().is_none() => return None,
        VK_BACK => words.erase(false),
        VK_DELETE => words.erase(true),
        // (Left and right walk the caret while it has room to move; at its end they walk the tiles — as the shell's
        // own search boxes come to.)
        VK_LEFT if edge(words, false) => words.go(words.step(words.caret(), false), false),
        VK_LEFT => {
            return match walk(words, -1) {
                Some(answered) => Some(answered),
                None => {
                    words.go(0, false);
                    Some(LRESULT(0))
                }
            };
        }
        VK_RIGHT if edge(words, true) => words.go(words.step(words.caret(), true), false),
        VK_RIGHT => {
            return match walk(words, 1) {
                Some(answered) => Some(answered),
                None => {
                    words.go(words.reach(), false);
                    Some(LRESULT(0))
                }
            };
        }
        VK_TAB => return walk(words, if key_was_shifted() { -1 } else { 1 }),
        // (Up and down walk the tiles a row at a time; where the search holds words they turn the page, as they did,
        // for the caret is nowhere to go between one line's ends.)
        VK_UP | VK_DOWN => {
            if words.blank() && words.count > 0 {
                return Some(
                    walk(
                        words,
                        if key == VK_UP {
                            -(per as isize)
                        } else {
                            per as isize
                        },
                    )
                    .unwrap_or(LRESULT(0)),
                );
            }
            words.page = if key == VK_UP {
                words.page.saturating_sub(1)
            } else {
                (words.page + 1).min(words.pages.saturating_sub(1))
            };
        }
        VK_HOME => {
            // (The pick begins at the whole list's first tile; the caret only where there is nothing picked.)
            if words.blank() {
                words.picked = Some(0);
                words.crossed = Some(0);
                return Some(LRESULT(0));
            }
            words.go(0, false);
        }
        VK_END => {
            if words.blank() {
                words.picked = words.count.checked_sub(1);
                if let Some(at) = words.picked {
                    words.crossed = Some(at / per);
                }
                return Some(LRESULT(0));
            }
            words.go(words.reach(), false);
        }
        VK_PRIOR => words.page = words.page.saturating_sub(1),
        VK_NEXT => words.page = (words.page + 1).min(words.pages.saturating_sub(1)),
        VK_RETURN => words.enter = true,
        _ => return None,
    }
    Some(LRESULT(0))
}

/// Whether the Shift key is down as a message is answered: Tab walks the page backwards with it held.
fn key_was_shifted() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    unsafe { (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16) & 0x8000 != 0 }
}
