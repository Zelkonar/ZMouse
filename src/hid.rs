//! Milestone-1 proof #1: enumerate connected mice and read their identity.
//!
//! Uses the `objc2-io-kit` / `objc2-core-foundation` ecosystem (distinct from `tap.rs`).

use std::ffi::c_void;

use objc2_core_foundation::{CFNumber, CFRetained, CFSet, CFString, CFType};
use objc2_io_kit::{
    IOHIDDevice, IOHIDManager, IORegistryEntryGetRegistryEntryID, kIOHIDOptionsTypeNone,
};

/// Identity we can read for a connected mouse.
#[derive(Debug, Clone)]
pub struct MouseDevice {
    pub name: Option<String>,
    pub vendor_id: Option<i64>,
    pub product_id: Option<i64>,
    pub serial: Option<String>,
    pub location_id: Option<i64>,
    /// Most stable per-device identity; our candidate correlation key for per-device mapping.
    pub registry_entry_id: Option<u64>,
    pub primary_usage: Option<i64>,
    pub primary_usage_page: Option<i64>,
}

/// Enumerate all HID devices and keep the ones that look like pointers/mice.
pub fn list_mice() -> Vec<MouseDevice> {
    let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);

    // Match everything (None), then filter by usage below. Avoids building a match dictionary.
    unsafe { manager.set_device_matching(None) };
    let _ = manager.open(kIOHIDOptionsTypeNone);

    let Some(set) = manager.devices() else {
        return Vec::new();
    };

    devices_from_set(&set)
        .into_iter()
        .map(read_device)
        // Generic Desktop (0x01), Mouse (0x02) or Pointer (0x01).
        .filter(|d| {
            d.primary_usage_page == Some(0x01) && matches!(d.primary_usage, Some(0x02) | Some(0x01))
        })
        // Never surface the built-in Apple trackpad/keyboard: remapping the internal pointer is a
        // great way to lock yourself out, so it's excluded from discovery entirely.
        .filter(|d| !is_builtin_trackpad(d))
        .collect()
}

/// Is this a trackpad (the Mac's built-in, or a Magic Trackpad)? Matched by name — Apple's internal
/// device is consistently "Apple Internal Keyboard / Trackpad". Deliberately excluded from remapping
/// so the user can't accidentally disable their own primary input. A Magic *Mouse* (also Apple, but
/// no "trackpad" in its name) is intentionally NOT excluded — that's a legitimate remap target.
fn is_builtin_trackpad(d: &MouseDevice) -> bool {
    d.name
        .as_deref()
        .map(|n| {
            let n = n.to_ascii_lowercase();
            n.contains("trackpad") || n.contains("apple internal")
        })
        .unwrap_or(false)
}

/// Pretty-print the enumerated mice (used by the `list` subcommand).
pub fn print_mice() {
    let mice = list_mice();
    if mice.is_empty() {
        println!("No mice found (is anything connected? did the HID manager open?).");
        return;
    }
    println!("Found {} mouse device(s):\n", mice.len());
    for (i, d) in mice.iter().enumerate() {
        println!("[{}] {}", i, d.name.as_deref().unwrap_or("<unknown>"));
        println!(
            "    vendor=0x{:04x} product=0x{:04x}",
            d.vendor_id.unwrap_or(0),
            d.product_id.unwrap_or(0),
        );
        println!("    serial={}", d.serial.as_deref().unwrap_or("<none>"));
        println!(
            "    locationID={} registryEntryID={}",
            d.location_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<none>".into()),
            d.registry_entry_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<none>".into()),
        );
        println!();
    }
}

/// Pull the raw device pointers out of the CFSet and turn them into `&IOHIDDevice`.
fn devices_from_set(set: &CFSet) -> Vec<&IOHIDDevice> {
    let count = set.count();
    if count <= 0 {
        return Vec::new();
    }
    let mut ptrs: Vec<*const c_void> = vec![std::ptr::null(); count as usize];
    unsafe { set.values(ptrs.as_mut_ptr()) };
    ptrs.into_iter()
        .filter(|p| !p.is_null())
        .map(|p| unsafe { &*(p as *const IOHIDDevice) })
        .collect()
}

fn read_device(device: &IOHIDDevice) -> MouseDevice {
    MouseDevice {
        name: string_prop(device, "Product"),
        vendor_id: number_prop(device, "VendorID"),
        product_id: number_prop(device, "ProductID"),
        serial: string_prop(device, "SerialNumber"),
        location_id: number_prop(device, "LocationID"),
        registry_entry_id: registry_entry_id(device),
        primary_usage: number_prop(device, "PrimaryUsage"),
        primary_usage_page: number_prop(device, "PrimaryUsagePage"),
    }
}

fn get_prop(device: &IOHIDDevice, key: &str) -> Option<CFRetained<CFType>> {
    let key = CFString::from_str(key);
    device.property(&key)
}

fn string_prop(device: &IOHIDDevice, key: &str) -> Option<String> {
    let value = get_prop(device, key)?;
    let s = value.downcast_ref::<CFString>()?;
    Some(s.to_string())
}

fn number_prop(device: &IOHIDDevice, key: &str) -> Option<i64> {
    let value = get_prop(device, key)?;
    let n = value.downcast_ref::<CFNumber>()?;
    n.as_i64()
}

/// Read a device's USB vendor id (e.g. 0x1532 = Razer), if it exposes one.
pub fn vendor_id(device: &IOHIDDevice) -> Option<i64> {
    number_prop(device, "VendorID")
}

/// Read a device's USB product id, if it exposes one.
pub fn product_id(device: &IOHIDDevice) -> Option<i64> {
    number_prop(device, "ProductID")
}

/// Read the kernel registry entry ID — the stable per-device identity key.
pub fn registry_entry_id(device: &IOHIDDevice) -> Option<u64> {
    let service = device.service();
    if service == 0 {
        return None;
    }
    let mut id: u64 = 0;
    let kr = unsafe { IORegistryEntryGetRegistryEntryID(service, &mut id) };
    if kr == 0 { Some(id) } else { None }
}
