//! TOML configuration: per-device button and keystroke mappings, keyed by a device's stable
//! vendor/product id (with a registry-id fallback).
//!
//! Example (default path on macOS: `~/Library/Application Support/zmouse/config.toml`):
//!
//! ```toml
//! [[device]]
//! # Preferred: match by USB vendor/product id — stable across reconnects, reboots, and whether
//! # the mouse is on its wireless dongle or Bluetooth. `registry_id` still works as a fallback.
//! vendor_id = 0x1532
//! product_id = 0x00b7
//! name = "ROG CHAKRAM CORE"   # comment only; matching is by vendor/product id
//!
//! # The SAME physical mouse can present different identities (e.g. on its wireless dongle vs
//! # Bluetooth, or as separate pointer + keyboard interfaces). List the extras under `also` and
//! # they all share this device's mappings below:
//! [[device.also]]
//! vendor_id = 0x1532
//! product_id = 0x00aa          # e.g. the same mouse on its 2.4 GHz dongle
//! [[device.also]]
//! registry_id = 4295298668     # or a fallback registry id for an interface with no vendor/product
//!
//! [[device.mapping]]
//! button = 3                                              # CGEvent button (back)
//! action = { type = "keystroke", key = "c", mods = ["cmd"] }
//!
//! [[device.mapping]]
//! button = 4                                              # forward
//! action = { type = "button", button = 2 }               # -> middle click
//!
//! [[device.mapping]]
//! button = 5
//! action = { type = "disabled" }
//!
//! # A button that emits a keystroke (e.g. an MMO side button that types "7") is remapped by
//! # its source key under [[device.key]] on that device's (keyboard-interface) registry_id:
//! [[device.key]]
//! key = "7"                                               # the key the button emits
//! action = { type = "keystroke", key = "v", mods = ["cmd"] }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub scroll: ScrollConfig,
    #[serde(default)]
    pub device: Vec<DeviceConfig>,
}

/// Global scroll-wheel cleanup settings (targets worn-encoder "jump" glitches).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScrollConfig {
    /// Drop spurious opposite-direction ticks on discrete mouse wheels (trackpad untouched).
    #[serde(default)]
    pub jitter_filter: bool,
    /// An opposite-direction tick within this many ms of scrolling is treated as a glitch.
    #[serde(default = "default_reversal_guard_ms")]
    pub reversal_guard_ms: u64,
    /// Floor a discrete wheel tick's pixel delta so slow ticks aren't ignored by pixel-precise
    /// apps. A worn encoder can emit a `line=1` tick that only moves 1 pixel (a tenth of a normal
    /// detent); modern Mac apps scroll by pixels and effectively drop it. Trackpads untouched.
    #[serde(default)]
    pub boost_weak_ticks: bool,
    /// The minimum pixels a single discrete tick should move (per line). ~10 = one normal detent.
    #[serde(default = "default_min_tick_pixels")]
    pub min_tick_pixels: i64,
}

fn default_reversal_guard_ms() -> u64 {
    80
}

fn default_min_tick_pixels() -> i64 {
    10
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            jitter_filter: false,
            reversal_guard_ms: default_reversal_guard_ms(),
            boost_weak_ticks: false,
            min_tick_pixels: default_min_tick_pixels(),
        }
    }
}

/// The left mouse button (CGEvent button 0). It's the user's primary, recovery-critical input, so
/// ZMouse refuses to remap it — losing left-click on your only pointer would lock you out. The
/// editor won't assign it and `resolve` drops any hand-edited button-0 mapping.
pub const PROTECTED_BUTTON: i64 = 0;

/// High bit marks a vendor/product composite key so it can never collide with a kernel registry
/// entry id (those are plane-encoded and realistically well under 2^40).
pub const VIDPID_TAG: u64 = 0x8000_0000_0000_0000;

/// Pack a USB vendor id + product id into one stable 64-bit lookup key.
pub fn vidpid_key(vendor_id: i64, product_id: i64) -> u64 {
    VIDPID_TAG | (((vendor_id as u64) & 0xFFFF) << 16) | ((product_id as u64) & 0xFFFF)
}

/// One hardware identity: a vendor/product pair (preferred) or a registry id (fallback).
/// The same physical mouse can present several — e.g. one over Bluetooth and a different one on
/// its wireless dongle (which also splits into pointer + keyboard interfaces). Listing them under
/// a single device's `also` makes them all share that device's mappings.
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct DeviceId {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<i64>,
}

