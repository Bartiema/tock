#!/usr/bin/env python3
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
"""Lock and unlock the Arduino Due repeatedly and check every cycle.

Needs the loopback test rig (pin 23 wired to the trigger, pin 22), the `poc`
apps (keeper, vault, worker) and a probe. Each cycle:

1. Open the tamper loop, wait, dump SRAM: no vault canary may remain
   anywhere, full or partial (kernel buffers and stack included).
2. Close the loop, wait for the hold time and the restart, dump SRAM: the
   vault must have restarted and planted all four canaries again.

Example:
    lockdown_soak.py --elf arduino_due.elf --apps apps.bin --cycles 100
"""

import argparse
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def tamper(state):
    subprocess.run([os.path.join(HERE, "tamper.sh"), state], check=True,
                   capture_output=True)


def scan(dump, elf, apps, checks):
    cmd = [sys.executable, os.path.join(HERE, "canary_scan.py"), dump, "--dump",
           "--elf", elf, "--apps", apps] + checks
    result = subprocess.run(cmd, capture_output=True, text=True)
    return result.returncode == 0, result.stdout


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--elf", required=True, help="kernel ELF")
    ap.add_argument("--apps", required=True, help="flashed apps.bin")
    ap.add_argument("--cycles", type=int, default=100)
    ap.add_argument("--lock-wait", type=float, default=0.3,
                    help="seconds between opening the loop and the dump")
    ap.add_argument("--unlock-wait", type=float, default=1.5,
                    help="seconds between closing the loop and the dump "
                    "(hold time + restart + vault start-up)")
    ap.add_argument("--dump", default="soak_sram.bin")
    ap.add_argument("--log", default="soak.log")
    args = ap.parse_args()

    failures = 0
    with open(args.log, "w") as log:
        tamper("close")
        time.sleep(args.unlock_wait)
        for cycle in range(1, args.cycles + 1):
            tamper("open")
            time.sleep(args.lock_wait)
            locked_ok, locked_out = scan(args.dump, args.elf, args.apps, ["--expect-none"])
            tamper("close")
            time.sleep(args.unlock_wait)
            unlocked_ok, unlocked_out = scan(args.dump, args.elf, args.apps,
                                             ["--expect-in", "due_vault:S,D,H,C"])
            status = "ok" if locked_ok and unlocked_ok else "FAIL"
            line = (f"cycle {cycle:3d}: locked {'clean' if locked_ok else 'CANARY LEFT'}, "
                    f"unlocked {'vault restarted' if unlocked_ok else 'VAULT MISSING'}: {status}")
            print(line, flush=True)
            log.write(line + "\n")
            if not locked_ok:
                log.write(locked_out)
            if not unlocked_ok:
                log.write(unlocked_out)
            failures += 0 if status == "ok" else 1
    print(f"{args.cycles - failures}/{args.cycles} cycles passed; log in {args.log}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
