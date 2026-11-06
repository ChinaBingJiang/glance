//! Quick launch: the programs, files, folders and web addresses kept at hand on the workbench, in their order.
//! Each is kept as Windows names it to be opened: a program from the Start
//! menu by its shell name (`shell:AppsFolder\…`, a Store app's and a desktop program's alike), a shortcut, file or
//! folder by its path (a shortcut kept as the shortcut), a web address as it is. Opened as the user, by the shell, on
//! the station's thread (see `run::Ask::Launch`): Glance's UI runs unelevated, so what it opens does too.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One thing kept at hand: what opens it, and its name as it is shown.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct App {
    pub target: String,
    pub name: String,
}

impl App {
    /// A file, folder or shortcut dropped to be kept at hand: by its name without its extension (a folder's whole).
    pub fn of_path(path: &Path) -> Option<App> {
        let name = if path.is_dir() { path.file_name()? } else { path.file_stem()? };
        Some(App { target: path.display().to_string(), name: name.to_string_lossy().into_owned() })
    }

    /// A web address kept at hand: by its site.
    pub fn of_url(url: &str) -> Option<App> {
        let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
        let site = rest.split(['/', '?', '#']).next()?.trim_start_matches("www.");
        (!site.is_empty()).then(|| App { target: url.to_string(), name: site.to_string() })
    }

    /// Whether it is a web address.
    pub fn is_url(&self) -> bool {
        self.target.starts_with("https://") || self.target.starts_with("http://")
    }

    /// Where it is on the disk, for it to be shown in Explorer (none for a Start menu program or a web address).
    pub fn location(&self) -> Option<PathBuf> {
        (!self.is_url() && !self.target.starts_with("shell:")).then(|| PathBuf::from(&self.target))
    }
}

static SHOWN: Mutex<Option<Arc<Vec<App>>>> = Mutex::new(None);

#[cfg(any(test, feature = "studio"))]
thread_local! {
    /// What this thread's pictures pretend (see `pretend`): each test's its own, as tests run side by side.
    static PRETENDED: std::cell::RefCell<Option<Arc<Vec<App>>>> = const { std::cell::RefCell::new(None) };
}

/// What is kept at hand, in its order.
pub fn shown() -> Arc<Vec<App>> {
    #[cfg(any(test, feature = "studio"))]
    if let Some(pretended) = PRETENDED.with(|pretended| pretended.borrow().clone()) {
        return pretended;
    }
    SHOWN.lock().unwrap().clone().unwrap_or_default()
}

static NUMBERS: crate::numbered::Numbered = crate::numbered::Numbered::new();

/// The number a press on what is kept at hand opening `target` names it by (see `numbered`).
pub fn number_of(target: &str) -> u64 {
    NUMBERS.of(target)
}

/// What is kept at hand that `number` stands for (see `number_of`): none where it is gone.
pub fn numbered(number: u64) -> Option<App> {
    shown().iter().find(|app| number_of(&app.target) == number).cloned()
}

/// The settings' list taken as what is kept at hand (as they are read, and each time they change).
pub fn follow(apps: &[App]) {
    let mut shown = SHOWN.lock().unwrap();
    if shown.as_deref().map(Vec::as_slice) != Some(apps) {
        *shown = Some(Arc::new(apps.to_vec()));
    }
}

/// For pictures: as if these were kept at hand.
#[cfg(any(test, feature = "studio"))]
pub fn pretend(apps: Vec<App>) {
    PRETENDED.with(|pretended| *pretended.borrow_mut() = Some(Arc::new(apps)));
}

/// What a thing kept at hand opens (see `opens`): the path of what it opens (or a Store app's name, a web address),
/// and what that is given (a shortcut's arguments: two shortcuts to one browser, each to a profile of its own, open two
/// things).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Opened {
    pub path: String,
    pub arguments: String,
}

/// What `target` (a thing kept at hand, or a program the Start menu lists) opens, as one thing however it is named:
/// a desktop program the Start menu names by a known folder and its path in it (`shell:AppsFolder\{folder}\a.exe`)
/// by the file's path, a shortcut by what it points at with what it gives it, a path the same in any case; a Store
/// app, a folder, a web address as it is. Two that open the same are one thing kept (a program dropped on quick launch is the Start
/// menu's). Shortcuts are read through the shell (a thread in a COM apartment); found once each (a shortcut not
/// read for want of an apartment is not kept as found: read again where there is one).
pub fn opens(target: &str) -> Opened {
    static FOUND: Mutex<Option<std::collections::HashMap<String, Opened>>> = Mutex::new(None);
    if let Some(found) = FOUND.lock().unwrap().as_ref().and_then(|found| found.get(target).cloned()) {
        return found;
    }
    let (found, settled) = resolved(target);
    if !settled {
        return found;
    }
    let mut held = FOUND.lock().unwrap();
    let held = held.get_or_insert_with(Default::default);
    if held.len() > 2000 {
        held.clear();
    }
    held.insert(target.to_string(), found.clone());
    found
}

