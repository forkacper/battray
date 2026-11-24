# battray

A super lightweight Windows tray app that shows the battery level of your wireless
mice, keyboards and gamepads.

- Native Win32 in Rust: a single small `.exe`, a few MB of RAM, idle between polls.
- No vendor software needed (G HUB, Steam), though it runs fine alongside it.
- Supports Logitech HID++ devices, PlayStation and Xbox controllers, Keychron
  wireless keyboards, Bluetooth devices and standard HID batteries. See
  [supported devices](DEVICES.md).

Download `battray.exe` from [Releases](https://github.com/forkacper/battray/releases).
See the [changelog](CHANGELOG.md) for what changed.

## Contributing

Contributions are welcome, especially support for new devices. Each device family is
one provider in `src/devices/`. Open an issue or a pull request, and add your change
to the Unreleased section of the [changelog](CHANGELOG.md).

## License

[MIT](LICENSE), except `firmware/`, which is GPL-2.0-or-later like the QMK code it patches.
