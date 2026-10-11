// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Power Management Controller (PMC).
//!
//! Configures the master clock (MCK) and gates peripheral clocks. After reset
//! the SAM3X runs from the 4 MHz internal RC oscillator. Boards switch to the
//! PLL with [`Pmc::use_crystal_pll`].

use core::cell::Cell;

use kernel::ErrorCode;
use kernel::utilities::StaticRef;
use kernel::utilities::registers::interfaces::{ReadWriteable, Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly, register_bitfields};

use crate::eefc::Eefc;

#[repr(C)]
struct PmcRegisters {
    /// System Clock Enable Register.
    scer: WriteOnly<u32>,
    /// System Clock Disable Register.
    scdr: WriteOnly<u32>,
    /// System Clock Status Register.
    scsr: ReadOnly<u32>,
    _reserved0: u32,
    /// Peripheral Clock Enable Register 0.
    pcer0: WriteOnly<u32>,
    /// Peripheral Clock Disable Register 0.
    pcdr0: WriteOnly<u32>,
    /// Peripheral Clock Status Register 0.
    pcsr0: ReadOnly<u32>,
    /// UTMI Clock Register.
    ckgr_uckr: ReadWrite<u32>,
    /// Main Oscillator Register.
    ckgr_mor: ReadWrite<u32, MainOscillator::Register>,
    /// Main Clock Frequency Register.
    ckgr_mcfr: ReadOnly<u32>,
    /// PLLA Register.
    ckgr_pllar: ReadWrite<u32, PllA::Register>,
    _reserved1: u32,
    /// Master Clock Register.
    mckr: ReadWrite<u32, MasterClock::Register>,
    _reserved2: u32,
    /// USB Clock Register.
    usb: ReadWrite<u32>,
    _reserved3: u32,
    /// Programmable Clock Registers.
    pck: [ReadWrite<u32>; 3],
    _reserved4: [u32; 5],
    /// Interrupt Enable Register.
    ier: WriteOnly<u32>,
    /// Interrupt Disable Register.
    idr: WriteOnly<u32>,
    /// Status Register.
    sr: ReadOnly<u32, Status::Register>,
    /// Interrupt Mask Register.
    imr: ReadOnly<u32>,
    /// Fast Startup Mode Register.
    fsmr: ReadWrite<u32>,
    /// Fast Startup Polarity Register.
    fspr: ReadWrite<u32>,
    /// Fault Output Clear Register.
    focr: WriteOnly<u32>,
    _reserved5: [u32; 26],
    /// Write Protect Mode Register.
    wpmr: ReadWrite<u32>,
    /// Write Protect Status Register.
    wpsr: ReadOnly<u32>,
    _reserved6: [u32; 5],
    /// Peripheral Clock Enable Register 1.
    pcer1: WriteOnly<u32>,
    /// Peripheral Clock Disable Register 1.
    pcdr1: WriteOnly<u32>,
    /// Peripheral Clock Status Register 1.
    pcsr1: ReadOnly<u32>,
    /// Peripheral Control Register.
    pcr: ReadWrite<u32>,
}

register_bitfields![u32,
    MainOscillator [
        /// Main crystal oscillator enable.
        MOSCXTEN OFFSET(0) NUMBITS(1) [],
        /// Main crystal oscillator bypass.
        MOSCXTBY OFFSET(1) NUMBITS(1) [],
        /// Main on-chip RC oscillator enable.
        MOSCRCEN OFFSET(3) NUMBITS(1) [],
        /// Main on-chip RC oscillator frequency selection.
        MOSCRCF OFFSET(4) NUMBITS(3) [
            Rc4MHz = 0,
            Rc8MHz = 1,
            Rc12MHz = 2
        ],
        /// Main crystal oscillator start-up time, in units of 8 slow clock
        /// cycles.
        MOSCXTST OFFSET(8) NUMBITS(8) [],
        /// Write access password.
        KEY OFFSET(16) NUMBITS(8) [
            PASSWD = 0x37
        ],
        /// Main oscillator selection: 0 = RC, 1 = crystal.
        MOSCSEL OFFSET(24) NUMBITS(1) [],
        /// Clock failure detector enable.
        CFDEN OFFSET(25) NUMBITS(1) []
    ],
    PllA [
        DIVA OFFSET(0) NUMBITS(8) [],
        PLLACOUNT OFFSET(8) NUMBITS(6) [],
        /// The PLL multiplies by MULA + 1. Zero disables the PLL.
        MULA OFFSET(16) NUMBITS(11) [],
        /// Must be written to 1.
        ONE OFFSET(29) NUMBITS(1) []
    ],
    MasterClock [
        CSS OFFSET(0) NUMBITS(2) [
            SlowClock = 0,
            MainClock = 1,
            PllaClock = 2,
            UpllClock = 3
        ],
        PRES OFFSET(4) NUMBITS(3) [
            Clk1 = 0,
            Clk2 = 1,
            Clk4 = 2,
            Clk8 = 3,
            Clk16 = 4,
            Clk32 = 5,
            Clk64 = 6,
            Clk3 = 7
        ],
        PLLADIV2 OFFSET(12) NUMBITS(1) [],
        UPLLDIV2 OFFSET(13) NUMBITS(1) []
    ],
    Status [
        /// Main crystal oscillator stabilized.
        MOSCXTS OFFSET(0) NUMBITS(1) [],
        /// PLLA locked.
        LOCKA OFFSET(1) NUMBITS(1) [],
        /// Master clock ready.
        MCKRDY OFFSET(3) NUMBITS(1) [],
        /// UTMI PLL locked.
        LOCKU OFFSET(6) NUMBITS(1) [],
        /// Slow clock oscillator selection.
        OSCSELS OFFSET(7) NUMBITS(1) [],
        /// Main oscillator selection done.
        MOSCSELS OFFSET(16) NUMBITS(1) [],
        /// Main on-chip RC oscillator stabilized.
        MOSCRCS OFFSET(17) NUMBITS(1) []
    ]
];

