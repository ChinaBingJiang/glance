//! The installed memory modules, from the hardware inventory the firmware
//! hands Windows (SMBIOS). Readable without administrator rights.

use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};

/// SMBIOS structure type of one memory device (a module, or an empty slot).
const MEMORY_DEVICE: u8 = 17;
/// The BIOS's own information: its version string is the fifth field.
const BIOS_INFO: u8 = 0;
/// The system information: who made it, what it is called, and the number
/// stamped on it.
const SYSTEM_INFO: u8 = 1;
/// The case it comes in: its type (the sixth byte) says whether the machine
/// opens like a book or sits on a desk.
const ENCLOSURE: u8 = 3;
const END_OF_TABLE: u8 = 127;
/// Bytes before the table in what Windows returns: calling method, version
/// (major, minor, revision) and the table's length.
const RAW_HEADER: usize = 8;

#[derive(Debug, PartialEq)]
pub struct Module {
    pub megabytes: u64,
    pub kind: &'static str,
    /// The speed the module runs at, in MT/s, if the firmware says (else
    /// what it is rated for).
    pub speed: Option<u16>,
    /// The speed it is rated for (MT/s).
    pub rated: Option<u16>,
}

/// Every populated memory slot.
pub fn memory_modules() -> Vec<Module> {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    let mut raw = vec![0u8; size as usize];
    if size == 0 || unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut raw)) } != size {
        return Vec::new();
    }
    parse(&raw[RAW_HEADER.min(raw.len())..])
}

fn parse(table: &[u8]) -> Vec<Module> {
    let mut modules = Vec::new();
    let mut at = 0;
    while at + 4 <= table.len() {
        let kind = table[at];
        let length = table[at + 1] as usize;
        if kind == END_OF_TABLE || length < 4 || at + length > table.len() {
            break;
        }
        let fields = &table[at..at + length];
        if kind == MEMORY_DEVICE {
            if let Some(module) = memory_device(fields) {
                modules.push(module);
            }
        }
        // The formatted part is followed by its strings, ended by two zeros.
        let mut next = at + length;
        while next + 1 < table.len() && !(table[next] == 0 && table[next + 1] == 0) {
            next += 1;
        }
        at = next + 2;
    }
    modules
}

fn word(fields: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(fields.get(at..at + 2)?.try_into().ok()?))
}

fn memory_device(fields: &[u8]) -> Option<Module> {
    let size = word(fields, 0x0C)?;
    let megabytes = match size {
        // No module in this slot, or unknown.
        0 | 0xFFFF => return None,
        // The real size is in the extended field, in MB.
        0x7FFF => u32::from_le_bytes(fields.get(0x1C..0x20)?.try_into().ok()?) as u64 & 0x7FFF_FFFF,
        // Bit 15 set: the size is in KB.
        size if size & 0x8000 != 0 => (size & 0x7FFF) as u64 / 1024,
        size => size as u64,
    };
    let kind = match *fields.get(0x12)? {
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    };
    // The configured speed (what it runs at) where given, else the rated one.
    let rated = word(fields, 0x15).filter(|&s| s != 0);
    let speed = word(fields, 0x20).filter(|&s| s != 0).or(rated);
    Some(Module {
        megabytes,
        kind,
        speed,
        rated,
    })
}

/// What the memory runs at (MT/s, the first module's), and what it is
/// rated for, where that is more: run below it (XMP or EXPO off, often).
pub fn speed(modules: &[Module]) -> Option<(u16, Option<u16>)> {
    let first = modules.first()?;
    Some((
        first.speed?,
        first
            .rated
            .filter(|&rated| rated > first.speed.unwrap_or(0)),
    ))
}

/// The machine as a whole, named by its own firmware: whose making it is,
/// which model, the number stamped on it, and the BIOS it wears.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Inventory {
    pub maker: String,
    pub product: String,
    pub serial: String,
    pub bios: String,
    /// A machine you carry and open: a laptop, a notebook, a thing you hold
    /// in one hand. False says the case says desktop, or nothing at all.
    pub laptop: bool,
}

/// The inventory read whole off the firmware's table; nothing where the
/// firmware hands over no table at all.
pub fn system() -> Inventory {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    let mut raw = vec![0u8; size as usize];
    if size == 0 || unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut raw)) } != size {
        return Inventory::default();
    }
    told(&raw[RAW_HEADER.min(raw.len())..])
}

/// The two modules that speak for the machine, picked in one walk; every
/// other one is stepped over as `parse` steps over it.
fn told(table: &[u8]) -> Inventory {
    let (mut found, mut bios, mut case) = (Inventory::default(), String::new(), 0u8);
    let mut at = 0;
    while at + 4 <= table.len() {
        let kind = table[at];
        let length = table[at + 1] as usize;
        if kind == END_OF_TABLE || length < 4 || at + length > table.len() {
            break;
        }
        let fields = &table[at..at + length];
        // The formatted part is followed by its strings, ended by two zeros.
        let mut next = at + length;
        while next + 1 < table.len() && !(table[next] == 0 && table[next + 1] == 0) {
            next += 1;
        }
        let said = strings(&table[at + length..next]);
        match kind {
            SYSTEM_INFO if found.product.is_empty() && found.maker.is_empty() => {
                found = Inventory {
                    maker: kept(fields, &said, 4),
                    product: kept(fields, &said, 5),
                    serial: kept(fields, &said, 7),
                    bios: String::new(),
                    laptop: false,
                };
            }
            BIOS_INFO if bios.is_empty() => {
                bios = kept(fields, &said, 5);
            }
            ENCLOSURE if case == 0 => {
                case = fields.get(5).copied().unwrap_or(0);
            }
            _ => {}
        }
        at = next + 2;
    }
    found.bios = bios;
    // (SMBIOS 2.x: 8 portable, 9 laptop, 10 notebook, 11 handheld, 12 dock,
    // 14 sub-notebook. A machine of no told case is taken to sit still.)
    found.laptop = matches!(case, 8..=12 | 14);
    found
}

