// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Interrupt numbers of the SAM3X.
//!
//! On the SAM3X the NVIC interrupt number of a peripheral is its peripheral
//! identifier (datasheet section 9.1, "Peripheral Identifiers"). Only the
//! interrupts used by this crate are listed.

pub const SUPC: u32 = 0;
pub const RSTC: u32 = 1;
pub const RTC: u32 = 2;
pub const RTT: u32 = 3;
pub const WDT: u32 = 4;
pub const PMC: u32 = 5;
pub const EFC0: u32 = 6;
pub const EFC1: u32 = 7;
pub const UART: u32 = 8;
pub const PIOA: u32 = 11;
pub const PIOB: u32 = 12;
pub const PIOC: u32 = 13;
pub const PIOD: u32 = 14;
