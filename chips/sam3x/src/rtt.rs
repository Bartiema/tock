// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Real-time Timer (RTT) used as a 32 kHz alarm.
//!
//! The RTT is a 32-bit counter clocked by the slow clock through a 16-bit
//! prescaler. It keeps counting when the master clock changes and in Wait
//! mode, which is why it backs the kernel alarm rather than a timer counter on
//! MCK.
//!
//! The prescaler is set to 1, so the counter runs at the slow clock rate.
//!
//! The status register is only cleared two slow clock cycles after it is read
//! (datasheet section 13.4), and the alarm interrupt is asserted while both
//! the alarm flag and its enable are set. Re-enabling the interrupt within
//! those two cycles would make it fire again at once, and keep firing: the
//! kernel services the RTT (interrupt 3) before the UART (8), so such a storm
//! starves the UART until its receive register overruns. As the datasheet
//! prescribes, the handler therefore disables the alarm interrupt, and it is
//! re-enabled only once the counter has moved past the stale window. When an
//! alarm is set inside that window, a deferred call enables it later and then
//! catches an expiry that passed in the meantime.

use core::cell::Cell;

use kernel::ErrorCode;
use kernel::deferred_call::{DeferredCall, DeferredCallClient};
use kernel::hil::time::{self, Alarm, Ticks, Ticks32, Time};
use kernel::utilities::StaticRef;
use kernel::utilities::cells::OptionalCell;
use kernel::utilities::registers::interfaces::{ReadWriteable, Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, register_bitfields};

#[repr(C)]
struct RttRegisters {
    /// Mode Register.
    mr: ReadWrite<u32, Mode::Register>,
    /// Alarm Register. The alarm fires when the counter equals ALMV + 1.
    ar: ReadWrite<u32>,
    /// Value Register.
    vr: ReadOnly<u32>,
    /// Status Register. Cleared on read.
    sr: ReadOnly<u32, Status::Register>,
}

register_bitfields![u32,
    Mode [
        /// Prescaler: the counter increments every RTPRES slow clock cycles,
        /// or every 2^16 cycles if RTPRES is 0.
        RTPRES OFFSET(0) NUMBITS(16) [],
        /// Alarm interrupt enable.
        ALMIEN OFFSET(16) NUMBITS(1) [],
        /// Increment interrupt enable.
        RTTINCIEN OFFSET(17) NUMBITS(1) [],
        /// Restart the prescaler with the new value and reset the counter.
        RTTRST OFFSET(18) NUMBITS(1) []
    ],
    Status [
        /// Alarm reached since the last read.
        ALMS OFFSET(0) NUMBITS(1) [],
        /// Counter incremented since the last read.
        RTTINC OFFSET(1) NUMBITS(1) []
    ]
];

// ### Safety
//
// 0x400E1A30 is the base address of the RTT register block in the SAM3X
// memory map (datasheet section 7). The register layout matches
// `RttRegisters`, and the block is valid for the lifetime of the program.
const RTT_BASE: StaticRef<RttRegisters> =
    unsafe { StaticRef::new(0x400E_1A30 as *const RttRegisters) };

/// Smallest distance between now and an alarm. The counter value and the
/// alarm comparison are synchronized to the slow clock, so an alarm only a
/// tick or two away could be missed.
const MIN_DT: u32 = 3;

/// Ticks after a status register read before its flags are surely clear: two
/// slow clock cycles, plus one because the read can happen just before a tick.
const STATUS_CLEAR_TICKS: u32 = 3;

/// Upper bound on counter reads while waiting for a counter reset.
const RESET_POLLS: u32 = 100_000;

pub struct Rtt<'a> {
    registers: StaticRef<RttRegisters>,
    client: OptionalCell<&'a dyn time::AlarmClient>,
    armed: Cell<bool>,
    reference: Cell<u32>,
    expiry: Cell<u32>,
    /// Counter value until which the alarm flag may still read as set from
    /// the last status register read.
    stale_until: Cell<u32>,
    deferred_call: DeferredCall,
}

