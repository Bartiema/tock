// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Peripheral implementations for the Microchip (Atmel) SAM3X MCU family.
//!
//! The driver set is intentionally minimal: clocks (PMC, SUPC, EEFC),
//! watchdog, GPIO (PIO), the debug UART and an alarm on the Real-time Timer.
//!
//! Datasheet: <https://ww1.microchip.com/downloads/en/DeviceDoc/Atmel-11057-32-bit-Cortex-M3-Microcontroller-SAM3X-SAM3A_Datasheet.pdf>

#![no_std]

pub mod chip;
pub mod eefc;
pub mod nvic;
pub mod pio;
pub mod pmc;
pub mod rtt;
pub mod supc;
pub mod uart;
pub mod wdt;

use cortexm3::{CortexM3, CortexMVariant, initialize_ram_jump_to_main, unhandled_interrupt};

extern "C" {
    // _estack is not really a function, but it makes the types work
    // You should never actually invoke it!!
    fn _estack();
}

#[cfg_attr(
    all(target_arch = "arm", target_os = "none"),
    link_section = ".vectors"
)]
// used Ensures that the symbol is kept until the final binary
#[cfg_attr(all(target_arch = "arm", target_os = "none"), used)]
pub static BASE_VECTORS: [unsafe extern "C" fn(); 16] = [
    _estack,
    initialize_ram_jump_to_main,
    unhandled_interrupt,          // NMI
    CortexM3::HARD_FAULT_HANDLER, // Hard Fault
    unhandled_interrupt,          // MemManage
    unhandled_interrupt,          // BusFault
    unhandled_interrupt,          // UsageFault
    unhandled_interrupt,
    unhandled_interrupt,
    unhandled_interrupt,
    unhandled_interrupt,
    CortexM3::SVC_HANDLER, // SVC
    unhandled_interrupt,   // DebugMon
    unhandled_interrupt,
    unhandled_interrupt,       // PendSV
    CortexM3::SYSTICK_HANDLER, // SysTick
];

// The SAM3X has 45 peripheral interrupts. The NVIC interrupt number of a
// peripheral equals its peripheral identifier (datasheet section 9.1).
#[cfg_attr(all(target_arch = "arm", target_os = "none"), link_section = ".irqs")]
// `used` ensures that the symbol is kept until the final binary.
#[cfg_attr(all(target_arch = "arm", target_os = "none"), used)]
pub static IRQS: [unsafe extern "C" fn(); 45] = [CortexM3::GENERIC_ISR; 45];
