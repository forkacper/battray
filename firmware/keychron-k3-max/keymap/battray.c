// Copyright 2026 Kacper Formas
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Battery query for battray, answered on VIA's custom value channel so it
// works over USB, Bluetooth and the 2.4 GHz dongle without changing any of
// Keychron's own commands. The dongle forwards these commands to the keyboard.
//
// Request (32 bytes): 08 00 42
//   id_custom_get_value, id_custom_channel, BATTRAY_VALUE_ID
// Reply (32 bytes):   08 00 42 <ver> <percent> <mV lo> <mV hi> <charge> <transport> <name...>
//   ver:       reply layout version, currently 1
//   charge:    0 not charging, 1 charging, 2 fully charged
//   transport: 1 USB, 2 Bluetooth, 4 2.4 GHz
//   name:      keyboard name, null-terminated

#include <string.h>
#include QMK_KEYBOARD_H
#include "via.h"
#include "battery.h"
#include "lpm.h"
#include "transport.h"

#define BATTRAY_VALUE_ID 0x42
#define BATTRAY_REPLY_VERSION 1
#define BATTRAY_NAME_OFFSET 9

void via_custom_value_command_kb(uint8_t *data, uint8_t length) {
    // data = [ command_id, channel_id, value_id, value_data ]
    if (length < 32 || data[0] != id_custom_get_value || data[1] != id_custom_channel || data[2] != BATTRAY_VALUE_ID) {
        data[0] = id_unhandled;
        return;
    }

    uint16_t millivolts = battery_get_voltage();
    uint8_t  charge     = 0;
#ifdef BAT_CHARGING_PIN
    if (usb_power_connected()) {
        charge = gpio_read_pin(BAT_CHARGING_PIN) == BAT_CHARGING_LEVEL ? 1 : 2;
    }
#endif

    data[3] = BATTRAY_REPLY_VERSION;
    data[4] = battery_get_percentage();
    data[5] = millivolts & 0xFF;
    data[6] = millivolts >> 8;
    data[7] = charge;
    data[8] = get_transport();
    memset(&data[BATTRAY_NAME_OFFSET], 0, length - BATTRAY_NAME_OFFSET);
    strncpy((char *)&data[BATTRAY_NAME_OFFSET], PRODUCT, length - BATTRAY_NAME_OFFSET - 1);
}
