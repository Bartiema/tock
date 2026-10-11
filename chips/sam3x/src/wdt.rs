// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Watchdog Timer (WDT).
//!
//! The SAM3X watchdog runs from reset with a period of about 16 seconds, and
//! its mode register can be written only once after reset. This driver only
//! supports disabling it, which must happen early during boot.

use kernel::utilities::StaticRef;
use kernel::utilities::registers::interfaces::{Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly, register_bitfields};

#[repr(C)]
struct WdtRegisters {
    /// Control Register.
    cr: WriteOnly<u32, Control::Register>,
    /// Mode Register. Write-once after reset.
    mr: ReadWrite<u32, Mode::Register>,
    /// Status Register.
    sr: ReadOnly<u32>,
}

register_bitfields![u32,
    Control [
        WDRSTT OFFSET(0) NUMBITS(1) [],
        KEY OFFSET(24) NUMBITS(8) [
            PASSWD = 0xA5
        ]
    ],
    Mode [
        WDV OFFSET(0) NUMBITS(12) [],
        WDFIEN OFFSET(12) NUMBITS(1) [],
        WDRSTEN OFFSET(13) NUMBITS(1) [],
        WDRPROC OFFSET(14) NUMBITS(1) [],
        WDDIS OFFSET(15) NUMBITS(1) [],
        WDD OFFSET(16) NUMBITS(12) [],
        WDDBGHLT OFFSET(28) NUMBITS(1) [],
        WDIDLEHLT OFFSET(29) NUMBITS(1) []
    ]
];

// ### Safety
//
// 0x400E1A50 is the base address of the WDT register block in the SAM3X
// memory map (datasheet section 7.1, "Product Mapping"). The register layout matches
// `WdtRegisters`, and the block is valid for the lifetime of the program.
const WDT_BASE: StaticRef<WdtRegisters> =
    unsafe { StaticRef::new(0x400E_1A50 as *const WdtRegisters) };

pub struct Wdt {
    registers: StaticRef<WdtRegisters>,
}

impl Wdt {
    pub const fn new() -> Self {
        Self {
            registers: WDT_BASE,
        }
    }

    /// Disable the watchdog.
    ///
    /// The mode register is write-once, so this only works if nothing has
    /// written it since reset.
    pub fn disable(&self) {
        self.registers.mr.write(Mode::WDDIS::SET);
    }

    /// Returns `true` if the watchdog is disabled.
    pub fn is_disabled(&self) -> bool {
        self.registers.mr.is_set(Mode::WDDIS)
    }
}
