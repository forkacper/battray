# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- DualShock 4 next to DualSense, over USB, Bluetooth and the Sony wireless adapter.
- Xbox controllers and other XInput gamepads.
- Any HID device that reports the standard Battery Strength usage.
- The Keychron firmware patch is built for every wireless Keychron keyboard on the
  shared wireless code (184 variants), released as `keychron-battray-firmware.zip`.
- Issue templates for device reports and bugs.

### Changed

- Keychron keyboards are remembered across restarts, so one that is asleep at
  start-up still shows up.
- The firmware moved from `firmware/keychron-k3-max` to `firmware/keychron`.

### Fixed

- Keychron keyboards no longer show up as "Keychron Link" when a query goes
  unanswered, and a silent query is retried once with a longer timeout.

## [0.2.0] - 2026-11-24

### Added

- Battery levels of Bluetooth devices, as reported by Windows.
- Keychron K3 Max battery over the 2.4 GHz dongle, with a firmware patch for all
  six K3 Max variants.
- Releases now include the K3 Max firmware files next to `battray.exe`.

## [0.1.0] - 2026-11-24

### Added

- Tray app with one icon for the lowest battery, one icon per device, or a single
  selected device.
- Logitech HID++ 2.0 devices on Lightspeed, Unifying and Bolt receivers or USB.
- Sony DualSense and DualSense Edge over Bluetooth and USB.
- Refresh interval, low battery alerts, start with Windows and a diagnostics report.

[Unreleased]: https://github.com/forkacper/battray/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/forkacper/battray/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/forkacper/battray/releases/tag/v0.1.0
