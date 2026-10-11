// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Keeper: the allowlisted app. Heartbeat on the LED and one console line per
//! second, which must continue across lockdown.

#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::leds::Leds;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut n: u32 = 0;
    loop {
        let _ = Leds::toggle(0);
        let _ = writeln!(Console::writer(), "keeper: heartbeat {n}");
        n = n.wrapping_add(1);
        Alarm::sleep_for(Milliseconds(1000)).unwrap();
    }
}
