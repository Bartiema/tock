#!/usr/bin/env python3
# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.
"""Scan an SRAM dump of the Arduino Due for the vault app's canaries.

The vault app (test_apps/due_vault.rs) plants 32-byte canaries tagged S
(stack), D (data), H (heap) and C (buffer shared with the console). This
script finds every full copy and every partial copy of at least --min-run
bytes, and attributes each to the kernel stack, a kernel symbol (from the
kernel ELF) or a process RAM slot (from the TBF headers in apps.bin).

Examples:
    canary_scan.py --dump sram.bin --elf arduino_due.elf --apps apps.bin
    canary_scan.py sram.bin --elf arduino_due.elf --apps apps.bin \\
        --expect-in due_vault:S,D,H,C

Exit status: 0 if every --expect-in / --expect-none holds, 1 otherwise.
"""

import argparse
import bisect
import struct
import subprocess
import sys

SRAM_BASE = 0x20070000
SRAM_SIZE = 0x18000
TAGS = "SDHC"
TAG_NAMES = {"S": "stack", "D": "data", "H": "heap", "C": "shared buffer"}


def canary(tag):
    """Same generator as `canary()` in test_apps/due_vault.rs."""
    out = bytearray(b"CNRY" + tag.encode() + b"-")
    state = (0x12345678 ^ (ord(tag) * 0x01010101)) & 0xFFFFFFFF
    while len(out) < 32:
        state = (state * 1664525 + 1013904223) & 0xFFFFFFFF
        out.append(ord("A") + ((state >> 24) % 26))
    return bytes(out)


def dump_sram(path, openocd_args):
    cmd = ["openocd"] + openocd_args + [
        "-c",
        f"init; halt; dump_image {path} {SRAM_BASE:#x} {SRAM_SIZE:#x}; resume; shutdown",
    ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"openocd failed:\n{result.stderr}")


