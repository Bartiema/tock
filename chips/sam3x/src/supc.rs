// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Supply Controller (SUPC).
//!
//! Only slow clock source selection is supported. The slow clock drives the
//! Real-time Timer, so selecting the 32.768 kHz crystal makes the alarm
//! accurate.
//!
//! After the switch command, the SUPC enables the crystal oscillator, waits a
//! fixed 32,768 slow clock cycles (about one second on the RC oscillator) and
//! then switches, whether or not the crystal oscillates (datasheet section
//! 16.4.2). Only a VDDBU power-on reset switches back to the RC oscillator, so
//! only select the crystal on boards that have one.

use kernel::utilities::StaticRef;
use kernel::utilities::registers::interfaces::{Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly, register_bitfields};

#[repr(C)]
struct SupcRegisters {
    /// Control Register.
    cr: WriteOnly<u32, Control::Register>,
    /// Supply Monitor Mode Register.
    smmr: ReadWrite<u32>,
    /// Mode Register.
    mr: ReadWrite<u32>,
    /// Wake-up Mode Register.
    wumr: ReadWrite<u32>,
    /// Wake-up Inputs Register.
    wuir: ReadWrite<u32>,
    /// Status Register.
    sr: ReadOnly<u32, Status::Register>,
}

register_bitfields![u32,
    Control [
        VROFF OFFSET(2) NUMBITS(1) [],
        XTALSEL OFFSET(3) NUMBITS(1) [],
        KEY OFFSET(24) NUMBITS(8) [
            PASSWD = 0xA5
        ]
    ],
    Status [
        /// Slow clock source: 0 = internal RC, 1 = 32 kHz crystal.
        OSCSEL OFFSET(7) NUMBITS(1) []
    ]
];

// ### Safety
//
// 0x400E1A10 is the base address of the SUPC register block in the SAM3X
// memory map (datasheet section 7). The register layout matches
// `SupcRegisters`, and the block is valid for the lifetime of the program.
const SUPC_BASE: StaticRef<SupcRegisters> =
    unsafe { StaticRef::new(0x400E_1A10 as *const SupcRegisters) };

pub struct Supc {
    registers: StaticRef<SupcRegisters>,
}

impl Supc {
    pub const fn new() -> Self {
        Self {
            registers: SUPC_BASE,
        }
    }

    /// Switch the slow clock to the external 32.768 kHz crystal.
    ///
    /// The switch happens about one second later, see the module
    /// documentation. It cannot be undone except by a VDDBU power-on reset.
    pub fn select_crystal_slow_clock(&self) {
        if !self.is_crystal_selected() {
            self.registers
                .cr
                .write(Control::KEY::PASSWD + Control::XTALSEL::SET);
        }
    }

    /// Returns `true` once the slow clock runs from the 32 kHz crystal.
    pub fn is_crystal_selected(&self) -> bool {
        self.registers.sr.is_set(Status::OSCSEL)
    }
}