/// Whether two things kept at hand (or listed) open the same (see `opens`).
pub fn same(a: &str, b: &str) -> bool {
    a == b || opens(a) == opens(b)
}

/// What `target` opens (see `opens`), and whether that is settled: not where a shortcut could not be read and this
/// thread is in no COM apartment (where the shell cannot be asked).
fn resolved(target: &str) -> (Opened, bool) {
    let itself = || Opened { path: target.to_string(), arguments: String::new() };
    if target.starts_with("https://") || target.starts_with("http://") {
        return (itself(), true);
    }
    let path = match target.strip_prefix("shell:AppsFolder\\") {
        // (A known folder's number, then the path in it; or a path; else a Store app's name.)
        Some(named) => match named.strip_prefix('{').and_then(|rest| rest.split_once("}\\")) {
            Some((folder, rest)) => match known_folder(folder) {
                Some(folder) => folder.join(rest),
                None => return (itself(), true),
            },
            None if Path::new(named).is_absolute() => PathBuf::from(named),
            None => return (itself(), true),
        },
        None => PathBuf::from(target),
    };
    let ((path, arguments), settled) = if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("lnk")) {
        match link_target(&path) {
            Some(target) => (target, true),
            None => ((path, String::new()), in_apartment()),
        }
    } else {
        ((path, String::new()), true)
    };
    (Opened { path: path.to_string_lossy().to_lowercase(), arguments: arguments.trim().to_string() }, settled)
}

/// Whether this thread is in a COM apartment (the shell may be asked from it).
#[cfg(windows)]
fn in_apartment() -> bool {
    let (mut kind, mut qualifier) = (Default::default(), Default::default());
    unsafe { windows::Win32::System::Com::CoGetApartmentType(&mut kind, &mut qualifier) }.is_ok()
}

#[cfg(not(windows))]
fn in_apartment() -> bool {
    true
}

/// The path of the known folder numbered `id` (its GUID's digits, without braces).
#[cfg(windows)]
fn known_folder(id: &str) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let guid = windows::core::GUID::try_from(id).ok()?;
    unsafe {
        let found = SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None).ok()?;
        let path = found.to_string().ok().map(PathBuf::from);
        windows::Win32::System::Com::CoTaskMemFree(Some(found.0 as *const _));
        path
    }
}

/// What the shortcut at `path` points at, where it says, and what it gives it (on a thread in a COM apartment).
#[cfg(windows)]
fn link_target(path: &Path) -> Option<(PathBuf, String)> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        link.cast::<IPersistFile>().ok()?.Load(&windows::core::HSTRING::from(path.as_os_str()), STGM_READ).ok()?;
        let mut found = [0u16; 1024];
        link.GetPath(&mut found, std::ptr::null_mut(), 0).ok()?;
        let text = |units: &[u16]| String::from_utf16_lossy(&units[..units.iter().position(|unit| *unit == 0).unwrap_or(units.len())]);
        let found = text(&found);
        // (As long as a command line may be.)
        let mut given = vec![0u16; 32_768];
        link.GetArguments(&mut given).ok()?;
        (!found.is_empty()).then(|| (PathBuf::from(found), text(&given)))
    }
}

#[cfg(not(windows))]
fn known_folder(_: &str) -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
fn link_target(_: &Path) -> Option<(PathBuf, String)> {
    None
}

/// Whether `target` (a thing the Start menu lists) is a program: a Store app, or a program known by its own name
/// (neither has a folder in it), or a program file (a shortcut the shell could not read is taken for one: what it
/// points at is a program installed to be set up when first opened). The Start menu lists documents, folders and
/// web addresses beside them, which quick launch's picker leaves out.
pub fn is_program(target: &str) -> bool {
    let opened = opens(target).path;
    let named = opened.strip_prefix("shell:AppsFolder\\").unwrap_or(&opened);
    if named.starts_with("http://") || named.starts_with("https://") {
        return false;
    }
    !named.contains('\\') || runs(Path::new(named))
}

/// Whether the file at `path` is a program shown by its own picture (a program, a shortcut, a console Windows opens:
/// one with an icon of its own), not a file shown by its kind (a document, or a script, which is a file to its user).
pub fn pictured(path: &Path) -> bool {
    const OWN: [&str; 6] = ["exe", "com", "msc", "cpl", "appref-ms", "lnk"];
    path.extension().is_some_and(|extension| OWN.iter().any(|own| extension.eq_ignore_ascii_case(own)))
}

