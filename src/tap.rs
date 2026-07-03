//! Milestone-1 proof #2: intercept a mouse button system-wide via a CGEventTap and
//! rewrite it into a keystroke.
//!
//! Uses the `core-graphics` (0.25) CF ecosystem. Kept deliberately separate from `hid.rs`,
//! which lives in the `objc2-core-foundation` ecosystem.

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

/// The physical "back" button. In CGEvent button-number terms:
/// 0 = left, 1 = right, 2 = middle, 3 = back, 4 = forward.
const TARGET_BUTTON: i64 = 3;

/// Virtual key code for ANSI 'C' (kVK_ANSI_C).
const KEY_C: u16 = 8;

/// Run the remap tap until the process is killed. Blocks on the current thread's run loop.
pub fn run_remap() {
    println!(
        "zmouse tap: remapping mouse button {} (\"back\") -> Cmd+C.\n\
         To stop: focus THIS terminal, then Ctrl+C (or `pkill -f zmouse` elsewhere).\n\
         (Requires Accessibility permission for your terminal.)",
        TARGET_BUTTON
    );

    // One event source, reused to synthesize the keystrokes.
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .expect("failed to create CGEventSource");

    let result = CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
        ],
        move |_proxy, event_type, event| {
            let button = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            if button != TARGET_BUTTON {
                return CallbackResult::Keep;
            }
            match event_type {
                CGEventType::OtherMouseDown => {
                    synthesize_cmd_c(&source);
                    // Swallow the original button press.
                    CallbackResult::Drop
                }
                CGEventType::OtherMouseUp => CallbackResult::Drop,
                _ => CallbackResult::Keep,
            }
        },
        || {
            // Run loop is entered here; with_enabled has already installed & enabled the tap.
            core_foundation::runloop::CFRunLoop::run_current();
        },
    );

    if result.is_err() {
        eprintln!(
            "\nERROR: failed to create the event tap.\n\
             This almost always means Accessibility permission is missing.\n\n\
             Grant it in: System Settings -> Privacy & Security -> Accessibility\n\
             Add (and enable) the app you launched `zmouse` from \
             (e.g. Terminal / iTerm / your IDE), then re-run.\n"
        );
        std::process::exit(1);
    }
}

/// Post a Cmd+C key-down / key-up pair to the session.
fn synthesize_cmd_c(source: &CGEventSource) {
    if let Ok(down) = CGEvent::new_keyboard_event(source.clone(), KEY_C, true) {
        down.set_flags(CGEventFlags::CGEventFlagCommand);
        down.post(CGEventTapLocation::HID);
    }
    if let Ok(up) = CGEvent::new_keyboard_event(source.clone(), KEY_C, false) {
        up.set_flags(CGEventFlags::CGEventFlagCommand);
        up.post(CGEventTapLocation::HID);
    }
}