/// The stable lookup key for an identity: its vendor/product composite if both are set, else its
/// registry id. `None` if neither is present.
pub fn identity_key(
    registry_id: Option<u64>,
    vendor_id: Option<i64>,
    product_id: Option<i64>,
) -> Option<u64> {
    match (vendor_id, product_id) {
        (Some(v), Some(p)) => Some(vidpid_key(v, p)),
        _ => registry_id,
    }
}

/// Human-readable form of an identity, for logs and the editor.
pub fn identity_desc(
    registry_id: Option<u64>,
    vendor_id: Option<i64>,
    product_id: Option<i64>,
) -> String {
    match (vendor_id, product_id, registry_id) {
        (Some(v), Some(p), _) => format!("vendor 0x{v:04x} product 0x{p:04x}"),
        (_, _, Some(r)) => format!("registry_id {r}"),
        _ => "<no identity>".into(),
    }
}

impl DeviceId {
    pub fn device_key(&self) -> Option<u64> {
        identity_key(self.registry_id, self.vendor_id, self.product_id)
    }
    pub fn desc(&self) -> String {
        identity_desc(self.registry_id, self.vendor_id, self.product_id)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeviceConfig {
    /// Kernel registry entry id — a fallback identity. Ephemeral: it changes when the device
    /// reconnects or switches transport (dongle vs Bluetooth), so prefer vendor/product id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<u64>,
    /// USB vendor id (e.g. 0x1532 = Razer). With `product_id`, this is the stable match key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<i64>,
    /// USB product id. Paired with `vendor_id` to identify the device across reconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<i64>,
    /// Additional identities for the *same physical device* (e.g. its dongle vs Bluetooth ids, or a
    /// mouse's separate pointer + keyboard interfaces). All share this device's mappings/keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub mapping: Vec<Mapping>,
    /// Keystroke-triggered mappings, for buttons that emit a keystroke rather than a mouse button
    /// (e.g. an MMO mouse's 12 thumb buttons, which appear on a separate HID keyboard interface).
    /// Matched per-device by `registry_id`, so a mouse's key remaps never touch a real keyboard.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<KeyMapping>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Mapping {
    /// CGEvent button number (0=left,1=right,2=middle,3=back,4=forward,…).
    pub button: i64,
    pub action: Action,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct KeyMapping {
    /// Source key name the device emits, e.g. "1", "7", "-", "=" (see `keymap::key_code`).
    pub key: String,
    pub action: Action,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Send a keystroke, optionally with modifiers.
    Keystroke {
        key: String,
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Remap to a different mouse button number.
    Button { button: i64 },
    /// Swallow the button entirely.
    Disabled,
}

impl DeviceConfig {
    /// This device's primary lookup key (from its top-level identity fields).
    pub fn device_key(&self) -> Option<u64> {
        identity_key(self.registry_id, self.vendor_id, self.product_id)
    }

    /// Every lookup key this device answers to: its primary identity plus each `also` identity.
    /// The same mappings are shared across all of them.
    pub fn device_keys(&self) -> Vec<u64> {
        let mut keys = Vec::new();
        if let Some(k) = self.device_key() {
            keys.push(k);
        }
        for id in &self.also {
            if let Some(k) = id.device_key() {
                keys.push(k);
            }
        }
        keys
    }

    /// Human-readable identity for log/validation messages (primary + any grouped identities).
    pub fn ident_desc(&self) -> String {
        let primary = identity_desc(self.registry_id, self.vendor_id, self.product_id);
        if self.also.is_empty() {
            primary
        } else {
            let extra = self
                .also
                .iter()
                .map(|id| id.desc())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{primary} (+ {extra})")
        }
    }
}

impl Config {
    /// Flatten into a fast lookup: (device_key, button) -> action. A grouped device contributes
    /// the same actions under every one of its identities.
    pub fn resolve(&self) -> HashMap<(u64, i64), Action> {
        let mut map = HashMap::new();
        for dev in &self.device {
            for key in dev.device_keys() {
                for m in &dev.mapping {
                    // The left (primary) button is protected: never remap it, even if a config
                    // hand-edit tries to. It's your recovery input — see PROTECTED_BUTTON.
                    if m.button == PROTECTED_BUTTON {
                        continue;
                    }
                    map.insert((key, m.button), m.action.clone());
                }
            }
        }
        map
    }

    /// Flatten keystroke triggers into a lookup: (device_key, macOS keycode) -> action.
    /// Unknown source key names are skipped (they're surfaced by `validate`).
    pub fn resolve_keys(&self) -> HashMap<(u64, i64), Action> {
        let mut map = HashMap::new();
        for dev in &self.device {
            for key in dev.device_keys() {
                for k in &dev.key {
                    if let Some(code) = crate::keymap::key_code(&k.key) {
                        map.insert((key, code as i64), k.action.clone());
                    }
                }
            }
        }
        map
    }

    /// Validate that every keystroke action references a known key/modifier.
    /// Returns a list of human-readable problems (empty = OK).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for dev in &self.device {
            let id = dev.ident_desc();
            if dev.device_keys().is_empty() {
                problems.push(format!(
                    "device '{}' has no identity: set vendor_id+product_id or registry_id",
                    dev.name.as_deref().unwrap_or("<unnamed>")
                ));
            }
            for extra in &dev.also {
                if extra.device_key().is_none() {
                    problems.push(format!(
                        "device {} has an `also` entry with no identity (needs vendor_id+product_id or registry_id)",
                        id
                    ));
                }
            }
            for m in &dev.mapping {
                if m.button == PROTECTED_BUTTON {
                    problems.push(format!(
                        "device {} button {}: the left (primary) button is protected and won't be remapped",
                        id, m.button
                    ));
                }
                if let Action::Keystroke { key, mods } = &m.action {
                    if crate::keymap::key_code(key).is_none() {
                        problems.push(format!(
                            "device {} button {}: unknown key '{}'",
                            id, m.button, key
                        ));
                    }
                    for md in mods {
                        if crate::keymap::modifier_flag(md).is_none() {
                            problems.push(format!(
                                "device {} button {}: unknown modifier '{}'",
                                id, m.button, md
                            ));
                        }
                    }
                }
            }
            for k in &dev.key {
                if crate::keymap::key_code(&k.key).is_none() {
                    problems.push(format!("device {} key '{}': unknown source key", id, k.key));
                }
                if let Action::Keystroke { key, mods } = &k.action {
                    if crate::keymap::key_code(key).is_none() {
                        problems.push(format!(
                            "device {} key '{}': unknown target key '{}'",
                            id, k.key, key
                        ));
                    }
                    for md in mods {
                        if crate::keymap::modifier_flag(md).is_none() {
                            problems.push(format!(
                                "device {} key '{}': unknown modifier '{}'",
                                id, k.key, md
                            ));
                        }
                    }
                }
            }
        }
        problems
    }
}

/// Default config location (macOS `dirs::config_dir()`):
/// `~/Library/Application Support/zmouse/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("zmouse").join("config.toml"))
}

/// Load a config from an explicit path, or the default location.
pub fn load(path: Option<&Path>) -> Result<Config, String> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => default_path().ok_or("could not determine config directory")?,
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("parse error in {}: {e}", path.display()))
}

