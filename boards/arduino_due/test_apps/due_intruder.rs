// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Phase 1 gate app: prints a few lines, then writes into another process's
//! RAM. The MPU must fault it, and the kernel must stop only this process.

#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

/// An address inside the keeper's RAM region (built for 0x20078000).
const TARGET: usize = 0x2007_8100;

fn main() {
    for i in 0..3 {
        let _ = writeln!(Console::writer(), "intruder: running {i}");
        Alarm::sleep_for(Milliseconds(700)).unwrap();
    }
    let _ = writeln!(Console::writer(), "intruder: writing to {TARGET:#x}");
    // SAFETY: deliberately invalid; this write must be stopped by the MPU.
    unsafe { core::ptr::write_volatile(TARGET as *mut u32, 0xDEAD_BEEF) };
    let _ = writeln!(Console::writer(), "intruder: write was NOT blocked");
    loop {
        Alarm::sleep_for(Milliseconds(1000)).unwrap();
    }
}
