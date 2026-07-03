# remouse — Milestone 1 findings

## Status

| Proof | Command | Verified |
|-------|---------|----------|
| #1 device enumeration | `remouse list` | ✅ works — see below |
| #2 button → keystroke remap | `remouse tap` | ✅ verified — back button → Cmd+C (paste confirmed) |
| #3 per-device correlation spike | `remouse probe` | ✅ works — correlation is reliable |

## Proof #1 — enumeration (verified)

`cargo run -- list` output on this machine:

```
[0] Apple Internal Keyboard / Trackpad
    vendor=0x0000 product=0x0000  serial=<none>
    locationID=165 registryEntryID=4294971452
[1] ROG CHAKRAM CORE
    vendor=0x0b05 product=0x1958  serial=<none>
    locationID=17969152 registryEntryID=4295289702
```

Findings:
- **`registryEntryID` is present and distinct per device** — this is our per-device key.
- Apple's internal trackpad reports `VendorID`/`ProductID` = 0 (property absent). Real USB/BT
  mice report proper IDs. Don't rely on VID/PID being present; `registryEntryID` is the anchor.
- `SerialNumber` is often absent — not a reliable identity source.

## Proof #2 — remap (manual)

1. **Grant Accessibility** to your terminal/IDE:
   System Settings → Privacy & Security → Accessibility → add & enable Terminal/iTerm/VS Code.
2. `cargo run -- tap`
3. Select some text, press the mouse **back button** (button 3) → expect **Cmd+C** (copy).
   The original back-button action should be suppressed.
4. Without permission, confirm the guidance message prints instead of silent no-op.

Record result here: _pending_

## Proof #3 — per-device correlation spike (manual — THIS GATES MILESTONE 2)

`cargo run -- probe` logs two interleaved streams for each button press:
- `[HID]` — carries `device=<registryEntryID>` (which physical mouse) + mach-abs timestamp.
- `[TAP]` — the CGEventTap view (button number) + mach-abs arrival time. **No device identity.**

Both timestamps are mach absolute time, so they're directly comparable.

**Question to answer by eyeballing the logs:** for a single physical click, do the `[HID]` and
`[TAP]` lines have timestamps close enough (and 1:1 ordered) to reliably attribute the tap event
to the HID device? Test with the ROG mouse vs. the trackpad.

- If **yes** → Milestone 2 can build per-device mapping on `CGEventTap` + a small correlation
  window keyed by timestamp, maintaining "last device seen per button" state.
- If **no** (races, dropped/merged events, ambiguous ordering) → per-device remapping must move
  down to IOKit HID-level interception (harder; may need a virtual HID device + entitlement).

### FINDING (verified on ROG Chakram Core) — YES, correlation is reliable

Sample run: every `[TAP]` event was immediately preceded by a `[HID]` event from the same
device (`4295289702`), 1:1 with no misses, HID always ~10k–50k mach-time units (sub-ms) *before*
the tap. Because both callbacks run on the same run-loop thread and HID is upstream, **by the
time the tap callback fires the matching HID callback has already run.**

**Decision: per-device remapping rides on `CGEventTap` + `IOHIDManager`.** No timestamp math and
no low-level IOKit interception / virtual device needed. Mechanism:

```
HID input-value callback:  last_device_for_button[button] = device_registry_id
CGEventTap callback:       device = last_device_for_button[button];  apply per-device mapping
```

Button numbering: **HID usage (page 0x09) = CGEvent button number + 1**
(HID is 1-indexed: 1=left,2=right,3=middle,4=back,5=forward; CGEvent is 0-indexed).
`value=1` = press (…MouseDown), `value=0` = release (…MouseUp).

## Milestone 2 — headless per-device mapping engine (built)

New modules: `config.rs` (TOML schema + loader/validator), `keymap.rs` (key/modifier tables),
`engine.rs` (device tracking + tap dispatch). New subcommand: `remouse run [config.toml]`.

