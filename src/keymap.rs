//! Name → macOS virtual key code, and modifier name → CGEventFlags.
//!
//! Key codes are the standard `kVK_*` values from Carbon's `HIToolbox/Events.h`.

use core_graphics::event::CGEventFlags;

/// Resolve a config key name (case-insensitive) to a virtual key code.
pub fn key_code(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    let code: u16 = match n.as_str() {
        // Letters (kVK_ANSI_A = 0 … layout order, not alphabetical).
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "o" => 31,
        "u" => 32,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "k" => 40,
        "n" => 45,
        "m" => 46,
        // Digits.
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "5" => 23,
        "6" => 22,
        "7" => 26,
        "8" => 28,
        "9" => 25,
        "0" => 29,
        // Whitespace / editing.
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "forward_delete" => 117,
        // Navigation.
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "home" => 115,
        "end" => 119,
        "page_up" | "pageup" => 116,
        "page_down" | "pagedown" => 121,
        // Function keys.
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        // Punctuation.
        "minus" | "-" => 27,
        "equal" | "=" => 24,
        "left_bracket" | "[" => 33,
        "right_bracket" | "]" => 30,
        "backslash" | "\\" => 42,
        "semicolon" | ";" => 41,
        "quote" | "'" => 39,
        "comma" | "," => 43,
        "period" | "." => 47,
        "slash" | "/" => 44,
        "grave" | "`" => 50,
        _ => return None,
    };
    Some(code)
}

/// Resolve a modifier name (case-insensitive) to its CGEventFlags bit.
pub fn modifier_flag(name: &str) -> Option<CGEventFlags> {
    match name.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" => Some(CGEventFlags::CGEventFlagCommand),
        "shift" => Some(CGEventFlags::CGEventFlagShift),
        "opt" | "option" | "alt" => Some(CGEventFlags::CGEventFlagAlternate),
        "ctrl" | "control" => Some(CGEventFlags::CGEventFlagControl),
        "fn" | "function" => Some(CGEventFlags::CGEventFlagSecondaryFn),
        _ => None,
    }
}

/// Combine a list of modifier names into a single flag set. Unknown names are ignored
/// by the caller (validated at config-load time).
pub fn combine_mods(mods: &[String]) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    for m in mods {
        if let Some(f) = modifier_flag(m) {
            flags |= f;
        }
    }
    flags
}