impl Rtt<'_> {
    pub fn new() -> Self {
        Self {
            registers: RTT_BASE,
            client: OptionalCell::empty(),
            armed: Cell::new(false),
            reference: Cell::new(0),
            expiry: Cell::new(0),
            stale_until: Cell::new(0),
            deferred_call: DeferredCall::new(),
        }
    }

    /// Reset the counter and run it at the slow clock rate.
    ///
    /// The RTT is not reset with the core, so the counter may hold an old
    /// value. The reset only takes effect two slow clock cycles after the
    /// write (datasheet section 13.4), so wait for it; otherwise an alarm set
    /// right after this would be computed from the old count.
    pub fn start(&self) {
        let before = self.now().into_u32();
        self.registers
            .mr
            .write(Mode::RTPRES.val(1) + Mode::RTTRST::SET);
        // Two slow clock cycles are about 5000 MCK cycles at 84 MHz; the bound
        // only guards against a stopped slow clock.
        for _ in 0..RESET_POLLS {
            let now = self.now().into_u32();
            if now < before || now <= MIN_DT {
                break;
            }
        }
        self.stale_until.set(self.now().into_u32());
    }

    pub fn handle_interrupt(&self) {
        // Reading the status register clears it, two slow clock cycles later.
        let _ = self.registers.sr.get();
        self.registers.mr.modify(Mode::ALMIEN::CLEAR);
        let now = self.now();
        self.stale_until
            .set(now.into_u32().wrapping_add(STATUS_CLEAR_TICKS));

        if !self.armed.get() {
            return;
        }
        if self.has_expired(now) {
            self.armed.set(false);
            self.client.map(|client| client.alarm());
        } else {
            // A match one tick early (the datasheet describes the comparison
            // both against ALMV and ALMV + 1); wait for the real expiry.
            self.enable_when_clear();
        }
    }

    fn has_expired(&self, now: Ticks32) -> bool {
        !now.within_range(
            Ticks32::from(self.reference.get()),
            Ticks32::from(self.expiry.get()),
        )
    }

    /// Enable the alarm interrupt, or defer that until the status register
    /// is clear.
    fn enable_when_clear(&self) {
        let now = self.now();
        let stale_from = Ticks32::from(self.stale_until.get().wrapping_sub(STATUS_CLEAR_TICKS));
        if now.within_range(stale_from, Ticks32::from(self.stale_until.get())) {
            self.deferred_call.set();
        } else {
            self.registers.mr.modify(Mode::ALMIEN::SET);
            // The comparison is for equality, so an expiry that passed while
            // the interrupt was off would never match. Report it from the
            // deferred call: this may run inside `set_alarm`, and clients
            // must not be called back from a downcall.
            if self.has_expired(self.now()) {
                self.registers.mr.modify(Mode::ALMIEN::CLEAR);
                self.deferred_call.set();
            }
        }
    }
}

impl DeferredCallClient for Rtt<'_> {
    fn register(&'static self) {
        self.deferred_call.register(self);
    }

    fn handle_deferred_call(&self) {
        if !self.armed.get() {
            return;
        }
        if self.has_expired(self.now()) {
            self.registers.mr.modify(Mode::ALMIEN::CLEAR);
            self.armed.set(false);
            self.client.map(|client| client.alarm());
        } else {
            self.enable_when_clear();
        }
    }
}

impl Time for Rtt<'_> {
    type Frequency = time::Freq32KHz;
    type Ticks = Ticks32;

    fn now(&self) -> Ticks32 {
        // The counter is updated asynchronously to the bus clock. The datasheet
        // asks for two consecutive reads to agree.
        let mut value = self.registers.vr.get();
        loop {
            let again = self.registers.vr.get();
            if again == value {
                return Ticks32::from(value);
            }
            value = again;
        }
    }
}

impl<'a> Alarm<'a> for Rtt<'a> {
    fn set_alarm_client(&self, client: &'a dyn time::AlarmClient) {
        self.client.set(client);
    }

    fn set_alarm(&self, reference: Ticks32, dt: Ticks32) {
        let mut reference = reference;
        let mut expiry = reference.wrapping_add(dt);
        let now = self.now();
        if !now.within_range(reference, expiry) || expiry.wrapping_sub(now).into_u32() < MIN_DT {
            reference = now;
            expiry = now.wrapping_add(Ticks32::from(MIN_DT));
        }

        // The alarm value must not change while its interrupt is enabled.
        self.registers.mr.modify(Mode::ALMIEN::CLEAR);
        self.reference.set(reference.into_u32());
        self.expiry.set(expiry.into_u32());
        // The alarm flag is set when the counter reaches ALMV + 1.
        self.registers.ar.set(expiry.into_u32().wrapping_sub(1));
        self.armed.set(true);
        self.enable_when_clear();
    }

    fn get_alarm(&self) -> Ticks32 {
        Ticks32::from(self.expiry.get())
    }

    fn disarm(&self) -> Result<(), ErrorCode> {
        self.registers.mr.modify(Mode::ALMIEN::CLEAR);
        self.armed.set(false);
        Ok(())
    }

    fn is_armed(&self) -> bool {
        self.armed.get()
    }

    fn minimum_dt(&self) -> Ticks32 {
        Ticks32::from(MIN_DT)
    }
}
