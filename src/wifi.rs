//! The wireless networks Windows sees: the ones near, the one joined, and the
//! doing of what a person asks of them — joining, forgetting, the radio's own
//! switch. It leans on Windows' Wireless LAN API (`wlanapi`), the same the
//! metrics read the current network from. Nothing here blocks the drawn frame:
//! `read` takes the snapshot in one call and is asked off the render thread.

#![cfg(windows)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::WiFi::{
    WlanConnect, WlanDeleteProfile, WlanDisconnect, WlanEnumInterfaces, WlanFreeMemory, WlanCloseHandle, WlanGetProfile, WlanGetProfileList, WlanOpenHandle,
    WlanQueryInterface, WlanScan, WlanSetInterface, WlanSetProfile, WLAN_AVAILABLE_NETWORK_CONNECTED, WLAN_AVAILABLE_NETWORK_LIST, WLAN_CONNECTION_ATTRIBUTES,
    WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_GET_PLAINTEXT_KEY, WLAN_PROFILE_INFO_LIST, WLAN_RADIO_STATE, dot11_BSS_type_infrastructure,
    dot11_radio_state_on, wlan_connection_mode_profile, wlan_interface_state_connected, wlan_intf_opcode_channel_number, wlan_intf_opcode_current_connection,
    wlan_intf_opcode_radio_state,
};

/// One network the radio can hear, or has heard and kept.
#[derive(Clone)]
pub struct Net {
    /// Its name, as it broadcasts.
    pub ssid: String,
    /// The profile Windows keeps for it, if it keeps one.
    pub profile: String,
    /// Signal, 0-100.
    pub signal: u32,
    /// Whether it asks a key.
    pub secure: bool,
    /// Whether the machine is on it now.
    pub connected: bool,
    /// Whether Windows keeps a profile for it (so it can be forgotten).
    pub known: bool,
}

/// The network joined now, as the interface reports it: the readouts a details card shows.
#[derive(Clone)]
pub struct Details {
    /// Its name, as broadcast.
    pub ssid: String,
    /// The profile kept for it, to look its key up by.
    pub profile: String,
    /// Signal, 0-100.
    pub signal: u32,
    /// Receive rate, in bits per second.
    pub rx: u32,
    /// Transmit rate, in bits per second.
    pub tx: u32,
    /// The channel it sits on, where the driver tells.
    pub channel: u32,
    /// Whether it asks a key.
    pub secure: bool,
}

/// What the radio says all at once, for a frame to show.
#[derive(Clone)]
pub struct Snapshot {
    /// Whether the radio is switched on.
    pub on: bool,
    /// The wireless adapter's own name, where there is one.
    pub adapter: String,
    /// The networks near, strongest first.
    pub nets: Vec<Net>,
    /// The networks kept but out of range now, by their profile.
    pub saved: Vec<Net>,
    /// The network joined now, with its rates, where there is one.
    pub details: Option<Details>,
}

/// Opens a client against the Wireless AutoConfig service; None where it will not answer.
fn open() -> Option<HANDLE> {
    let (mut version, mut client) = (0u32, HANDLE::default());
    (unsafe { WlanOpenHandle(2, None, &mut version, &mut client) } == ERROR_SUCCESS.0).then_some(client)
}

/// The machine's wireless adapters, by their identifier and description.
fn interfaces(client: HANDLE) -> Vec<(GUID, String)> {
    let mut list = std::ptr::null_mut::<WLAN_INTERFACE_INFO_LIST>();
    let mut out = Vec::new();
    if unsafe { WlanEnumInterfaces(client, None, &mut list) } != ERROR_SUCCESS.0 || list.is_null() {
        return out;
    }
    let table = unsafe { &*list };
    let n = table.dwNumberOfItems as usize;
    for i in 0..n {
        let one = unsafe { &*table.InterfaceInfo.as_ptr().add(i) };
        let ended = one.strInterfaceDescription.iter().position(|&c| c == 0).unwrap_or(one.strInterfaceDescription.len());
        out.push((one.InterfaceGuid, String::from_utf16_lossy(&one.strInterfaceDescription[..ended])));
    }
    unsafe { WlanFreeMemory(list as *mut c_void) };
    out
}

