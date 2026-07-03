//! Milestone 4: GUI config editor (egui / eframe), run as a separate process.
//!
//! Launched via `zmouse edit [config.toml]` (the menu-bar app spawns this as a child so the
//! two never fight over an event loop). It edits the same TOML the engine reads; the running
//! agent watches that file and auto-applies changes on save.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;

use eframe::egui;
use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_io_kit::{IOHIDDevice, IOHIDManager, IOHIDValue, IOReturn, kIOHIDOptionsTypeNone};

use crate::config::{
    self, Action, Config, DeviceConfig, DeviceId, KeyMapping, Mapping, ScrollConfig,
};
use crate::hid::{self, MouseDevice, registry_entry_id};

/// A button press captured from the HID stream, with enough identity to tell whether it came from
/// the device the user is editing.
#[derive(Clone, Copy)]
struct CapturedPress {
    registry: u64,
    vendor_id: Option<i64>,
    product_id: Option<i64>,
    button: i64,
}

thread_local! {
    /// Last physical button PRESS seen while the editor is listening. Written by the HID callback,
    /// drained by the egui loop when a row is capturing.
    static LAST_PRESS: RefCell<Option<CapturedPress>> = const { RefCell::new(None) };
}

/// HID input-value callback used only for "capture button". Records the most recent button press
/// (any button, including left/right). The click that starts capture is discarded by the one-frame
/// arming in `central_contents`, so it doesn't self-trigger.
unsafe extern "C-unwind" fn capture_callback(
    _context: *mut c_void,
    _result: IOReturn,
    sender: *mut c_void,
    value: core::ptr::NonNull<IOHIDValue>,
) {
    let value = unsafe { value.as_ref() };
    let element = value.element();
    if element.usage_page() != 0x09 {
        return; // buttons only
    }
    if value.integer_value() == 0 {
        return; // presses (down) only, not releases
    }
    // HID usage is 1-indexed; CGEvent button is 0-indexed (left=0, right=1, middle=2, …).
    let button = element.usage() as i64 - 1;
    let (reg, vendor_id, product_id) = if sender.is_null() {
        (0, None, None)
    } else {
        let device = unsafe { &*(sender as *const IOHIDDevice) };
        (
            registry_entry_id(device).unwrap_or(0),
            hid::vendor_id(device),
            hid::product_id(device),
        )
    };
    LAST_PRESS.with(|p| {
        *p.borrow_mut() = Some(CapturedPress {
            registry: reg,
            vendor_id,
            product_id,
            button,
        })
    });
}

/// Which action a mapping row represents (drives the combo box).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Keystroke,
    Button,
    Disabled,
}

#[derive(Default, Clone)]
struct ModSet {
    cmd: bool,
    shift: bool,
    opt: bool,
    ctrl: bool,
}

impl ModSet {
    fn from_slice(mods: &[String]) -> Self {
        let mut s = Self::default();
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" => s.cmd = true,
                "shift" => s.shift = true,
                "opt" | "option" | "alt" => s.opt = true,
                "ctrl" | "control" => s.ctrl = true,
                _ => {}
            }
        }
        s
    }

    fn to_vec(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.cmd {
            v.push("cmd".to_string());
        }
        if self.shift {
            v.push("shift".to_string());
        }
        if self.opt {
            v.push("opt".to_string());
        }
        if self.ctrl {
            v.push("ctrl".to_string());
        }
        v
    }
}

/// Editable form of one button mapping.
struct MappingRow {
    button: i64,
    kind: ActionKind,
    key: String,
    mods: ModSet,
    target_button: i64,
}

impl MappingRow {
    fn from_mapping(m: &Mapping) -> Self {
        let mut row = MappingRow {
            button: m.button,
            kind: ActionKind::Disabled,
            key: String::new(),
            mods: ModSet::default(),
            target_button: 2,
        };
        match &m.action {
            Action::Keystroke { key, mods } => {
                row.kind = ActionKind::Keystroke;
                row.key = key.clone();
                row.mods = ModSet::from_slice(mods);
            }
            Action::Button { button } => {
                row.kind = ActionKind::Button;
                row.target_button = *button;
            }
            Action::Disabled => row.kind = ActionKind::Disabled,
        }
        row
    }

