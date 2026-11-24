#!/bin/sh
# Builds battray-enabled firmware for every Keychron K3 Max variant from
# Keychron's own QMK tree at a pinned commit: applies wireless-raw-hid.patch
# and adds a "battray" keymap (the stock "keychron" keymap plus keymap/).
# Needs the QMK build environment, e.g. the ghcr.io/qmk/qmk_cli image.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$here/build}
commit=57e682caa0a821e6de1b589e6f983f266151ab02 # Keychron/qmk_firmware 2025q3, 2026-10-30
variants="ansi/rgb ansi/white iso/rgb iso/white jis/rgb jis/white"

if [ ! -d "$work/qmk/.git" ]; then
    git init -q "$work/qmk"
    git -C "$work/qmk" remote add origin https://github.com/Keychron/qmk_firmware.git
fi
cd "$work/qmk"
git fetch -q --depth 1 origin "$commit"
git checkout -q --force FETCH_HEAD
git submodule update --init --depth 1 lib/chibios lib/chibios-contrib lib/printf
git apply "$here/wireless-raw-hid.patch"

mkdir -p "$here/out"
for variant in $variants; do
    keymaps="keyboards/keychron/k3_max/$variant/keymaps"
    rm -rf "$keymaps/battray"
    cp -r "$keymaps/keychron" "$keymaps/battray"
    cp "$here"/keymap/* "$keymaps/battray/"
    make -j"$(nproc)" "keychron/k3_max/$variant:battray"
    cp "keychron_k3_max_$(echo "$variant" | tr / _)_battray.bin" "$here/out/"
done