/// Whether the file at `path` is one that runs (a program, a script Windows runs: a batch file, Windows Script Host's,
/// a shortcut, by its extension), not a document opened by one.
pub fn runs(path: &Path) -> bool {
    const PROGRAMS: [&str; 14] = ["exe", "com", "bat", "cmd", "msc", "cpl", "appref-ms", "lnk", "vbs", "vbe", "js", "jse", "wsf", "wsh"];
    path.extension().is_some_and(|extension| PROGRAMS.iter().any(|program| extension.eq_ignore_ascii_case(program)))
}

/// Whether what the Start menu lists under `name`, lying at `where_it_lies`, is a program's door out rather than a
/// program to be used: "Uninstall Foo", "卸载 Foo", a shortcut standing in an installer's own `Uninstall` folder, or
/// Inno Setup's `unins000.exe`. Such a thing says nothing of itself: it is only ever run to take another program off
/// the machine, and so is no program to be found by name. A utility whose own name says what it is (Revo Uninstaller)
/// stays, for all its word: only a name beginning with the word is a door out.
pub fn is_uninstaller(name: &str, where_it_lies: &str) -> bool {
    let named = name.trim().to_lowercase();
    if named.starts_with("卸载") || named.starts_with("uninstall") { return true; }
    let lies = where_it_lies.to_lowercase().replace('/', "\\");
    let (folder, last) = match lies.rsplit_once('\\') { Some((here, one)) => (here, one), None => ("", lies.as_str()) };
    let stem = last.split('.').next().unwrap_or(last);
    folder.split('\\').any(|part| part == "uninstall") || stem.starts_with("unins") || stem == "uninstall"
}

/// The thing kept that opens `moving` put before the one that opens `before` (none: last), each found where it is
/// now (what is kept may have changed since they were shown): nothing where it is gone, or put before itself.
pub fn put_before(apps: &mut Vec<App>, moving: &str, before: Option<&str>) {
    if before == Some(moving) {
        return;
    }
    let Some(from) = apps.iter().position(|app| app.target == moving) else { return };
    let app = apps.remove(from);
    let to = before.and_then(|before| apps.iter().position(|app| app.target == before)).unwrap_or(apps.len());
    apps.insert(to, app);
}

