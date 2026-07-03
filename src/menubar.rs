//! Milestone 3: native macOS menu-bar app (`NSStatusItem`).
//!
//! Shares the main thread's run loop with the event tap: `engine::install()` puts the tap +
//! HID manager onto the current run loop, then `NSApplication::run()` drives that same loop, so
//! remapping and the menu run in one process with no daemon.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff, NSControlStateValueOn,
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSTimer};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::config::{self, Config, ScrollConfig};
use crate::engine::{self, SharedMappings};

/// Preset values (ms) offered in the "Jitter guard" submenu.
const GUARD_PRESETS: [i64; 6] = [40, 60, 80, 100, 120, 150];

/// Preset values (pixels/line) offered in the "Weak-tick floor" submenu.
const FLOOR_PRESETS: [i64; 5] = [6, 8, 10, 14, 20];

/// Is the app currently registered to launch at login?
fn login_enabled() -> bool {
    unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
}

/// Register/unregister the app as a login item. Only works when running as the bundled .app.
fn set_login_item(enabled: bool) -> Result<(), String> {
    let service = unsafe { SMAppService::mainAppService() };
    let result = if enabled {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    result.map_err(|e| e.localizedDescription().to_string())
}

/// State the menu actions need. Stored as instance variables on the handler class.
struct Ivars {
    mappings: SharedMappings,
    key_mappings: SharedMappings,
    config_path: Option<PathBuf>,
    toggle_item: Retained<NSMenuItem>,
    login_item: Retained<NSMenuItem>,
    scroll_filter_item: Retained<NSMenuItem>,
    /// Guard-preset menu items; each carries its ms value in its `tag`.
    guard_items: Vec<Retained<NSMenuItem>>,
    /// Weak-tick boost toggle + floor presets (each carries its px/line value in its `tag`).
    boost_item: Retained<NSMenuItem>,
    floor_items: Vec<Retained<NSMenuItem>>,
    /// Last-seen config file mtime, for auto-reload-on-save (interior mutability: single thread).
    last_mtime: Cell<Option<SystemTime>>,
}

define_class!(
    // SAFETY: superclass NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[name = "ZMouseMenuHandler"]
    #[ivars = Ivars]
    struct MenuHandler;

    impl MenuHandler {
        /// Toggle the event tap on/off without tearing it down.
        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: Option<&AnyObject>) {
            engine::set_enabled(!engine::is_enabled());
            self.refresh_toggle();
        }

        /// Re-read the config file and hot-swap the mapping table (menu action).
        #[unsafe(method(reloadConfig:))]
        fn reload_config(&self, _sender: Option<&AnyObject>) {
            self.reload_from_disk("Reloaded config.");
        }

        /// Timer tick: if the config file changed on disk (e.g. the editor saved), reload it.
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&AnyObject>) {
            let current = self.config_mtime();
            if current.is_some() && current != self.ivars().last_mtime.get() {
                self.reload_from_disk("Auto-reloaded config after edit.");
            }
        }

        /// Launch the GUI editor as a separate process (avoids fighting over the event loop).
        #[unsafe(method(editMappings:))]
        fn edit_mappings(&self, _sender: Option<&AnyObject>) {
            let Ok(exe) = std::env::current_exe() else {
                eprintln!("cannot locate current executable to launch editor");
                return;
            };
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("edit");
            if let Some(path) = &self.ivars().config_path {
                cmd.arg(path);
            }
            if let Err(e) = cmd.spawn() {
                eprintln!("failed to launch editor: {e}");
            }
        }

        /// Toggle the scroll-wheel jitter filter on/off (persists to config, applies live).
        #[unsafe(method(toggleScrollFilter:))]
        fn toggle_scroll_filter(&self, _sender: Option<&AnyObject>) {
            self.edit_scroll(|s| s.jitter_filter = !s.jitter_filter);
        }

        /// Set the jitter guard (ms) from the clicked preset (its value is in the item's tag).
        #[unsafe(method(setScrollGuard:))]
        fn set_scroll_guard(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let ms: isize = unsafe { msg_send![sender, tag] };
            let ms = ms.max(0) as u64;
            // Choosing a guard implies you want the filter on.
            self.edit_scroll(|s| {
                s.reversal_guard_ms = ms;
                s.jitter_filter = true;
            });
        }

        /// Toggle the weak-tick boost on/off (persists to config, applies live).
        #[unsafe(method(toggleScrollBoost:))]
        fn toggle_scroll_boost(&self, _sender: Option<&AnyObject>) {
            self.edit_scroll(|s| s.boost_weak_ticks = !s.boost_weak_ticks);
        }

        /// Set the weak-tick floor (px/line) from the clicked preset (value in the item's tag).
        #[unsafe(method(setScrollFloor:))]
        fn set_scroll_floor(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let px: isize = unsafe { msg_send![sender, tag] };
            let px = px.max(1) as i64;
            // Choosing a floor implies you want the boost on.
            self.edit_scroll(|s| {
                s.min_tick_pixels = px;
                s.boost_weak_ticks = true;
            });
        }

        /// Toggle "launch at login" (registers/unregisters via SMAppService).
        #[unsafe(method(toggleLoginItem:))]
        fn toggle_login_item(&self, _sender: Option<&AnyObject>) {
            let want = !login_enabled();
            if let Err(e) = set_login_item(want) {
                eprintln!(
                    "Launch-at-login change failed: {e}\n\
                     (This only works when running the bundled zmouse.app, not via `cargo`/CLI.)"
                );
            }
            self.refresh_login_item();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            std::process::exit(0);
        }
    }

    unsafe impl NSObjectProtocol for MenuHandler {}
);

