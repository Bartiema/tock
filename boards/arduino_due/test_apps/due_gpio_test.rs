// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Hardware test for the SAM3X PIO driver on the loopback-wired Due.
//!
//! Wires used: D25 -> D26 (PD0 -> PD1), and D24 (PA15) -> D2 (PB25, the
//! board's button, active low with pull-up and 10 ms debounce).

#![no_main]
#![no_std]
use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::{ButtonListener, ButtonState, Buttons};
use libtock::console::Console;
use libtock::gpio::{self, Gpio, GpioState, PinInterruptEdge};
use libtock::runtime::{set_main, stack_size};
use libtock_platform::share;

set_main! {main}
stack_size! {0x800}

fn settle() {
    Alarm::sleep_for(Milliseconds(30)).unwrap();
}

fn check(name: &str, ok: bool, fails: &Cell<u32>) {
    let _ = writeln!(Console::writer(), "gpio test: {} {}", if ok { "PASS" } else { "FAIL" }, name);
    if !ok {
        fails.set(fails.get() + 1);
    }
}

fn main() {
    let fails = Cell::new(0u32);
    let gpio_irqs = Cell::new(0u32);
    let last_state = Cell::new(GpioState::Low);
    let presses = Cell::new(0u32);
    let releases = Cell::new(0u32);

    let gpio_listener = gpio::GpioInterruptListener(|pin, state| {
        if pin == 26 {
            gpio_irqs.set(gpio_irqs.get() + 1);
            last_state.set(state);
        }
    });
    let button_listener = ButtonListener(|_, state| match state {
        ButtonState::Pressed => presses.set(presses.get() + 1),
        ButtonState::Released => releases.set(releases.get() + 1),
    });

    share::scope(|gpio_sub| {
    share::scope(|button_sub| {
        Gpio::register_listener(&gpio_listener, gpio_sub).unwrap();
        Buttons::register_listener(&button_listener, button_sub).unwrap();

        let mut out = Gpio::get_pin(25).unwrap();
        let mut out = out.make_output().unwrap();
        let in_pin = Gpio::get_pin(26).unwrap();
        let input = in_pin.make_input::<gpio::PullNone>().unwrap();

        // Levels.
        out.clear().unwrap();
        settle();
        check("D25 low reads low on D26", matches!(input.read(), Ok(GpioState::Low)), &fails);
        out.set().unwrap();
        settle();
        check("D25 high reads high on D26", matches!(input.read(), Ok(GpioState::High)), &fails);
        out.clear().unwrap();
        settle();

        // Either edge: 3 full cycles give 6 interrupts.
        input.enable_interrupts(PinInterruptEdge::Either).unwrap();
        gpio_irqs.set(0);
        for _ in 0..3 {
            out.set().unwrap();
            settle();
            out.clear().unwrap();
            settle();
        }
        let n = gpio_irqs.get();
        let _ = writeln!(Console::writer(), "gpio test: either-edge interrupts: {n}");
        check("either edge: 6 interrupts for 3 pulses", n == 6, &fails);

        // Rising only: 3 pulses give 3 interrupts, each reporting high.
        input.disable_interrupts().unwrap();
        input.enable_interrupts(PinInterruptEdge::Rising).unwrap();
        gpio_irqs.set(0);
        for _ in 0..3 {
            out.set().unwrap();
            settle();
            out.clear().unwrap();
            settle();
        }
        let n = gpio_irqs.get();
        let _ = writeln!(Console::writer(), "gpio test: rising-edge interrupts: {n}");
        check("rising edge: 3 interrupts for 3 pulses", n == 3, &fails);

        // Falling only.
        input.disable_interrupts().unwrap();
        input.enable_interrupts(PinInterruptEdge::Falling).unwrap();
        gpio_irqs.set(0);
        for _ in 0..3 {
            out.set().unwrap();
            settle();
            out.clear().unwrap();
            settle();
        }
        let n = gpio_irqs.get();
        let _ = writeln!(Console::writer(), "gpio test: falling-edge interrupts: {n}");
        check("falling edge: 3 interrupts for 3 pulses", n == 3, &fails);
        input.disable_interrupts().unwrap();

        // Button on D2, driven through D24.
        let mut drive = Gpio::get_pin(24).unwrap();
        let mut drive = drive.make_output().unwrap();
        drive.set().unwrap();
        settle();
        check("button released while D24 high", Buttons::is_released(0), &fails);
        Buttons::enable_interrupts(0).unwrap();
        drive.clear().unwrap();
        settle();
        check("button pressed while D24 low", Buttons::is_pressed(0), &fails);
        drive.set().unwrap();
        settle();
        let (p, r) = (presses.get(), releases.get());
        let _ = writeln!(Console::writer(), "gpio test: button events: {p} pressed, {r} released");
        check("one press and one release event", p == 1 && r == 1, &fails);

        // A pulse far shorter than the 10 ms debounce period is filtered.
        presses.set(0);
        releases.set(0);
        drive.clear().unwrap();
        drive.set().unwrap();
        settle();
        let (p, r) = (presses.get(), releases.get());
        let _ = writeln!(Console::writer(), "gpio test: glitch events: {p} pressed, {r} released");
        check("debounce filters a short glitch", p == 0 && r == 0, &fails);
        Buttons::disable_interrupts(0).unwrap();

        let f = fails.get();
        let _ = writeln!(Console::writer(), "gpio test: done, {f} failures");
    });
    });
}
