//! Discovery tool: attribute an intercepted button/keystroke event to a *specific* mouse.
//!
//! Strategy: run an `IOHIDManager` input-value callback (which carries the originating
//! device) at the same time as a `CGEventTap` (which does not), and log both streams with
//! their mach-absolute-time timestamps. If the timestamps line up per click, per-device
//! remapping can be built on timestamp correlation; if not, we must intercept lower down.
//!
//! This intentionally lives across both CF ecosystems: HID via objc2-io-kit, tap via
//! core-graphics. Both drive the same global run loop.

use std::ffi::c_void;

// mach absolute time — same clock the HID value timestamps use, so tap-callback arrival
// times are directly comparable to the [HID] timestamps.
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_io_kit::{
    IOHIDDevice, IOHIDManager, IOHIDValue, IORegistryEntryGetRegistryEntryID, IOReturn,
    kIOHIDOptionsTypeNone,
};

use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

/// HID input-value callback: fires for every element change (button, axis) on any matched
/// device. `sender` is the originating IOHIDDevice.
unsafe extern "C-unwind" fn input_value_callback(
    _context: *mut c_void,
    _result: IOReturn,
    sender: *mut c_void,
    value: core::ptr::NonNull<IOHIDValue>,
) {
    let value = unsafe { value.as_ref() };
    let element = value.element();
    let usage_page = element.usage_page();
    let usage = element.usage();
    // Log the pages a mouse's buttons might use; skip pointer-motion / axis spam (0x01).
    let page = match usage_page {
        0x07 => "keyboard",
        0x09 => "button",
        0x0C => "consumer",
        _ => return,
    };
    let int_val = value.integer_value();
    // Only log presses/releases, not the endless 0->0 keepalives some devices emit.
    if int_val == 0 && usage_page == 0x0C {
        return;
    }
    let ts = value.time_stamp();

    let (reg_id, vid, pid) = if sender.is_null() {
        (None, None, None)
    } else {
        let device = unsafe { &*(sender as *const IOHIDDevice) };
        (
            registry_entry_id(device),
            crate::hid::vendor_id(device),
            crate::hid::product_id(device),
        )
    };

    println!(
        "[HID] t={:>20} device={:<20} vid=0x{:04x} pid=0x{:04x} page={:<8} usage={} value={}",
        ts,
        reg_id
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<none>".into()),
        vid.unwrap_or(0),
        pid.unwrap_or(0),
        page,
        usage,
        int_val,
    );
}

fn registry_entry_id(device: &IOHIDDevice) -> Option<u64> {
    let service = device.service();
    if service == 0 {
        return None;
    }
    let mut id: u64 = 0;
    let kr = unsafe { IORegistryEntryGetRegistryEntryID(service, &mut id) };
    if kr == 0 { Some(id) } else { None }
}

/// Run both streams until killed. Logs interleaved [HID] and [TAP] lines for eyeball correlation.
pub fn run_probe() {
    println!(
        "zmouse probe: discovery tool. Logs HID input (button/keyboard/consumer pages) with\n\
         device identity, plus the CGEventTap view (mouse buttons AND keystrokes).\n\
         Press every button on the mouse to see how each one presents:\n\
           - a [TAP] type=OtherMouseDown  => a real mouse button (remappable now)\n\
           - a [TAP] type=KeyDown         => the button emits a keystroke instead\n\
           - [HID] page/device tells you which physical device (and interface) it came from.\n\
         To quit: click THIS terminal to focus it, then Ctrl+C \
         (or `pkill -f zmouse` from another terminal).\n"
    );

    // 1) HID manager with an input-value callback, scheduled on the current run loop.
    let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);
    unsafe { manager.set_device_matching(None) };
    unsafe {
        manager.register_input_value_callback(Some(input_value_callback), std::ptr::null_mut());
    }
    let _ = manager.open(kIOHIDOptionsTypeNone);
    unsafe {
        let rl = CFRunLoop::current().expect("no current run loop");
        manager.schedule_with_run_loop(&rl, kCFRunLoopDefaultMode.unwrap());
    }

    // 2) Event tap on the same run loop, listen-only (we don't rewrite anything here).
    let result = CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::KeyDown,
            CGEventType::KeyUp,
        ],
        |_proxy, event_type, event| {
            let ts = unsafe { mach_absolute_time() };
            let detail = match event_type {
                CGEventType::KeyDown | CGEventType::KeyUp => {
                    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    format!("keycode={code}")
                }
                _ => {
                    let button =
                        event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
                    format!("button={button}")
                }
            };
            println!(
                "[TAP] t={:>20} type={:<16} {}",
                ts,
                format!("{:?}", event_type),
                detail,
            );
            CallbackResult::Keep
        },
        core_foundation::runloop::CFRunLoop::run_current,
    );

    if result.is_err() {
        eprintln!(
            "\nERROR: failed to create the event tap (Accessibility permission needed).\n\
             The HID half may still work; grant Accessibility to see the [TAP] lines.\n"
        );
        std::process::exit(1);
    }
}
