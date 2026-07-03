//! The per-device mapping engine.
//!
//! Combines two pieces:
//!   * an `IOHIDManager` input-value callback records *which device* pressed each button
//!     (the `probe` tool confirms HID fires before the tap on the same thread), and
//!   * a `CGEventTap` intercepts the button and applies that device's configured mapping.
//!
//! Everything runs on one run-loop thread, so the device-tracking state is a `thread_local`
//! shared between the C HID callback (which can't capture) and the tap closure. The mapping
//! table is an `Rc<RefCell<..>>` so it can be swapped live (e.g. menu "Reload config").
//!
//! `install()` sets the tap + HID manager onto the *current* run loop and returns a handle,
//! but does NOT run the loop — so the caller can drive it either with `CFRunLoop::run_current`
//! (headless `run`) or `NSApplication::run` (the menu-bar app).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::time::Instant;

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_io_kit::{IOHIDDevice, IOHIDManager, IOHIDValue, IOReturn, kIOHIDOptionsTypeNone};

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPortRef;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CGMouseButton, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::config::{self, Action, Config};
use crate::hid::{self, registry_entry_id};
use crate::keymap;

/// (device_key, CGEvent button number) -> action. The device key is a vendor/product composite
/// when available (stable across reconnects), else the kernel registry id — see `config::device_key`.
pub type Mappings = HashMap<(u64, i64), Action>;

/// Shared, hot-swappable mapping table (single-threaded; shared with the menu handler).
pub type SharedMappings = Rc<RefCell<Mappings>>;

/// The identity of the device that produced an HID event, carrying both possible lookup keys so
/// a mapping keyed by either vendor/product id *or* registry id matches.
#[derive(Clone, Copy)]
struct DeviceIdent {
    registry: u64,
    vidpid: Option<u64>,
}

impl DeviceIdent {
    /// Find this device's action for `sub` (a button number or keycode), preferring the stable
    /// vendor/product key and falling back to the registry id.
    fn lookup<'a>(&self, map: &'a Mappings, sub: i64) -> Option<&'a Action> {
        if let Some(vp) = self.vidpid
            && let Some(a) = map.get(&(vp, sub))
        {
            return Some(a);
        }
        map.get(&(self.registry, sub))
    }
}

thread_local! {
    /// button number (CGEvent-numbered) -> identity of the device that last pressed it.
    static LAST_DEVICE_FOR_BUTTON: RefCell<HashMap<i64, DeviceIdent>> = RefCell::new(HashMap::new());
    /// Identity of the device whose keyboard-page HID event fired most recently. Since HID fires
    /// just before the tap on the same thread, this identifies the source of the KeyDown the tap is
    /// currently handling — so mouse-key remaps never touch a real keyboard. None = unknown.
    static LAST_KEYBOARD_DEVICE: Cell<Option<DeviceIdent>> = const { Cell::new(None) };
    /// Scroll jitter-filter state: (last accepted direction sign, when it happened).
    static SCROLL_STATE: RefCell<Option<(i8, Instant)>> = const { RefCell::new(None) };
}

/// The tap's CFMachPort, stashed so the menu can enable/disable the tap. Main-thread only in
/// practice, but atomics keep it a valid `static`.
static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Scroll-wheel jitter filter settings (set from config by `apply_settings`).
static SCROLL_FILTER: AtomicBool = AtomicBool::new(false);
static SCROLL_GUARD_MS: AtomicU64 = AtomicU64::new(80);
/// Boost weak discrete wheel ticks so slow ticks aren't ignored by pixel-precise apps.
static SCROLL_BOOST: AtomicBool = AtomicBool::new(false);
static SCROLL_MIN_PIXELS: AtomicU64 = AtomicU64::new(10);

/// Apply global (non-mapping) settings from the config. Safe to call on every (re)load.
pub fn apply_settings(cfg: &Config) {
    SCROLL_FILTER.store(cfg.scroll.jitter_filter, Ordering::SeqCst);
    SCROLL_GUARD_MS.store(cfg.scroll.reversal_guard_ms, Ordering::SeqCst);
    SCROLL_BOOST.store(cfg.scroll.boost_weak_ticks, Ordering::SeqCst);
    SCROLL_MIN_PIXELS.store(cfg.scroll.min_tick_pixels.max(1) as u64, Ordering::SeqCst);
}