    fn to_action(&self) -> Action {
        match self.kind {
            ActionKind::Keystroke => Action::Keystroke {
                key: self.key.clone(),
                mods: self.mods.to_vec(),
            },
            ActionKind::Button => Action::Button {
                button: self.target_button,
            },
            ActionKind::Disabled => Action::Disabled,
        }
    }
}

/// Editable form of one keystroke-triggered mapping (a button that emits a key, e.g. MMO thumb
/// buttons). Same action shape as `MappingRow`, but triggered by a source key name.
struct KeyRow {
    source: String,
    kind: ActionKind,
    key: String,
    mods: ModSet,
    target_button: i64,
}

impl KeyRow {
    fn from_mapping(k: &KeyMapping) -> Self {
        let mut row = KeyRow {
            source: k.key.clone(),
            kind: ActionKind::Disabled,
            key: String::new(),
            mods: ModSet::default(),
            target_button: 2,
        };
        match &k.action {
            Action::Keystroke { key, mods } => {
                row.kind = ActionKind::Keystroke;
                row.key = key.clone();
                row.mods = ModSet::from_slice(mods);
            }
            Action::Button { button } => {
                row.kind = ActionKind::Button;
                row.target_button = *button;
            }
            Action::Disabled => row.kind = ActionKind::Disabled,
        }
        row
    }

    fn to_action(&self) -> Action {
        match self.kind {
            ActionKind::Keystroke => Action::Keystroke {
                key: self.key.clone(),
                mods: self.mods.to_vec(),
            },
            ActionKind::Button => Action::Button {
                button: self.target_button,
            },
            ActionKind::Disabled => Action::Disabled,
        }
    }

    fn to_key_mapping(&self) -> KeyMapping {
        KeyMapping {
            key: self.source.clone(),
            action: self.to_action(),
        }
    }
}

/// Editable form of one device and its mappings.
struct DeviceRow {
    /// Fallback identity; ephemeral (changes on reconnect / transport switch).
    registry_id: Option<u64>,
    /// Stable USB identity — preferred. Set when the device was added from the connected list.
    vendor_id: Option<i64>,
    product_id: Option<i64>,
    /// Additional identities for the same physical device (grouped in via "merge into").
    also: Vec<DeviceId>,
    name: String,
    mappings: Vec<MappingRow>,
    keys: Vec<KeyRow>,
}

impl DeviceRow {
    /// Short description of how this device's primary identity is expressed, for the header line.
    fn ident_desc(&self) -> String {
        config::identity_desc(self.registry_id, self.vendor_id, self.product_id)
    }

    /// This device's primary identity as a `DeviceId` (used when comparing / promoting).
    fn primary_id(&self) -> DeviceId {
        DeviceId {
            registry_id: self.registry_id,
            vendor_id: self.vendor_id,
            product_id: self.product_id,
        }
    }

    /// True when the device has no usable identity at all (primary or grouped).
    fn device_key_is_empty(&self) -> bool {
        self.primary_id().device_key().is_none()
            && self.also.iter().all(|id| id.device_key().is_none())
    }
}

pub struct EditorApp {
    path: PathBuf,
    devices: Vec<DeviceRow>,
    connected: Vec<MouseDevice>,
    selected: Option<usize>,
    status: String,
    /// Global scroll settings — preserved across saves and editable in the UI.
    scroll: ScrollConfig,
    /// HID manager kept alive to receive capture callbacks (lazily created on first frame).
    hid_manager: Option<CFRetained<IOHIDManager>>,
    /// Index (within the selected device) of the mapping row currently capturing a button.
    capturing: Option<usize>,
    /// Becomes true one frame after capture begins, so the left-click on the "Capture" button
    /// itself is discarded and only the *next* button press is captured (lets left/right work).
    capture_armed: bool,
    /// Transient note shown after a capture (e.g. "press came from a different device").
    capture_note: String,
}

