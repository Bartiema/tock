// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Chip trait setup.

use core::cell::Cell;
use core::fmt::Write;

use cortexm3::{CortexM3, CortexMVariant};
use kernel::platform::chip::{Chip, InterruptService};
use kernel::utilities::cells::OptionalCell;

use crate::nvic;

/// Notified around each WFI, e.g. to drive a marker pin or measure sleep.
pub trait SleepObserver {
    /// Called just before the core executes WFI.
    fn sleeping(&self);
    /// Called just after the core wakes up.
    fn awake(&self);
}

pub struct Sam3x<'a, I: InterruptService + 'a> {
    mpu: cortexm3::mpu::MPU,
    userspace_kernel_boundary: cortexm3::syscall::SysCall,
    interrupt_service: &'a I,
    wfi_sleep: Cell<bool>,
    /// Optional observer of WFI entry and exit.
    sleep_observer: OptionalCell<&'a dyn SleepObserver>,
    /// Number of times the core has entered WFI.
    sleep_count: Cell<u32>,
}

/// The peripherals this crate supports.
///
/// The clock drivers (`Pmc`, `Eefc`, `Supc`, `Wdt`) are not here: boards use
/// them during early boot, before this structure exists.
pub struct Sam3xDefaultPeripherals<'a> {
    pub pio: crate::pio::Ports<'a>,
    pub uart: crate::uart::Uart<'a>,
    pub rtt: crate::rtt::Rtt<'a>,
}

impl<'a> Sam3xDefaultPeripherals<'a> {
    pub fn new(pmc: &'a crate::pmc::Pmc) -> Self {
        Self {
            pio: crate::pio::Ports::new(),
            uart: crate::uart::Uart::new(pmc),
            rtt: crate::rtt::Rtt::new(),
        }
    }

    /// Register deferred calls. Must be called once the peripherals are at
    /// their final, static address.
    pub fn setup_circular_deps(&'static self) {
        self.pio.setup_circular_deps();
        kernel::deferred_call::DeferredCallClient::register(&self.uart);
        kernel::deferred_call::DeferredCallClient::register(&self.rtt);
    }
}

impl InterruptService for Sam3xDefaultPeripherals<'_> {
    fn service_interrupt(&self, interrupt: u32) -> bool {
        use crate::pio::PortId;
        match interrupt {
            nvic::UART => self.uart.handle_interrupt(),
            nvic::RTT => self.rtt.handle_interrupt(),
            nvic::PIOA => self.pio.port(PortId::A).handle_interrupt(),
            nvic::PIOB => self.pio.port(PortId::B).handle_interrupt(),
            nvic::PIOC => self.pio.port(PortId::C).handle_interrupt(),
            nvic::PIOD => self.pio.port(PortId::D).handle_interrupt(),
            _ => return false,
        }
        true
    }
}

impl<'a, I: InterruptService + 'a> Sam3x<'a, I> {
    /// # Safety
    ///
    /// Must be called at most once: the returned value owns the MPU.
    pub unsafe fn new(interrupt_service: &'a I) -> Self {
        Self {
            // ### Safety
            //
            // The caller guarantees this is the only instance, so there is
            // exactly one owner of the MPU registers.
            mpu: unsafe { cortexm3::mpu::new() },
            userspace_kernel_boundary: cortexm3::syscall::SysCall::new(),
            interrupt_service,
            wfi_sleep: Cell::new(false),
            sleep_observer: OptionalCell::empty(),
            sleep_count: Cell::new(0),
        }
    }
}

impl<'a, I: InterruptService> Sam3x<'a, I> {
    /// Notify `observer` around each WFI.
    pub fn set_sleep_observer(&self, observer: &'a dyn SleepObserver) {
        self.sleep_observer.set(observer);
    }

    /// Choose whether the kernel idles in WFI (Sleep mode) or spins.
    ///
    /// Spinning is the default. On the SAM3X, a core sleeping in WFI makes the
    /// debug port answer WAIT to every memory access until the next wake-up,
    /// and with no wake-up pending until reset. That breaks flashing, RTT and
    /// GDB, so enable WFI only once the board has a reliable wake-up source or
    /// no debugger is needed.
    pub fn set_wfi_sleep(&self, enable: bool) {
        self.wfi_sleep.set(enable);
    }

    pub fn wfi_sleep(&self) -> bool {
        self.wfi_sleep.get()
    }

    /// Number of times the core has entered WFI since boot.
    pub fn sleep_count(&self) -> u32 {
        self.sleep_count.get()
    }
}

impl<'a, I: InterruptService + 'a> Chip for Sam3x<'a, I> {
    type MPU = cortexm3::mpu::MPU;
    type UserspaceKernelBoundary = cortexm3::syscall::SysCall;
    type ThreadIdProvider = cortexm3::thread_id::CortexMThreadIdProvider;

    fn init() {
        cortexm3::nvic::disable_all();
        cortexm3::nvic::clear_all_pending();
        cortexm3::nvic::enable_all();
    }

    fn service_pending_interrupts(&self) {
        while let Some(interrupt) = cortexm3::nvic::next_pending() {
            if !self.interrupt_service.service_interrupt(interrupt) {
                panic!("unhandled interrupt {}", interrupt);
            }
            let n = cortexm3::nvic::Nvic::new(interrupt);
            n.clear_pending();
            n.enable();
        }
    }

    fn has_pending_interrupts(&self) -> bool {
        cortexm3::nvic::has_pending()
    }

    fn mpu(&self) -> &cortexm3::mpu::MPU {
        &self.mpu
    }

    fn userspace_kernel_boundary(&self) -> &cortexm3::syscall::SysCall {
        &self.userspace_kernel_boundary
    }

    fn sleep(&self) {
        if !self.wfi_sleep.get() {
            // Return to the kernel loop, which checks for work again.
            return;
        }
        self.sleep_count.set(self.sleep_count.get().wrapping_add(1));
        self.sleep_observer.map(|observer| observer.sleeping());
        // Sleep mode: the CPU clock stops, peripherals keep running and any
        // enabled interrupt wakes the core.
        //
        // ### Safety
        //
        // Clearing SLEEPDEEP and executing WFI have no memory-safety
        // implications; the core resumes at the next interrupt.
        unsafe {
            cortexm3::scb::unset_sleepdeep();
            cortexm3::support::wfi();
        }
        self.sleep_observer.map(|observer| observer.awake());
    }

    fn with_interrupts_disabled<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        cortexm3::support::with_interrupts_disabled(f)
    }

    unsafe fn print_state(_this: Option<&Self>, write: &mut dyn Write) {
        // ### Safety
        //
        // Only reads core registers for debug output.
        unsafe { CortexM3::print_cortexm_state(write) };
    }
}