/// What is kept at hand changed as `change` changes it (off the calling thread, which the settings then tell).
pub fn change(change: impl FnOnce(&mut Vec<App>) + Send + 'static) {
    std::thread::spawn(move || {
        // (In a COM apartment: what is kept is told apart by what it opens, a shortcut read through the shell.)
        #[cfg(windows)]
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
        crate::app().change(|settings| change(&mut settings.apps));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program the Start menu names by a known folder and the path in it is the program dropped from that folder,
    /// in any case; a Store app and a web address are only themselves.
    #[test]
    #[cfg(windows)]
    fn knows_one_program_however_it_is_named() {
        let system = known_folder("1AC14E77-02E7-4E5D-B744-2EB1AE5198B7").expect("the System folder");
        let dropped = system.join("charmap.exe").to_string_lossy().to_uppercase();
        assert!(same(r"shell:AppsFolder\{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\charmap.exe", &dropped));
        assert!(!same(r"shell:AppsFolder\{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\charmap.exe", &system.join("notepad.exe").to_string_lossy()));
        assert!(!same(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", r"shell:AppsFolder\Microsoft.Paint_8wekyb3d8bbwe!App"));
        assert!(same("https://github.com", "https://github.com"));
        assert!(!same("https://github.com", "https://GITHUB.com/a"));
    }

    /// Shortcuts to one program are one thing kept where they give it the same, two where they give it each its own
    /// (two profiles of a browser); one giving it nothing is the program itself.
    #[test]
    #[cfg(windows)]
    fn knows_shortcuts_apart_by_what_they_give() {
        use windows::core::{Interface, HSTRING};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let dir = crate::settings::local_dir().join("apps-shortcuts");
        std::fs::create_dir_all(&dir).unwrap();
        let program = known_folder("1AC14E77-02E7-4E5D-B744-2EB1AE5198B7").expect("the System folder").join("notepad.exe");
        let shortcut = |name: &str, arguments: &str| {
            let path = dir.join(name);
            unsafe {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(&HSTRING::from(program.as_os_str())).unwrap();
                link.SetArguments(&HSTRING::from(arguments)).unwrap();
                link.cast::<IPersistFile>().unwrap().Save(&HSTRING::from(path.as_os_str()), true).unwrap();
            }
            path.to_string_lossy().into_owned()
        };
        let (one, two, again, bare) = (shortcut("one.lnk", "--profile-directory=Default"), shortcut("two.lnk", "--profile-directory=\"Profile 1\""), shortcut("again.lnk", "--profile-directory=Default"), shortcut("bare.lnk", ""));
        assert!(!same(&one, &two));
        assert!(same(&one, &again));
        assert!(same(&bare, &program.to_string_lossy()));
        assert!(!same(&one, &program.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the Start menu lists that is a program: a Store app, a program known by its own name, a program file
    /// however its folder is named; not a document, a folder or a web address it lists beside them.
    #[test]
    #[cfg(windows)]
    fn lists_programs_only() {
        for target in [
            r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
            r"shell:AppsFolder\Microsoft.Windows.Explorer",
            r"shell:AppsFolder\{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\charmap.exe",
            r"shell:AppsFolder\D:\App\RivaTuner Statistics Server\RTSS.exe",
            r"shell:AppsFolder\C:\Tools\Setup.CMD",
        ] {
            assert!(is_program(target), "{target}");
        }
        for target in [
            r"shell:AppsFolder\D:\App\HWiNFO\HWiNFO Manual.pdf",
            r"shell:AppsFolder\D:\App\MSI Afterburner\SDK\Samples",
            r"shell:AppsFolder\{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\Windows Kits\10\Shortcuts\DesktopAppsSamples.url",
            r"shell:AppsFolder\http://cidian.youdao.com/",
            r"shell:AppsFolder\https://nodejs.org/",
        ] {
            assert!(!is_program(target), "{target}");
        }
    }

    /// A thing dragged among what is kept goes before the one it was dropped before, found where it is now: what
    /// is kept changed meanwhile (one taken off) moves neither another nor it elsewhere.
    #[test]
    fn puts_a_dragged_thing_before_the_one_it_was_dropped_before() {
        let kept = |names: &[&str]| names.iter().map(|name| App { target: (*name).into(), name: (*name).into() }).collect::<Vec<_>>();
        let names = |apps: &[App]| apps.iter().map(|app| app.target.clone()).collect::<Vec<_>>();
        let mut apps = kept(&["a", "b", "c", "d"]);
        put_before(&mut apps, "d", Some("b"));
        assert_eq!(names(&apps), ["a", "d", "b", "c"]);
        put_before(&mut apps, "a", None);
        assert_eq!(names(&apps), ["d", "b", "c", "a"]);
        // Shown as [a, b, c], b dragged before c; a taken off meanwhile: b stays before c, c stays where it is.
        let mut apps = kept(&["b", "c"]);
        put_before(&mut apps, "b", Some("c"));
        assert_eq!(names(&apps), ["b", "c"]);
        // Gone meanwhile: nothing moves.
        let mut apps = kept(&["a", "c"]);
        put_before(&mut apps, "b", Some("a"));
        assert_eq!(names(&apps), ["a", "c"]);
        // Its neighbour gone meanwhile: last.
        let mut apps = kept(&["a", "b"]);
        put_before(&mut apps, "a", Some("x"));
        assert_eq!(names(&apps), ["b", "a"]);
    }

    /// A dropped file by its name without its extension, a web address by its site; where each is on the disk.
    #[test]
    fn keeps_what_is_dropped() {
        let file = App::of_path(Path::new(r"C:\Tools\Notepad++.lnk")).unwrap();
        assert_eq!((file.name.as_str(), file.location()), ("Notepad++", Some(PathBuf::from(r"C:\Tools\Notepad++.lnk"))));
        let site = App::of_url("https://www.github.com/lulu-loopp/glance").unwrap();
        assert_eq!((site.name.as_str(), site.is_url(), site.location()), ("github.com", true, None));
        let store = App { target: r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), name: "Calculator".into() };
        assert_eq!(store.location(), None);
        assert!(App::of_url("ftp://x").is_none());
    }

    /// A door out known by its words or by where it stands; a utility whose own name says it stays on the list.
    #[test]
    fn leaves_the_door_out_off_the_list() {
        assert!(is_uninstaller("Uninstall Foo", r"C:\Tools\Foo\Uninstall.exe"));
        assert!(is_uninstaller("卸载 Foo", r"C:\Tools\Foo\foo.exe"));
        assert!(is_uninstaller("Foo", r"C:\Tools\Foo\uninstall\remove.exe"));
        assert!(is_uninstaller("Foo", r"C:\Tools\Foo\unins000.exe"));
        assert!(!is_uninstaller("Revo Uninstaller", r"C:\Tools\Revo\RevoUninstaller.exe"));
        assert!(!is_uninstaller("Foo", r"C:\Tools\Foo\Foo.exe"));
    }
}