/// The strings kept after one module, in the order they lie there: each up
/// to its own zero, the run over when a zero opens where a letter should.
fn strings(raw: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < raw.len() && raw[at] != 0 {
        let end = at
            + raw[at..]
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(raw.len() - at);
        out.push(String::from_utf8_lossy(&raw[at..end]).trim().to_string());
        at = end + 1;
    }
    out
}

/// A string the module points at from its `at`th byte — the references count
/// from one, and zero says there is none. What the factory left unfilled
/// counts as none too: it would only make a page lie.
fn kept(fields: &[u8], said: &[String], at: usize) -> String {
    let Some(&reference) = fields.get(at) else {
        return String::new();
    };
    if reference == 0 {
        return String::new();
    }
    let Some(said) = said.get(reference as usize - 1) else {
        return String::new();
    };
    if said.is_empty()
        || said.eq_ignore_ascii_case("to be filled by o.e.m.")
        || said.eq_ignore_ascii_case("default string")
        || said.eq_ignore_ascii_case("system manufacturer")
        || said.eq_ignore_ascii_case("system product name")
        || said.eq_ignore_ascii_case("system serial number")
    {
        return String::new();
    }
    said.clone()
}

/// "2 × 32 GB DDR5-6000", or as much of it as is known.
pub fn describe(modules: &[Module]) -> Option<String> {
    let first = modules.first()?;
    let alike = modules.iter().all(|m| m.megabytes == first.megabytes);
    let size = |mb: u64| {
        if mb % 1024 == 0 {
            format!("{} GB", mb / 1024)
        } else {
            format!("{mb} MB")
        }
    };
    let mut text = if alike {
        format!("{} × {}", modules.len(), size(first.megabytes))
    } else {
        size(modules.iter().map(|m| m.megabytes).sum())
    };
    if !first.kind.is_empty() {
        text.push(' ');
        text.push_str(first.kind);
        if let Some(speed) = first.speed {
            text.push_str(&format!("-{speed}"));
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_machines_memory() {
        let modules = memory_modules();
        println!(
            "{modules:?} -> {:?}, {:?}",
            describe(&modules),
            speed(&modules)
        );
        assert!(!modules.is_empty());
    }

    #[test]
    fn tells_a_speed_below_the_rated_one() {
        let module = |speed, rated| Module {
            megabytes: 16384,
            kind: "DDR5",
            speed: Some(speed),
            rated: Some(rated),
        };
        assert_eq!(speed(&[module(4800, 6000)]), Some((4800, Some(6000))));
        assert_eq!(speed(&[module(6000, 6000)]), Some((6000, None)));
        assert_eq!(speed(&[]), None);
    }

    #[test]
    fn describes_modules() {
        let ddr5 = |mb| Module {
            megabytes: mb,
            kind: "DDR5",
            speed: Some(6000),
            rated: Some(6000),
        };
        assert_eq!(
            describe(&[ddr5(32768), ddr5(32768)]).unwrap(),
            "2 × 32 GB DDR5-6000"
        );
        assert_eq!(
            describe(&[ddr5(16384), ddr5(32768)]).unwrap(),
            "48 GB DDR5-6000"
        );
        assert_eq!(describe(&[]), None);
    }

    #[test]
    fn strings_run_until_a_zero_opens_where_a_letter_should() {
        assert_eq!(
            strings(b"HONOR\0FMB-P\0A7YC9\0"),
            ["HONOR", "FMB-P", "A7YC9"]
        );
        // (None at all: the module carries no strings.)
        assert!(strings(b"\0\0").is_empty());
    }

    #[test]
    fn an_unfilled_stamping_counts_as_none() {
        // (One module's formatted part, byte 4 pointing at its first string.)
        let fields = [1u8, 8, 0, 0, 1, 0, 0, 2];
        let said = vec!["LENOVO".to_string(), "To be filled by O.E.M.".to_string()];
        assert_eq!(kept(&fields, &said, 4), "LENOVO");
        assert_eq!(kept(&fields, &said, 7), "");
        // (A reference of none, and one past what is kept.)
        assert_eq!(kept(&[0u8; 8], &said, 4), "");
        assert_eq!(kept(&[1, 1, 1, 1, 9, 1, 1, 1], &said, 4), "");
    }

    #[test]
    fn the_machine_names_itself() {
        let said = system();
        println!("{said:?}");
        assert!(!said.product.is_empty() || !said.maker.is_empty());
    }

    #[test]
    fn a_laptop_is_told_by_its_case() {
        // (One enclosure module: its formatted part, then its one string, the
        // run ended by two zeros — byte 5 carrying the case's type.)
        let case = |kind: u8| {
            let mut table = vec![ENCLOSURE, 6, 0, 0, 1, kind];
            table.extend_from_slice(b"Case\0\0");
            told(&table).laptop
        };
        assert!(case(10)); // a notebook
        assert!(case(9)); // a laptop
        assert!(!case(3)); // a desktop
        assert!(!case(0)); // nothing said
    }
}
