// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Power profiles for normal operation and lockdown.
//!
//! Normal: 84 MHz from the crystal and PLL, all used peripheral clocks on.
//! Lockdown: master clock from the 12 MHz RC oscillator, PLL and crystal
//! off, unused peripheral clocks gated, and optionally idling in WFI.
//!
//! Idling in WFI makes the SAM3X debug port answer WAIT until the core wakes.
//! A core that is woken every 10 ms stays debuggable, so while WFI is enabled
//! a keep-alive alarm wakes it at that rate.

use core::cell::Cell;

use kernel::hil::gpio::Output;
use kernel::hil::time::{Alarm, AlarmClient, ConvertTicks, Ticks, Time};
use sam3x::pmc::{PeripheralId, RcFrequency};

use crate::{AlarmHw, CONSOLE, CRYSTAL_HZ, ChipHw, ConsoleDevice, PLLA_DIV, PLLA_MUL};

/// Interval of the keep-alive wake-up while idling in WFI.
const KEEPALIVE_MS: u32 = 10;

/// Clock of the lockdown profile.
const LOCKDOWN_CLOCK: RcFrequency = RcFrequency::Rc12MHz;

/// Peripheral clocks the lockdown profile gates. PIOB stays on: it has the
/// tamper loop pin (PB26) and the LED (PB27). Outputs on gated ports keep
/// their level.
const LOCKDOWN_GATED: [PeripheralId; 4] = [
    PeripheralId::PioA,
    PeripheralId::PioC,
    PeripheralId::PioD,
    PeripheralId::Uart,
];

kernel::define_capability_type!(SysTickCap: cortexm3::systick::SysTickFrequencyCapability);

pub struct PowerProfiles {
    pmc: &'static sam3x::pmc::Pmc,
    uart: &'static sam3x::uart::Uart<'static>,
    systick: &'static cortexm3::systick::SysTick,
    chip: &'static ChipHw,
    keepalive:
        &'static capsules_core::virtualizers::virtual_alarm::VirtualMuxAlarm<'static, AlarmHw>,
    /// Idle in WFI while locked down.
    sleep_in_lockdown: Cell<bool>,
    /// Peripheral clocks that were on before lockdown gated them.
    gated: Cell<u64>,
    /// End of a WFI test window, in RTT ticks, and the counters at its start.
    wfi_test_end: Cell<Option<u32>>,
    wfi_test_start: Cell<(u32, u32, u32)>,
    /// Pin high while sleeping, for a logic analyzer.
    sleep_marker: &'static sam3x::pio::Pin<'static>,
    /// RTT time of the last WFI entry, and total RTT ticks spent in WFI.
    slept_at: Cell<u32>,
    sleep_ticks: Cell<u32>,
    capability: SysTickCap,
}

impl PowerProfiles {
    pub fn new(
        pmc: &'static sam3x::pmc::Pmc,
        uart: &'static sam3x::uart::Uart<'static>,
        systick: &'static cortexm3::systick::SysTick,
        chip: &'static ChipHw,
        keepalive: &'static capsules_core::virtualizers::virtual_alarm::VirtualMuxAlarm<
            'static,
            AlarmHw,
        >,
        sleep_in_lockdown: bool,
        sleep_marker: &'static sam3x::pio::Pin<'static>,
    ) -> Self {
        Self {
            pmc,
            uart,
            systick,
            chip,
            keepalive,
            sleep_in_lockdown: Cell::new(sleep_in_lockdown),
            gated: Cell::new(0),
            wfi_test_end: Cell::new(None),
            wfi_test_start: Cell::new((0, 0, 0)),
            sleep_marker,
            slept_at: Cell::new(0),
            sleep_ticks: Cell::new(0),
            // ### Safety
            //
            // The power profiles own the only copy of this capability, and
            // change the SysTick frequency only together with the clock.
            capability: unsafe { kernel::mint_defined_capability!(SysTickCap) },
        }
    }

    /// Whether the lockdown profile idles in WFI. Takes effect at the next
    /// lockdown.
    pub fn set_sleep_in_lockdown(&self, enable: bool) {
        self.sleep_in_lockdown.set(enable);
    }

    /// Idle in WFI for `ms` milliseconds, then stop and report how much of
    /// the time the core slept.
    ///
    /// The debug probe cannot reach a sleeping SAM3X, so sleeping is measured
    /// here: the CPU cycle counter stops during WFI while the RTT does not.
    /// Run it locked down to measure the lockdown profile.
    pub fn start_wfi_test(&self, ms: u32) {
        let now = self.keepalive.now();
        self.wfi_test_start.set((
            now.into_u32(),
            self.sleep_ticks.get(),
            self.chip.sleep_count(),
        ));
        self.wfi_test_end.set(Some(
            now.wrapping_add(self.keepalive.ticks_from_ms(ms))
                .into_u32(),
        ));
        self.chip.set_wfi_sleep(true);
        self.keepalive
            .set_alarm(now, self.keepalive.ticks_from_ms(KEEPALIVE_MS));
    }

