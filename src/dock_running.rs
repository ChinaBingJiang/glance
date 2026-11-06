//! What the dock says is running: the programs with a window a person could go to, and which of the dock's tiles one
//! of them belongs to.
//!
//! The windows the taskbar would list are gone through — those shown, with a name of their own, that no other window
//! owns, and that are not Glance's own nor the desktop's (see `listable`) — and each is asked which program it runs. A
//! tile is lit where one of them runs the same thing the tile opens (see `station::apps::opens`: a shortcut read
//! through to the program it points at, so a tile of the shortcut and a window of the program are one thing). The
//! list is taken again only as often as it is looked at, and never while the dock is away.
//!
//! Pressing a lit tile goes to its window: up if it was down, front if it was not, and back down if it already had
//! the front — what the taskbar does with its button.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetShellWindow, GetWindow, GetWindowLongW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, ShowWindow, GWL_EXSTYLE, GW_OWNER, SW_MINIMIZE, SW_RESTORE,
    WS_EX_TOOLWINDOW,
};

/// One window a person could go to.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub hwnd: HWND,
    /// What its process runs, by its image on the disk, lowercase; nothing where it could not be read. A tile is
    /// matched against this.
    pub path: Option<String>,
    /// Its name, as it is titled.
    pub title: String,
    /// Whether it holds the foreground.
    pub front: bool,
    /// Whether it is down.
    pub down: bool,
}

/// How often the windows are gone through again. The dock is drawn as the pointer moves over it, and would else ask
/// at every frame; a program opening or closing is told within this long of its happening.
const LOOKED: Duration = Duration::from_millis(400);

// (A window handle is the system's number for a window, good on any thread of the process that owns it: the list is
// kept once for the threads that ask for it, as the tray keeps its own window's (see `crate::tray`)).
unsafe impl Send for Found {}
unsafe impl Sync for Found {}

static HELD: Mutex<Option<(Instant, Vec<Found>)>> = Mutex::new(None);

/// The windows the taskbar would list, and what runs them: gone through again where the last going has run its
/// course. Glance's own windows are none of them, and so the dock never lists itself.
pub fn listed() -> Vec<Found> {
    let again = Instant::now();
    if let Some((was, found)) = HELD.lock().unwrap().as_ref() {
        if again.duration_since(*was) < LOOKED {
            return found.clone();
        }
    }
    let found = gather();
    *HELD.lock().unwrap() = Some((again, found.clone()));
    found
}

/// The window of the program `target` opens, where it is running one: none where it runs no window, or opens no
/// program at all (a folder, an address, one of Glance's doors).
pub fn for_target(target: &str) -> Option<Found> {
    let opened = crate::station::apps::opens(target).path;
    window_of(&opened)
}

/// The window of the program at `path` (lowercase, as `station::apps::opens` gives it), where one is open.
pub fn window_of(path: &str) -> Option<Found> {
    (!path.is_empty())
        .then(|| {
            listed()
                .into_iter()
                .find(|found| found.path.as_deref() == Some(path))
        })
        .flatten()
}

/// Going to `found`: up if it was down, front if it was not, and down again if it already had the front (what the
/// taskbar's button does). The dock was pressed to get here, which gives Glance the right to change the foreground.
pub fn go_to(found: &Found) {
    unsafe {
        if found.down {
            let _ = ShowWindow(found.hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(found.hwnd);
        } else if found.front {
            let _ = ShowWindow(found.hwnd, SW_MINIMIZE);
        } else {
            let _ = SetForegroundWindow(found.hwnd);
        }
    }
}

/// The windows a person could go to, and what runs each: gone through in turn, topmost first, so the first of a
/// program's windows is the one kept (two Explorer windows, the one the pointer last touched).
fn gather() -> Vec<Found> {
    let front = unsafe { GetForegroundWindow() };
    let own = std::process::id();
    let shell = pid_of(unsafe { GetShellWindow() });
    let mut seen = Seen {
        found: Vec::new(),
        front,
        own,
        shell,
    };
    // (The callback is given a pointer to what it gathers; `EnumWindows` goes through every window before it
    // returns, so what it points at is live all the while it is asked (see `crate::presents`).)
    let _ = unsafe { EnumWindows(Some(asked), LPARAM(&mut seen as *mut Seen as isize)) };
    let mut found = seen.found;
    // One window to a program: the first of its own seen, the topmost of them.
    let mut each: Vec<(String, HWND)> = Vec::new();
    found.retain(|one| {
        let Some(path) = &one.path else { return true };
        if each.iter().any(|(kept, _)| kept == path) {
            return false;
        }
        each.push((path.clone(), one.hwnd));
        true
    });
    found
}

/// What every window seen is asked against: the foreground, Glance's own process, the shell's.
struct Seen {
    found: Vec<Found>,
    front: HWND,
    own: u32,
    shell: u32,
}

/// One window seen by `EnumWindows`: kept where a person would go to it.
unsafe extern "system" fn asked(window: HWND, seen: LPARAM) -> BOOL {
    let seen = unsafe { &mut *(seen.0 as *mut Seen) };
    // (Glance's own windows, and the shell's, are never its own to go to.)
    let pid = pid_of(window);
    if pid == 0 || pid == seen.own || pid == seen.shell || !listable(window) {
        return BOOL(1);
    }
    let title = title_of(window);
    if title.is_empty() {
        return BOOL(1);
    }
    seen.found.push(Found {
        hwnd: window,
        path: path_of(pid),
        title,
        front: window == seen.front,
        down: unsafe { IsIconic(window).as_bool() },
    });
    BOOL(1)
}

/// Whether `window` is one the taskbar would list: shown, its own rather than a dialog of another, not a tool
/// window's, and not drawn behind the glass — a host such as the input experience keeps a visible window the
/// desktop compositor hides (cloaks), and the shell would never list it (see `widget`'s and `catch`'s same ask).
unsafe fn listable(window: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    if !IsWindowVisible(window).as_bool() || GetShellWindow() == window {
        return false;
    }
    // (A dialog goes with the window it was opened from, which is the one listed.)
    if GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
        return false;
    }
    if GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    let mut cloaked = 0u32;
    let cloaked = unsafe {
        DwmGetWindowAttribute(window, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4)
    }
    .is_ok()
        && cloaked != 0;
    !cloaked
}

/// What process `window` runs.
fn pid_of(window: HWND) -> u32 {
    if window.is_invalid() {
        return 0;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    pid
}

/// What the process `pid` runs, by its image on the disk, lowercase: nothing where it could not be opened, or named
/// (a process of another's, kept from this one). A tile of what runs here never lights: Glance's own windows are not
/// listed at all.
fn path_of(pid: u32) -> Option<String> {
    let mut name = [0u16; 1024];
    let mut length = name.len() as u32;
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        if named.is_err() || length == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&name[..length as usize]).to_lowercase())
    }
}

/// A window's name, as it is titled: empty where it says nothing.
fn title_of(window: HWND) -> String {
    let long = unsafe { GetWindowTextLengthW(window) };
    if long <= 0 {
        return String::new();
    }
    let mut title = vec![0u16; long as usize + 1];
    let length = unsafe { GetWindowTextW(window, &mut title) };
    String::from_utf16_lossy(&title[..length.max(0) as usize])
        .trim()
        .to_string()
}