impl EditorApp {
    fn new(config: Config, path: PathBuf, connected: Vec<MouseDevice>) -> Self {
        let devices = config
            .device
            .iter()
            .map(|d| DeviceRow {
                registry_id: d.registry_id,
                vendor_id: d.vendor_id,
                product_id: d.product_id,
                also: d.also.clone(),
                name: d.name.clone().unwrap_or_default(),
                mappings: d.mapping.iter().map(MappingRow::from_mapping).collect(),
                keys: d.key.iter().map(KeyRow::from_mapping).collect(),
            })
            .collect::<Vec<_>>();
        let selected = if devices.is_empty() { None } else { Some(0) };
        Self {
            path,
            devices,
            connected,
            selected,
            status: String::new(),
            scroll: config.scroll,
            hid_manager: None,
            capturing: None,
            capture_armed: false,
            capture_note: String::new(),
        }
    }

    /// Lazily open an IOHIDManager on the current (main-thread) run loop so the capture callback
    /// fires. eframe drives the main CFRunLoop, so a scheduled source is serviced normally.
    fn ensure_capture_hid(&mut self) {
        if self.hid_manager.is_some() {
            return;
        }
        let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);
        unsafe { manager.set_device_matching(None) };
        unsafe {
            manager.register_input_value_callback(Some(capture_callback), std::ptr::null_mut());
        }
        let _ = manager.open(kIOHIDOptionsTypeNone);
        unsafe {
            if let Some(rl) = CFRunLoop::current() {
                manager.schedule_with_run_loop(&rl, kCFRunLoopDefaultMode.unwrap());
            }
        }
        self.hid_manager = Some(manager);
    }

    /// Begin capturing for a row: clear any stale press so the click that started capture
    /// (or an earlier press) doesn't count.
    fn begin_capture(&mut self, row: usize) {
        LAST_PRESS.with(|p| *p.borrow_mut() = None);
        self.capture_note.clear();
        self.capturing = Some(row);
        self.capture_armed = false; // skip the frame the Capture click lands on
    }

    fn to_config(&self) -> Config {
        Config {
            scroll: self.scroll.clone(),
            device: self
                .devices
                .iter()
                .map(|d| DeviceConfig {
                    registry_id: d.registry_id,
                    vendor_id: d.vendor_id,
                    product_id: d.product_id,
                    also: d.also.clone(),
                    name: if d.name.trim().is_empty() {
                        None
                    } else {
                        Some(d.name.clone())
                    },
                    mapping: d
                        .mappings
                        .iter()
                        .map(|m| Mapping {
                            button: m.button,
                            action: m.to_action(),
                        })
                        .collect(),
                    key: d.keys.iter().map(KeyRow::to_key_mapping).collect(),
                })
                .collect(),
        }
    }

    fn is_connected(&self, dev: &DeviceRow) -> bool {
        // Any of the device's identities (primary or grouped) being present counts as connected.
        std::iter::once(dev.primary_id())
            .chain(dev.also.iter().cloned())
            .any(|id| self.identity_connected(&id))
    }

    fn identity_connected(&self, id: &DeviceId) -> bool {
        self.connected.iter().any(|c| {
            // Prefer the stable USB identity; fall back to registry id.
            if let (Some(v), Some(p)) = (id.vendor_id, id.product_id) {
                c.vendor_id == Some(v) && c.product_id == Some(p)
            } else if let Some(r) = id.registry_id {
                c.registry_entry_id == Some(r)
            } else {
                false
            }
        })
    }

    fn save(&mut self) {
        match config::save(&self.path, &self.to_config()) {
            Ok(()) => {
                self.status = format!(
                    "Saved to {} — the running app applies it automatically.",
                    self.path.display()
                )
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }
}

/// Launch the editor window. Blocks until the window is closed.
pub fn run(config: Config, path: PathBuf) -> Result<(), String> {
    let connected = hid::list_mice();
    let app = EditorApp::new(config, path, connected);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([820.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ZMouse — mappings",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| e.to_string())
}

impl eframe::App for EditorApp {
    // egui 0.35: the app is handed a root `Ui`; panels are shown *into* it via `.show(ui, ..)`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ensure_capture_hid();
        // While waiting for a button press, keep repainting so we notice it promptly.
        if self.capturing.is_some() {
            ui.ctx().request_repaint();
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ZMouse mappings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() {
                        self.save();
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.scroll.jitter_filter, "Filter scroll-wheel jitter")
                    .on_hover_text(
                        "Drops spurious opposite-direction ticks from a worn wheel encoder. \
                         Trackpad scrolling is unaffected.",
                    );
                if self.scroll.jitter_filter {
                    ui.label("guard:");
                    ui.add(
                        egui::DragValue::new(&mut self.scroll.reversal_guard_ms)
                            .range(10..=500)
                            .suffix(" ms"),
                    )
                    .on_hover_text(
                        "Opposite ticks within this window of scrolling are treated as glitches. \
                         Raise it if jumps still get through; lower it if quick reversals feel sticky.",
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.scroll.boost_weak_ticks, "Boost weak scroll ticks")
                    .on_hover_text(
                        "Floors a slow wheel tick to a full detent so pixel-precise apps don't \
                         ignore it (a worn encoder can emit a tick that moves only 1 pixel). \
                         Trackpad scrolling is unaffected.",
                    );
                if self.scroll.boost_weak_ticks {
                    ui.label("floor:");
                    ui.add(
                        egui::DragValue::new(&mut self.scroll.min_tick_pixels)
                            .range(1..=60)
                            .suffix(" px/line"),
                    )
                    .on_hover_text(
                        "Minimum pixels a single tick moves. ~10 = one normal detent. \
                         Raise it if slow scrolls still feel weak.",
                    );
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });

        egui::Panel::left("devices")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("left_scroll")
                    .auto_shrink([false; 2])
                    .show(ui, |ui| self.left_contents(ui));
            });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("central_scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| self.central_contents(ui));
        });
    }
}

