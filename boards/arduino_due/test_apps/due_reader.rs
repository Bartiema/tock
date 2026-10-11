// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Reader: reads from the console and echoes what it got. A second console
//! reader next to the process console makes the UART mux abort and restart
//! reads, which exercises the underlying UART's `receive_abort`.

#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut buf = [0u8; 4];
    loop {
        let (n, result) = Console::read(&mut buf);
        let text = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        let _ = writeln!(Console::writer(), "reader: got {n} bytes {text:?} ({result:?})");
    }
}
