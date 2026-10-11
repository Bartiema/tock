#!/usr/bin/env bash
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
#
# Build a set of Arduino Due test apps with libtock-rs.
#
# Usage: build.sh <libtock-rs checkout> [set]
#
# Sets (flashed in this order, back to back from 0xC0000):
#   poc     keeper vault worker   (default; the lockdown PoC apps)
#   mpu     keeper intruder       (Phase 1 MPU gate)
#   gpio    keeper gpio_test      (PIO driver test; drives D24 and D25)
#
# libtock-rs apps are linked for fixed addresses. Each app's flash address is
# where the previous TBF ends, so apps are built in order. Each app gets a
# 16 KB RAM slot from 0x20078000; the kernel skips ahead to an app's fixed RAM
# address, and 16 KB keeps every slot aligned for the MPU.
#
# Output: build/<set>/<app>.tbf and build/<set>/apps.bin, for
#   make flash-apps APPS=build/<set>/apps.bin
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
libtock=$(cd "$1" && pwd)
set_name=${2:-poc}
elf2tab=${ELF2TAB:-elf2tab}

case "$set_name" in
    poc) apps=(due_keeper due_vault due_worker) ;;
    mpu) apps=(due_keeper due_intruder) ;;
    gpio) apps=(due_keeper due_gpio_test) ;;
    *) echo "unknown set: $set_name" >&2; exit 1 ;;
esac

out=$here/build/$set_name
mkdir -p "$out"
flash=$((0x000C0000))
ram=$((0x20078000))
ram_slot=$((0x4000))
ram_end=$((0x20088000))

for app in "${apps[@]}"; do
    if (( ram + ram_slot > ram_end )); then
        echo "out of RAM slots at $app" >&2; exit 1
    fi
    cp "$here/$app.rs" "$libtock/examples/$app.rs"
    (cd "$libtock" &&
        LIBTOCK_LINKER_FLASH=$(printf '0x%08X' $flash) LIBTOCK_LINKER_FLASH_LENGTH=0x10000 \
        LIBTOCK_LINKER_RAM=$(printf '0x%08X' $ram) LIBTOCK_LINKER_RAM_LENGTH=$ram_slot \
        cargo build --quiet --release --example "$app" --target thumbv7m-none-eabi \
            -Zbuild-std=core,compiler_builtins --target-dir "target/$app")
    cp "$libtock/target/$app/thumbv7m-none-eabi/release/examples/$app" "$out/$app.elf"
    "$elf2tab" --kernel-major 2 --kernel-minor 1 -n "$app" -o "$out/$app.tab" \
        --stack 2048 --minimum-footer-size 256 "$out/$app.elf" >/dev/null
    size=$(stat -c %s "$out/$app.tbf")
    printf '%-14s flash 0x%08X (%5d bytes)  RAM 0x%08X\n' "$app" $flash "$size" $ram
    flash=$((flash + size))
    ram=$((ram + ram_slot))
done

for app in "${apps[@]}"; do cat "$out/$app.tbf"; done > "$out/apps.bin"
echo "$out/apps.bin"