/// Serialize a config to TOML and write it (creating parent dirs). Used by the GUI editor.
pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    let text = toml::to_string_pretty(config).map_err(|e| format!("serialize error: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_round_trip() {
        let cfg = Config {
            scroll: ScrollConfig {
                jitter_filter: true,
                reversal_guard_ms: 90,
                boost_weak_ticks: true,
                min_tick_pixels: 12,
            },
            device: vec![DeviceConfig {
                registry_id: Some(42),
                vendor_id: None,
                product_id: None,
                also: vec![],
                name: Some("Test Mouse".into()),
                mapping: vec![
                    Mapping {
                        button: 3,
                        action: Action::Keystroke {
                            key: "c".into(),
                            mods: vec!["cmd".into()],
                        },
                    },
                    Mapping {
                        button: 4,
                        action: Action::Button { button: 2 },
                    },
                    Mapping {
                        button: 5,
                        action: Action::Disabled,
                    },
                ],
                key: vec![KeyMapping {
                    key: "7".into(),
                    action: Action::Keystroke {
                        key: "v".into(),
                        mods: vec!["cmd".into()],
                    },
                }],
            }],
        };
        let dir = std::env::temp_dir().join("zmouse-test");
        let path = dir.join("round_trip.toml");
        save(&path, &cfg).expect("save");
        let loaded = load(Some(&path)).expect("load");

        assert!(loaded.scroll.jitter_filter);
        assert_eq!(loaded.scroll.reversal_guard_ms, 90);
        assert!(loaded.scroll.boost_weak_ticks);
        assert_eq!(loaded.scroll.min_tick_pixels, 12);
        assert_eq!(loaded.device.len(), 1);
        let d = &loaded.device[0];
        assert_eq!(d.registry_id, Some(42));
        assert_eq!(d.name.as_deref(), Some("Test Mouse"));
        assert_eq!(d.mapping.len(), 3);
        // Tagged enum survives the round trip.
        assert!(matches!(d.mapping[2].action, Action::Disabled));
        assert!(
            matches!(&d.mapping[0].action, Action::Keystroke { key, mods }
            if key == "c" && mods == &["cmd".to_string()])
        );
        // Keystroke-triggered mapping survives too, and resolves to the right keycode.
        assert_eq!(d.key.len(), 1);
        assert_eq!(d.key[0].key, "7");
        let keys = loaded.resolve_keys();
        // "7" -> keycode 26; action is Cmd+V.
        assert!(matches!(keys.get(&(42, 26)), Some(Action::Keystroke { key, .. }) if key == "v"));
    }

    #[test]
    fn vidpid_device_resolves_under_composite_key() {
        let cfg = Config {
            scroll: ScrollConfig::default(),
            device: vec![DeviceConfig {
                registry_id: None,
                vendor_id: Some(0x1532),
                product_id: Some(0x00b7),
                also: vec![],
                name: Some("Wireless Mouse".into()),
                mapping: vec![Mapping {
                    button: 3,
                    action: Action::Button { button: 2 },
                }],
                key: vec![],
            }],
        };
        assert!(cfg.validate().is_empty());
        let key = vidpid_key(0x1532, 0x00b7);
        // The vendor/product key is tagged above the registry-id range, so it can't collide.
        assert!(key & VIDPID_TAG != 0);
        assert!(matches!(
            cfg.resolve().get(&(key, 3)),
            Some(Action::Button { button: 2 })
        ));
        // A device with no identity at all is flagged.
        let orphan = Config {
            scroll: ScrollConfig::default(),
            device: vec![DeviceConfig {
                registry_id: None,
                vendor_id: None,
                product_id: None,
                also: vec![],
                name: None,
                mapping: vec![],
                key: vec![],
            }],
        };
        assert!(!orphan.validate().is_empty());
    }

    #[test]
    fn grouped_device_shares_mappings_across_identities() {
        // One physical mouse, three identities (Bluetooth vid/pid, dongle pointer, dongle keyboard).
        let cfg = Config {
            scroll: ScrollConfig::default(),
            device: vec![DeviceConfig {
                registry_id: None,
                vendor_id: Some(0x068e), // Bluetooth
                product_id: Some(0x00b5),
                also: vec![
                    DeviceId {
                        vendor_id: Some(0x1532), // dongle pointer
                        product_id: Some(0x00aa),
                        ..Default::default()
                    },
                    DeviceId {
                        registry_id: Some(4295298668), // dongle keyboard (fallback id)
                        ..Default::default()
                    },
                ],
                name: Some("Naga V2 HS".into()),
                mapping: vec![Mapping {
                    button: 3,
                    action: Action::Button { button: 2 },
                }],
                key: vec![KeyMapping {
                    key: "7".into(),
                    action: Action::Keystroke {
                        key: "v".into(),
                        mods: vec!["cmd".into()],
                    },
                }],
            }],
        };
        assert!(cfg.validate().is_empty());
        let buttons = cfg.resolve();
        // The button mapping is reachable under every identity.
        assert!(buttons.contains_key(&(vidpid_key(0x068e, 0x00b5), 3)));
        assert!(buttons.contains_key(&(vidpid_key(0x1532, 0x00aa), 3)));
        assert!(buttons.contains_key(&(4295298668, 3)));
        // The key mapping ("7" -> keycode 26) too.
        let keys = cfg.resolve_keys();
        assert!(keys.contains_key(&(vidpid_key(0x068e, 0x00b5), 26)));
        assert!(keys.contains_key(&(4295298668, 26)));
    }
}