// Catch transcription errors in the register layout (datasheet section 28.15).
const _: () = assert!(core::mem::offset_of!(PmcRegisters, mckr) == 0x30);
const _: () = assert!(core::mem::offset_of!(PmcRegisters, sr) == 0x68);
const _: () = assert!(core::mem::offset_of!(PmcRegisters, wpmr) == 0xE4);
const _: () = assert!(core::mem::offset_of!(PmcRegisters, pcer1) == 0x100);
const _: () = assert!(core::mem::offset_of!(PmcRegisters, pcr) == 0x10C);

// ### Safety
//
// 0x400E0600 is the base address of the PMC register block in the SAM3X
// memory map (datasheet section 7). The register layout matches
// `PmcRegisters`, and the block is valid for the lifetime of the program.
const PMC_BASE: StaticRef<PmcRegisters> =
    unsafe { StaticRef::new(0x400E_0600 as *const PmcRegisters) };

/// Write protection key: "PMC" in ASCII (datasheet section 28.15.19).
const WPKEY_PMC: u32 = 0x50_4D43;

/// Master clock frequency after reset: the 4 MHz internal RC oscillator.
const RESET_MCK_HZ: u32 = 4_000_000;

/// Valid PLLA input (after DIVA) and output frequency ranges (datasheet
/// Table 45-24, "PLLA Characteristics").
const PLLA_IN_MIN_HZ: u32 = 1_000_000;
const PLLA_IN_MAX_HZ: u32 = 40_000_000;
const PLLA_MIN_HZ: u32 = 80_000_000;
const PLLA_MAX_HZ: u32 = 240_000_000;

/// Upper bound on status register polls before a clock switch is reported as
/// failed. Generous: the slowest step, crystal start-up, takes a few
/// milliseconds.
const MAX_POLLS: u32 = 1_000_000;

/// Peripheral identifiers (datasheet section 9.1).
///
/// These are used both to gate peripheral clocks and as NVIC interrupt numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PeripheralId {
    Supc = 0,
    Rstc = 1,
    Rtc = 2,
    Rtt = 3,
    Wdt = 4,
    Pmc = 5,
    Efc0 = 6,
    Efc1 = 7,
    Uart = 8,
    Smc = 9,
    PioA = 11,
    PioB = 12,
    PioC = 13,
    PioD = 14,
    Usart0 = 17,
    Usart1 = 18,
    Usart2 = 19,
    Usart3 = 20,
    Hsmci = 21,
    Twi0 = 22,
    Twi1 = 23,
    Spi0 = 24,
    Ssc = 26,
    Tc0 = 27,
    Tc1 = 28,
    Tc2 = 29,
    Tc3 = 30,
    Tc4 = 31,
    Tc5 = 32,
    Tc6 = 33,
    Tc7 = 34,
    Tc8 = 35,
    Pwm = 36,
    Adc = 37,
    Dacc = 38,
    Dmac = 39,
    Uotghs = 40,
    Trng = 41,
    Emac = 42,
    Can0 = 43,
    Can1 = 44,
}