/// Decide whether a discrete mouse-wheel scroll event is a spurious encoder "jump" to drop.
/// Trackpad (continuous) events are never touched. Returns true = drop.
fn is_scroll_glitch(event: &CGEvent) -> bool {
    if !SCROLL_FILTER.load(Ordering::SeqCst) {
        return false;
    }
    // Leave trackpad / continuous scrolling completely alone.
    if event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0 {
        return false;
    }
    let delta = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
    let dir: i8 = match delta.cmp(&0) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => return false, // no vertical movement; pass
    };
    let guard = std::time::Duration::from_millis(SCROLL_GUARD_MS.load(Ordering::SeqCst));
    let now = Instant::now();

    SCROLL_STATE.with(|s| {
        let mut st = s.borrow_mut();
        match *st {
            // Opposite direction, arriving too soon after the last accepted scroll => glitch.
            Some((last_dir, last_at)) if dir != last_dir && now.duration_since(last_at) < guard => {
                true // drop; keep state anchored to the real scroll direction
            }
            // Same direction, or a genuine reversal after a pause => accept and update state.
            _ => {
                *st = Some((dir, now));
                false
            }
        }
    })
}

/// Raise a weak discrete wheel tick to a full detent. A worn encoder can emit a `line=1` tick
/// carrying only ~1 pixel of movement; pixel-precise Mac apps then move imperceptibly and seem to
/// "ignore" the scroll. We floor the pixel/fixed-point delta to `min_pixels` per line (preserving
/// sign and leaving already-strong/accelerated ticks alone). Trackpads (continuous) untouched.
fn boost_weak_scroll(event: &CGEvent) {
    if !SCROLL_BOOST.load(Ordering::SeqCst) {
        return;
    }
    if event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0 {
        return;
    }
    let line = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
    if line == 0 {
        return; // no vertical movement to boost
    }
    let min_px = SCROLL_MIN_PIXELS.load(Ordering::SeqCst) as i64;
    let want_pixel = line * min_px; // signed: same direction as the line delta
    let pixel = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
    if pixel.abs() < want_pixel.abs() {
        event.set_integer_value_field(
            EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
            want_pixel,
        );
        // Fixed-point delta is in line units; a normal single detent is ~1.0 per line.
        event.set_double_value_field(
            EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
            line as f64,
        );
    }
}

