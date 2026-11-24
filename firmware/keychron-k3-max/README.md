# Keychron K3 Max firmware for battray

Stock Keychron firmware does not report the battery level to the computer, and
ignores raw HID commands that arrive through the 2.4 GHz dongle. This is Keychron's
own firmware with two small changes, so battray can read the battery:

- `wireless-raw-hid.patch`: fixes the disabled code path that hands raw HID from the
  wireless module (dongle or Bluetooth) to the command handler.
- `keymap/`: the stock `keychron` keymap plus `WILRESS_RAW_ENABLE` and a battery
  query on VIA's custom value channel (`battray.c`). Keychron's own commands and
  the Launcher keep working.

`build.sh` builds it from Keychron's QMK tree at a pinned commit, for every variant.
CI runs it and uploads the `.bin` files as the `keychron-k3-max-firmware` artifact.

## Flashing (Windows)

Flashing is at your own risk. The bootloader lives in the chip's ROM, so a failed
flash can be repeated, and the official firmware can always be flashed back.

1. Pick the file for your layout and backlight, e.g. `keychron_k3_max_ansi_rgb_battray.bin`.
2. Install [QMK Toolbox](https://github.com/qmk/qmk_toolbox/releases) and open the file.
3. Set the mode switch to Cable and unplug the keyboard.
4. Remove the space bar keycap, hold the reset button next to the space bar switch,
   plug the cable in and release after about two seconds. QMK Toolbox reports
   "DFU device connected".
5. Click Flash and do not unplug until it finishes.

To go back, flash the official file from
[Keychron's firmware page](https://www.keychron.com/pages/firmware-and-json-files-of-the-keychron-qmk-k-pro-and-k-max-series-keyboards)
the same way, following
[Keychron's K3 Max guide](https://keychron.de/pages/how-to-factory-reset-and-flash-firmware-for-your-k3-max-keyboard).

## Protocol

One 32-byte VIA packet each way, on the raw HID interface (usage page `0xFF60`) of
the keyboard or of the Keychron Link dongle:

| Byte | Request | Reply |
|---|---|---|
| 0 | `0x08` (custom get value) | `0x08`, or `0xFF` on firmware without the patch |
| 1 | `0x00` (custom channel) | `0x00` |
| 2 | `0x42` (battery) | `0x42` |
| 3 | | reply version, `1` |
| 4 | | battery level in percent |
| 5–6 | | battery voltage in mV, little endian |
| 7 | | `0` not charging, `1` charging, `2` fully charged |
| 8 | | transport: `1` USB, `2` Bluetooth, `4` 2.4 GHz |
| 9– | | keyboard name, null-terminated |

## License

GPL-2.0-or-later, like the QMK and Keychron code it patches. The wireless raw HID
fix was first described in the [bluish](https://github.com/bluish/keychron-k3-max-firmware)
and [Freedye](https://github.com/Freedye/keychron-k3-max-qmk) projects.
