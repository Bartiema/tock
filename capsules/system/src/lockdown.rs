// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Tamper lockdown: stop untrusted processes, wipe their memory and the
//! kernel's secrets, and switch to a low-power profile.
//!
//! The trigger is a normally closed tamper loop on a GPIO pin with a pull-up:
//! the pin reads low while the loop is closed. Opening the loop (a rising
//! edge) locks; closing it again unlocks once the pin has stayed low for a
//! minimum hold time.
//!
//! Lock sequence:
//!
//! 1. Marker A goes high. The lockdown flag is set, so the fault policy stops
//!    faulting processes without restarting them, and the process console
//!    refuses to start processes that are not on the allowlist.
//! 2. Every process whose name is not on the allowlist is terminated. Marker
//!    B toggles when the last one is done.
//! 3. The memory of each terminated process is zeroed
//!    ([`Process::zeroize_memory`]).
//! 4. The platform wipes the kernel's secret buffers and the unused kernel
//!    stack, then switches to its low-power profile.
//! 5. Marker A goes low. Allowlisted processes keep running.
//!
//! Unlock restores the power profile, clears the flag and restarts the
//! processes lockdown terminated. Their memory is already zero; they start
//! fresh from flash.
//!
//! Processes are identified by the name in their TBF header. Any process can
//! claim any name, so this is only as strong as the control over which
//! processes are installed.
//!
//! [`Process::zeroize_memory`]: kernel::process::Process::zeroize_memory

use core::cell::Cell;

use kernel::Kernel;
use kernel::capabilities::{
    ProcessManagementCapability, ProcessStartCapability, ProcessZeroizeCapability,
};
use kernel::debug;
use kernel::deferred_call::{DeferredCall, DeferredCallClient};
use kernel::hil::gpio;
use kernel::hil::time::{Alarm, AlarmClient, ConvertTicks};
use kernel::process::{FaultAction, Process, ProcessFaultPolicy, State};

/// Platform-specific parts of the lockdown sequence, provided by the board.
pub trait LockdownPlatform {
    /// Drive debug marker A, high for the duration of the lock sequence.
    fn marker_a(&self, high: bool);
    /// Toggle debug marker B, once all untrusted processes are stopped.
    fn toggle_marker_b(&self);
    /// A free-running cycle counter, for timing the sequence.
    fn cycle_count(&self) -> u32;
    /// Zero the kernel's buffers that may hold secrets. Returns bytes zeroed.
    fn wipe_kernel_secrets(&self) -> usize;
    /// Zero the unused part of the kernel stack. Returns bytes zeroed.
    fn scrub_kernel_stack(&self) -> usize;
    /// Switch to the low-power profile used while locked.
    fn enter_low_power(&self);
    /// Restore the normal power profile.
    fn exit_low_power(&self);
}

/// Cycle counts at each step of the last lock sequence, relative to its start.
#[derive(Clone, Copy, Default, Debug)]
pub struct LockTiming {
    /// All untrusted processes terminated.
    pub stopped: u32,
    /// Their memory zeroed.
    pub processes_wiped: u32,
    /// Kernel secrets and stack wiped.
    pub kernel_wiped: u32,
    /// Low-power profile active; sequence complete.
    pub done: u32,
}

/// Minimum, maximum and sum of a timing over all locks.
#[derive(Clone, Copy, Debug)]
pub struct Spread {
    pub min: u32,
    pub max: u32,
    pub sum: u64,
}

impl Spread {
    const fn new() -> Self {
        Self {
            min: u32::MAX,
            max: 0,
            sum: 0,
        }
    }

    fn add(&mut self, value: u32) {
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.sum += value as u64;
    }
}

/// What started a lock or unlock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The tamper loop pin.
    Pin,
    /// The process console.
    Console,
    /// The pin was open at boot.
    Boot,
}

pub struct LockdownManager<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> {
    kernel: &'static Kernel,
    trigger: &'a P,
    alarm: &'a A,
    platform: &'a dyn LockdownPlatform,
    allowlist: &'static [&'static str],
    /// Fault policy used while not locked.
    unlocked_fault_policy: &'a dyn ProcessFaultPolicy,
    capability: C,
    /// How long the loop must stay closed before unlocking.
    hold_ms: u32,

    locked: Cell<bool>,
    /// Processes (by position in the process array) to restart on unlock.
    to_restart: Cell<u32>,
    /// A process faulted while locked and must be terminated and wiped.
    fault_pending: Cell<bool>,
    deferred_call: DeferredCall,

    lock_count: Cell<u32>,
    unlock_count: Cell<u32>,
    wipe_failures: Cell<u32>,
    last_timing: Cell<LockTiming>,
    /// Spread of `LockTiming::stopped` and `LockTiming::kernel_wiped`.
    stopped_spread: Cell<Spread>,
    wiped_spread: Cell<Spread>,
}

impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> LockdownManager<'a, P, A, C>
{
    pub fn new(
        kernel: &'static Kernel,
        trigger: &'a P,
        alarm: &'a A,
        platform: &'a dyn LockdownPlatform,
        allowlist: &'static [&'static str],
        unlocked_fault_policy: &'a dyn ProcessFaultPolicy,
        capability: C,
        hold_ms: u32,
    ) -> Self {
        Self {
            kernel,
            trigger,
            alarm,
            platform,
            allowlist,
            unlocked_fault_policy,
            capability,
            hold_ms,
            locked: Cell::new(false),
            to_restart: Cell::new(0),
            fault_pending: Cell::new(false),
            deferred_call: DeferredCall::new(),
            lock_count: Cell::new(0),
            unlock_count: Cell::new(0),
            wipe_failures: Cell::new(0),
            last_timing: Cell::new(LockTiming::default()),
            stopped_spread: Cell::new(Spread::new()),
            wiped_spread: Cell::new(Spread::new()),
        }
    }

    /// Configure the trigger pin. Call before processes are loaded.
    pub fn setup(&self) {
        self.trigger.make_input();
        self.trigger.set_floating_state(gpio::FloatingState::PullUp);
        self.trigger
            .enable_interrupts(gpio::InterruptEdge::EitherEdge);
    }

    /// Lock if the tamper loop is open. Call after processes are loaded and
    /// before the kernel loop starts, so no untrusted process runs first.
    pub fn check_boot_state(&self) {
        if self.loop_open() {
            self.lock(Source::Boot);
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked.get()
    }

    pub fn last_timing(&self) -> LockTiming {
        self.last_timing.get()
    }

    fn loop_open(&self) -> bool {
        self.trigger.read()
    }

    fn allowlisted(&self, name: &str) -> bool {
        self.allowlist.contains(&name)
    }

    /// Run the lock sequence. Does nothing if already locked.
    pub fn lock(&self, source: Source) {
        // A pending unlock is cancelled by any new lock request.
        let _ = self.alarm.disarm();
        if self.locked.get() {
            return;
        }
        let start = self.platform.cycle_count();
        self.platform.marker_a(true);
        self.locked.set(true);

        // Terminate every process that is not allowlisted.
        let mut terminated = self.to_restart.get();
        let mut index = 0;
        self.kernel
            .process_each_capability(&self.capability, |process| {
                if !self.allowlisted(process.get_process_name()) {
                    if process.get_state() != State::Terminated {
                        process.terminate(None);
                        terminated |= 1 << index;
                    }
                }
                index += 1;
            });
        self.to_restart.set(terminated);
        self.platform.toggle_marker_b();
        let stopped = self.platform.cycle_count();

        let wiped_bytes = self.wipe_terminated();
        let processes_wiped = self.platform.cycle_count();

        let secrets = self.platform.wipe_kernel_secrets();
        let stack = self.platform.scrub_kernel_stack();
        let kernel_wiped = self.platform.cycle_count();

        self.platform.enter_low_power();
        let done = self.platform.cycle_count();
        self.platform.marker_a(false);

        let timing = LockTiming {
            stopped: stopped.wrapping_sub(start),
            processes_wiped: processes_wiped.wrapping_sub(start),
            kernel_wiped: kernel_wiped.wrapping_sub(start),
            done: done.wrapping_sub(start),
        };
        self.last_timing.set(timing);
        let mut spread = self.stopped_spread.get();
        spread.add(timing.stopped);
        self.stopped_spread.set(spread);
        let mut spread = self.wiped_spread.get();
        spread.add(timing.kernel_wiped);
        self.wiped_spread.set(spread);
        self.lock_count.set(self.lock_count.get().wrapping_add(1));
        debug!(
            "lockdown: locked ({:?}): {} process bytes, {} secret bytes, {} stack bytes zeroed; cycles {:?}",
            source, wiped_bytes, secrets, stack, timing
        );
    }

    /// Zero every terminated process that is not allowlisted. Returns the
    /// number of bytes zeroed.
    fn wipe_terminated(&self) -> usize {
        let mut total = 0;
        self.kernel
            .process_each_capability(&self.capability, |process| {
                if process.get_state() == State::Terminated
                    && !self.allowlisted(process.get_process_name())
                {
                    match process.zeroize_memory(&self.capability) {
                        Ok(bytes) => total += bytes,
                        Err(e) => {
                            self.wipe_failures
                                .set(self.wipe_failures.get().wrapping_add(1));
                            debug!(
                                "lockdown: wiping {} failed: {:?}",
                                process.get_process_name(),
                                e
                            );
                        }
                    }
                }
            });
        total
    }

    /// Run the unlock sequence. Does nothing if not locked.
    pub fn unlock(&self, source: Source) {
        let _ = self.alarm.disarm();
        if !self.locked.get() {
            return;
        }
        self.platform.exit_low_power();
        self.locked.set(false);

        let restart = self.to_restart.get();
        let mut index = 0;
        let mut restarted = 0;
        self.kernel
            .process_each_capability(&self.capability, |process| {
                if restart & (1 << index) != 0 && process.get_state() == State::Terminated {
                    process.start(&self.capability);
                    restarted += 1;
                }
                index += 1;
            });
        self.to_restart.set(0);
        self.unlock_count
            .set(self.unlock_count.get().wrapping_add(1));
        debug!(
            "lockdown: unlocked ({:?}), {} processes restarted",
            source, restarted
        );
    }

    fn print_status(&self) {
        debug!(
            "lockdown: {}, loop {}, {} locks, {} unlocks, {} wipe failures, last {:?}",
            if self.locked.get() {
                "locked"
            } else {
                "unlocked"
            },
            if self.loop_open() { "open" } else { "closed" },
            self.lock_count.get(),
            self.unlock_count.get(),
            self.wipe_failures.get(),
            self.last_timing.get()
        );
        let locks = u64::from(self.lock_count.get().max(1));
        let stopped = self.stopped_spread.get();
        let wiped = self.wiped_spread.get();
        debug!(
            "lockdown: cycles to stopped min {} mean {} max {}; to wiped min {} mean {} max {}",
            stopped.min,
            stopped.sum / locks,
            stopped.max,
            wiped.min,
            wiped.sum / locks,
            wiped.max
        );
    }
}

impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> gpio::Client for LockdownManager<'a, P, A, C>
{
    fn fired(&self) {
        if self.loop_open() {
            // Lock at once on the first edge; bounces are harmless, as
            // locking again does nothing.
            self.lock(Source::Pin);
        } else if self.locked.get() {
            // Unlock only once the loop has stayed closed for the hold time.
            // Any rising edge before then cancels it.
            self.alarm
                .set_alarm(self.alarm.now(), self.alarm.ticks_from_ms(self.hold_ms));
        }
    }
}

impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> AlarmClient for LockdownManager<'a, P, A, C>
{
    fn alarm(&self) {
        if !self.loop_open() {
            self.unlock(Source::Pin);
        }
    }
}

impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> ProcessFaultPolicy for LockdownManager<'a, P, A, C>
{
    fn action(&self, process: &dyn Process) -> FaultAction {
        if self.locked.get() {
            // Stop it now; terminate and wipe it from a deferred call, outside
            // the kernel's fault handling. It restarts only on unlock.
            self.fault_pending.set(true);
            self.deferred_call.set();
            FaultAction::Stop
        } else {
            self.unlocked_fault_policy.action(process)
        }
    }
}

impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> DeferredCallClient for LockdownManager<'a, P, A, C>
{
    fn register(&'static self) {
        self.deferred_call.register(self);
    }

    fn handle_deferred_call(&self) {
        if !self.fault_pending.take() || !self.locked.get() {
            return;
        }
        let mut terminated = self.to_restart.get();
        let mut index = 0;
        self.kernel
            .process_each_capability(&self.capability, |process| {
                if process.get_state() == State::Faulted {
                    process.terminate(None);
                    terminated |= 1 << index;
                    match process.zeroize_memory(&self.capability) {
                        Ok(bytes) => debug!(
                            "lockdown: {} faulted; terminated, {} bytes zeroed",
                            process.get_process_name(),
                            bytes
                        ),
                        Err(e) => {
                            self.wipe_failures
                                .set(self.wipe_failures.get().wrapping_add(1));
                            debug!(
                                "lockdown: wiping {} failed: {:?}",
                                process.get_process_name(),
                                e
                            );
                        }
                    }
                }
                index += 1;
            });
        self.to_restart.set(terminated);
    }
}

/// Process console integration: commands `lockdown`, `unlock` and
/// `lockstat`, and no starting of untrusted processes while locked.
impl<
    'a,
    P: gpio::InterruptPin<'a>,
    A: Alarm<'a>,
    C: ProcessManagementCapability + ProcessStartCapability + ProcessZeroizeCapability,
> LockdownManager<'a, P, A, C>
{
    /// Whether the process console may start or resume `name`.
    pub fn console_start_allowed(&self, name: &str) -> bool {
        !self.locked.get() || self.allowlisted(name)
    }

    /// Handle a process console command. Returns the reply, or `None` if the
    /// command is not a lockdown command.
    pub fn console_command(&self, command: &str) -> Option<&'static str> {
        match command.split_whitespace().next() {
            Some("lockdown") => {
                self.lock(Source::Console);
                Some("")
            }
            Some("unlock") => {
                if self.loop_open() {
                    debug!("lockdown: warning, the tamper loop is still open");
                }
                self.unlock(Source::Console);
                Some("")
            }
            Some("lockstat") => {
                self.print_status();
                Some("")
            }
            _ => None,
        }
    }

    /// Help text for the console commands.
    pub fn console_help(&self) -> &'static str {
        "lockdown unlock lockstat\r\n"
    }
}
