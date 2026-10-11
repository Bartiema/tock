// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Parallel Input/Output Controller (PIO).
//!
//! Each of the four PIO controllers (A to D on the SAM3X8E) drives up to 32
//! pins and has one interrupt line. Reading a controller's interrupt status
//! register clears the pending flag of every pin on it, so a [`Port`] reads it
//! once per interrupt and dispatches to all fired pins.
//!
//! Pin inputs can only be read and interrupts only work while the
//! controller's peripheral clock is on; see [`Port::enable_clock`].

use core::cell::Cell;

use kernel::deferred_call::{DeferredCall, DeferredCallClient};
use kernel::hil::gpio;
use kernel::utilities::StaticRef;
use kernel::utilities::cells::OptionalCell;
use kernel::utilities::registers::interfaces::{Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly};

use crate::pmc::{PeripheralId, Pmc};

/// Every register is a 32-bit bitmask with one bit per pin.
#[repr(C)]
struct PioRegisters {
    /// PIO Enable Register.
    per: WriteOnly<u32>,
    /// PIO Disable Register.
    pdr: WriteOnly<u32>,
    /// PIO Status Register.
    psr: ReadOnly<u32>,
    _reserved0: u32,
    /// Output Enable Register.
    oer: WriteOnly<u32>,
    /// Output Disable Register.
    odr: WriteOnly<u32>,
    /// Output Status Register.
    osr: ReadOnly<u32>,
    _reserved1: u32,
    /// Glitch Input Filter Enable Register.
    ifer: WriteOnly<u32>,
    /// Glitch Input Filter Disable Register.
    ifdr: WriteOnly<u32>,
    /// Glitch Input Filter Status Register.
    ifsr: ReadOnly<u32>,
    _reserved2: u32,
    /// Set Output Data Register.
    sodr: WriteOnly<u32>,
    /// Clear Output Data Register.
    codr: WriteOnly<u32>,
    /// Output Data Status Register.
    odsr: ReadWrite<u32>,
    /// Pin Data Status Register.
    pdsr: ReadOnly<u32>,
    /// Interrupt Enable Register.
    ier: WriteOnly<u32>,
    /// Interrupt Disable Register.
    idr: WriteOnly<u32>,
    /// Interrupt Mask Register.
    imr: ReadOnly<u32>,
    /// Interrupt Status Register. Cleared on read.
    isr: ReadOnly<u32>,
    /// Multi-driver Enable Register.
    mder: WriteOnly<u32>,
    /// Multi-driver Disable Register.
    mddr: WriteOnly<u32>,
    /// Multi-driver Status Register.
    mdsr: ReadOnly<u32>,
    _reserved3: u32,
    /// Pull-up Disable Register.
    pudr: WriteOnly<u32>,
    /// Pull-up Enable Register.
    puer: WriteOnly<u32>,
    /// Pad Pull-up Status Register. A 0 bit means the pull-up is enabled.
    pusr: ReadOnly<u32>,
    _reserved4: u32,
    /// Peripheral AB Select Register: 0 selects peripheral A, 1 peripheral B.
    absr: ReadWrite<u32>,
    _reserved5: [u32; 3],
    /// System Clock Glitch Input Filter Select Register.
    scifsr: WriteOnly<u32>,
    /// Debouncing Input Filter Select Register.
    difsr: WriteOnly<u32>,
    /// Glitch or Debouncing Input Filter Clock Selection Status Register.
    ifdgsr: ReadOnly<u32>,
    /// Slow Clock Divider Debouncing Register.
    scdr: ReadWrite<u32>,
    _reserved6: [u32; 4],
    /// Output Write Enable Register.
    ower: WriteOnly<u32>,
    /// Output Write Disable Register.
    owdr: WriteOnly<u32>,
    /// Output Write Status Register.
    owsr: ReadOnly<u32>,
    _reserved7: u32,
    /// Additional Interrupt Modes Enable Register.
    aimer: WriteOnly<u32>,
    /// Additional Interrupt Modes Disable Register.
    aimdr: WriteOnly<u32>,
    /// Additional Interrupt Modes Mask Register.
    aimmr: ReadOnly<u32>,
    _reserved8: u32,
    /// Edge Select Register.
    esr: WriteOnly<u32>,
    /// Level Select Register.
    lsr: WriteOnly<u32>,
    /// Edge/Level Status Register.
    elsr: ReadOnly<u32>,
    _reserved9: u32,
    /// Falling Edge/Low Level Select Register.
    fellsr: WriteOnly<u32>,
    /// Rising Edge/High Level Select Register.
    rehlsr: WriteOnly<u32>,
    /// Fall/Rise - Low/High Status Register.
    frlhsr: ReadOnly<u32>,
    _reserved10: u32,
    /// Lock Status Register.
    locksr: ReadOnly<u32>,
    /// Write Protect Mode Register.
    wpmr: ReadWrite<u32>,
    /// Write Protect Status Register.
    wpsr: ReadOnly<u32>,
}

