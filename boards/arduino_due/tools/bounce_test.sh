#!/usr/bin/env bash
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
#
# Switch bounce test on the loopback rig: toggle the tamper loop (pin 23 ->
# pin 22) faster than the unlock hold time, in one OpenOCD session so the
# timing is within a few milliseconds. Start with the loop open (locked).
#
# Expected: closing with bounces unlocks once, 500 ms after the last close;
# opening with bounces locks once, on the first open.
set -euo pipefail
A=0x400E0E00
close="mww $((A + 0x34)) 0x4000; mww $((A + 0x10)) 0x4000; mww $A 0x4000"
open="mww $((A + 0x14)) 0x4000"
seq="$close; sleep 200; $open; sleep 30; $close; sleep 300; $open; sleep 10; $close; sleep 1500;"
seq+=" $open; sleep 20; $close; sleep 20; $open; sleep 50; $close; sleep 5; $open; sleep 1000"
openocd -f "${OPENOCD_INTERFACE:-interface/jlink.cfg}" -c "transport select ${OPENOCD_TRANSPORT:-swd}" \
    -f target/at91sam3ax_8x.cfg -c "reset_config none" -c "init; $seq; shutdown" >/dev/null 2>&1
echo "bounce sequence done; check lockstat: +1 unlock, +1 lock"
