#!/bin/sh
# Builds battray-enabled firmware for Keychron's wireless keyboards from
# Keychron's own QMK tree at a pinned commit: applies wireless-raw-hid.patch and
# adds a "battray" keymap (each variant's stock "keychron" keymap plus keymap/).
#
#   build.sh --list          print the supported boards as a JSON array
#   build.sh [board ...]     build the given boards (default: all), e.g. k3_max
#
# Building needs the QMK build environment, e.g. the ghcr.io/qmk/qmk_cli image.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$here/build}
commit=57e682caa0a821e6de1b589e6f983f266151ab02 # Keychron/qmk_firmware 2025q3, 2026-10-30
tree="$work/qmk"

fetch() {
    if [ ! -d "$tree/.git" ]; then
        git init -q "$tree"
        git -C "$tree" remote add origin https://github.com/Keychron/qmk_firmware.git
    fi
    git -C "$tree" fetch -q --depth 1 "$@" origin "$commit"
    git -C "$tree" checkout -q --force FETCH_HEAD
}

# Boards built on Keychron's shared wireless code, which the patch targets.
boards() {
    for rules in "$tree"/keyboards/keychron/*/rules.mk; do
        grep -q "wireless/wireless.mk" "$rules" && basename "$(dirname "$rules")"
    done
}

if [ "${1:-}" = "--list" ]; then
    fetch --filter=blob:none
    boards | awk 'BEGIN { printf "[" } { printf "%s\"%s\"", (NR > 1 ? "," : ""), $0 } END { print "]" }'
    exit 0
fi

fetch
git -C "$tree" submodule update -q --init --depth 1 lib/chibios lib/chibios-contrib lib/printf
git -C "$tree" apply "$here/wireless-raw-hid.patch"
cd "$tree"

[ $# -gt 0 ] || set -- $(boards)
mkdir -p "$here/out"
for board in "$@"; do
    find "keyboards/keychron/$board" -type d -path "*/keymaps/keychron" | sort | while read -r stock; do
        keymaps=$(dirname "$stock")
        variant=${keymaps#keyboards/}
        variant=${variant%/keymaps}
        rm -rf "$keymaps/battray"
        cp -r "$stock" "$keymaps/battray"
        cp "$here/keymap/battray.c" "$keymaps/battray/"
        # Append rather than replace, in case a stock keymap has its own settings.
        { echo; cat "$here/keymap/rules.mk"; } >> "$keymaps/battray/rules.mk"
        if [ -f "$stock/config.h" ]; then
            { echo; sed '/#pragma once/d' "$here/keymap/config.h"; } >> "$keymaps/battray/config.h"
        else
            cp "$here/keymap/config.h" "$keymaps/battray/"
        fi
        make -j"$(nproc)" "$variant:battray"
        cp "$(echo "$variant" | tr / _)_battray".* "$here/out/"
    done
done