// Catch transcription errors in the register layout (datasheet section 31.7).
const _: () = assert!(core::mem::offset_of!(PioRegisters, absr) == 0x70);
const _: () = assert!(core::mem::offset_of!(PioRegisters, scdr) == 0x8C);
const _: () = assert!(core::mem::offset_of!(PioRegisters, aimer) == 0xB0);
const _: () = assert!(core::mem::offset_of!(PioRegisters, wpsr) == 0xE8);

// ### Safety
//
// These are the base addresses of the PIOA to PIOD register blocks in the
// SAM3X memory map (datasheet section 7). The register layout matches
// `PioRegisters`, and the blocks are valid for the lifetime of the program.
const PIOA_BASE: StaticRef<PioRegisters> =
    unsafe { StaticRef::new(0x400E_0E00 as *const PioRegisters) };
const PIOB_BASE: StaticRef<PioRegisters> =
    unsafe { StaticRef::new(0x400E_1000 as *const PioRegisters) };
const PIOC_BASE: StaticRef<PioRegisters> =
    unsafe { StaticRef::new(0x400E_1200 as *const PioRegisters) };
const PIOD_BASE: StaticRef<PioRegisters> =
    unsafe { StaticRef::new(0x400E_1400 as *const PioRegisters) };

/// The PIO controllers of the SAM3X8E.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortId {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
}

impl PortId {
    const fn registers(self) -> StaticRef<PioRegisters> {
        match self {
            PortId::A => PIOA_BASE,
            PortId::B => PIOB_BASE,
            PortId::C => PIOC_BASE,
            PortId::D => PIOD_BASE,
        }
    }

    const fn peripheral_id(self) -> PeripheralId {
        match self {
            PortId::A => PeripheralId::PioA,
            PortId::B => PeripheralId::PioB,
            PortId::C => PeripheralId::PioC,
            PortId::D => PeripheralId::PioD,
        }
    }
}

/// Peripheral function of a pin when the PIO controller does not drive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peripheral {
    A,
    B,
}

pub struct Pin<'a> {
    port_id: PortId,
    pin: u8,
    /// The port that dispatches this pin's interrupts. Empty for standalone
    /// pins.
    port: OptionalCell<&'a Port<'a>>,
    client: OptionalCell<&'a dyn gpio::Client>,
}

impl Pin<'_> {
    /// Create a standalone handle to pin `pin` (0 to 31) of `port`.
    ///
    /// Boards normally use the pins owned by [`Ports`], which receive
    /// interrupts. A standalone pin is for contexts such as the panic handler,
    /// where those are not available.
    pub const fn new(port: PortId, pin: u32) -> Self {
        Self {
            port_id: port,
            pin: (pin % 32) as u8,
            port: OptionalCell::empty(),
            client: OptionalCell::empty(),
        }
    }

    fn registers(&self) -> StaticRef<PioRegisters> {
        self.port_id.registers()
    }

    fn mask(&self) -> u32 {
        1 << self.pin
    }

    /// Hand the pin to one of its peripheral functions.
    pub fn select_peripheral(&self, peripheral: Peripheral) {
        let regs = self.registers();
        let absr = regs.absr.get();
        match peripheral {
            Peripheral::A => regs.absr.set(absr & !self.mask()),
            Peripheral::B => regs.absr.set(absr | self.mask()),
        }
        regs.pdr.set(self.mask());
    }

    /// Enable or disable the debouncing input filter on this pin.
    ///
    /// The filter rejects pulses shorter than half a debounce period, which is
    /// set per port with [`Port::set_debounce_divider`]. It applies to both the
    /// input value and the interrupt.
    pub fn set_debounce(&self, enable: bool) {
        if enable {
            self.registers().difsr.set(self.mask());
            self.registers().ifer.set(self.mask());
        } else {
            self.registers().ifdr.set(self.mask());
        }
    }

    /// Enable or disable the glitch filter on this pin, which rejects pulses
    /// shorter than half a master clock cycle without the delay of the
    /// debouncing filter.
    pub fn set_glitch_filter(&self, enable: bool) {
        if enable {
            self.registers().scifsr.set(self.mask());
            self.registers().ifer.set(self.mask());
        } else {
            self.registers().ifdr.set(self.mask());
        }
    }

    fn handle_interrupt(&self) {
        self.client.map(|client| client.fired());
    }
}

