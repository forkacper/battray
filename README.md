# battray

A lightweight Windows tray app that shows the battery level of wireless peripherals.

- Native Win32, written in Rust: no WebView, no runtime, a single small `.exe`.
- Talks to devices directly over HID, so vendor apps (G HUB, Steam) are not needed,
  but it also works while they are running.
- Polls in a background thread every 30 s to 15 min, otherwise idle.

## Supported devices

| Device | Connection | Notes |
|---|---|---|
| Logitech HID++ 2.0 devices, e.g. PRO X SUPERLIGHT 2 | Lightspeed / Unifying / Bolt receiver, USB cable | Features 0x1004, 0x1000 and 0x1001 |
| Sony DualSense, DualSense Edge | Bluetooth, USB | |

Keychron keyboards on the 2.4 GHz dongle are not supported yet: the stock firmware does
not expose the battery level to the host.

## Usage

Download `battray.exe` from the latest release (or the `battray` artifact of a CI run)
and start it. Left or right click the tray icon for the menu:

- **Device list** with the current level, charging state, and the last known level of
  devices that are asleep.
- **Tray display**: one icon with the lowest level, one icon per device, or only a
  selected device.
- **Refresh interval**, **low battery alerts** and **start with Windows**.
- **Save diagnostics report**: lists every HID collection and what each provider found.
  Attach it when reporting a device that does not work.

Icon colours: green is fine, amber is getting low, red is below the alert threshold,
blue is charging, grey is unknown or last known.

Settings, the log and diagnostics live in `%APPDATA%\battray`.

## Adding a device

Each device family is a provider in `src/devices/`:

1. Create `src/devices/<vendor>.rs` with a type implementing `Provider`.
   `poll` receives the HID collections enumerated for this poll; filter them by
   vendor ID, product ID and usage page, open the matching ones with `hid::Device`
   and push a `Reading` for each device.
2. Register it in `providers()` in `src/devices/mod.rs`.

`hid::Device` covers what most protocols need: output reports, input reports with a
timeout, feature reports, and `hid::read_any` for protocols that answer on several
collections.

## Building

CI builds on every push and attaches `battray.exe` to a GitHub release for `v*` tags.

Locally on Windows:

```sh
cargo build --release
```

From another OS you can type-check with `cargo check --target x86_64-pc-windows-msvc`.
