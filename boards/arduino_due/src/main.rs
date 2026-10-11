// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Board file for the Arduino Due.
//!
//! - <https://docs.arduino.cc/hardware/due/>
//! - ATSAM3X8E, Cortex-M3 at 84 MHz from a 12 MHz crystal.

#![no_std]
#![no_main]
#![deny(missing_docs)]

use core::ptr::addr_of_mut;

use capsules_core::virtualizers::virtual_alarm::VirtualMuxAlarm;
use components::gpio::GpioComponent;
use kernel::capabilities;
use kernel::component::Component;
use kernel::debug::PanicResources;
use kernel::hil::gpio::{Configure, Output};
use kernel::hil::led::LedHigh;
use kernel::platform::chip::Chip;
use kernel::platform::{KernelResources, SyscallDriverLookup};
use kernel::utilities::single_thread_value::SingleThreadValue;
use kernel::{create_capability, debug, static_init};
use sam3x::chip::Sam3xDefaultPeripherals;
use sam3x::pio::{Peripheral, PortId};

/// Support routines for debugging I/O.
pub mod io;

/// Tamper lockdown: platform hooks and process console commands.
mod lockdown;
/// Normal and lockdown power profiles.
mod power;
/// UART self-test, run at boot when the console is not on the UART.
mod uart_loopback_test;

// Number of concurrent processes this platform supports.
const NUM_PROCS: usize = 4;

/// Where the console, `debug!()` and the process console go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConsoleDevice {
    /// The UART to the 16U2 USB-serial bridge (the programming USB port).
    Uart,
    /// SEGGER RTT through the debug probe, for benches without the
    /// programming USB port. Read it with `make rtt`.
    SeggerRtt,
}

const CONSOLE: ConsoleDevice = ConsoleDevice::SeggerRtt;

/// Frequency of the main crystal on the Arduino Due.
const CRYSTAL_HZ: u32 = 12_000_000;
/// PLLA multiplier and divider: 12 MHz * 14 / 1 = 168 MHz, and MCK = PLLA / 2
/// = 84 MHz.
const PLLA_MUL: u32 = 14;
const PLLA_DIV: u32 = 1;

type ChipHw = sam3x::chip::Sam3x<'static, Sam3xDefaultPeripherals<'static>>;
type ProcessPrinterInUse = capsules_system::process_printer::ProcessPrinterText;

/// Resources for when a board panics used by io.rs.
static PANIC_RESOURCES: SingleThreadValue<PanicResources<ChipHw, ProcessPrinterInUse>> =
    SingleThreadValue::new();

/// Processes that keep running in lockdown, by TBF name.
const ALLOWLIST: &[&str] = &["due_keeper"];

/// How long the tamper loop must stay closed before lockdown ends.
const UNLOCK_HOLD_MS: u32 = 500;

/// Idle in WFI (Sleep mode) while locked down.
const SLEEP_IN_LOCKDOWN: bool = false;

/// Places a buffer in `.kernel_secrets`, which lockdown wipes.
macro_rules! secret_buf {
    ($T:ty) => {
        kernel::static_buf_in_section!(".kernel_secrets", $T)
    };
}

// How should the kernel respond when a process faults while not locked down.
// Stopping (rather than panicking) keeps the other processes running, which
// the MPU test relies on.
const FAULT_RESPONSE: capsules_system::process_policies::StopWithDebugFaultPolicy =
    capsules_system::process_policies::StopWithDebugFaultPolicy {};

kernel::stack_size! {0x2000}

type SchedulerInUse = components::sched::round_robin::RoundRobinComponentType;
type AlarmHw = sam3x::rtt::Rtt<'static>;
type GpioPin = sam3x::pio::Pin<'static>;

/// A structure representing this platform that holds references to all
/// capsules for this platform.
struct ArduinoDue {
    console: &'static capsules_core::console::Console<'static>,
    ipc: kernel::ipc::IPC<{ NUM_PROCS as u8 }>,
    led: &'static capsules_core::led::LedDriver<'static, LedHigh<'static, GpioPin>, 1>,
    button: &'static capsules_core::button::Button<'static, GpioPin>,
    alarm: &'static capsules_core::alarm::AlarmDriver<'static, VirtualMuxAlarm<'static, AlarmHw>>,
    gpio: &'static capsules_core::gpio::GPIO<'static, GpioPin>,