impl gpio::Configure for Pin<'_> {
    fn configuration(&self) -> gpio::Configuration {
        if self.registers().psr.get() & self.mask() == 0 {
            gpio::Configuration::Function
        } else if self.registers().osr.get() & self.mask() != 0 {
            // The input stays readable while the output driver is on.
            gpio::Configuration::InputOutput
        } else {
            gpio::Configuration::Input
        }
    }

    fn make_output(&self) -> gpio::Configuration {
        self.registers().oer.set(self.mask());
        self.registers().per.set(self.mask());
        self.configuration()
    }

    fn disable_output(&self) -> gpio::Configuration {
        self.registers().odr.set(self.mask());
        self.configuration()
    }

    fn make_input(&self) -> gpio::Configuration {
        self.registers().per.set(self.mask());
        self.configuration()
    }

    fn disable_input(&self) -> gpio::Configuration {
        // The input buffer of a PIO pin is always on.
        self.configuration()
    }

    fn deactivate_to_low_power(&self) {
        self.registers().idr.set(self.mask());
        self.registers().odr.set(self.mask());
        self.registers().pudr.set(self.mask());
        self.registers().per.set(self.mask());
    }

    fn set_floating_state(&self, state: gpio::FloatingState) {
        match state {
            gpio::FloatingState::PullUp => self.registers().puer.set(self.mask()),
            // The SAM3X PIO has no pull-down resistors.
            gpio::FloatingState::PullDown | gpio::FloatingState::PullNone => {
                self.registers().pudr.set(self.mask())
            }
        }
    }

    fn floating_state(&self) -> gpio::FloatingState {
        if self.registers().pusr.get() & self.mask() == 0 {
            gpio::FloatingState::PullUp
        } else {
            gpio::FloatingState::PullNone
        }
    }
}

impl gpio::Input for Pin<'_> {
    fn read(&self) -> bool {
        self.registers().pdsr.get() & self.mask() != 0
    }
}

impl gpio::Output for Pin<'_> {
    fn set(&self) {
        self.registers().sodr.set(self.mask());
    }

    fn clear(&self) {
        self.registers().codr.set(self.mask());
    }

    fn toggle(&self) -> bool {
        let new_value = self.registers().odsr.get() & self.mask() == 0;
        if new_value {
            self.registers().sodr.set(self.mask());
        } else {
            self.registers().codr.set(self.mask());
        }
        new_value
    }
}

impl<'a> gpio::Interrupt<'a> for Pin<'a> {
    fn set_client(&self, client: &'a dyn gpio::Client) {
        self.client.set(client);
    }

    fn enable_interrupts(&self, mode: gpio::InterruptEdge) {
        match mode {
            gpio::InterruptEdge::EitherEdge => {
                // Without additional interrupt modes, the PIO detects any
                // input change.
                self.registers().aimdr.set(self.mask());
            }
            gpio::InterruptEdge::RisingEdge => {
                self.registers().esr.set(self.mask());
                self.registers().rehlsr.set(self.mask());
                self.registers().aimer.set(self.mask());
            }
            gpio::InterruptEdge::FallingEdge => {
                self.registers().esr.set(self.mask());
                self.registers().fellsr.set(self.mask());
                self.registers().aimer.set(self.mask());
            }
        }
        // The interrupt status register latches input changes even while a
        // pin's interrupt is disabled. Drop this pin's stale flag, so only
        // edges after this call are reported.
        self.port.map(|port| port.clear_stale_flag(self.mask()));
        self.registers().ier.set(self.mask());
    }

    fn disable_interrupts(&self) {
        self.registers().idr.set(self.mask());
    }

    fn is_pending(&self) -> bool {
        // The interrupt status register clears every pin's flag when read, and
        // the port dispatches pending pins immediately, so a pin is never
        // observably pending outside of the interrupt handler.
        false
    }
}