impl EditorApp {
    fn left_contents(&mut self, ui: &mut egui::Ui) {
        ui.heading("Devices");
        ui.separator();
        for i in 0..self.devices.len() {
            let d = &self.devices[i];
            let dot = if self.is_connected(d) { "●" } else { "○" };
            let label = format!(
                "{dot} {}",
                if d.name.trim().is_empty() {
                    d.ident_desc()
                } else {
                    d.name.clone()
                }
            );
            if ui
                .selectable_label(self.selected == Some(i), label)
                .clicked()
            {
                self.selected = Some(i);
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Add a connected device:");
            // The connected list is a snapshot from launch; rescan to pick up a mouse that was
            // just (re)connected or to re-offer one whose row you removed.
            if ui.button("⟳ Rescan").clicked() {
                self.connected = hid::list_mice();
            }
        });
        if self.connected.is_empty() {
            ui.weak("(no mice detected — connect one and click Rescan)");
        }
        // Every identity key already in the model (primary + grouped), so we don't offer to add a
        // device we already know under some identity.
        let mut existing_keys: Vec<u64> = Vec::new();
        for d in &self.devices {
            if let Some(k) = config::identity_key(d.registry_id, d.vendor_id, d.product_id) {
                existing_keys.push(k);
            }
            for id in &d.also {
                if let Some(k) = id.device_key() {
                    existing_keys.push(k);
                }
            }
        }
        // Connected devices we don't already have (matched by their stable identity key).
        #[derive(Clone)]
        struct Addable {
            id: DeviceId,
            name: String,
        }
        let addable: Vec<Addable> = self
            .connected
            .iter()
            .filter_map(|c| {
                // Prefer the stable USB identity; only fall back to registry id when absent.
                let id = if c.vendor_id.is_some() && c.product_id.is_some() {
                    DeviceId {
                        registry_id: None,
                        vendor_id: c.vendor_id,
                        product_id: c.product_id,
                    }
                } else {
                    DeviceId {
                        registry_id: c.registry_entry_id,
                        vendor_id: None,
                        product_id: None,
                    }
                };
                let key = id.device_key()?;
                if existing_keys.contains(&key) {
                    return None;
                }
                let name = c.name.clone().unwrap_or_else(|| format!("({})", id.desc()));
                Some(Addable { id, name })
            })
            .collect();
        if addable.is_empty() {
            ui.weak("(all connected devices already listed)");
        }
        // Name of the currently-selected device, for the "merge into…" label.
        let merge_target: Option<(usize, String)> = self.selected.and_then(|i| {
            self.devices.get(i).map(|d| {
                let n = if d.name.trim().is_empty() {
                    d.ident_desc()
                } else {
                    d.name.clone()
                };
                (i, n)
            })
        });
        for a in addable {
            ui.horizontal(|ui| {
                if ui.button(format!("+ {}", a.name)).clicked() {
                    self.devices.push(DeviceRow {
                        registry_id: a.id.registry_id,
                        vendor_id: a.id.vendor_id,
                        product_id: a.id.product_id,
                        also: Vec::new(),
                        name: if a.name.starts_with('(') {
                            String::new()
                        } else {
                            a.name.clone()
                        },
                        mappings: Vec::new(),
                        keys: Vec::new(),
                    });
                    self.selected = Some(self.devices.len() - 1);
                }
                // Merge this connected identity into the selected device as an extra identity.
                if let Some((sel, ref target_name)) = merge_target
                    && ui
                        .small_button("⊕ merge")
                        .on_hover_text(format!(
                            "Treat this as the same physical device as \"{target_name}\" \
                             (shares its mappings)."
                        ))
                        .clicked()
                {
                    self.devices[sel].also.push(a.id.clone());
                }
            });
        }
    }