/// Whether the adapter's radio is on, read through the interface's radio-state opcode.
fn radio(client: HANDLE, adapter: &GUID) -> bool {
    let (mut size, mut told) = (0u32, std::ptr::null_mut::<c_void>());
    let asked = unsafe { WlanQueryInterface(client, adapter, wlan_intf_opcode_radio_state, None, &mut size, &mut told, None) };
    let on = asked == ERROR_SUCCESS.0
        && !told.is_null()
        && size as usize >= std::mem::size_of::<WLAN_RADIO_STATE>()
        && unsafe { (*(told as *const WLAN_RADIO_STATE)).PhyRadioState[0].dot11SoftwareRadioState == dot11_radio_state_on };
    unsafe {
        if !told.is_null() {
            WlanFreeMemory(told);
        }
    }
    on
}

/// Turns the adapter's radio on or off through the same opcode.
fn set_radio(client: HANDLE, adapter: &GUID, on: bool) -> bool {
    let mut state = WLAN_RADIO_STATE::default();
    state.dwNumberOfPhys = 1;
    state.PhyRadioState[0].dot11SoftwareRadioState = if on { dot11_radio_state_on } else { windows::Win32::NetworkManagement::WiFi::dot11_radio_state_off };
    let done = unsafe {
        WlanSetInterface(client, adapter, wlan_intf_opcode_radio_state, std::mem::size_of::<WLAN_RADIO_STATE>() as u32, &state as *const _ as *const c_void, None)
    };
    done == ERROR_SUCCESS.0
}

/// A wide fixed array turned to a String at its first NUL.
fn wide(text: &[u16]) -> String {
    let ended = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..ended])
}

/// Everything a frame wants to show: the radio, the adapter, the networks near.
pub fn read() -> Snapshot {
    let Some(client) = open() else {
        return Snapshot { on: false, adapter: String::new(), nets: Vec::new(), saved: Vec::new(), details: None };
    };
    let Some((guid, name)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return Snapshot { on: false, adapter: String::new(), nets: Vec::new(), saved: Vec::new(), details: None };
    };
    let on = radio(client, &guid);
    let mut nets = available(client, &guid);
    let details = on.then(|| connection(client, &guid)).flatten();
    // Ask for a fresh scan so the next read shows more than what was cached.
    unsafe { let _ = WlanScan(client, &guid, None, None, None); };
    nets.sort_by(|a, b| b.signal.cmp(&a.signal).then(b.connected.cmp(&a.connected)));
    // The profiles kept that are not heard now: named by their profile, shown apart as the saved ones.
    let heard: Vec<String> = nets.iter().flat_map(|n| [n.profile.clone(), n.ssid.clone()]).map(|s| s.to_lowercase()).collect();
    let saved = profiles(client, &guid)
        .into_iter()
        .filter(|p| !heard.contains(&p.to_lowercase()))
        .map(|p| Net { ssid: p.clone(), profile: p, signal: 0, secure: false, connected: false, known: true })
        .collect();
    unsafe { let _ = WlanCloseHandle(client, None); };
    Snapshot { on, adapter: name, nets, saved, details }
}

/// The networks near an adapter, with which one the machine is on.
fn available(client: HANDLE, guid: &GUID) -> Vec<Net> {
    let mut list = std::ptr::null_mut::<WLAN_AVAILABLE_NETWORK_LIST>();
    if unsafe { windows::Win32::NetworkManagement::WiFi::WlanGetAvailableNetworkList(client, guid, 0, None, &mut list) } != ERROR_SUCCESS.0 || list.is_null() {
        return Vec::new();
    }
    let table = unsafe { &*list };
    let mut out = Vec::new();
    for i in 0..(table.dwNumberOfItems as usize) {
        let one = unsafe { &*table.Network.as_ptr().add(i) };
        let ssid = String::from_utf8_lossy(&one.dot11Ssid.ucSSID[..(one.dot11Ssid.uSSIDLength as usize).min(32)]).into_owned();
        if ssid.is_empty() {
            continue;
        }
        out.push(Net {
            ssid,
            profile: wide(&one.strProfileName),
            signal: one.wlanSignalQuality,
            secure: one.bSecurityEnabled.0 != 0,
            connected: one.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
            known: one.dwFlags & windows::Win32::NetworkManagement::WiFi::WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0,
        });
    }
    unsafe { WlanFreeMemory(list as *mut c_void) };
    out
}

