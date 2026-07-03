//! zmouse — a macOS mouse-button remapper (SteerMouse-style), Milestone 1 PoC.
//!
//! Three subcommands, each proving one load-bearing macOS API in isolation:
//!   zmouse list   -> enumerate connected mice (IOHIDManager)
//!   zmouse tap    -> intercept button "back" and remap to Cmd+C (CGEventTap)
//!   zmouse probe  -> HID + event-tap correlation spike for per-device mapping

mod config;
mod editor;
mod engine;
mod hid;
mod keymap;
mod menubar;
mod probe;
mod scrolldbg;
mod tap;

use std::path::PathBuf;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    // Default to the menu-bar app: bare `zmouse`, or a double-clicked .app (which launches with
    // no args, or on older macOS a `-psn_...` process-serial argument).
    let cmd = if arg.is_empty() || arg.starts_with("-psn") {
        "menu"
    } else {
        arg.as_str()
    };
    match cmd {
        "list" => hid::print_mice(),
        "tap" => tap::run_remap(),
        "probe" => probe::run_probe(),
        "scrolldbg" => scrolldbg::run(),
        "run" => run_engine(),
        "menu" => run_menu(),
        "edit" => run_edit(),
        "help" | "-h" | "--help" => print_usage(),
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!(
        "zmouse — macOS mouse-button remapper\n\n\
         usage:\n  \
         zmouse           launch the menu-bar app (default)\n  \
         zmouse menu [config.toml]  menu-bar app\n  \
         zmouse edit [config.toml]  open the GUI mapping editor\n  \
         zmouse run [config.toml]   apply mappings headless (no UI)\n  \
         zmouse list      list connected mice\n  \
         zmouse probe     log HID + event-tap streams (find button numbers)\n  \
         zmouse scrolldbg dump raw scroll-wheel delta fields (diagnose weak ticks)\n  \
         zmouse tap       remap the \"back\" button to Cmd+C (hardcoded demo)\n"
    );
}

fn run_edit() {
    let path = std::env::args().nth(2).map(PathBuf::from);
    // The editor works even if the config doesn't exist yet — start from empty.
    let cfg = config::load(path.as_deref()).unwrap_or_default();
    let save_path = path
        .or_else(config::default_path)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    if let Err(e) = editor::run(cfg, save_path) {
        eprintln!("Editor error: {e}");
        std::process::exit(1);
    }
}

fn run_menu() {
    let path = std::env::args().nth(2).map(PathBuf::from);
    match config::load(path.as_deref()) {
        Ok(cfg) => menubar::run(cfg, path),
        Err(e) => {
            eprintln!("Could not load config: {e}\n");
            print_config_help();
            std::process::exit(1);
        }
    }
}

fn run_engine() {
    let path = std::env::args().nth(2).map(PathBuf::from);
    match config::load(path.as_deref()) {
        Ok(cfg) => engine::run(cfg),
        Err(e) => {
            eprintln!("Could not load config: {e}\n");
            print_config_help();
            std::process::exit(1);
        }
    }
}

fn print_config_help() {
    if let Some(p) = config::default_path() {
        eprintln!("Expected a config at: {}", p.display());
    }
    eprintln!(
        "Pass a path explicitly: `zmouse run ./config.toml`\n\
         See the sample config in the repo (config.example.toml). Use `zmouse list`\n\
         to find your mouse's registry_id."
    );
}