    fn central_contents(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.selected else {
            ui.label("Select or add a device on the left.");
            return;
        };
        if sel >= self.devices.len() {
            self.selected = None;
            return;
        }

        // Apply a pending captured button press to the row that requested it.
        if let Some(row) = self.capturing {
            if !self.capture_armed {
                // First frame after "Capture": swallow the left-click that started it, then arm.
                LAST_PRESS.with(|p| *p.borrow_mut() = None);
                self.capture_armed = true;
            } else if let Some(press) = LAST_PRESS.with(|p| p.borrow_mut().take()) {
                let dev = &self.devices[sel];
                // Does the press match any of the device's identities (primary or grouped)?
                let press_matches = |id: &DeviceId| match (id.vendor_id, id.product_id) {
                    (Some(v), Some(p)) => press.vendor_id == Some(v) && press.product_id == Some(p),
                    _ => id.registry_id == Some(press.registry),
                };
                let same_device = dev.device_key_is_empty()
                    || press_matches(&dev.primary_id())
                    || dev.also.iter().any(press_matches);
                let button = press.button;
                let name = match button {
                    0 => " (left click)",
                    1 => " (right click)",
                    2 => " (middle click)",
                    _ => "",
                };
                let wrong_device = !(press.registry == 0 || same_device);
                if button == config::PROTECTED_BUTTON {
                    // Detected the left button, but it's protected — don't assign it.
                    self.capture_note =
                        "That's your left (primary) click — it's protected and can't be remapped."
                            .to_string();
                } else {
                    if let Some(m) = self.devices[sel].mappings.get_mut(row) {
                        m.button = button;
                    }
                    self.capture_note = if wrong_device {
                        format!(
                            "Captured button {button}{name} — note: that press came from a different device, not this one."
                        )
                    } else {
                        format!("Captured button {button}{name}.")
                    };
                }
                self.capturing = None;
            }
        }

        let connected = self.is_connected(&self.devices[sel]);
        // Per-identity connection dots: index 0 = primary, then each `also` in order.
        let ids_connected: Vec<bool> = std::iter::once(self.devices[sel].primary_id())
            .chain(self.devices[sel].also.iter().cloned())
            .map(|id| self.identity_connected(&id))
            .collect();
        let capturing_row = self.capturing;
        let mut start_capture: Option<usize> = None;
        let mut remove_device = false;
        let mut remove_also: Option<usize> = None;

        {
            let dev = &mut self.devices[sel];

            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut dev.name);
                ui.label(if connected {
                    "● connected"
                } else {
                    "○ not connected"
                });
            });

            // Identities: the same physical mouse can present several (dongle vs Bluetooth, or
            // pointer + keyboard interfaces). All grouped identities share the mappings below.
            ui.add_space(4.0);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("Identities");
                    ui.weak("(all share this device's mappings)");
                });
                let primary_dot = if *ids_connected.first().unwrap_or(&false) {
                    "●"
                } else {
                    "○"
                };
                ui.label(format!("{primary_dot} {} — primary", dev.ident_desc()));
                for (i, id) in dev.also.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let dot = if *ids_connected.get(i + 1).unwrap_or(&false) {
                            "●"
                        } else {
                            "○"
                        };
                        ui.label(format!("{dot} {}", id.desc()));
                        if ui.small_button("Remove").clicked() {
                            remove_also = Some(i);
                        }
                    });
                }
                ui.weak(
                    "To add another: connect the device the other way (dongle/Bluetooth), pick it \
                     in the left list, and click \"⊕ merge\".",
                );
            });

            ui.separator();
            ui.label(
                "Button numbers: 3 = back, 4 = forward, 5+ = extra. Use `zmouse probe` to find them.",
            );
            ui.add_space(6.0);

            let mut remove: Option<usize> = None;
            egui::ScrollArea::vertical()
                .id_salt("buttons")
                .max_height(220.0)
                .show(ui, |ui| {
                    for (i, m) in dev.mappings.iter_mut().enumerate() {
                        ui.push_id(i, |ui| {
                            ui.group(|ui| {
                                // Wrapping row so every control (incl. Remove) stays visible when
                                // the panel is narrow, instead of the right-aligned button clipping.
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Button");
                                    // Source starts at 1: button 0 (left click) is protected.
                                    ui.add(egui::DragValue::new(&mut m.button).range(1..=31));
                                    if ui
                                        .button(if capturing_row == Some(i) {
                                            "press a button…"
                                        } else {
                                            "Capture"
                                        })
                                        .clicked()
                                    {
                                        start_capture = Some(i);
                                    }

                                    egui::ComboBox::from_id_salt("kind")
                                        .selected_text(match m.kind {
                                            ActionKind::Keystroke => "Keystroke",
                                            ActionKind::Button => "Mouse button",
                                            ActionKind::Disabled => "Disabled",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut m.kind,
                                                ActionKind::Keystroke,
                                                "Keystroke",
                                            );
                                            ui.selectable_value(
                                                &mut m.kind,
                                                ActionKind::Button,
                                                "Mouse button",
                                            );
                                            ui.selectable_value(
                                                &mut m.kind,
                                                ActionKind::Disabled,
                                                "Disabled",
                                            );
                                        });

                                    ui.add_space(8.0);
                                    if ui.button("Remove").clicked() {
                                        remove = Some(i);
                                    }
                                });

                                match m.kind {
                                    ActionKind::Keystroke => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("Key:");
                                            ui.add(
                                                egui::TextEdit::singleline(&mut m.key)
                                                    .desired_width(60.0)
                                                    .hint_text("c"),
                                            );
                                            ui.checkbox(&mut m.mods.cmd, "Cmd");
                                            ui.checkbox(&mut m.mods.shift, "Shift");
                                            ui.checkbox(&mut m.mods.opt, "Opt");
                                            ui.checkbox(&mut m.mods.ctrl, "Ctrl");
                                        });
                                    }
                                    ActionKind::Button => {
                                        ui.horizontal(|ui| {
                                            ui.label("Target mouse button:");
                                            ui.add(
                                                egui::DragValue::new(&mut m.target_button)
                                                    .range(0..=31),
                                            );
                                        });
                                    }
                                    ActionKind::Disabled => {
                                        ui.weak("Button press is swallowed.");
                                    }
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                });

            if let Some(i) = remove {
                dev.mappings.remove(i);
            }
            ui.add_space(4.0);
            if ui.button("+ Add mapping").clicked() {
                dev.mappings.push(MappingRow {
                    button: 3,
                    kind: ActionKind::Disabled,
                    key: String::new(),
                    mods: ModSet::default(),
                    target_button: 2,
                });
            }

            // Keystroke-emitting buttons (MMO thumb grid, etc.). These arrive as keystrokes on the
            // mouse's keyboard-interface registry_id, so they're remapped per-device by source key.
            ui.add_space(8.0);
            ui.separator();
            ui.label(
                "Keystroke buttons (e.g. MMO thumb grid). \"Source key\" is what the button types by \
                 default — find it with `zmouse probe`. Only keys from THIS device are remapped.",
            );
            ui.add_space(6.0);

            let mut remove_key: Option<usize> = None;
            egui::ScrollArea::vertical()
                .id_salt("keys")
                .max_height(220.0)
                .show(ui, |ui| {
                    for (i, k) in dev.keys.iter_mut().enumerate() {
                        ui.push_id(("key", i), |ui| {
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Source key");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut k.source)
                                            .desired_width(48.0)
                                            .hint_text("7"),
                                    );
                                    egui::ComboBox::from_id_salt("kkind")
                                        .selected_text(match k.kind {
                                            ActionKind::Keystroke => "Keystroke",
                                            ActionKind::Button => "Mouse button",
                                            ActionKind::Disabled => "Disabled",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut k.kind,
                                                ActionKind::Keystroke,
                                                "Keystroke",
                                            );
                                            ui.selectable_value(
                                                &mut k.kind,
                                                ActionKind::Button,
                                                "Mouse button",
                                            );
                                            ui.selectable_value(
                                                &mut k.kind,
                                                ActionKind::Disabled,
                                                "Disabled",
                                            );
                                        });
                                    ui.add_space(8.0);
                                    if ui.button("Remove").clicked() {
                                        remove_key = Some(i);
                                    }
                                });
                                match k.kind {
                                    ActionKind::Keystroke => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("Key:");
                                            ui.add(
                                                egui::TextEdit::singleline(&mut k.key)
                                                    .desired_width(60.0)
                                                    .hint_text("c"),
                                            );
                                            ui.checkbox(&mut k.mods.cmd, "Cmd");
                                            ui.checkbox(&mut k.mods.shift, "Shift");
                                            ui.checkbox(&mut k.mods.opt, "Opt");
                                            ui.checkbox(&mut k.mods.ctrl, "Ctrl");
                                        });
                                    }
                                    ActionKind::Button => {
                                        ui.horizontal(|ui| {
                                            ui.label("Target mouse button:");
                                            ui.add(
                                                egui::DragValue::new(&mut k.target_button)
                                                    .range(0..=31),
                                            );
                                        });
                                    }
                                    ActionKind::Disabled => {
                                        ui.weak("Key press is swallowed.");
                                    }
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
            if let Some(i) = remove_key {
                dev.keys.remove(i);
            }
            ui.add_space(4.0);
            if ui.button("+ Add key mapping").clicked() {
                dev.keys.push(KeyRow {
                    source: String::new(),
                    kind: ActionKind::Disabled,
                    key: String::new(),
                    mods: ModSet::default(),
                    target_button: 2,
                });
            }

            ui.add_space(8.0);
            ui.separator();
            if ui.button("Remove this device").clicked() {
                remove_device = true;
            }

            if !self.capture_note.is_empty() {
                ui.add_space(4.0);
                ui.weak(&self.capture_note);
            }
        }

        if let Some(row) = start_capture {
            self.begin_capture(row);
        }
        if let Some(i) = remove_also {
            self.devices[sel].also.remove(i);
        }
        if remove_device {
            self.devices.remove(sel);
            self.selected = None;
            // Re-scan so a device that's still physically connected immediately reappears in the
            // "Add a connected device" list (its row is gone, so it's no longer filtered out).
            self.connected = hid::list_mice();
        }
    }
}
