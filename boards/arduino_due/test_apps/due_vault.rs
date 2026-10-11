// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Vault: holds secrets that lockdown must destroy.
//!
//! Plants a 32-byte canary in each place a process keeps data:
//!
//! | Tag | Where                                                          |
//! |-----|----------------------------------------------------------------|
//! | S   | its stack                                                      |
//! | D   | its `.data` section                                            |
//! | H   | its heap (grown with memop)                                    |
//! | C   | a buffer shared with the console capsule, written every second |
//!
//! The console copies the shared buffer into kernel buffers, so tag C also
//! shows where a capsule leaks process data into kernel RAM.
//!
//! A canary is `CNRY`, the tag, `-`, then 26 letters from a generator seeded
//! by the tag. `tools/canary_scan.py` computes the same bytes.

#![no_main]
#![no_std]
use core::fmt::Write;
use core::hint::black_box;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::Syscalls;
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const fn canary(tag: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0] = b'C';
    out[1] = b'N';
    out[2] = b'R';
    out[3] = b'Y';
    out[4] = tag;
    out[5] = b'-';
    let mut state: u32 = 0x1234_5678 ^ (tag as u32).wrapping_mul(0x0101_0101);
    let mut i = 6;
    while i < 32 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out[i] = b'A' + ((state >> 24) % 26) as u8;
        i += 1;
    }
    out
}

/// Tag D: initialized data, copied from flash into the process's RAM at start.
static mut DATA_CANARY: [u8; 32] = canary(b'D');

/// Tag C: computed at run time into zeroed memory, then shared with the
/// console capsule.
static mut SHARED: [u8; 33] = [0; 33];

fn main() {
    // Tag S: lives in this stack frame for the life of the process.
    let stack_canary = canary(b'S');

    // Tag H: 32 bytes past the initial program break.
    let heap = TockSyscalls::memop_increment_brk(32).unwrap() as *mut u8;
    let heap_canary = canary(b'H');
    for (i, b) in heap_canary.iter().enumerate() {
        // SAFETY: the 32 bytes after the old break were just given to this
        // process by the kernel.
        unsafe { heap.add(i).write_volatile(*b) };
    }

    // SAFETY: single-threaded; nothing else references these statics.
    let shared = unsafe { &mut *core::ptr::addr_of_mut!(SHARED) };
    shared[..32].copy_from_slice(&canary(b'C'));
    shared[32] = b'\n';
    // Keep the data canary from being optimized away.
    // SAFETY: as above.
    black_box(unsafe { core::ptr::addr_of!(DATA_CANARY).read_volatile() });

    let _ = writeln!(
        Console::writer(),
        "vault: canaries planted (stack {:p}, heap {:p})",
        &stack_canary,
        heap
    );
    loop {
        black_box(&stack_canary);
        let _ = Console::write(shared);
        Alarm::sleep_for(Milliseconds(1000)).unwrap();
    }
}