    scheduler: &'static SchedulerInUse,
    systick: &'static cortexm3::systick::SysTick,
}

/// Mapping of integer syscalls to objects that implement syscalls.
impl SyscallDriverLookup for ArduinoDue {
    fn with_driver<F, R>(&self, driver_num: usize, f: F) -> R
    where
        F: FnOnce(Option<&dyn kernel::syscall::SyscallDriver>) -> R,
    {
        match driver_num {
            capsules_core::console::DRIVER_NUM => f(Some(self.console)),
            capsules_core::led::DRIVER_NUM => f(Some(self.led)),
            capsules_core::button::DRIVER_NUM => f(Some(self.button)),
            capsules_core::alarm::DRIVER_NUM => f(Some(self.alarm)),
            capsules_core::gpio::DRIVER_NUM => f(Some(self.gpio)),
            kernel::ipc::DRIVER_NUM => f(Some(&self.ipc)),
            _ => f(None),
        }
    }
}

impl KernelResources<ChipHw> for ArduinoDue {
    type SyscallDriverLookup = Self;
    type SyscallFilter = ();
    type ProcessFault = ();
    type Scheduler = SchedulerInUse;
    type SchedulerTimer = cortexm3::systick::SysTick;
    type WatchDog = ();
    type ContextSwitchCallback = ();

    fn syscall_driver_lookup(&self) -> &Self::SyscallDriverLookup {
        self
    }
    fn syscall_filter(&self) -> &Self::SyscallFilter {
        &()
    }
    fn process_fault(&self) -> &Self::ProcessFault {
        &()
    }
    fn scheduler(&self) -> &Self::Scheduler {
        self.scheduler
    }
    fn scheduler_timer(&self) -> &Self::SchedulerTimer {
        self.systick
    }
    fn watchdog(&self) -> &Self::WatchDog {
        &()
    }
    fn context_switch_callback(&self) -> &Self::ContextSwitchCallback {
        &()
    }
}

