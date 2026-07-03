# ZMouse

A per-device mouse remapper for macOS, written in Rust. Remap mouse buttons — and the keystrokes
that MMO-style thumb buttons emit — **per physical device**, so a mapping on one mouse never affects
another (or your keyboard). It runs as a native menu-bar app with a small GUI editor.

Think SteerMouse, but focused, scriptable, and yours.

## Features

- **Per-device mappings** keyed by stable USB **vendor/product id** — survive reconnects, reboots,
  and switching a wireless mouse between its dongle and Bluetooth.
- **Device grouping**: one physical mouse can present several identities (dongle vs Bluetooth, or a
  split pointer + keyboard interface); group them so they share one set of mappings.
- **Button actions**: send a keystroke (with modifiers), remap to another mouse button, or disable a
  button entirely.
- **Keystroke-button remapping**: MMO thumb buttons that emit keys (1–9, 0, -, =) can be remapped
  per-device without touching a real keyboard.
- **Scroll-wheel cleanup** for worn encoders: a jitter filter (drops spurious reverse ticks) and a
  weak-tick boost (floors a slow tick to a full detent so pixel-precise apps stop ignoring it).
  Trackpads are never touched.
- **Native menu bar** (NSStatusItem) + an **egui** config editor. Edits apply live.
- **Safety**: the built-in Apple trackpad/keyboard is excluded from discovery — you can't
  accidentally remap your own primary input.

## Requirements

- macOS (Apple Silicon or Intel).
- Rust toolchain (`rustup`).
- Two privacy permissions on first launch: **Accessibility** and **Input Monitoring**
  (System Settings → Privacy & Security).

## Build & install

```sh
# Build and run from source
cargo run --release            # launches the menu-bar app

# Or build a proper .app bundle (menu-bar agent, self-signed; displays as "ZMouse")
scripts/bundle.sh              # -> dist/remouse.app
cp -R dist/remouse.app /Applications/
open /Applications/remouse.app
```

On first launch, grant **Accessibility** and **Input Monitoring** to the app. `scripts/bundle.sh`
signs with a stable self-signed identity so those grants persist across rebuilds.

## Usage

The GUI is the easy path: open the menu-bar app and choose **Edit mappings…** (or run
`remouse edit`). Select a connected device, add button/key mappings, and save — the running app
applies changes automatically.

CLI subcommands (the binary is currently named `remouse`):

| Command | What it does |
| --- | --- |
| `remouse` / `remouse menu` | Launch the menu-bar app (default) |
| `remouse edit [config.toml]` | Open the GUI mapping editor |
| `remouse run [config.toml]` | Apply mappings headless (no UI) |
| `remouse list` | List connected mice (with vendor/product ids) |
| `remouse probe` | Log HID + event-tap streams — find device ids and button numbers |
| `remouse scrolldbg` | Dump raw scroll-wheel delta fields (diagnose weak ticks) |

## Configuration

Config lives at `~/Library/Application Support/remouse/config.toml`. You normally don't edit it by
hand — the editor writes it — but see [`config.example.toml`](config.example.toml) for a fully
commented reference covering per-device mappings, `also` grouping, keystroke buttons, and scroll
settings. Use `remouse probe` to discover a device's ids and button numbers.

## How it works

An `IOHIDManager` input-value callback records *which device* produced each button/keystroke; a
`CGEventTap` then intercepts the event and applies that device's mapping. Both run on one run loop,
and synthesized events are tagged so they never re-enter the tap. See [`NOTES.md`](NOTES.md) for the
correlation mechanism and other macOS specifics.

## Status

Personal project, macOS-only. Works on the author's hardware; expect rough edges.