def kernel_symbols(elf):
    """Sorted (address, size, name) of data objects, plus section markers."""
    out = subprocess.run(
        ["llvm-nm", "-S", "-C", "--defined-only", elf],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    syms, markers = [], {}
    marker_names = {"_estack", "_sappmem", "_eappmem"}
    for line in out.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 3:
            parts = [parts[0], "0", parts[1], parts[2]]
        if len(parts) != 4:
            continue
        addr, size, kind, name = parts
        if name in marker_names:
            markers[name] = int(addr, 16)
        elif kind.lower() in "bdr" and int(addr, 16) >= SRAM_BASE:
            syms.append((int(addr, 16), int(size, 16), name))
    syms.sort()
    return syms, markers


def app_slots(apps_bin):
    """(ram_start, name) for each TBF with a fixed RAM address."""
    data = open(apps_bin, "rb").read()
    slots, off = [], 0
    while off + 16 <= len(data):
        version, header_len, total = struct.unpack_from("<HHI", data, off)
        if version != 2 or total == 0:
            break
        name, ram = None, None
        p = off + 16
        while p + 4 <= off + header_len:
            tlv, length = struct.unpack_from("<HH", data, p)
            if tlv == 3:
                name = data[p + 4 : p + 4 + length].decode(errors="replace")
            elif tlv == 5:
                ram = struct.unpack_from("<I", data, p + 4)[0]
            p += 4 + ((length + 3) & ~3)
        if ram is not None:
            slots.append((ram, name or f"app@{off:#x}"))
        off += total
    return sorted(slots)


class Regions:
    def __init__(self, syms, markers, slots):
        self.syms = syms
        self.starts = [s[0] for s in syms]
        self.estack = markers.get("_estack", SRAM_BASE)
        self.sappmem = markers.get("_sappmem")
        self.eappmem = markers.get("_eappmem", SRAM_BASE + SRAM_SIZE)
        self.slots = slots

    def region(self, addr):
        """(region, detail) for an address."""
        if addr < self.estack:
            return "kernel stack", ""
        if self.sappmem is None or addr < self.sappmem:
            i = bisect.bisect_right(self.starts, addr) - 1
            if i >= 0:
                start, size, name = self.syms[i]
                if start <= addr < start + max(size, 1):
                    return "kernel data", f"{name} @{start:#x} (size {size:#x}) +{addr - start:#x}"
            return "kernel data", "(no symbol)"
        for i, (start, name) in enumerate(self.slots):
            end = self.slots[i + 1][0] if i + 1 < len(self.slots) else self.eappmem
            if start <= addr < end:
                return f"process {name}", f"+{addr - start:#x}"
        return "app memory (unallocated)", ""


def find_hits(data, min_run):
    """(address, tag, offset in canary, length) for maximal matching runs."""
    hits = []
    for tag in TAGS:
        c = canary(tag)
        windows = {c[j : j + min_run]: j for j in range(32 - min_run + 1)}
        i = 0
        while i <= len(data) - min_run:
            j = windows.get(data[i : i + min_run])
            if j is None:
                i += 1
                continue
            n = min_run
            while j + n < 32 and i + n < len(data) and data[i + n] == c[j + n]:
                n += 1
            hits.append((SRAM_BASE + i, tag, j, n))
            i += n
    return sorted(hits)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("dump", help="SRAM dump (96 KB from 0x20070000)")
    ap.add_argument("--dump", dest="do_dump", action="store_true",
                    help="first dump SRAM to DUMP through OpenOCD")
    ap.add_argument("--interface", default="interface/jlink.cfg",
                    help="OpenOCD interface config (default: J-Link)")
    ap.add_argument("--transport", default="swd", help="debug transport (default: swd)")
    ap.add_argument("--elf", help="kernel ELF, to name kernel buffers")
    ap.add_argument("--apps", help="apps.bin, to find process RAM slots")
    ap.add_argument("--min-run", type=int, default=8,
                    help="shortest partial copy to report (default 8)")
    ap.add_argument("--expect-in", action="append", default=[], metavar="APP:TAGS",
                    help="require a full copy of each tag in this process")
    ap.add_argument("--expect-none", action="store_true",
                    help="require no copy (full or partial) anywhere")
    args = ap.parse_args()

    if args.do_dump:
        dump_sram(args.dump, [
            "-f", args.interface, "-c", f"transport select {args.transport}",
            "-f", "target/at91sam3ax_8x.cfg", "-c", "reset_config none",
        ])
    data = open(args.dump, "rb").read()
    if len(data) != SRAM_SIZE:
        sys.exit(f"{args.dump}: expected {SRAM_SIZE} bytes, got {len(data)}")

    syms, markers = kernel_symbols(args.elf) if args.elf else ([], {})
    slots = app_slots(args.apps) if args.apps else []
    regions = Regions(syms, markers, slots)

    hits = find_hits(data, args.min_run)
    print(f"{'address':>10}  tag  bytes   kind     region / detail")
    for addr, tag, off, n in hits:
        region, detail = regions.region(addr)
        kind = "full" if (off == 0 and n == 32) else f"[{off}:{off + n}]"
        print(f"{addr:#010x}  {tag}    {n:>3}    {kind:<8} {region}  {detail}")

    print()
    summary = {}
    for addr, tag, off, n in hits:
        summary.setdefault((regions.region(addr)[0], tag), 0)
        summary[(regions.region(addr)[0], tag)] += 1
    for (region, tag), count in sorted(summary.items()):
        print(f"{count:>3} x {tag} ({TAG_NAMES[tag]}) in {region}")
    if not hits:
        print("no canaries found")

    ok = True
    for spec in args.expect_in:
        app, tags = spec.split(":")
        for tag in tags.split(","):
            found = any(
                t == tag and o == 0 and n == 32 and regions.region(a)[0] == f"process {app}"
                for a, t, o, n in hits
            )
            print(f"{'PASS' if found else 'FAIL'}: full {tag} canary in process {app}")
            ok &= found
    if args.expect_none:
        print(f"{'PASS' if not hits else 'FAIL'}: no canary anywhere ({len(hits)} found)")
        ok &= not hits
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