/// One PIO controller and its 32 pins.
pub struct Port<'a> {
    registers: StaticRef<PioRegisters>,
    id: PeripheralId,
    pins: [Pin<'a>; 32],
    /// Flags of enabled pins taken out of the interrupt status register
    /// outside the interrupt handler, still to be dispatched.
    pending: Cell<u32>,
    deferred_call: DeferredCall,
}

impl<'a> Port<'a> {
    fn new(port: PortId) -> Self {
        macro_rules! pins {
            ($($n:literal)*) => { [$(Pin::new(port, $n)),*] };
        }
        Self {
            registers: port.registers(),
            id: port.peripheral_id(),
            pins: pins!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15
                        16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31),
            pending: Cell::new(0),
            deferred_call: DeferredCall::new(),
        }
    }

    /// Read (and so clear) the interrupt status register, dropping the flags
    /// in `mask`. Flags of other enabled pins are kept and dispatched from a
    /// deferred call.
    fn clear_stale_flag(&self, mask: u32) {
        let others = self.registers.isr.get() & self.registers.imr.get() & !mask;
        if others != 0 {
            self.pending.set(self.pending.get() | others);
            self.deferred_call.set();
        }
    }

    fn dispatch(&self, mut fired: u32) {
        while fired != 0 {
            let pin = fired.trailing_zeros() as usize;
            fired &= !(1 << pin);
            if let Some(p) = self.pins.get(pin) {
                p.handle_interrupt();
            }
        }
    }

    /// Get a pin of this port, or `None` if `pin` is not below 32.
    pub fn pin(&self, pin: usize) -> Option<&Pin<'a>> {
        self.pins.get(pin)
    }

    /// Enable the controller's clock, needed to read inputs and for
    /// interrupts.
    pub fn enable_clock(&self, pmc: &Pmc) {
        pmc.enable_peripheral_clock(self.id);
    }

    /// Disable the controller's clock. Outputs keep their level.
    pub fn disable_clock(&self, pmc: &Pmc) {
        pmc.disable_peripheral_clock(self.id);
    }

    /// Set the debounce period of this port's pins.
    ///
    /// The debounce clock period is `2 * (div + 1)` slow clock periods, and
    /// the filter rejects pulses shorter than half of that. For example,
    /// `div` = 163 gives a 10 ms period from the 32.768 kHz slow clock.
    pub fn set_debounce_divider(&self, div: u32) {
        self.registers.scdr.set(div & 0x3FFF);
    }

    pub fn handle_interrupt(&self) {
        // Reading ISR clears all flags. Flags of masked pins are dropped.
        let fired = (self.registers.isr.get() | self.pending.take()) & self.registers.imr.get();
        self.dispatch(fired);
    }
}

impl DeferredCallClient for Port<'_> {
    fn register(&'static self) {
        self.deferred_call.register(self);
    }

    fn handle_deferred_call(&self) {
        let fired = self.pending.take() & self.registers.imr.get();
        self.dispatch(fired);
    }
}

pub struct Ports<'a> {
    ports: [Port<'a>; 4],
}

impl<'a> Ports<'a> {
    pub fn new() -> Self {
        Self {
            ports: [
                Port::new(PortId::A),
                Port::new(PortId::B),
                Port::new(PortId::C),
                Port::new(PortId::D),
            ],
        }
    }

    /// Link pins to their ports and register the ports' deferred calls. Must
    /// be called once the ports are at their final, static address.
    pub fn setup_circular_deps(&'static self) {
        for port in self.ports.iter() {
            for pin in port.pins.iter() {
                pin.port.set(port);
            }
            port.register();
        }
    }

    pub fn port(&self, port: PortId) -> &Port<'a> {
        &self.ports[port as usize]
    }

    /// Shorthand for `self.port(port).pin(pin)`.
    pub fn pin(&self, port: PortId, pin: usize) -> Option<&Pin<'a>> {
        self.port(port).pin(pin)
    }
}
