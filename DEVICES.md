# Supported devices

| Device | Connection | How battray reads it | Tested |
|---|---|---|---|
| Logitech PRO X SUPERLIGHT 2 | Lightspeed receiver | HID++ 2.0, feature 0x1004 | Yes |
| Other Logitech HID++ 2.0 devices | Lightspeed, Unifying or Bolt receiver, USB cable | HID++ 2.0, features 0x1004, 0x1000 or 0x1001 | No |
| Sony DualSense, DualSense Edge | Bluetooth, USB | Status byte of the input report | No |
| Keychron K3 Max | 2.4 GHz dongle, USB cable | Raw HID query, needs the [firmware patch](firmware/keychron-k3-max) | Yes |
| Bluetooth devices that report their battery to Windows | Bluetooth | Battery level stored by Windows, as shown in Settings | No |

Untested devices should work, but have not been confirmed on real hardware yet.
If your device works or does not, open an issue with the diagnostics report from
the tray menu (Save diagnostics report).

## Not supported

- Keychron keyboards on stock firmware over the 2.4 GHz dongle: the stock firmware
  does not report the battery to the computer. Over Bluetooth they are covered by
  the Bluetooth row above.
