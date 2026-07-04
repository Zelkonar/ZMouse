//! GUI config editor (egui / eframe), run as a separate process.
//!
//! Launched via `zmouse edit [config.toml]` (the menu-bar app spawns this as a child so the
//! two never fight over an event loop). It edits the same TOML the engine reads; the running
//! agent watches that file and auto-applies changes on save.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;

use eframe::egui;
use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_io_kit::{IOHIDManager, IOHIDValue, IOReturn, kIOHIDOptionsTypeNone};

use crate::config::{
    self, Action, Config, DeviceConfig, DeviceId, KeyMapping, Mapping, ScrollConfig,
};
use crate::hid::{self, MouseDevice};

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
    let (reg, vendor_id, product_id) =
        unsafe { hid::identity_from_sender(sender) }.unwrap_or((None, None, None));
    LAST_PRESS.with(|p| {
        *p.borrow_mut() = Some(CapturedPress {
            registry: reg.unwrap_or(0),
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

/// The action half of a mapping row: what to do when the trigger fires. Shared by button mappings
/// (`MappingRow`) and keystroke mappings (`KeyRow`), which differ only in their trigger.
#[derive(Clone)]
struct ActionForm {
    kind: ActionKind,
    key: String,
    mods: ModSet,
    target_button: i64,
}

impl ActionForm {
    fn disabled() -> Self {
        ActionForm {
            kind: ActionKind::Disabled,
            key: String::new(),
            mods: ModSet::default(),
            target_button: 2,
        }
    }

    fn from_action(action: &Action) -> Self {
        let mut f = ActionForm::disabled();
        match action {
            Action::Keystroke { key, mods } => {
                f.kind = ActionKind::Keystroke;
                f.key = key.clone();
                f.mods = ModSet::from_slice(mods);
            }
            Action::Button { button } => {
                f.kind = ActionKind::Button;
                f.target_button = *button;
            }
            Action::Disabled => {}
        }
        f
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

/// Editable form of one button mapping.
struct MappingRow {
    button: i64,
    action: ActionForm,
}

impl MappingRow {
    fn from_mapping(m: &Mapping) -> Self {
        MappingRow {
            button: m.button,
            action: ActionForm::from_action(&m.action),
        }
    }
}

/// Editable form of one keystroke-triggered mapping (a button that emits a key, e.g. MMO thumb
/// buttons). Same action shape as `MappingRow`, but triggered by a source key name.
struct KeyRow {
    source: String,
    action: ActionForm,
}

impl KeyRow {
    fn from_mapping(k: &KeyMapping) -> Self {
        KeyRow {
            source: k.key.clone(),
            action: ActionForm::from_action(&k.action),
        }
    }

    fn to_key_mapping(&self) -> KeyMapping {
        KeyMapping {
            key: self.source.clone(),
            action: self.action.to_action(),
        }
    }
}

/// The action-kind ComboBox (Keystroke / Mouse button / Disabled), shared by both row kinds.
fn action_kind_combo(ui: &mut egui::Ui, kind: &mut ActionKind, salt: &str) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(match kind {
            ActionKind::Keystroke => "Keystroke",
            ActionKind::Button => "Mouse button",
            ActionKind::Disabled => "Disabled",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(kind, ActionKind::Keystroke, "Keystroke");
            ui.selectable_value(kind, ActionKind::Button, "Mouse button");
            ui.selectable_value(kind, ActionKind::Disabled, "Disabled");
        });
}

/// The per-kind parameter widgets shown below a row header (key + mods / target button / nothing).
fn action_params_ui(ui: &mut egui::Ui, action: &mut ActionForm) {
    match action.kind {
        ActionKind::Keystroke => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Key:");
                ui.add(
                    egui::TextEdit::singleline(&mut action.key)
                        .desired_width(60.0)
                        .hint_text("c"),
                );
                ui.checkbox(&mut action.mods.cmd, "Cmd");
                ui.checkbox(&mut action.mods.shift, "Shift");
                ui.checkbox(&mut action.mods.opt, "Opt");
                ui.checkbox(&mut action.mods.ctrl, "Ctrl");
            });
        }
        ActionKind::Button => {
            ui.horizontal(|ui| {
                ui.label("Target mouse button:");
                ui.add(egui::DragValue::new(&mut action.target_button).range(0..=31));
            });
        }
        ActionKind::Disabled => {
            ui.weak("Press is swallowed.");
        }
    }
}

/// Editable form of one identity: a human-friendly label plus the id data. All of a device's
/// identities are equal — there is no primary/secondary.
#[derive(Clone, Default)]
struct IdentityRow {
    label: String,
    registry_id: Option<u64>,
    vendor_id: Option<i64>,
    product_id: Option<i64>,
}

impl IdentityRow {
    fn from_device_id(id: &DeviceId) -> Self {
        IdentityRow {
            label: id.label.clone().unwrap_or_default(),
            registry_id: id.registry_id,
            vendor_id: id.vendor_id,
            product_id: id.product_id,
        }
    }

    fn to_device_id(&self) -> DeviceId {
        DeviceId {
            label: (!self.label.trim().is_empty()).then(|| self.label.trim().to_string()),
            registry_id: self.registry_id,
            vendor_id: self.vendor_id,
            product_id: self.product_id,
        }
    }

    fn device_key(&self) -> Option<u64> {
        config::identity_key(self.registry_id, self.vendor_id, self.product_id)
    }

    fn desc(&self) -> String {
        config::identity_desc(self.registry_id, self.vendor_id, self.product_id)
    }

    /// Does this identity refer to the given connected device?
    fn matches(&self, c: &MouseDevice) -> bool {
        if let (Some(v), Some(p)) = (self.vendor_id, self.product_id) {
            c.vendor_id == Some(v) && c.product_id == Some(p)
        } else if let Some(r) = self.registry_id {
            c.registry_entry_id == Some(r)
        } else {
            false
        }
    }

    /// Auto label from the matching connected device's transport (e.g. "Bluetooth", "USB").
    fn auto_label(&self, connected: &[MouseDevice]) -> Option<String> {
        let c = connected.iter().find(|c| self.matches(c))?;
        hid::friendly_transport(c.transport.as_deref())
    }

    /// The label to show: the live transport label if the device is connected, else the last one we
    /// stored (auto-generated when it was connected). Empty string if we've never seen it connected.
    fn display_label(&self, connected: &[MouseDevice]) -> String {
        self.auto_label(connected)
            .unwrap_or_else(|| self.label.trim().to_string())
    }
}

/// Editable form of one device and its mappings.
struct DeviceRow {
    name: String,
    /// The device's identities — all equal (no primary/secondary), all sharing the mappings.
    identities: Vec<IdentityRow>,
    mappings: Vec<MappingRow>,
    keys: Vec<KeyRow>,
}

impl DeviceRow {
    /// Description of the device's identities, for list/header display when it has no name.
    fn ident_desc(&self) -> String {
        if self.identities.is_empty() {
            "no identity".into()
        } else {
            self.identities
                .iter()
                .map(|i| i.desc())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    /// True when no identity has usable id data.
    fn has_no_identity(&self) -> bool {
        self.identities.iter().all(|i| i.device_key().is_none())
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
                name: d.name.clone().unwrap_or_default(),
                identities: d
                    .identities()
                    .iter()
                    .map(|id| {
                        let mut row = IdentityRow::from_device_id(id);
                        // Auto-fill an empty label from the device's transport, if it's connected.
                        if row.label.trim().is_empty()
                            && let Some(l) = row.auto_label(&connected)
                        {
                            row.label = l;
                        }
                        row
                    })
                    .collect(),
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
                    name: if d.name.trim().is_empty() {
                        None
                    } else {
                        Some(d.name.clone())
                    },
                    identity: d.identities.iter().map(IdentityRow::to_device_id).collect(),
                    mapping: d
                        .mappings
                        .iter()
                        .map(|m| Mapping {
                            button: m.button,
                            action: m.action.to_action(),
                        })
                        .collect(),
                    key: d.keys.iter().map(KeyRow::to_key_mapping).collect(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    fn is_connected(&self, dev: &DeviceRow) -> bool {
        // Any of the device's identities being present counts as connected.
        dev.identities.iter().any(|id| self.identity_connected(id))
    }

    fn identity_connected(&self, id: &IdentityRow) -> bool {
        self.connected.iter().any(|c| id.matches(c))
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
        // Every identity key already in the model, so we don't offer to add a device we already
        // know under some identity.
        let mut existing_keys: Vec<u64> = Vec::new();
        for d in &self.devices {
            for id in &d.identities {
                if let Some(k) = id.device_key() {
                    existing_keys.push(k);
                }
            }
        }
        // Connected devices we don't already have (matched by their stable identity key).
        #[derive(Clone)]
        struct Addable {
            id: IdentityRow,
            name: String,
        }
        let addable: Vec<Addable> = self
            .connected
            .iter()
            .filter_map(|c| {
                // Prefer the stable USB identity; only fall back to registry id when absent.
                let mut id = if c.vendor_id.is_some() && c.product_id.is_some() {
                    IdentityRow {
                        vendor_id: c.vendor_id,
                        product_id: c.product_id,
                        ..Default::default()
                    }
                } else {
                    IdentityRow {
                        registry_id: c.registry_entry_id,
                        ..Default::default()
                    }
                };
                // Auto-label from how it's connected (Bluetooth / USB); persists in the config.
                id.label = hid::friendly_transport(c.transport.as_deref()).unwrap_or_default();
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
                        name: if a.name.starts_with('(') {
                            String::new()
                        } else {
                            a.name.clone()
                        },
                        identities: vec![a.id.clone()],
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
                    self.devices[sel].identities.push(a.id.clone());
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
                // Does the press match any of the device's identities?
                let press_matches = |id: &IdentityRow| match (id.vendor_id, id.product_id) {
                    (Some(v), Some(p)) => press.vendor_id == Some(v) && press.product_id == Some(p),
                    _ => id.registry_id == Some(press.registry),
                };
                let same_device = dev.has_no_identity() || dev.identities.iter().any(press_matches);
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
        // Per-identity connection dots + auto-generated labels, in identity order.
        let ids_connected: Vec<bool> = self.devices[sel]
            .identities
            .iter()
            .map(|id| self.identity_connected(id))
            .collect();
        let id_labels: Vec<String> = self.devices[sel]
            .identities
            .iter()
            .map(|id| id.display_label(&self.connected))
            .collect();
        let capturing_row = self.capturing;
        let mut start_capture: Option<usize> = None;
        let mut remove_device = false;
        let mut remove_identity: Option<usize> = None;

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
            // pointer + keyboard interfaces). They are all equal and share the mappings below.
            ui.add_space(4.0);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong("Identities");
                    if !dev.name.trim().is_empty() {
                        ui.weak(format!("· {}", dev.name.trim()));
                    }
                    ui.weak("(all equal — they share the mappings below)");
                });
                for (i, id) in dev.identities.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let dot = if *ids_connected.get(i).unwrap_or(&false) {
                            "●"
                        } else {
                            "○"
                        };
                        // Label is auto-generated from the transport (Bluetooth / USB); read-only.
                        let label = id_labels.get(i).map(String::as_str).unwrap_or("");
                        let text = if label.is_empty() {
                            id.desc()
                        } else {
                            format!("{label} — {}", id.desc())
                        };
                        ui.label(format!("{dot} {text}"));
                        if ui.small_button("Remove").clicked() {
                            remove_identity = Some(i);
                        }
                    });
                }
                ui.weak(
                    "Labels are auto-detected from how each is connected. Add another: connect the \
                     device the other way (dongle/Bluetooth), pick it in the left list, click \"⊕ merge\".",
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

                                    action_kind_combo(ui, &mut m.action.kind, "kind");

                                    ui.add_space(8.0);
                                    if ui.button("Remove").clicked() {
                                        remove = Some(i);
                                    }
                                });

                                action_params_ui(ui, &mut m.action);
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
                    action: ActionForm::disabled(),
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
                                    action_kind_combo(ui, &mut k.action.kind, "kkind");
                                    ui.add_space(8.0);
                                    if ui.button("Remove").clicked() {
                                        remove_key = Some(i);
                                    }
                                });
                                action_params_ui(ui, &mut k.action);
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
                    action: ActionForm::disabled(),
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
        if let Some(i) = remove_identity {
            self.devices[sel].identities.remove(i);
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
