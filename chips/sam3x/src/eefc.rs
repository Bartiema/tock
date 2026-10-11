// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Enhanced Embedded Flash Controller (EEFC).
//!
//! The SAM3X8E has two flash banks, each with its own controller. This driver
//! only configures the number of flash wait states, which must be raised
//! before the master clock is raised and may be lowered after it is lowered.

use kernel::utilities::StaticRef;
use kernel::utilities::registers::interfaces::ReadWriteable;
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly, register_bitfields};

#[repr(C)]
struct EefcRegisters {
    /// Flash Mode Register.
    fmr: ReadWrite<u32, FlashMode::Register>,
    /// Flash Command Register.
    fcr: WriteOnly<u32>,
    /// Flash Status Register.
    fsr: ReadOnly<u32>,
    /// Flash Result Register.
    frr: ReadOnly<u32>,
}

register_bitfields![u32,
    FlashMode [
        /// Ready interrupt enable.
        FRDY OFFSET(0) NUMBITS(1) [],
        /// Flash wait states. The number of cycles for a flash access is
        /// FWS + 1.
        FWS OFFSET(8) NUMBITS(4) [],
        /// Flash access mode: 0 for 128-bit access (performance), 1 for 64-bit
        /// access (power).
        FAM OFFSET(24) NUMBITS(1) []
    ]
];

// ### Safety
//
// 0x400E0A00 and 0x400E0C00 are the base addresses of the EEFC0 and EEFC1
// register blocks in the SAM3X memory map (datasheet section 7). The register
// layout matches `EefcRegisters`, and the blocks are valid for the lifetime of
// the program.
const EEFC0_BASE: StaticRef<EefcRegisters> =
    unsafe { StaticRef::new(0x400E_0A00 as *const EefcRegisters) };
const EEFC1_BASE: StaticRef<EefcRegisters> =
    unsafe { StaticRef::new(0x400E_0C00 as *const EefcRegisters) };

/// Maximum value of the FWS field that the datasheet permits.
const MAX_WAIT_STATES: u32 = 6;

pub struct Eefc {
    banks: [StaticRef<EefcRegisters>; 2],
}

impl Eefc {
    pub const fn new() -> Self {
        Self {
            banks: [EEFC0_BASE, EEFC1_BASE],
        }
    }

    /// Set the flash wait states (FWS) of both banks.
    ///
    /// A flash access then takes `wait_states + 1` cycles. Values above the
    /// datasheet maximum are clamped.
    pub fn set_wait_states(&self, wait_states: u32) {
        let fws = core::cmp::min(wait_states, MAX_WAIT_STATES);
        for bank in self.banks.iter() {
            bank.fmr.modify(FlashMode::FWS.val(fws));
        }
    }

    /// The minimum number of wait states (FWS) for a master clock frequency.
    ///
    /// From datasheet Table 45-62, "Embedded Flash Wait State", at
    /// VDDCORE = 1.80 V (the internal regulator's default output). FWS = 4
    /// covers up to 90 MHz, above the chip's 84 MHz maximum.
    pub const fn wait_states_for(mck_hz: u32) -> u32 {
        if mck_hz <= 19_000_000 {
            0
        } else if mck_hz <= 50_000_000 {
            1
        } else if mck_hz <= 64_000_000 {
            2
        } else if mck_hz <= 80_000_000 {
            3
        } else {
            4
        }
    }
}
