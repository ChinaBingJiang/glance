//! The machine's network adapters, as Windows' address lists tells them: which
//! are up, the addresses each holds, and which one the internet leaves by. It
//! leans on `GetAdaptersAddresses`, the same the metrics read the current
//! network from. Nothing here blocks the drawn frame: `read` takes the
//! snapshot in one call and is asked off the render thread.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetBestInterface, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

/// Windows' interface types, of the ones a person would name a card.
const ETHERNET: u32 = 6;
const WIRELESS: u32 = 71;
/// Loopback and tunnels: no card of the machine's own, left out of the list.
const LOOPBACK: u32 = 24;
const TUNNEL: u32 = 131;

/// One of the machine's adapters, as the address list tells it.
#[derive(Clone)]
pub struct Adapter {
    /// The name the connection goes by ("WLAN", "Ethernet").
    pub name: String,
    /// The card's own description, as its driver names it.
    pub description: String,
    /// Windows' interface type: 6 wired, 71 wireless, other numbers the rest.
    pub kind: u32,
    /// Whether it is up and carrying.
    pub up: bool,
    /// Whether traffic to the internet leaves by this one.
    pub current: bool,
    /// Its first IPv4 address; empty where it holds none.
    pub ipv4: String,
    /// Its first gateway; empty where it names none.
    pub gateway: String,
    /// Its DNS servers, joined; empty where it lists none.
    pub dns: String,
    /// Its hardware address, coloned and upper; empty where it has none.
    pub mac: String,
    /// The link's rate, in bits per second; 0 where the driver does not say.
    pub link_bps: u64,
}

/// What the adapters say all at once, for a frame to show.
#[derive(Clone)]
pub struct Snapshot {
    pub adapters: Vec<Adapter>,
}

/// The first IPv4 of a chain of socket addresses (unicast, gateway, or DNS);
/// empty where the chain holds none. `head` and each link's `Next` are of the
/// list's own record type, but every one begins with its `Address` and `Next`,
/// which is all this walks.
fn first_v4<T>(head: *mut T, mut address: impl FnMut(&T) -> *mut SOCKADDR, mut next: impl FnMut(&T) -> *mut T) -> Option<String> {
    let mut one = head;
    while !one.is_null() {
        let a = address(unsafe { &*one });
        if !a.is_null() && unsafe { (*a).sa_family } == AF_INET {
            let octets = unsafe { (*a.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr }.to_ne_bytes();
            return Some(format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]));
        }
        one = next(unsafe { &*one });
    }
    None
}

/// Every IPv4 of a chain (DNS servers, which come several), joined.
fn all_v4(head: *mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_DNS_SERVER_ADDRESS_XP) -> String {
    let mut out = Vec::new();
    let mut one = head;
    while !one.is_null() {
        let a = unsafe { &*one };
        let sa = a.Address.lpSockaddr;
        if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
            let octets = unsafe { (*sa.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr }.to_ne_bytes();
            out.push(format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]));
        }
        one = a.Next;
        if out.len() == 2 {
            break;
        }
    }
    out.join("  ")
}

/// The adapters, in the order the list gives them, the one the internet
/// leaves by first, then those up, then the rest.
pub fn read() -> Snapshot {
    // Asking for the route to 1.1.1.1 sends nothing; it only says which
    // interface Windows would send by.
    let mut best = 0u32;
    let has_best = unsafe { GetBestInterface(u32::from_ne_bytes([1, 1, 1, 1]), &mut best) } == ERROR_SUCCESS.0;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS;
    let mut buf: Vec<u64> = Vec::new();
    let mut size = 0u32;
    loop {
        let list = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        let status = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(list), &mut size) };
        if status == ERROR_BUFFER_OVERFLOW.0 {
            buf.resize((size as usize).div_ceil(8), 0);
            continue;
        }
        if status != ERROR_SUCCESS.0 {
            return Snapshot { adapters: Vec::new() };
        }
        let mut adapters = Vec::new();
        let mut adapter = list as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() {
            let a = unsafe { &*adapter };
            let kind = a.IfType;
            let name = unsafe { a.FriendlyName.to_string() }.unwrap_or_default();
            if !name.is_empty() && kind != LOOPBACK && kind != TUNNEL {
                let octets = &a.PhysicalAddress[..a.PhysicalAddressLength.min(6) as usize];
                let current = has_best && unsafe { a.Anonymous1.Anonymous.IfIndex } == best;
                adapters.push(Adapter {
                    name,
                    description: unsafe { a.Description.to_string() }.unwrap_or_default(),
                    kind,
                    up: a.OperStatus == IfOperStatusUp,
                    current,
                    ipv4: first_v4(a.FirstUnicastAddress, |u| u.Address.lpSockaddr, |u| u.Next).unwrap_or_default(),
                    gateway: first_v4(a.FirstGatewayAddress, |g| g.Address.lpSockaddr, |g| g.Next).unwrap_or_default(),
                    dns: all_v4(a.FirstDnsServerAddress),
                    mac: octets.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"),
                    link_bps: a.ReceiveLinkSpeed,
                });
            }
            adapter = a.Next;
        }
        adapters.sort_by_key(|a| (!a.current, !a.up, a.name.to_lowercase()));
        return Snapshot { adapters };
    }
}

/// Whether a kind is the wireless one, to word its row and preview.
pub fn is_wire(kind: u32) -> bool {
    kind == WIRELESS
}

/// Whether a kind is the wired one.
pub fn is_wired(kind: u32) -> bool {
    kind == ETHERNET
}

// ---- Cached, off-thread reads for the settings window's preview ----

/// The last snapshot taken, and whether one is being taken now.
static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The last snapshot, at once — never blocking the frame. Where it has grown
/// stale, a background read is started and the settings window woken when it
/// lands; the caller sees `None` until the first read is done.
pub fn snapshot() -> Option<Snapshot> {
    let last = LAST_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 4000 {
        if !BUSY.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let shot = read();
                *CACHE.lock().unwrap() = Some(shot);
                LAST_MS.store(millis(), Ordering::Relaxed);
                BUSY.store(false, Ordering::SeqCst);
                crate::ui::settings_window::follow_settings();
            });
        }
    }
    CACHE.lock().unwrap().clone()
}