/// The names of every profile the machine keeps for an adapter, whether its network is heard now or not.
fn profiles(client: HANDLE, guid: &GUID) -> Vec<String> {
    let mut list = std::ptr::null_mut::<WLAN_PROFILE_INFO_LIST>();
    if unsafe { WlanGetProfileList(client, guid, None, &mut list) } != ERROR_SUCCESS.0 || list.is_null() {
        return Vec::new();
    }
    let table = unsafe { &*list };
    let mut out = Vec::new();
    for i in 0..(table.dwNumberOfItems as usize) {
        let one = unsafe { &*table.ProfileInfo.as_ptr().add(i) };
        let name = wide(&one.strProfileName);
        if !name.is_empty() {
            out.push(name);
        }
    }
    unsafe { WlanFreeMemory(list as *mut c_void) };
    out
}

/// Joins a network Windows already keeps a profile for, by its name.
pub fn connect(profile: &str) -> bool {
    let Some(client) = open() else { return false };
    let Some((guid, _)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return false;
    };
    let name: Vec<u16> = profile.encode_utf16().chain([0]).collect();
    let params = WLAN_CONNECTION_PARAMETERS {
        wlanConnectionMode: wlan_connection_mode_profile,
        strProfile: PCWSTR(name.as_ptr()),
        pDot11Ssid: std::ptr::null_mut(),
        pDesiredBssidList: std::ptr::null_mut(),
        dot11BssType: dot11_BSS_type_infrastructure,
        dwFlags: 0,
    };
    let done = unsafe { WlanConnect(client, &guid, &params, None) } == ERROR_SUCCESS.0;
    unsafe { let _ = WlanCloseHandle(client, None); };
    done
}