impl MenuHandler {
    fn new(ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn refresh_toggle(&self) {
        let state = if engine::is_enabled() {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        };
        self.ivars().toggle_item.setState(state);
    }

    fn refresh_login_item(&self) {
        let state = if login_enabled() {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        };
        self.ivars().login_item.setState(state);
    }

    /// Load config from disk, mutate the `[scroll]` settings, persist, apply live, refresh menu.
    fn edit_scroll(&self, f: impl FnOnce(&mut ScrollConfig)) {
        let Some(save_path) = self
            .ivars()
            .config_path
            .clone()
            .or_else(config::default_path)
        else {
            eprintln!("cannot determine config path to save scroll settings");
            return;
        };
        let mut cfg = config::load(Some(&save_path)).unwrap_or_default();
        f(&mut cfg.scroll);
        engine::apply_settings(&cfg);
        if let Err(e) = config::save(&save_path, &cfg) {
            eprintln!("saving scroll settings failed: {e}");
        }
        // Our own write; keep last_mtime in sync so the file-watch tick doesn't redo it.
        self.ivars().last_mtime.set(self.config_mtime());
        self.refresh_scroll_menu(&cfg.scroll);
    }

    fn refresh_scroll_menu(&self, scroll: &ScrollConfig) {
        self.ivars()
            .scroll_filter_item
            .setState(if scroll.jitter_filter {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        for item in &self.ivars().guard_items {
            let on = item.tag() as u64 == scroll.reversal_guard_ms;
            item.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        self.ivars()
            .boost_item
            .setState(if scroll.boost_weak_ticks {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        for item in &self.ivars().floor_items {
            let on = item.tag() as i64 == scroll.min_tick_pixels;
            item.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    /// mtime of the config file we're watching, if it exists.
    fn config_mtime(&self) -> Option<SystemTime> {
        let path = self
            .ivars()
            .config_path
            .clone()
            .or_else(config::default_path)?;
        std::fs::metadata(path).ok()?.modified().ok()
    }

    /// Load the config from disk, swap the live mapping table, and note the new mtime.
    fn reload_from_disk(&self, announce: &str) {
        match config::load(self.ivars().config_path.as_deref()) {
            Ok(cfg) => {
                *self.ivars().mappings.borrow_mut() = cfg.resolve();
                *self.ivars().key_mappings.borrow_mut() = cfg.resolve_keys();
                engine::apply_settings(&cfg);
                self.ivars().last_mtime.set(self.config_mtime());
                println!("{announce}");
                engine::print_startup(&cfg);
            }
            Err(e) => eprintln!("Reload failed: {e}"),
        }
    }
}

/// Build the menu-bar UI, install the engine, and run the app (blocks until Quit).
pub fn run(config: Config, config_path: Option<PathBuf>) {
    let mtm = MainThreadMarker::new().expect("menu-bar app must run on the main thread");

    let app = NSApplication::sharedApplication(mtm);
    // Accessory = menu-bar agent: no Dock icon, no main window.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    engine::print_startup(&config);
    engine::apply_settings(&config);
    let mappings: SharedMappings = std::rc::Rc::new(std::cell::RefCell::new(config.resolve()));
    let key_mappings: SharedMappings =
        std::rc::Rc::new(std::cell::RefCell::new(config.resolve_keys()));

    // Install the tap + HID tracker onto this (main) thread's run loop.
    let installed = match engine::install(mappings.clone(), key_mappings.clone()) {
        Ok(i) => i,
        Err(()) => {
            eprintln!(
                "\nERROR: failed to create the event tap (Accessibility permission needed).\n\
                 Grant it: System Settings -> Privacy & Security -> Accessibility, add the app\n\
                 you launched this from, then re-run.\n"
            );
            std::process::exit(1);
        }
    };

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = status_item.button(mtm) {
        // Native SF Symbol — crisp and auto-adapts to light/dark menu bars. Fall back to emoji.
        let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("computermouse.fill"),
            Some(&NSString::from_str("ZMouse")),
        );
        match symbol {
            Some(img) => {
                img.setTemplate(true);
                button.setImage(Some(&img));
            }
            None => button.setTitle(&NSString::from_str("🖱")),
        }
    }

    // These items are created first so the handler can hold references to update their checkmarks.
    let toggle_item = NSMenuItem::new(mtm);
    toggle_item.setTitle(&NSString::from_str("Enabled"));
    toggle_item.setState(NSControlStateValueOn);

    let login_item = NSMenuItem::new(mtm);
    login_item.setTitle(&NSString::from_str("Launch at login"));
    login_item.setState(if login_enabled() {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });

    // Scroll jitter filter toggle + guard presets.
    let scroll_filter_item = NSMenuItem::new(mtm);
    scroll_filter_item.setTitle(&NSString::from_str("Filter scroll jitter"));
    scroll_filter_item.setState(if config.scroll.jitter_filter {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    let guard_items: Vec<Retained<NSMenuItem>> = GUARD_PRESETS
        .iter()
        .map(|&ms| {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(&format!("{ms} ms")));
            item.setTag(ms as isize);
            item.setState(if config.scroll.reversal_guard_ms == ms as u64 {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            item
        })
        .collect();

    // Weak-tick boost toggle + floor presets.
    let boost_item = NSMenuItem::new(mtm);
    boost_item.setTitle(&NSString::from_str("Boost weak scroll ticks"));
    boost_item.setState(if config.scroll.boost_weak_ticks {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    let floor_items: Vec<Retained<NSMenuItem>> = FLOOR_PRESETS
        .iter()
        .map(|&px| {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(&format!("{px} px/line")));
            item.setTag(px as isize);
            item.setState(if config.scroll.min_tick_pixels == px {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            item
        })
        .collect();

    // Record the config's current mtime so auto-reload only fires on *later* changes.
    let initial_mtime = config_path
        .clone()
        .or_else(config::default_path)
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());

    let handler = MenuHandler::new(Ivars {
        mappings,
        key_mappings,
        config_path,
        toggle_item: toggle_item.clone(),
        login_item: login_item.clone(),
        scroll_filter_item: scroll_filter_item.clone(),
        guard_items: guard_items.clone(),
        boost_item: boost_item.clone(),
        floor_items: floor_items.clone(),
        last_mtime: Cell::new(initial_mtime),
    });

    // Watch the config file: every second, reload if it changed on disk (editor Save applies live).
    unsafe {
        let timer = NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            1.0,
            &handler,
            sel!(tick:),
            None,
            true,
        );
        std::mem::forget(timer);
    }

    let menu = NSMenu::new(mtm);

    // Enabled toggle.
    unsafe {
        toggle_item.setAction(Some(sel!(toggleEnabled:)));
        toggle_item.setTarget(Some(&handler));
    }
    menu.addItem(&toggle_item);

    // Launch at login.
    unsafe {
        login_item.setAction(Some(sel!(toggleLoginItem:)));
        login_item.setTarget(Some(&handler));
    }
    menu.addItem(&login_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Scroll jitter filter toggle.
    unsafe {
        scroll_filter_item.setAction(Some(sel!(toggleScrollFilter:)));
        scroll_filter_item.setTarget(Some(&handler));
    }
    menu.addItem(&scroll_filter_item);

    // "Jitter guard" submenu of ms presets.
    let guard_menu = NSMenu::new(mtm);
    for item in &guard_items {
        unsafe {
            item.setAction(Some(sel!(setScrollGuard:)));
            item.setTarget(Some(&handler));
        }
        guard_menu.addItem(item);
    }
    let guard_parent = NSMenuItem::new(mtm);
    guard_parent.setTitle(&NSString::from_str("Jitter guard"));
    guard_parent.setSubmenu(Some(&guard_menu));
    menu.addItem(&guard_parent);

    // Weak-tick boost toggle.
    unsafe {
        boost_item.setAction(Some(sel!(toggleScrollBoost:)));
        boost_item.setTarget(Some(&handler));
    }
    menu.addItem(&boost_item);

    // "Weak-tick floor" submenu of px/line presets.
    let floor_menu = NSMenu::new(mtm);
    for item in &floor_items {
        unsafe {
            item.setAction(Some(sel!(setScrollFloor:)));
            item.setTarget(Some(&handler));
        }
        floor_menu.addItem(item);
    }
    let floor_parent = NSMenuItem::new(mtm);
    floor_parent.setTitle(&NSString::from_str("Weak-tick floor"));
    floor_parent.setSubmenu(Some(&floor_menu));
    menu.addItem(&floor_parent);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Edit mappings (opens the GUI editor).
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(&NSString::from_str("Edit mappings…"));
    unsafe {
        edit_item.setAction(Some(sel!(editMappings:)));
        edit_item.setTarget(Some(&handler));
    }
    menu.addItem(&edit_item);

    // Reload config.
    let reload_item = NSMenuItem::new(mtm);
    reload_item.setTitle(&NSString::from_str("Reload config"));
    unsafe {
        reload_item.setAction(Some(sel!(reloadConfig:)));
        reload_item.setTarget(Some(&handler));
    }
    menu.addItem(&reload_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Devices section (informational; items are disabled because they have no action).
    let header = NSMenuItem::new(mtm);
    header.setTitle(&NSString::from_str("Configured devices:"));
    menu.addItem(&header);
    for dev in &config.device {
        let label = dev.name.as_deref().unwrap_or("<unnamed>");
        let item = NSMenuItem::new(mtm);
        item.setTitle(&NSString::from_str(&format!(
            "  {label} — {} mapping(s)",
            dev.mapping.len()
        )));
        menu.addItem(&item);
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Quit.
    let quit_item = NSMenuItem::new(mtm);
    quit_item.setTitle(&NSString::from_str("Quit ZMouse"));
    quit_item.setKeyEquivalent(&NSString::from_str("q"));
    unsafe {
        quit_item.setAction(Some(sel!(quit:)));
        quit_item.setTarget(Some(&handler));
    }
    menu.addItem(&quit_item);

    status_item.setMenu(Some(&menu));

    // Keep the tap/HID handle and UI objects alive for the program lifetime. The process
    // exits via the Quit menu item, so these deliberate leaks are fine.
    std::mem::forget(installed);
    std::mem::forget(handler);
    std::mem::forget(status_item);
    std::mem::forget(menu);

    app.run();
}
