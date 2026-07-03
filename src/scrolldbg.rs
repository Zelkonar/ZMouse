//! `zmouse scrolldbg` — a read-only diagnostic that dumps the raw delta fields of every
//! discrete mouse-wheel event, so we can see *why* some apps ignore weak up-ticks.
//!
//! A worn encoder can emit a scroll-up event whose line delta (AXIS_1 = 11) is 0 or tiny even
//! though the pixel/fixed-point delta is nonzero. Apps that key off the line delta then round it
//! to zero and "ignore" the scroll. Run this, scroll DOWN a few times (works) then UP a few times
//! (the flaky direction), and compare the columns. Ctrl-C to quit.

use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

pub fn run() {
    println!(
        "scrolldbg — scroll DOWN a few, then UP a few, and compare 'line' across directions.\n\
         (trackpads report continuous=1 and are shown too.)  Ctrl-C to quit.\n"
    );
    println!(
        "{:>8}  {:>8}  {:>10}  {:>10}",
        "cont", "line", "fixedpt", "pixel"
    );

    let tap = unsafe {
        CGEventTap::new_unchecked(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly, // observe only; never alter/drop
            vec![CGEventType::ScrollWheel],
            |_proxy, _etype, event| {
                let cont = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
                let line = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
                let fixed = event
                    .get_double_value_field(EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1);
                let pixel =
                    event.get_double_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
                let arrow = if line > 0 || fixed > 0.0 {
                    "up"
                } else if line < 0 || fixed < 0.0 {
                    "down"
                } else {
                    "--"
                };
                println!("{cont:>8}  {line:>8}  {fixed:>10.3}  {pixel:>10.3}   {arrow}");
                CallbackResult::Keep
            },
        )
    };

    let tap = match tap {
        Ok(t) => t,
        Err(_) => {
            eprintln!(
                "Could not create the event tap. Grant Accessibility permission to the terminal\n\
                 (System Settings -> Privacy & Security -> Accessibility) and retry."
            );
            std::process::exit(1);
        }
    };

    let loop_source = tap
        .mach_port()
        .create_runloop_source(0)
        .expect("runloop source");
    CFRunLoop::get_current()
        .add_source(&loop_source, unsafe { core_foundation::runloop::kCFRunLoopCommonModes });
    tap.enable();
    CFRunLoop::run_current();
}