- Config keyed by `registry_id`; actions: `keystroke` (key + mods), `button` (remap to another
  mouse button), `disabled`. Sample: `config.example.toml`. Default path:
  `~/.config/remouse/config.toml`.
- Mechanism is exactly the probe-validated design: `hid_value_callback` records
  `LAST_DEVICE_FOR_BUTTON[button] = registry_id` (thread_local), the tap closure reads it to
  resolve the device, then applies `mappings[(device, button)]`.
- Only "other" mouse buttons (3+) are tapped; left/right/middle left alone for safety.

Verified: `remouse run ./config.example.toml` loads, validates, prints the mapping table, and
starts the tap. **Live remap behavior still needs a manual click-test** (record result below).

Manual test result: ✅ verified on ROG Chakram Core — `remouse run ./config.example.toml`
gives back→Copy and forward→Paste. (Note: `tap` is the hardcoded back-only demo; the real
engine is `run`.) Probe also confirmed forward = CGEvent button 4 (HID button usage 5).

## `probe` is now a discovery tool (for new hardware)

`remouse probe` logs HID input across button (0x09) / keyboard (0x07) / consumer (0x0C) pages
with device identity, alongside the CGEventTap view of **both** mouse buttons and keystrokes.
Use it to learn how a new mouse's buttons present:
- `[TAP] OtherMouseDown` => a real mouse button (remappable by the engine today).
- `[TAP] KeyDown`        => the button emits a keystroke (engine does NOT intercept these yet).
- `[HID] page=… device=…` => which physical device/interface produced it.

### Known open item: MMO mice with many buttons

A 19-button MMO mouse is incoming. On many such mice (Razer Naga / Logitech G600 style) the
thumb-grid buttons are exposed as a **separate HID interface that emits keystrokes**, not
`OtherMouseDown` events — and the mouse may show up as **multiple devices** in `remouse list`.
If `probe` shows `[TAP] KeyDown` for those buttons, the engine needs a new interception path:
tap `KeyDown`/`KeyUp`, and remap keystroke->action per-device (same HID-correlation trick, keyed
on keyboard usage instead of button usage). Decide this once we see the probe output.

## Milestone 3 — native menu-bar app (built)

New module `menubar.rs` (objc2-app-kit `NSStatusItem`), new subcommand `remouse menu [config]`.
`engine.rs` refactored: `install()` puts the tap + HID manager on the *current* run loop and
returns a handle (no loop run); `run()` = install + `CFRunLoop::run_current` (headless);
`menubar::run()` = install + `NSApplication::run()`. Both share the one main-thread run loop —
single process, no daemon.

- Activation policy = Accessory (menu-bar agent; no Dock icon). Icon is the 🖱 emoji title.
- Menu: **Enabled** (checkbox toggling the tap via `CGEventTapEnable`), **Reload config**
  (hot-swaps the `Rc<RefCell<Mappings>>` from disk), a **Configured devices** section, **Quit**.
- Mappings are now `Rc<RefCell<..>>` so reload swaps them live without rebuilding the tap.
- Menu actions handled by a `define_class!` NSObject subclass (`RemouseMenuHandler`) whose ivars
  hold the shared mappings + config path + toggle item.

Verified: `remouse menu ./config.example.toml` launches, installs the tap, and stays alive.
**Visual/interaction test (icon + menu items) still needs a manual check.**

Manual menu test result: ✅ verified — 🖱 icon appears, remapping works, Enabled toggle
enables/disables the tap as expected, Quit exits cleanly.

Not yet done (future): bundle as a real `.app` (so Accessibility is granted to remouse itself,
not the terminal), launch-at-login, and a GUI config editor. Scroll-wheel + keystroke-emitting
buttons (for the MMO mouse) still pending too.

## Milestone 4 — GUI config editor (built)

