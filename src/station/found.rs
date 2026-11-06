//! Every program Windows lists in its Start menu (`shell:AppsFolder`: desktop programs and Store apps alike, not the
//! documents, folders and web addresses it lists beside them), by name and by what opens it, for quick launch's choosing (see `ui::beside`). Read on a thread of its own, a single-threaded
//! apartment (the shell's folders expect one), when first asked and again each time after; searched by name and by
//! the first letters of a Chinese name's pinyin.

use std::sync::{Arc, Mutex};

/// A program the Start menu lists: its name, what opens it (`shell:AppsFolder\…`), and the first letters its name's
/// words may have (its pinyin's, for Chinese: see `initials`) for searching.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Found {
    pub name: String,
    pub target: String,
    pub initials: Vec<u64>,
}

static FOUND: Mutex<Option<Arc<Vec<Found>>>> = Mutex::new(None);
static LOOKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, feature = "studio"))]
thread_local! {
    /// What this thread's pictures pretend (see `pretend`): each test's its own, as tests run side by side.
    static PRETENDED: std::cell::RefCell<Option<Arc<Vec<Found>>>> = const { std::cell::RefCell::new(None) };
}

/// What the Start menu was found to list, by name (none till first read).
pub fn all() -> Option<Arc<Vec<Found>>> {
    #[cfg(all(test, feature = "studio"))]
    if let Some(pretended) = PRETENDED.with(|pretended| pretended.borrow().clone()) {
        return Some(pretended);
    }
    FOUND.lock().unwrap().clone()
}

/// For pictures: as if the Start menu listed these.
#[cfg(all(test, feature = "studio"))]
pub fn pretend(found: Vec<Found>) {
    PRETENDED.with(|pretended| *pretended.borrow_mut() = Some(Arc::new(found)));
}

/// The Start menu read (again) on a thread of its own; `told` is called once it is.
pub fn look(told: impl Fn() + Send + 'static) {
    use std::sync::atomic::Ordering;
    if LOOKING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::Builder::new()
        .name("glance: start menu".into())
        .spawn(move || {
            // (Its apartment left once the shell's objects are let go of, as `read` returns.)
            let joined = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) }.is_ok();
            let found = read();
            if joined {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
            if let Some(found) = found {
                *FOUND.lock().unwrap() = Some(Arc::new(found));
            }
            LOOKING.store(false, Ordering::Release);
            told();
        })
        .expect("a thread started");
}

/// The Start menu's programs, by name.
fn read() -> Option<Vec<Found>> {
    use windows::Win32::UI::Shell::{BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING};
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None).ok()?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems).ok()?;
        let named = |item: &IShellItem, how| -> Option<String> {
            let name = item.GetDisplayName(how).ok()?;
            let text = name.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
            text
        };
        let mut found = Vec::new();
        loop {
            let mut one = [None];
            let mut got = 0u32;
            if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(item) = one[0].take() else { break };
            let (Some(name), Some(parsing)) = (named(&item, SIGDN_NORMALDISPLAY), named(&item, SIGDN_PARENTRELATIVEPARSING)) else { continue };
            // (Programs only: the documents, folders and web addresses listed beside them are left out.)
            let target = format!("shell:AppsFolder\\{parsing}");
            if !crate::station::apps::is_program(&target) {
                continue;
            }
            // (And a program's door out is left out with them: the same word is on the tile's menu instead.)
            if crate::station::apps::is_uninstaller(&name, &target) {
                continue;
            }
            let initials = initials(&name);
            found.push(Found { name, target, initials });
        }
        // (By the first letter each is listed under, as the Start menu groups them, then by name.)
        found.sort_by(|a, b| (a.initials.first().map(|bits| bits.trailing_zeros()), &a.name).cmp(&(b.initials.first().map(|bits| bits.trailing_zeros()), &b.name)));
        found.dedup_by(|a, b| a.target == b.target);
        Some(found)
    }
}

/// The first letters of `name`'s words, each as the letters it may be (bits 0 to 25 'a' to 'z', 26 to 35 the digits):
/// a Latin word's first letter (and its digits), a Chinese character's readings' (all of them: 乐 is lè and yuè);
/// other characters none.
pub fn initials(name: &str) -> Vec<u64> {
    let mut initials = Vec::new();
    let mut in_word = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if !in_word || c.is_ascii_digit() {
                initials.push(bit(c));
            }
            in_word = true;
        } else if let Some(letters) = pinyin_initials(c) {
            initials.push(letters);
            in_word = false;
        } else {
            in_word = false;
        }
    }
    initials
}

/// The bit of a letter or a digit (see `initials`).
fn bit(c: char) -> u64 {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => 1 << (c as u32 - 'a' as u32),
        c @ '0'..='9' => 1 << (26 + c as u32 - '0' as u32),
        _ => 0,
    }
}

/// The first letters of a Chinese character's Mandarin readings (see `initials`), as the Unicode Character Database
/// gives them (built from its Unihan readings by scripts/pinyin.py, for U+3400 to U+9FFF); none for another.
fn pinyin_initials(c: char) -> Option<u64> {
    static TABLE: &[u8] = include_bytes!("pinyin.bin");
    let at = (c as u32).checked_sub(0x3400)? as usize * 4;
    let mask = u32::from_le_bytes(TABLE.get(at..at + 4)?.try_into().ok()?);
    (mask != 0).then_some(u64::from(mask))
}

/// Whether `found` is one `query` asks for: its name holding it (in any case), or its words' first letters beginning
/// with it (each of the query's letters one its word may begin with).
pub fn matches(found: &Found, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() || found.name.to_lowercase().contains(&query) {
        return true;
    }
    let letters: Vec<u64> = query.chars().filter(|c| !c.is_whitespace()).map(bit).collect();
    letters.iter().all(|letter| *letter != 0) && letters.len() <= found.initials.len() && letters.iter().zip(&found.initials).all(|(letter, may)| letter & may != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a name may be searched by: a Latin word's first letter, a Chinese character's pinyin's (each of its
    /// readings'), digits as they are; part of its name, in any case.
    #[test]
    fn finds_by_name_or_initials() {
        let found = |name: &str| Found { name: name.into(), target: String::new(), initials: initials(name) };
        for (name, query) in [("网易云音乐", "wyyyy"), ("网易云音乐", "wyyyl"), ("网易云音乐", "云音乐"), ("QQ音乐", "qyy"), ("微信", "WX"), ("文件资源管理器", "wjzyglq"), ("Visual Studio Code", "vsc"), ("火绒安全软件", "hraq"), ("7-Zip", "7z")] {
            assert!(matches(&found(name), query), "{name} by {query}");
        }
        for (name, query) in [("网易云音乐", "yyy"), ("微信", "wxx"), ("Visual Studio Code", "vsd")] {
            assert!(!matches(&found(name), query), "{name} not by {query}");
        }
        assert!(matches(&found("微信"), ""));
    }
}
