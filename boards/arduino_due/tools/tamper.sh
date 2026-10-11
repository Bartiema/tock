#!/usr/bin/env bash
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
#
# Simulate the tamper loop on the loopback-wired test rig, where Arduino pin
# 23 (PA14) is wired to the trigger, pin 22 (PB26). Drives PA14 through the
# debug probe, without halting the core.
#
# Usage: tamper.sh close   drive PA14 low: loop closed (normal)
#        tamper.sh open    release PA14: the pull-up on PB26 opens the loop
#        tamper.sh status  print the PB26 level as Tock sees it
set -euo pipefail
PIOA=0x400E0E00
PIOB=0x400E1000
BIT14=0x4000
case "${1:-}" in
    close) cmd="mww $((PIOA + 0x34)) $BIT14; mww $((PIOA + 0x10)) $BIT14; mww $PIOA $BIT14" ;;
    open) cmd="mww $((PIOA + 0x14)) $BIT14" ;;
    status) cmd="echo \"PB26 [expr {([read_memory $((PIOB + 0x3C)) 32 1] >> 26) & 1}] PA14 output [expr {([read_memory $((PIOA + 0x18)) 32 1] >> 14) & 1}]\"" ;;
    *) echo "usage: $0 close|open|status" >&2; exit 1 ;;
esac
openocd -f "${OPENOCD_INTERFACE:-interface/jlink.cfg}" \
    -c "transport select ${OPENOCD_TRANSPORT:-swd}" \
    -f target/at91sam3ax_8x.cfg -c "reset_config none" \
    -c "init; $cmd; shutdown" 2>&1 | grep -vE "^(Info|Open On|Licensed|For bug|\s*http|swd|none)" || true
