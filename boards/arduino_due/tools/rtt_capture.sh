#!/usr/bin/env bash
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
# Usage: rtt_capture.sh SECONDS ["openocd commands run before RTT; "]
# Attaches with OpenOCD (no reset), starts RTT and prints the console for N s.
set -u
secs=${1:-5}
pre=${2:-}
log=${TMPDIR:-/tmp}/arduino_due_rtt.log
openocd -f interface/jlink.cfg -c 'transport select swd' -f target/at91sam3ax_8x.cfg \
    -c 'reset_config none' \
    -c "init; $pre rtt setup ${RTT_START:-0x20072000} ${RTT_SIZE:-0x16000} {SEGGER RTT}; rtt start; rtt server start 9090 0" \
    >"$log" 2>&1 &
pid=$!
for _ in $(seq 40); do grep -q "Listening on port 9090" "$log" 2>/dev/null && break; sleep 0.25; done
timeout "$secs" python3 -c '
import socket,sys
s=socket.create_connection(("127.0.0.1",9090)); s.settimeout(0.5)
while True:
    try:
        d=s.recv(4096)
        if not d: break
        sys.stdout.write(d.decode("latin1")); sys.stdout.flush()
    except socket.timeout: pass
'
kill "$pid" 2>/dev/null
wait "$pid" 2>/dev/null
echo
echo "--- openocd log ---"
grep -iE "error|control block" "$log" | head -10
