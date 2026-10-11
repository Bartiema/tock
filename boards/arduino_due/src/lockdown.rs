// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Board side of the tamper lockdown.
//!
//! Provides the debug markers, timing, the wipe of the kernel's secret
//! buffers and of the unused kernel stack, the power profiles, and the
//! process console commands.

#[cfg(all(target_arch = "arm", target_os = "none"))]
use core::arch::asm;

use capsules_core::process_console::ProcessConsoleHooks;
use capsules_system::lockdown::LockdownPlatform;
use kernel::hil::gpio::Output;
use kernel::hil::hw_debug::CycleCounter;

/// Bytes below the stack pointer the stack scrub leaves alone, so the code
/// doing the scrub and anything it calls keeps a valid stack.
const STACK_SCRUB_MARGIN: usize = 256;

extern "C" {
    /// Start and end of the `.kernel_secrets` section (see `layout.ld`).
    static mut _skernel_secrets: u32;
    static mut _ekernel_secrets: u32;
    /// Lowest address of the kernel stack.
    static mut _sstack: u32;
}

/// Zero `[bottom, sp - margin)` of the current stack, returning the bytes
/// zeroed.
#[cfg(all(target_arch = "arm", target_os = "none"))]
fn scrub_stack_below_sp(bottom: usize, margin: usize) -> usize {
    let sp: usize;
    // ### Safety
    //
    // Reads the stack pointer; no memory is accessed.
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    let top = (sp - margin) & !3;
    if top <= bottom {
        return 0;
    }
    // ### Safety
    //
    // The kernel stack grows down from `_estack` towards `_sstack`, so
    // `[_sstack, sp)` holds no live frames. The loop zeroes
    // `[_sstack, sp - margin)` with registers only (it does not use the
    // stack), and the margin covers any frame pushed below `sp` since it was
    // read. Writing from assembly keeps the compiler from removing the stores.
    unsafe {
        asm!(
            "2:",
            "cmp {p}, {end}",
            "bhs 3f",
            "str {zero}, [{p}], #4",
            "b 2b",
            "3:",
            p = inout(reg) bottom => _,
            end = in(reg) top,
            zero = in(reg) 0u32,
            options(nostack),
        );
    }
    top - bottom
}

/// Host builds (CI checks) have no kernel stack to scrub.
#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn scrub_stack_below_sp(_bottom: usize, _margin: usize) -> usize {
    0
}

pub struct DuePlatform {
    marker_a: &'static sam3x::pio::Pin<'static>,
    marker_b: &'static sam3x::pio::Pin<'static>,
    dwt: cortexm3::dwt::Dwt,
    power: &'static crate::power::PowerProfiles,
}

impl DuePlatform {
    pub fn new(
        marker_a: &'static sam3x::pio::Pin<'static>,
        marker_b: &'static sam3x::pio::Pin<'static>,
        power: &'static crate::power::PowerProfiles,
    ) -> Self {
        let dwt = cortexm3::dwt::Dwt::new();
        dwt.start();
        Self {
            marker_a,
            marker_b,
            dwt,
            power,
        }
    }
}

impl LockdownPlatform for DuePlatform {
    fn marker_a(&self, high: bool) {
        if high {
            self.marker_a.set();
        } else {
            self.marker_a.clear();
        }
    }

    fn toggle_marker_b(&self) {
        self.marker_b.toggle();
    }

    fn cycle_count(&self) -> u32 {
        self.dwt.count() as u32
    }

    fn wipe_kernel_secrets(&self) -> usize {
        let start = core::ptr::addr_of_mut!(_skernel_secrets);
        let end = core::ptr::addr_of_mut!(_ekernel_secrets);
        let words = (end as usize - start as usize) / 4;
        // ### Safety
        //
        // `[_skernel_secrets, _ekernel_secrets)` is the linker section that
        // holds only plain byte buffers (console, UART mux, RTT and debug
        // writer data), word aligned by `layout.ld`. Zero is a valid value
        // for all of them, and the kernel is single threaded, so nothing
        // reads them while this runs. Volatile writes are not elided.
        unsafe {
            for i in 0..words {
                core::ptr::write_volatile(start.add(i), 0);
            }
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        words * 4
    }

    fn scrub_kernel_stack(&self) -> usize {
        let bottom = core::ptr::addr_of_mut!(_sstack) as usize;
        scrub_stack_below_sp(bottom, STACK_SCRUB_MARGIN)
    }

    fn enter_low_power(&self) {
        self.power.enter_lockdown();
    }

    fn exit_low_power(&self) {
        self.power.enter_normal();
    }
}

/// The lockdown manager as the board instantiates it.
pub type Manager = capsules_system::lockdown::LockdownManager<
    'static,
    sam3x::pio::Pin<'static>,
    capsules_core::virtualizers::virtual_alarm::VirtualMuxAlarm<'static, sam3x::rtt::Rtt<'static>>,
    LockdownCap,
>;

kernel::define_capability_type!(
    LockdownCap:
    kernel::capabilities::ProcessManagementCapability,
    kernel::capabilities::ProcessStartCapability,
    kernel::capabilities::ProcessZeroizeCapability
);

/// The capability set the lockdown manager needs. Call once.
///
/// # Safety
///
/// The caller hands the result only to the lockdown manager.
pub unsafe fn mint_capability() -> LockdownCap {
    unsafe { kernel::mint_defined_capability!(LockdownCap) }
}

/// Process console commands for lockdown.
///
/// Adds `lockdown`, `unlock`, `lockstat`, and `wfi on|off` to choose whether
/// lockdown idles in WFI.
pub struct ConsoleHooks {
    manager: &'static Manager,
    power: &'static crate::power::PowerProfiles,
}

impl ConsoleHooks {
    pub fn new(manager: &'static Manager, power: &'static crate::power::PowerProfiles) -> Self {
        Self { manager, power }
    }
}

impl ProcessConsoleHooks for ConsoleHooks {
    fn process_start_allowed(&self, name: &str) -> bool {
        self.manager.console_start_allowed(name)
    }

    fn command(&self, command: &str) -> Option<&'static str> {
        let mut words = command.split_whitespace();
        if words.next() == Some("wfi") {
            return match words.next() {
                Some("on") => {
                    self.power.set_sleep_in_lockdown(true);
                    Some("Lockdown idles in WFI from the next lockdown.\r\n")
                }
                Some("off") => {
                    self.power.set_sleep_in_lockdown(false);
                    Some("Lockdown spins when idle from the next lockdown.\r\n")
                }
                Some("test") => {
                    let ms = words.next().and_then(|w| w.parse().ok()).unwrap_or(5000);
                    self.power.start_wfi_test(ms);
                    Some("WFI test started; the probe cannot reach the chip until it ends.\r\n")
                }
                _ => Some("Usage: wfi on|off|test [ms]\r\n"),
            };
        }
        self.manager.console_command(command)
    }

    fn help(&self) -> &'static str {
        "lockdown unlock lockstat wfi\r\n"
    }
}