    fn finish_wfi_test(&self) {
        self.wfi_test_end.set(None);
        self.chip.set_wfi_sleep(false);
        let (start_ticks, start_sleep_ticks, start_sleeps) = self.wfi_test_start.get();
        let ticks = self.keepalive.now().into_u32().wrapping_sub(start_ticks);
        let slept = self.sleep_ticks.get().wrapping_sub(start_sleep_ticks);
        kernel::debug!(
            "power: WFI test: {} ms, {} WFI entries, asleep {} ms ({} per mille), MCK {} Hz",
            ticks as u64 * 1000 / 32_768,
            self.chip.sleep_count().wrapping_sub(start_sleeps),
            slept as u64 * 1000 / 32_768,
            slept as u64 * 1000 / u64::from(ticks.max(1)),
            self.pmc.master_clock_hz()
        );
    }

    pub fn enter_lockdown(&self) {
        // Gate unused peripheral clocks, remembering which were on.
        let mut gated = 0u64;
        for id in LOCKDOWN_GATED {
            if id == PeripheralId::Uart && CONSOLE == ConsoleDevice::Uart {
                continue;
            }
            if self.pmc.is_peripheral_clock_enabled(id) {
                gated |= 1 << (id as u32);
                self.pmc.disable_peripheral_clock(id);
            }
        }
        self.gated.set(gated);

        // Timed with the RTT, which runs from the slow clock through the
        // switch (the CPU cycle counter changes rate halfway).
        let before = self.keepalive.now();
        if let Err(e) = self
            .pmc
            .use_main_rc(&sam3x::eefc::Eefc::new(), LOCKDOWN_CLOCK)
        {
            kernel::debug!("power: switch to the RC oscillator failed: {:?}", e);
        }
        self.clock_changed();
        let ticks = self.keepalive.now().wrapping_sub(before).into_u32();
        kernel::debug!(
            "power: lockdown profile, MCK {} Hz, switch took {} us",
            self.pmc.master_clock_hz(),
            ticks * 1_000_000 / 32_768
        );

        if self.sleep_in_lockdown.get() {
            self.chip.set_wfi_sleep(true);
            self.keepalive.set_alarm(
                self.keepalive.now(),
                self.keepalive.ticks_from_ms(KEEPALIVE_MS),
            );
        }
    }

    pub fn enter_normal(&self) {
        self.chip.set_wfi_sleep(false);
        let _ = self.keepalive.disarm();

        if let Err(e) =
            self.pmc
                .use_crystal_pll(&sam3x::eefc::Eefc::new(), CRYSTAL_HZ, PLLA_MUL, PLLA_DIV)
        {
            kernel::debug!("power: switch to the PLL failed: {:?}", e);
        }
        self.clock_changed();

        let gated = self.gated.take();
        for id in LOCKDOWN_GATED {
            if gated & (1 << (id as u32)) != 0 {
                self.pmc.enable_peripheral_clock(id);
            }
        }
    }

    /// Update everything that derives timing from the master clock.
    fn clock_changed(&self) {
        self.uart.clock_changed();
        self.systick
            .set_hertz(self.pmc.master_clock_hz(), &self.capability);
    }
}

/// Times WFI with the RTT (the CPU cycle counter keeps running in Sleep mode)
/// and drives the sleep marker pin.
impl sam3x::chip::SleepObserver for PowerProfiles {
    fn sleeping(&self) {
        self.sleep_marker.set();
        self.slept_at.set(self.keepalive.now().into_u32());
    }

    fn awake(&self) {
        let slept = self
            .keepalive
            .now()
            .into_u32()
            .wrapping_sub(self.slept_at.get());
        self.sleep_ticks
            .set(self.sleep_ticks.get().wrapping_add(slept));
        self.sleep_marker.clear();
    }
}

impl AlarmClient for PowerProfiles {
    fn alarm(&self) {
        if let Some(end) = self.wfi_test_end.get() {
            let now = self.keepalive.now();
            let start = kernel::hil::time::Ticks32::from(self.wfi_test_start.get().0);
            if !now.within_range(start, kernel::hil::time::Ticks32::from(end)) {
                self.finish_wfi_test();
                return;
            }
        }
        if self.chip.wfi_sleep() {
            self.keepalive.set_alarm(
                self.keepalive.now(),
                self.keepalive.ticks_from_ms(KEEPALIVE_MS),
            );
        }
    }
}
