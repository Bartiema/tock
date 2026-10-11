#!/usr/bin/env bash
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
# Usage: rtt_cmd.sh SECONDS "command1" "command2" ...
# Sends each line to the RTT down channel (1.5 s apart) and prints output.
set -u
secs=$1; shift
log=${TMPDIR:-/tmp}/arduino_due_rtt.log
openocd -f interface/jlink.cfg -c 'transport select swd' -f target/at91sam3ax_8x.cfg \
    -c 'reset_config none' \
    -c "init; rtt setup 0x20072000 0x16000 {SEGGER RTT}; rtt start; rtt server start 9090 0" \
    >"$log" 2>&1 &
pid=$!
for _ in $(seq 40); do grep -q "Listening on port 9090" "$log" 2>/dev/null && break; sleep 0.25; done
timeout "$secs" python3 - "$@" <<'PY'
import socket,sys,time
s=socket.create_connection(("127.0.0.1",9090)); s.settimeout(0.2)
cmds=sys.argv[1:]; t0=time.time(); nxt=t0+1.0
while True:
    if cmds and time.time()>=nxt:
        for ch in (cmds.pop(0)+"\r").encode():
            s.send(bytes([ch])); time.sleep(0.12)  # RTT RX is polled every 100 ms, 32-byte buffer
        nxt=time.time()+1.5
    try:
        d=s.recv(4096)
        if not d: break
        sys.stdout.write(d.decode("latin1")); sys.stdout.flush()
    except socket.timeout: pass
PY
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; echo
