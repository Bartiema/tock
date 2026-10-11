// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Worker: CPU-bound loop with continuous console output, so lockdown can be
//! triggered while a process is preempted mid-computation and while console
//! output is in flight.

#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut hash: u32 = 0x811C_9DC5;
    let mut round: u32 = 0;
    loop {
        // Long enough to be preempted by the scheduler timeslice.
        for i in 0..200_000u32 {
            hash = (hash ^ i).wrapping_mul(0x0100_0193);
        }
        let _ = writeln!(Console::writer(), "worker: round {round} hash {hash:08x}");
        round = round.wrapping_add(1);
    }
}