impl PeripheralId {
    /// Peripherals below this identifier are always clocked and have no bit
    /// in the peripheral clock registers.
    ///
    /// Table 9-1 of the datasheet also lists the UART (8) as not
    /// PMC-controlled, but section 34.4.2 says its clock must be enabled in
    /// the PMC, so it is treated as gated.
    const FIRST_GATED: u32 = 8;
}

/// Frequencies of the main on-chip RC oscillator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcFrequency {
    Rc4MHz,
    Rc8MHz,
    Rc12MHz,
}

impl RcFrequency {
    pub const fn hz(self) -> u32 {
        match self {
            RcFrequency::Rc4MHz => 4_000_000,
            RcFrequency::Rc8MHz => 8_000_000,
            RcFrequency::Rc12MHz => 12_000_000,
        }
    }
}

pub struct Pmc {
    registers: StaticRef<PmcRegisters>,
    mck_hz: Cell<u32>,
}

impl Pmc {
    pub const fn new() -> Self {
        Self {
            registers: PMC_BASE,
            mck_hz: Cell::new(RESET_MCK_HZ),
        }
    }

    /// The current master clock (MCK) frequency in Hz.
    ///
    /// Peripheral clocks run at MCK, so drivers use this to compute baud
    /// rates and timer reloads.
    pub fn master_clock_hz(&self) -> u32 {
        self.mck_hz.get()
    }

    /// Run the master clock from PLLA, fed by the main crystal oscillator.
    ///
    /// MCK = `crystal_hz` * `mul` / `div` / 2. For the Arduino Due, a 12 MHz
    /// crystal with `mul` = 14 and `div` = 1 gives a 168 MHz PLL and an 84 MHz
    /// master clock.
    ///
    /// Raises the flash wait states before switching. Returns `INVAL` if the
    /// PLL output is out of range, and `FAIL` if an oscillator or the PLL does
    /// not become ready.
    pub fn use_crystal_pll(
        &self,
        eefc: &Eefc,
        crystal_hz: u32,
        mul: u32,
        div: u32,
    ) -> Result<(), ErrorCode> {
        if mul < 2 || mul > 2048 || div == 0 || div > 255 {
            return Err(ErrorCode::INVAL);
        }
        let plla_in_hz = crystal_hz / div;
        if !(PLLA_IN_MIN_HZ..=PLLA_IN_MAX_HZ).contains(&plla_in_hz) {
            return Err(ErrorCode::INVAL);
        }
        let plla_hz = plla_in_hz.checked_mul(mul).ok_or(ErrorCode::INVAL)?;
        if !(PLLA_MIN_HZ..=PLLA_MAX_HZ).contains(&plla_hz) {
            return Err(ErrorCode::INVAL);
        }
        let mck_hz = plla_hz / 2;
        let regs = &*self.registers;
        self.disable_write_protection();

        // Flash must be slowed down before the clock is raised.
        eefc.set_wait_states(Eefc::wait_states_for(mck_hz));

        // Start the crystal oscillator, keeping the RC oscillator running
        // until the switch is done.
        if !regs.ckgr_mor.is_set(MainOscillator::MOSCSEL) {
            regs.ckgr_mor.write(
                MainOscillator::KEY::PASSWD
                    + MainOscillator::MOSCXTST.val(8)
                    + MainOscillator::MOSCRCEN::SET
                    + MainOscillator::MOSCXTEN::SET,
            );
            self.wait_for(Status::MOSCXTS)?;
        }

        // Select the crystal as the main clock.
        regs.ckgr_mor.write(
            MainOscillator::KEY::PASSWD
                + MainOscillator::MOSCXTST.val(8)
                + MainOscillator::MOSCRCEN::SET
                + MainOscillator::MOSCXTEN::SET
                + MainOscillator::MOSCSEL::SET,
        );
        self.wait_for(Status::MOSCSELS)?;

        // Run MCK from the main clock while the PLL is reprogrammed.
        regs.mckr.modify(MasterClock::CSS::MainClock);
        self.wait_for(Status::MCKRDY)?;

        regs.ckgr_pllar.write(
            PllA::ONE::SET
                + PllA::MULA.val(mul - 1)
                + PllA::PLLACOUNT.val(0x3F)
                + PllA::DIVA.val(div),
        );
        self.wait_for(Status::LOCKA)?;

        // When switching to the PLL, the prescaler must be set first, then the
        // source (datasheet section 28.12, "Programming Sequence").
        regs.mckr
            .write(MasterClock::PRES::Clk2 + MasterClock::CSS::MainClock);
        self.wait_for(Status::MCKRDY)?;
        regs.mckr
            .write(MasterClock::PRES::Clk2 + MasterClock::CSS::PllaClock);
        self.wait_for(Status::MCKRDY)?;

        self.mck_hz.set(mck_hz);
        Ok(())
    }