unsafe extern "C" {
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

/// Live handle keeping the tap + HID manager alive. Drop it to tear everything down.
pub struct Installed {
    _manager: objc2_core_foundation::CFRetained<IOHIDManager>,
    _tap: CGEventTap<'static>,
}

const HID_USAGE_PAGE_BUTTON: u32 = 0x09;
const HID_USAGE_PAGE_KEYBOARD: u32 = 0x07;

/// Marker stamped into every event we synthesize (in the source-user-data field), so our own
/// tap can recognize and pass through its own injected events instead of re-processing them.
/// Without this, a button->button remap re-enters the HID-level tap and loops forever.
const ZMOUSE_TAG: i64 = 0x5245_4D53; // "REMS"

/// Install the HID tracker + event tap onto the current thread's run loop. Does not run the loop.
/// Returns `Err` if the event tap can't be created (usually missing Accessibility permission).
pub fn install(mappings: SharedMappings, key_mappings: SharedMappings) -> Result<Installed, ()> {
    // HID manager: record which device presses which button.
    let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);
    unsafe { manager.set_device_matching(None) };
    unsafe {
        manager.register_input_value_callback(Some(hid_value_callback), std::ptr::null_mut());
    }
    let _ = manager.open(kIOHIDOptionsTypeNone);
    unsafe {
        let rl = CFRunLoop::current().expect("no current run loop");
        manager.schedule_with_run_loop(&rl, kCFRunLoopDefaultMode.unwrap());
    }

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).map_err(|_| ())?;

    let tap = unsafe {
        CGEventTap::new_unchecked(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::ScrollWheel,
                CGEventType::KeyDown,
                CGEventType::KeyUp,
            ],
            move |_proxy, event_type, event| {
                // If macOS disabled the tap (e.g. the callback once ran too long), re-enable it
                // so the tap self-heals instead of going dead.
                if matches!(
                    event_type,
                    CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                ) {
                    set_enabled(true);
                    return CallbackResult::Keep;
                }
                // Ignore events we synthesized ourselves (prevents an infinite re-entrancy loop).
                if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == ZMOUSE_TAG {
                    return CallbackResult::Keep;
                }
                // Scroll-wheel jitter filter: drop spurious opposite-direction glitch ticks.
                if matches!(event_type, CGEventType::ScrollWheel) {
                    if is_scroll_glitch(event) {
                        return CallbackResult::Drop;
                    }
                    // Surviving tick: floor a weak (slow) tick up so apps don't ignore it.
                    boost_weak_scroll(event);
                    return CallbackResult::Keep;
                }
                // Keystroke-emitting buttons (e.g. MMO thumb buttons on the mouse's keyboard
                // interface): remap only when the key came from a device we have a mapping for,
                // leaving real keyboards untouched.
                if matches!(event_type, CGEventType::KeyDown | CGEventType::KeyUp) {
                    let Some(ident) = LAST_KEYBOARD_DEVICE.with(|c| c.get()) else {
                        return CallbackResult::Keep;
                    };
                    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    let action = ident.lookup(&key_mappings.borrow(), keycode).cloned();
                    let Some(action) = action else {
                        return CallbackResult::Keep; // no mapping -> let the keystroke through
                    };
                    let is_down = matches!(event_type, CGEventType::KeyDown);
                    apply_action(&action, &source, event.location(), is_down);
                    return CallbackResult::Drop;
                }
                let button = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
                // Hard floor: never touch the left (primary) button. Today left-clicks aren't even
                // in the tap mask, but if that ever changes this guarantees left-click can't be
                // dropped or remapped, so the user can always recover.
                if button == crate::config::PROTECTED_BUTTON {
                    return CallbackResult::Keep;
                }
                let ident = LAST_DEVICE_FOR_BUTTON.with(|m| m.borrow().get(&button).copied());
                let Some(ident) = ident else {
                    return CallbackResult::Keep; // unknown origin -> pass through
                };
                let action = ident.lookup(&mappings.borrow(), button).cloned();
                let Some(action) = action else {
                    return CallbackResult::Keep; // no mapping for this device+button
                };
                let is_down = matches!(event_type, CGEventType::OtherMouseDown);
                apply_action(&action, &source, event.location(), is_down);
                CallbackResult::Drop
            },
        )?
    };

    let loop_source = tap.mach_port().create_runloop_source(0).map_err(|_| ())?;
    core_foundation::runloop::CFRunLoop::get_current().add_source(&loop_source, unsafe {
        core_foundation::runloop::kCFRunLoopCommonModes
    });

    TAP_PORT.store(
        tap.mach_port().as_concrete_TypeRef() as *mut c_void,
        Ordering::SeqCst,
    );
    tap.enable();
    ENABLED.store(true, Ordering::SeqCst);

    Ok(Installed {
        _manager: manager,
        _tap: tap,
    })
}

/// Enable/disable the installed tap without tearing it down (menu "Enable/Disable").
pub fn set_enabled(on: bool) {
    let p = TAP_PORT.load(Ordering::SeqCst);
    if !p.is_null() {
        unsafe { CGEventTapEnable(p as CFMachPortRef, on) };
        ENABLED.store(on, Ordering::SeqCst);
    }
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Headless run: install, then block on this thread's run loop until killed.
pub fn run(config: Config) {
    print_startup(&config);
    apply_settings(&config);
    let mappings: SharedMappings = Rc::new(RefCell::new(config.resolve()));
    let key_mappings: SharedMappings = Rc::new(RefCell::new(config.resolve_keys()));
    let _installed = match install(mappings, key_mappings) {
        Ok(i) => i,
        Err(()) => {
            eprintln!(
                "\nERROR: failed to create the event tap (Accessibility permission needed).\n\
                 Grant it: System Settings -> Privacy & Security -> Accessibility.\n"
            );
            std::process::exit(1);
        }
    };
    core_foundation::runloop::CFRunLoop::run_current();
}

/// Human-readable summary of the loaded config (used by `run` and the menu-bar app).
pub fn print_startup(config: &Config) {
    let problems = config.validate();
    if !problems.is_empty() {
        eprintln!("Config problems (these mappings will be no-ops):");
        for p in &problems {
            eprintln!("  - {p}");
        }
        eprintln!();
    }

    if config.scroll.jitter_filter {
        println!(
            "Scroll-wheel jitter filter: ON (reversal guard {} ms).",
            config.scroll.reversal_guard_ms
        );
    }
    if config.scroll.boost_weak_ticks {
        println!(
            "Weak-tick boost: ON (floor {} px/line).",
            config.scroll.min_tick_pixels
        );
    }

    let count: usize = config
        .device
        .iter()
        .map(|d| d.mapping.len() + d.key.len())
        .sum();
    if count == 0 {
        println!("No mappings configured — nothing to remap. Add some to your config.\n");
        return;
    }
    println!("Loaded {count} mapping(s):");
    for dev in &config.device {
        let label = dev.name.as_deref().unwrap_or("<unnamed>");
        println!("  {} ({})", label, dev.ident_desc());
        for m in &dev.mapping {
            println!("    button {} -> {}", m.button, describe(&m.action));
        }
        for k in &dev.key {
            println!("    key '{}' -> {}", k.key, describe(&k.action));
        }
    }
    println!();
}

/// Perform a mapped action. `is_down` distinguishes the press from the release so we can
/// keep click semantics for button->button remaps and fire keystrokes once (on press).
fn apply_action(action: &Action, source: &CGEventSource, at: CGPoint, is_down: bool) {
    match action {
        Action::Disabled => {}
        Action::Keystroke { key, mods } => {
            if is_down {
                send_keystroke(source, key, mods);
            }
        }
        Action::Button { button } => {
            let mouse_type = if is_down {
                CGEventType::OtherMouseDown
            } else {
                CGEventType::OtherMouseUp
            };
            if let Ok(ev) =
                CGEvent::new_mouse_event(source.clone(), mouse_type, at, CGMouseButton::Left)
            {
                ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, *button);
                ev.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, ZMOUSE_TAG);
                ev.post(CGEventTapLocation::HID);
            }
        }
    }
}