New module `editor.rs` (egui/eframe **0.35**), new subcommand `remouse edit [config.toml]`, and
a menu item "Edit mappings…" that spawns the editor as a **separate process** (so eframe's winit
loop never fights the agent's NSApp/event-tap loop). Editor edits the TOML; agent applies via
"Reload config".

- Config types gained `Serialize` + `config::save()`; round-trip covered by a unit test
  (`cargo test`). Saved format uses `[device.mapping.action]` tables (valid, loads fine).
- Editor UI: device list (connected 🟢 / configured ⚪, add-connected buttons), per-device
  mapping rows (button number, action combo = keystroke/mouse-button/disabled, key + modifier
  checkboxes or target button), add/remove mapping, remove device, Save.
- **egui 0.35 API gotcha (post-training):** `App::ui(&mut self, ui: &mut Ui, frame)` (not
  `update`/Context); `TopBottomPanel`/`SidePanel` are gone — use `Panel::top()/left()`;
  panels `.show(ui, ..)` into the root Ui; `default_size` not `default_width`.

Not yet verified interactively: the editor window rendering + Save + the menu "Edit mappings…"
launch. (Launches without panic; needs a visual/click check.)

Manual editor test result: _pending_

### Live button capture (built)

Each mapping row has a "🎯 Capture" button: click it, press the physical mouse button, and the
editor fills in the number — no more guessing what "button 5" is. The editor opens its own
IOHIDManager on the eframe main run loop; the callback records the last extra-button press
(ignores left/right so the click that starts capture doesn't self-trigger). If the captured
press came from a *different* device than the one being edited, it says so. Both the agent and
editor can hold IOHIDManagers at once (HID input is broadcast, not exclusive).

Permission note: capture needs the editor's process to have Input-Monitoring/Accessibility. Via
`cargo run` the terminal's grant covers it; a bundled .app would request its own.

### Auto-reload + UI fixes (built)

- **Auto-reload:** the agent schedules an `NSTimer` (1s) that stats the config mtime and hot-swaps
  the mapping table when it changes — so editor **Save applies live**, no menu step. Manual
  "Reload config" kept as a force option. mtime stored in a `Cell` ivar on the handler.
- **Emoji fix:** egui's default font renders almost no emoji (they showed as blank boxes), which
  is why the per-row remove button (🗑) and the ⇧/⌥/⌃ modifier glyphs were invisible — only ⌘
  happened to render. Replaced with text: modifiers = "Cmd/Shift/Opt/Ctrl", remove = "Remove",
  connection dots = ●/○. **Lesson: don't rely on emoji glyphs in egui buttons/labels.**

Future: bundle as a real .app; consider bundling an emoji-capable font if we want icons.

## BUG FIXED: event-tap re-entrancy loop (locked cursor)

Symptom: a `button -> mouse button` remap locked the cursor to a fixed screen position.
Cause: the tap is at `CGEventTapLocation::HID`; the synthesized mouse event was **posted to HID
too**, so the tap saw its OWN injected event and re-processed it → infinite loop, each iteration
re-posting a mouse event at the click location (pegging the cursor). A `button N -> button N`
self-map made it fire instantly, but ANY button->button remap would loop.

Fix (engine.rs):
- Stamp every synthesized event with a marker in `EVENT_SOURCE_USER_DATA` (field 42),
  `REMOUSE_TAG = 0x5245_4D53`, and at the top of the tap callback **ignore events carrying that
  tag** (return `Keep`). Standard "don't reprocess your own injected events" technique.
- Also handle `TapDisabledByTimeout` / `TapDisabledByUserInput` by re-enabling the tap, so a tap
  the system kills (e.g. after a slow callback) self-heals instead of going dead.

Lesson: any time we `post()` a synthesized event to the same tap location we listen on, we MUST
tag-and-skip it. Applies to future scroll/keystroke synthesis too.

## Milestone 5 — .app bundle + launch-at-login (built)

- `scripts/bundle.sh` builds `dist/remouse.app`: release binary → `Contents/MacOS/remouse`,
  `Info.plist` with `LSUIElement` (menu-bar agent, no Dock icon), `PkgInfo`, then ad-hoc
  `codesign`. Rerun after code changes. `dist/` is gitignored.
- `main.rs`: bare `remouse` (and a `-psn_...` launch arg) now defaults to `menu`, so a
  double-clicked .app launches the agent instead of printing help+exiting. Added `help`.
- Menu gained **Launch at login** (objc2 `SMAppService::mainAppService().register/unregister`,
  status→checkmark). Only works from the bundled .app; via `cargo`/CLI it logs an error.

Caveats to remember:
- **Signing / permission persistence (SOLVED for local dev):** ad-hoc signing keys TCC on the
  content hash, so grants reset every rebuild. Fixed by signing with a **stable self-signed
  "Code Signing" cert** named `remouse-dev` (created in Keychain Access). `bundle.sh` auto-uses it
  if present (env `REMOUSE_SIGN_ID` overrides), else falls back to ad-hoc. The cert is untrusted
  (`CSSMERR_TP_NOT_TRUSTED`) — that's fine: `codesign` still signs with it, and the designated
  requirement becomes `identifier "com.jeffreywolf.remouse" and certificate leaf = H"<certhash>"`,
  which is **stable across rebuilds**, so TCC grants persist. (Untrusted only matters for
  Gatekeeper/distribution, which local use doesn't care about.) A real **Developer ID** cert
  ($99/yr) is only needed for notarized distribution.
- Bundled app needs BOTH Accessibility (event tap) and Input Monitoring (HID capture) granted in
  System Settings → Privacy & Security. Switching signing identity (e.g. ad-hoc → cert) is a
  one-time re-grant: remove the old entry, re-add `/Applications/remouse.app`.
- For launch-at-login + stable identity, copy to **/Applications** rather than running from `dist/`.
- No app icon yet (generic). Menu-bar agent so no Dock icon anyway.

## Scroll-wheel jitter filter (built)

Fixes a worn wheel encoder that throws spurious opposite-direction "jump" ticks (user's Chakram).
- `engine.rs`: `ScrollWheel` added to the tap mask; `is_scroll_glitch()` drops a scroll tick if
  it's opposite the recent scroll direction AND arrives within `reversal_guard_ms` (default 80).
  Gated on `SCROLL_WHEEL_EVENT_IS_CONTINUOUS == 0`, so **trackpad (continuous) scrolling is never
  touched** — no per-device correlation needed. State in `SCROLL_STATE` thread_local; settings in
  `SCROLL_FILTER`/`SCROLL_GUARD_MS` atomics set by `engine::apply_settings(&Config)` (called at
  startup + every reload).
- `config.rs`: new `[scroll]` table (`ScrollConfig { jitter_filter, reversal_guard_ms }`),
  default off. Editor preserves it and exposes a "Filter scroll-wheel jitter" checkbox + guard
  DragValue (top bar).
- Heuristic, tunable: raise guard if jumps still slip through; lower if quick manual reversals
  feel sticky. Can't perfectly catch *same-direction* at-rest jitter (would need lookahead); it
  targets the common opposite-tick glitch.
- **Menu tuning** (`menubar.rs`): "Filter scroll jitter" checkbox + "Jitter guard" submenu of ms
  presets (40/60/80/100/120/150). Each preset item carries its value in its NSMenuItem `tag`;
  one `setScrollGuard:` action reads `sender.tag()`. Menu actions load→mutate→save the config via
  `edit_scroll()` and call `engine::apply_settings`, so changes persist and apply live (and the
  editor's checkbox/DragValue stay in sync via auto-reload). Presets shown with a checkmark; a
  custom guard value set in the editor simply shows no checked preset.

## Architecture notes carried forward

- Two CF ecosystems in play, kept per-module: `hid.rs`/`probe.rs` use `objc2-io-kit` +
  `objc2-core-foundation`; `tap.rs` uses `core-graphics` + `core-foundation`. The run loop is
  global so both cooperate on one thread.
- `tap.rs` currently hardcodes button 3 → Cmd+C. The real mapping engine (Milestone 2) replaces
  this with a config-driven table.