/// The XML a new profile needs: WPA2-personal with the key, or an open network.
fn profile_xml(ssid: &str, key: &str) -> String {
    if key.is_empty() {
        format!(r#"<?xml version="1.0"?><WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{ssid}</name><SSIDConfig><SSID><name>{ssid}</name></SSID></SSIDConfig><connectionType>ESS</connectionType><MSM><security><authEncryption><authentication>open</authentication><encryption>none</encryption><useOneX>false</useOneX></authEncryption></security></MSM></WLANProfile>"#)
    } else {
        format!(r#"<?xml version="1.0"?><WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{ssid}</name><SSIDConfig><SSID><name>{ssid}</name></SSID></SSIDConfig><connectionType>ESS</connectionType><connectionMode>auto</connectionMode><MSM><security><authEncryption><authentication>WPA2PSK</authentication><encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType><protected>false</protected><keyMaterial>{key}</keyMaterial></sharedKey></security></MSM></WLANProfile>"#)
    }
}

/// Joins a network not kept before: writes its profile, then connects.
pub fn connect_new(ssid: &str, key: &str) -> bool {
    let Some(client) = open() else { return false };
    let Some((guid, _)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return false;
    };
    let xml = profile_xml(ssid, key);
    let xml_w: Vec<u16> = xml.encode_utf16().chain([0]).collect();
    let mut reason = 0u32;
    let set = unsafe {
        WlanSetProfile(client, &guid, 0, PCWSTR(xml_w.as_ptr()), PCWSTR::null(), true, None, &mut reason)
    };
    let done = set == ERROR_SUCCESS.0 && connect(ssid);
    unsafe { let _ = WlanCloseHandle(client, None); };
    done
}

/// Lets go of a kept network, forgetting its profile.
pub fn forget(profile: &str) -> bool {
    let Some(client) = open() else { return false };
    let Some((guid, _)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return false;
    };
    let name: Vec<u16> = profile.encode_utf16().chain([0]).collect();
    let done = unsafe { WlanDeleteProfile(client, &guid, PCWSTR(name.as_ptr()), None) } == ERROR_SUCCESS.0;
    unsafe { let _ = WlanCloseHandle(client, None); };
    done
}

/// Drops the current connection without forgetting the profile; the radio may rejoin on its own.
pub fn disconnect() -> bool {
    let Some(client) = open() else { return false };
    let Some((guid, _)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return false;
    };
    let done = unsafe { WlanDisconnect(client, &guid, None) } == ERROR_SUCCESS.0;
    unsafe { let _ = WlanCloseHandle(client, None); };
    done
}

/// A single u32 read through an interface opcode (the channel number), None where it will not answer.
fn query_u32(client: HANDLE, guid: &GUID, opcode: windows::Win32::NetworkManagement::WiFi::WLAN_INTF_OPCODE) -> Option<u32> {
    let (mut size, mut told) = (0u32, std::ptr::null_mut::<c_void>());
    let asked = unsafe { WlanQueryInterface(client, guid, opcode, None, &mut size, &mut told, None) };
    let out = (asked == ERROR_SUCCESS.0 && !told.is_null() && size >= 4).then(|| unsafe { *(told as *const u32) });
    unsafe {
        if !told.is_null() {
            WlanFreeMemory(told);
        }
    }
    out
}

/// What the interface says of the network joined now: signal, rates, channel, security. None where nothing is joined.
fn connection(client: HANDLE, guid: &GUID) -> Option<Details> {
    let (mut size, mut told) = (0u32, std::ptr::null_mut::<c_void>());
    let asked = unsafe { WlanQueryInterface(client, guid, wlan_intf_opcode_current_connection, None, &mut size, &mut told, None) };
    if asked != ERROR_SUCCESS.0 || told.is_null() || (size as usize) < std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() {
        unsafe {
            if !told.is_null() {
                WlanFreeMemory(told);
            }
        };
        return None;
    }
    let attr = unsafe { &*(told as *const WLAN_CONNECTION_ATTRIBUTES) };
    if attr.isState != wlan_interface_state_connected {
        unsafe { WlanFreeMemory(told) };
        return None;
    }
    let assoc = &attr.wlanAssociationAttributes;
    let n = (assoc.dot11Ssid.uSSIDLength as usize).min(32);
    let ssid = String::from_utf8_lossy(&assoc.dot11Ssid.ucSSID[..n]).into_owned();
    let profile = wide(&attr.strProfileName);
    let (signal, rx, tx) = (assoc.wlanSignalQuality, assoc.ulRxRate, assoc.ulTxRate);
    let secure = attr.wlanSecurityAttributes.bSecurityEnabled.0 != 0;
    unsafe { WlanFreeMemory(told) };
    let channel = query_u32(client, guid, wlan_intf_opcode_channel_number).unwrap_or(0);
    Some(Details { ssid, profile, signal, rx, tx, channel, secure })
}

/// The key kept for a network, in clear where the service will give it: read from the profile's XML.
pub fn password(profile: &str) -> Option<String> {
    if profile.is_empty() {
        return None;
    }
    let client = open()?;
    let guid = interfaces(client).first().cloned().map(|(g, _)| g)?;
    let name: Vec<u16> = profile.encode_utf16().chain([0]).collect();
    let mut xml = PWSTR::null();
    let mut flags = WLAN_PROFILE_GET_PLAINTEXT_KEY;
    let done = unsafe { WlanGetProfile(client, &guid, PCWSTR(name.as_ptr()), None, &mut xml, Some(&mut flags), None) };
    if done != ERROR_SUCCESS.0 || xml.is_null() {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return None;
    }
    let mut len = 0usize;
    unsafe {
        while *(xml.0.add(len)) != 0 {
            len += 1;
        }
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(xml.0, len) });
    unsafe {
        WlanFreeMemory(xml.0 as *mut c_void);
        let _ = WlanCloseHandle(client, None);
    };
    let mark = "<keyMaterial>";
    let start = text.find(mark)? + mark.len();
    let end = text[start..].find("</keyMaterial>")?;
    let key = text[start..start + end].to_string();
    (!key.is_empty()).then_some(key)
}

/// Whether the wireless radio is on, asked without a scan: cheap enough for a frame.
pub fn state() -> Option<bool> {
    let client = open()?;
    let first = interfaces(client).first().cloned().map(|(g, _)| g);
    let on = first.map(|guid| radio(client, &guid));
    unsafe { let _ = WlanCloseHandle(client, None); };
    on
}

/// Switches the wireless radio on or off.
pub fn set_on(on: bool) -> bool {
    let Some(client) = open() else { return false };
    let Some((guid, _)) = interfaces(client).first().cloned() else {
        unsafe { let _ = WlanCloseHandle(client, None); };
        return false;
    };
    let done = set_radio(client, &guid, on);
    unsafe { let _ = WlanCloseHandle(client, None); };
    done
}

// ---- Cached, off-thread reads for the settings window's preview ----

/// The last snapshot taken, and whether one is being taken now.
static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The last snapshot, at once — never blocking the frame. Where it has grown stale, a
/// background read is started and the settings window woken when it lands; the caller
/// sees `None` until the first read is done.
pub fn snapshot() -> Option<Snapshot> {
    let last = LAST_MS.load(Ordering::Relaxed);
    if last == 0 || millis().saturating_sub(last) > 6000 {
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