/// Configure pin multiplexing and the kernel debug GPIO.
unsafe fn set_pin_primary_functions(
    pmc: &sam3x::pmc::Pmc,
    pio: &'static sam3x::pio::Ports<'static>,
) {
    // Inputs and pin interrupts need the PIO clocks.
    for port in [PortId::A, PortId::B, PortId::C, PortId::D] {
        pio.port(port).enable_clock(pmc);
    }

    // UART to the 16U2 USB-serial bridge: URXD on PA8, UTXD on PA9.
    pio.pin(PortId::A, 8).map(|pin| {
        pin.set_floating_state(kernel::hil::gpio::FloatingState::PullUp);
        pin.select_peripheral(Peripheral::A);
    });
    pio.pin(PortId::A, 9)
        .map(|pin| pin.select_peripheral(Peripheral::A));

    // Kernel debug GPIOs, for `debug_gpio!(n, set | clear | toggle)`, and
    // logic analyzer markers:
    //   0: the "L" LED, PB27 (Arduino pin 13)
    //   1: marker A, PC23 (Arduino pin 7): high during the lock sequence
    //   2: marker B, PC22 (Arduino pin 8): toggles when untrusted processes
    //      are stopped
    //   3: marker C, PC24 (Arduino pin 6): high while sleeping in WFI
    // Pins 6, 7 and 8 are free on the loopback-wired test rig.
    let led = pio.pin(PortId::B, 27).unwrap();
    let marker_a = pio.pin(PortId::C, 23).unwrap();
    let marker_b = pio.pin(PortId::C, 22).unwrap();
    let marker_c = pio.pin(PortId::C, 24).unwrap();
    for pin in [led, marker_a, marker_b, marker_c] {
        pin.clear();
        pin.make_output();
    }
    let debug_gpios = static_init!(
        [&'static dyn kernel::hil::gpio::Pin; 4],
        [led, marker_a, marker_b, marker_c]
    );
    kernel::debug::initialize_debug_gpio::<<ChipHw as Chip>::ThreadIdProvider>();
    kernel::debug::assign_gpios(debug_gpios);
}

/// This is in a separate, inline(never) function so that its stack frame is
/// removed when this function returns. Otherwise, the stack space used for
/// these static_inits is wasted.
#[inline(never)]
unsafe fn start() -> (&'static kernel::Kernel, ArduinoDue, &'static ChipHw) {
    // The watchdog runs from reset with a ~16 s timeout. Its mode register is
    // write-once, so this must be the first write to it.
    sam3x::wdt::Wdt::new().disable();

    // The SAM3X boots with the ROM or the flash mapped at address 0. Point VTOR
    // at our vector table in flash so interrupts work either way.
    cortexm3::scb::set_vector_table_offset(sam3x::BASE_VECTORS.as_ptr().cast::<()>());

    ChipHw::init();

    // Initialize deferred calls very early.
    kernel::deferred_call::initialize_deferred_call_state::<<ChipHw as Chip>::ThreadIdProvider>();

    // Bind global variables to this thread.
    let _ =
        PANIC_RESOURCES.bind_to_thread::<<ChipHw as Chip>::ThreadIdProvider>(PanicResources::new());

    // Clocks: the RTT runs from the slow clock, so move that to the 32.768 kHz
    // crystal (Y2 on the Due schematic), then raise MCK to 84 MHz. On failure
    // the chip keeps running from the 4 MHz RC oscillator; this is reported
    // once the console is up.
    sam3x::supc::Supc::new().select_crystal_slow_clock();
    let pmc = static_init!(sam3x::pmc::Pmc, sam3x::pmc::Pmc::new());
    let clock_result =
        pmc.use_crystal_pll(&sam3x::eefc::Eefc::new(), CRYSTAL_HZ, PLLA_MUL, PLLA_DIV);

    let peripherals = static_init!(Sam3xDefaultPeripherals, Sam3xDefaultPeripherals::new(pmc));
    peripherals.setup_circular_deps();

    set_pin_primary_functions(pmc, &peripherals.pio);

    // Create an array to hold process references.
    let processes = components::process_array::ProcessArrayComponent::new()
        .finalize(components::process_array_component_static!(NUM_PROCS));
    PANIC_RESOURCES.get().map(|resources| {
        resources.processes.put(processes.as_slice());
    });

    // Setup space to store the core kernel data structure.
    let board_kernel = static_init!(kernel::Kernel, kernel::Kernel::new(processes.as_slice()));

    let chip = static_init!(ChipHw, sam3x::chip::Sam3x::new(peripherals));
    PANIC_RESOURCES.get().map(|resources| {
        resources.chip.put(chip);
    });

    // ALARM: the Real-time Timer, shared by all alarm users.
    let rtt = &peripherals.rtt;
    rtt.start();
    let mux_alarm = components::alarm::AlarmMuxComponent::new(rtt)
        .finalize(components::alarm_mux_component_static!(AlarmHw));

    // CONSOLE

    // SEGGER RTT: a console through the debug probe. Panic output goes here
    // too, whichever console is selected.
    // The RTT, console, UART mux and debug writer data buffers carry process
    // output, so they live in `.kernel_secrets`; their control structures do
    // not.
    let rtt_memory = components::segger_rtt::SeggerRttMemoryComponent::new().finalize((
        kernel::static_named_buf!(segger::rtt::SeggerRttMemory, "_SEGGER_RTT"),
        secret_buf!(
            [kernel::utilities::registers::InMemoryRegister<u8>;
                segger::rtt::DEFAULT_UP_BUFFER_LENGTH]
        ),
        secret_buf!(
            [kernel::utilities::registers::InMemoryRegister<u8>;
                segger::rtt::DEFAULT_DOWN_BUFFER_LENGTH]
        ),
    ));
    // ### Safety
    //
    // This aliases the `&mut` RTT memory that the RTT capsule receives below,
    // as `boards/sma_q3` does. The panic writer only uses it once the kernel
    // has stopped, so the two are never used at the same time.
    (*addr_of_mut!(io::WRITER)).set_rtt_memory(&*core::ptr::from_mut(rtt_memory.rtt_memory));
    let segger_rtt = components::segger_rtt::SeggerRttComponent::new(mux_alarm, rtt_memory)
        .finalize(components::segger_rtt_component_static!(AlarmHw));

    let console_device: &'static dyn kernel::hil::uart::Uart<'static> = match CONSOLE {
        ConsoleDevice::Uart => &peripherals.uart,
        ConsoleDevice::SeggerRtt => segger_rtt,
    };
    let uart_mux = components::console::UartMuxComponent::new(console_device, 115200).finalize((
        kernel::static_buf!(capsules_core::virtualizers::virtual_uart::MuxUart<'static>),
        secret_buf!([u8; capsules_core::virtualizers::virtual_uart::RX_BUF_LEN]),
    ));

    // `finalize()` configures the console device. If that is the UART, the
    // panic path must not configure it again.
    if CONSOLE == ConsoleDevice::Uart {
        (*addr_of_mut!(io::WRITER)).set_initialized();
    }

    // Create capabilities that the board needs to call certain protected kernel
    // functions.
    let memory_allocation_capability = create_capability!(capabilities::MemoryAllocationCapability);
    let process_management_capability =
        create_capability!(capabilities::ProcessManagementCapability);

    // Setup the console.
    let console = components::console::ConsoleComponent::new(
        board_kernel,
        capsules_core::console::DRIVER_NUM,
        uart_mux,
        create_capability!(capabilities::MemoryAllocationCapability),
    )
    .finalize((
        secret_buf!([u8; capsules_core::console::DEFAULT_BUF_SIZE]),
        secret_buf!([u8; capsules_core::console::DEFAULT_BUF_SIZE]),
        kernel::static_buf!(capsules_core::virtualizers::virtual_uart::UartDevice),
        kernel::static_buf!(capsules_core::console::Console<'static>),
    ));
    // Create the debugger object that handles calls to `debug!()`.
    components::debug_writer::DebugWriterComponent::new::<<ChipHw as Chip>::ThreadIdProvider>(
        uart_mux,
        create_capability!(capabilities::SetDebugWriterCapability),
    )
    .finalize((
        kernel::static_buf!(capsules_core::virtualizers::virtual_uart::UartDevice),
        kernel::static_buf!(kernel::collections::ring_buffer::RingBuffer<'static, u8>),
        secret_buf!([u8; 1024 * components::debug_writer::DEFAULT_DEBUG_BUFFER_KBYTE]),
        kernel::static_buf!(capsules_system::debug_writer::uart_debug_writer::UartDebugWriter),
    ));

    let pio = &peripherals.pio;

    // LED "L" on PB27 (Arduino pin 13).
    let led = components::led::LedsComponent::new().finalize(components::led_component_static!(
        LedHigh<'static, GpioPin>,
        LedHigh::new(pio.pin(PortId::B, 27).unwrap()),
    ));

    // The Due has no user button. Use Arduino pin 2 (PB25): wire it to GND
    // to press.
    let button_pin = pio.pin(PortId::B, 25).unwrap();
    button_pin.set_debounce(true);
    let button = components::button::ButtonComponent::new(
        board_kernel,
        capsules_core::button::DRIVER_NUM,
        components::button_component_helper!(
            GpioPin,
            (
                button_pin,
                kernel::hil::gpio::ActivationMode::ActiveLow,
                kernel::hil::gpio::FloatingState::PullUp
            )
        ),
        create_capability!(capabilities::MemoryAllocationCapability),
    )
    .finalize(components::button_component_static!(GpioPin));
    // Debounce period of 10 ms for port B: 2 * (163 + 1) slow clock cycles.
    pio.port(PortId::B).set_debounce_divider(163);

    // ALARM
    let alarm = components::alarm::AlarmDriverComponent::new(
        board_kernel,
        capsules_core::alarm::DRIVER_NUM,
        mux_alarm,
        create_capability!(capabilities::MemoryAllocationCapability),
    )
    .finalize(components::alarm_component_static!(AlarmHw));

    let process_printer = components::process_printer::ProcessPrinterTextComponent::new()
        .finalize(components::process_printer_text_component_static!());
    PANIC_RESOURCES.get().map(|resources| {
        resources.printer.put(process_printer);
    });

    // GPIO: Arduino digital pins that are free on the Due. Pins 0 and 1 are
    // the UART, 2 is the button, 6 to 8 are debug markers, 13 is the LED and
    // 22 is the tamper loop (driven by 23 on the test rig).
    let gpio = GpioComponent::new(
        board_kernel,
        capsules_core::gpio::DRIVER_NUM,
        components::gpio_component_helper!(
            GpioPin,
            3 => pio.pin(PortId::C, 28).unwrap(),
            5 => pio.pin(PortId::C, 25).unwrap(),
            9 => pio.pin(PortId::C, 21).unwrap(),
            11 => pio.pin(PortId::D, 7).unwrap(),
            12 => pio.pin(PortId::D, 8).unwrap(),
            24 => pio.pin(PortId::A, 15).unwrap(),
            25 => pio.pin(PortId::D, 0).unwrap(),
            26 => pio.pin(PortId::D, 1).unwrap(),
            27 => pio.pin(PortId::D, 2).unwrap(),
            28 => pio.pin(PortId::D, 3).unwrap(),
            29 => pio.pin(PortId::D, 6).unwrap(),
        ),
        create_capability!(capabilities::MemoryAllocationCapability),
    )
    .finalize(components::gpio_component_static!(GpioPin));

    // PROCESS CONSOLE
    kernel::create_typed_capability!(process_console_cap, ProcessConsoleCap:
        kernel::capabilities::ProcessManagementCapability,
        kernel::capabilities::ProcessStartCapability
    );
    let process_console = components::process_console::ProcessConsoleComponent::new(
        board_kernel,
        uart_mux,
        mux_alarm,
        process_printer,
        Some(cortexm3::support::reset),
        process_console_cap,
    )
    .finalize(components::process_console_component_static!(
        AlarmHw,
        ProcessConsoleCap
    ));
    let _ = process_console.start();

    // SCHEDULER TIMER
    let systick = static_init!(
        cortexm3::systick::SysTick,
        cortexm3::systick::SysTick::new_with_calibration(pmc.master_clock_hz())
    );

    // LOCKDOWN

    // Power profiles, with a keep-alive alarm for idling in WFI.
    let keepalive_alarm = static_init!(
        VirtualMuxAlarm<'static, AlarmHw>,
        VirtualMuxAlarm::new(mux_alarm)
    );
    keepalive_alarm.setup();
    let power = static_init!(
        power::PowerProfiles,
        power::PowerProfiles::new(
            pmc,
            &peripherals.uart,
            systick,
            chip,
            keepalive_alarm,
            SLEEP_IN_LOCKDOWN,
            pio.pin(PortId::C, 24).unwrap(),
        )
    );
    kernel::hil::time::Alarm::set_alarm_client(keepalive_alarm, power);
    chip.set_sleep_observer(power);

    let lockdown_platform = static_init!(
        lockdown::DuePlatform,
        lockdown::DuePlatform::new(
            pio.pin(PortId::C, 23).unwrap(),
            pio.pin(PortId::C, 22).unwrap(),
            power,
        )
    );

    // The tamper loop: Arduino pin 22 (PB26) with a pull-up. Closed (low) is
    // normal; open (high) locks. The glitch filter drops sub-cycle noise
    // without adding the debounce filter's delay; bounce is handled by the
    // unlock hold time.
    let trigger = pio.pin(PortId::B, 26).unwrap();
    trigger.set_glitch_filter(true);
    let lockdown_alarm = static_init!(
        VirtualMuxAlarm<'static, AlarmHw>,
        VirtualMuxAlarm::new(mux_alarm)
    );
    lockdown_alarm.setup();
    let lockdown = static_init!(
        lockdown::Manager,
        capsules_system::lockdown::LockdownManager::new(
            board_kernel,
            trigger,
            lockdown_alarm,
            lockdown_platform,
            ALLOWLIST,
            &FAULT_RESPONSE,
            lockdown::mint_capability(),
            UNLOCK_HOLD_MS,
        )
    );
    kernel::hil::time::Alarm::set_alarm_client(lockdown_alarm, lockdown);
    kernel::hil::gpio::Interrupt::set_client(trigger, lockdown);
    kernel::deferred_call::DeferredCallClient::register(lockdown);
    lockdown.setup();

    let console_hooks = static_init!(
        lockdown::ConsoleHooks,
        lockdown::ConsoleHooks::new(lockdown, power)
    );
    process_console.set_hooks(console_hooks);

    let scheduler = components::sched::round_robin::RoundRobinComponent::new(processes)
        .finalize(components::round_robin_component_static!(NUM_PROCS));

    let arduino_due = ArduinoDue {
        console,
        ipc: kernel::ipc::IPC::new(
            board_kernel,
            kernel::ipc::DRIVER_NUM,
            &memory_allocation_capability,
        ),
        led,
        button,
        alarm,
        gpio,

        scheduler,
        systick,
    };

    if let Err(e) = clock_result {
        debug!("Clock setup failed ({:?}), running from the 4 MHz RC", e);
    }
    debug!(
        "Arduino Due: MCK {} Hz. Initialization complete. Entering main loop",
        pmc.master_clock_hz()
    );

    // These symbols are defined in the linker script.
    extern "C" {
        /// Beginning of the ROM region containing app images.
        static _sapps: u8;
        /// End of the ROM region containing app images.
        static _eapps: u8;
        /// Beginning of the RAM region for app memory.
        static mut _sappmem: u8;
        /// End of the RAM region for app memory.
        static _eappmem: u8;
    }

    // Zero all application memory before any process gets some, so memory a
    // process is given, and any gap left unallocated, holds nothing from
    // earlier software (SRAM keeps its contents across resets).
    {
        let start = core::ptr::addr_of_mut!(_sappmem).cast::<u32>();
        let words = (core::ptr::addr_of!(_eappmem) as usize - start as usize) / 4;
        // ### Safety
        //
        // `[_sappmem, _eappmem)` is the application memory region from the
        // linker script, word aligned and not yet handed to any process or
        // referenced by the kernel.
        for i in 0..words {
            core::ptr::write_volatile(start.add(i), 0);
        }
    }

    kernel::process::load_processes(
        board_kernel,
        chip,
        core::slice::from_raw_parts(
            core::ptr::addr_of!(_sapps),
            core::ptr::addr_of!(_eapps) as usize - core::ptr::addr_of!(_sapps) as usize,
        ),
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(_sappmem),
            core::ptr::addr_of!(_eappmem) as usize - core::ptr::addr_of!(_sappmem) as usize,
        ),
        lockdown,
        &process_management_capability,
    )
    .unwrap_or_else(|err| {
        debug!("Error loading processes!");
        debug!("{:?}", err);
    });

    // Start locked down if the tamper loop is open, before any untrusted
    // process runs.
    lockdown.check_boot_state();

    // With the console on RTT the UART is free, so check it in loopback.
    if CONSOLE != ConsoleDevice::Uart {
        let test_alarm = static_init!(
            VirtualMuxAlarm<'static, AlarmHw>,
            VirtualMuxAlarm::new(mux_alarm)
        );
        test_alarm.setup();
        let test = static_init!(
            uart_loopback_test::UartLoopbackTest<VirtualMuxAlarm<'static, AlarmHw>>,
            uart_loopback_test::UartLoopbackTest::new(
                &peripherals.uart,
                test_alarm,
                static_init!([u8; uart_loopback_test::LEN], [0; uart_loopback_test::LEN]),
                static_init!([u8; uart_loopback_test::LEN], [0; uart_loopback_test::LEN]),
            )
        );
        test.start();
    }

    (board_kernel, arduino_due, chip)
}

/// Main function called after RAM initialized.
#[no_mangle]
pub unsafe fn main() {
    let main_loop_capability = create_capability!(capabilities::MainLoopCapability);

    let (board_kernel, platform, chip) = start();
    board_kernel.kernel_loop(&platform, chip, Some(&platform.ipc), &main_loop_capability);
}