    /// Run the master clock from the main on-chip RC oscillator, then stop
    /// the PLL and the main crystal oscillator and lower the flash wait
    /// states.
    ///
    /// The RC oscillator is less accurate than the crystal (see the
    /// datasheet's electrical characteristics), which matters for UART baud
    /// rates. Undo with [`Pmc::use_crystal_pll`].
    pub fn use_main_rc(&self, eefc: &Eefc, frequency: RcFrequency) -> Result<(), ErrorCode> {
        let regs = &*self.registers;
        self.disable_write_protection();

        // Enable the RC oscillator and wait until it runs, then select its
        // frequency and wait again. Setting the frequency in the same write
        // that enables it does not take effect.
        regs.ckgr_mor
            .modify(MainOscillator::KEY::PASSWD + MainOscillator::MOSCRCEN::SET);
        self.wait_for(Status::MOSCRCS)?;
        let rcf = match frequency {
            RcFrequency::Rc4MHz => MainOscillator::MOSCRCF::Rc4MHz,
            RcFrequency::Rc8MHz => MainOscillator::MOSCRCF::Rc8MHz,
            RcFrequency::Rc12MHz => MainOscillator::MOSCRCF::Rc12MHz,
        };
        regs.ckgr_mor.modify(MainOscillator::KEY::PASSWD + rcf);
        self.wait_for(Status::MOSCRCS)?;

        // Leave the PLL: the source first, then the prescaler (datasheet
        // section 28.12).
        regs.mckr.modify(MasterClock::CSS::MainClock);
        self.wait_for(Status::MCKRDY)?;
        regs.mckr.modify(MasterClock::PRES::Clk1);
        self.wait_for(Status::MCKRDY)?;

        // Switch the main clock from the crystal to the RC oscillator.
        regs.ckgr_mor
            .modify(MainOscillator::KEY::PASSWD + MainOscillator::MOSCSEL::CLEAR);
        self.wait_for(Status::MOSCSELS)?;
        self.mck_hz.set(frequency.hz());

        // Stop the crystal and the PLL.
        regs.ckgr_mor
            .modify(MainOscillator::KEY::PASSWD + MainOscillator::MOSCXTEN::CLEAR);
        regs.ckgr_pllar.write(PllA::ONE::SET + PllA::MULA.val(0));

        // Flash may be faster now that the clock is slower.
        eefc.set_wait_states(Eefc::wait_states_for(frequency.hz()));
        Ok(())
    }

    /// Disable PMC write protection.
    ///
    /// With protection on, writes to the clock and peripheral clock registers
    /// are silently ignored. Software that ran before the kernel, such as a
    /// debugger's flash algorithm, may leave it on, and a probe reset does not
    /// reset the PMC.
    pub fn disable_write_protection(&self) {
        self.registers.wpmr.set(WPKEY_PMC << 8);
    }

    fn wait_for(
        &self,
        flag: kernel::utilities::registers::Field<u32, Status::Register>,
    ) -> Result<(), ErrorCode> {
        for _ in 0..MAX_POLLS {
            if self.registers.sr.is_set(flag) {
                return Ok(());
            }
        }
        Err(ErrorCode::FAIL)
    }

    /// Enable the clock of a peripheral.
    pub fn enable_peripheral_clock(&self, id: PeripheralId) {
        self.disable_write_protection();
        let id = id as u32;
        if id < PeripheralId::FIRST_GATED {
            // Always clocked.
        } else if id < 32 {
            self.registers.pcer0.set(1 << id);
        } else {
            self.registers.pcer1.set(1 << (id - 32));
        }
    }

    /// Disable the clock of a peripheral.
    pub fn disable_peripheral_clock(&self, id: PeripheralId) {
        self.disable_write_protection();
        let id = id as u32;
        if id < PeripheralId::FIRST_GATED {
            // Cannot be gated.
        } else if id < 32 {
            self.registers.pcdr0.set(1 << id);
        } else {
            self.registers.pcdr1.set(1 << (id - 32));
        }
    }

    /// Returns `true` if the clock of a peripheral is running.
    pub fn is_peripheral_clock_enabled(&self, id: PeripheralId) -> bool {
        let id = id as u32;
        if id < PeripheralId::FIRST_GATED {
            true
        } else if id < 32 {
            self.registers.pcsr0.get() & (1 << id) != 0
        } else {
            self.registers.pcsr1.get() & (1 << (id - 32)) != 0
        }
    }
}