fn send_keystroke(source: &CGEventSource, key: &str, mods: &[String]) {
    let Some(code) = keymap::key_code(key) else {
        return;
    };
    let flags = keymap::combine_mods(mods);
    if let Ok(down) = CGEvent::new_keyboard_event(source.clone(), code, true) {
        if flags != CGEventFlags::empty() {
            down.set_flags(flags);
        }
        down.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, ZMOUSE_TAG);
        down.post(CGEventTapLocation::HID);
    }
    if let Ok(up) = CGEvent::new_keyboard_event(source.clone(), code, false) {
        if flags != CGEventFlags::empty() {
            up.set_flags(flags);
        }
        up.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, ZMOUSE_TAG);
        up.post(CGEventTapLocation::HID);
    }
}

pub fn describe(action: &Action) -> String {
    match action {
        Action::Disabled => "disabled".into(),
        Action::Button { button } => format!("mouse button {button}"),
        Action::Keystroke { key, mods } => {
            if mods.is_empty() {
                format!("key '{key}'")
            } else {
                format!("{}+{key}", mods.join("+"))
            }
        }
    }
}

/// HID input-value callback: on every button-page event, record device ownership of that button.
unsafe extern "C-unwind" fn hid_value_callback(
    _context: *mut c_void,
    _result: IOReturn,
    sender: *mut c_void,
    value: core::ptr::NonNull<IOHIDValue>,
) {
    let value = unsafe { value.as_ref() };
    let element = value.element();
    let page = element.usage_page();
    if page != HID_USAGE_PAGE_BUTTON && page != HID_USAGE_PAGE_KEYBOARD {
        return;
    }
    if sender.is_null() {
        return;
    }
    let device = unsafe { &*(sender as *const IOHIDDevice) };
    let Some(registry) = registry_entry_id(device) else {
        return;
    };
    // Reading vendor/product on each press is cheap (presses are rare) and lets us match the
    // device by its stable USB identity rather than the ephemeral registry id.
    let vidpid = match (hid::vendor_id(device), hid::product_id(device)) {
        (Some(v), Some(p)) => Some(config::vidpid_key(v, p)),
        _ => None,
    };
    let ident = DeviceIdent { registry, vidpid };
    if page == HID_USAGE_PAGE_BUTTON {
        // HID button usage is 1-indexed; CGEvent button numbers are 0-indexed.
        let button = element.usage() as i64 - 1;
        LAST_DEVICE_FOR_BUTTON.with(|m| {
            m.borrow_mut().insert(button, ident);
        });
    } else {
        // Any keyboard-page event from this device makes it the source of the keystroke the tap
        // is about to see (HID precedes the tap on this thread).
        LAST_KEYBOARD_DEVICE.with(|c| c.set(Some(ident)));
    }
}
