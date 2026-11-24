# battray

A super lightweight Windows tray app that shows the battery level of your wireless
mice, keyboards and gamepads.

- Native Win32 in Rust: a single small `.exe`, a few MB of RAM, idle between polls.
- No vendor software needed (G HUB, Steam), though it runs fine alongside it.
- Supports Logitech HID++ devices (e.g. PRO X SUPERLIGHT 2), DualSense, and any Bluetooth
  device that reports its battery to Windows.
- Keychron K3 Max on the 2.4 GHz dongle with the [firmware patch](firmware/keychron-k3-max).

Download `battray.exe` from [Releases](https://github.com/forkacper/battray/releases).

## Contributing

Contributions are welcome, especially support for new devices. Each device family is
one provider in `src/devices/`. Open an issue or a pull request.

## License

[MIT](LICENSE), except `firmware/`, which is GPL-2.0-or-later like the QMK code it patches.
