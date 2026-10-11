// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr::addr_of_mut;

use kernel::debug;
use kernel::hil::led;
use kernel::hil::uart;
use kernel::hil::uart::Configure;
use kernel::utilities::cells::OptionalCell;
use kernel::utilities::io_write::IoWrite;

use sam3x::pio::PortId;

/// Writer is used by kernel::debug to panic message to the serial port.
pub struct Writer {
    initialized: bool,
    rtt: OptionalCell<&'static segger::rtt::SeggerRttMemory<'static>>,
}

/// Global static for debug writer
pub static mut WRITER: Writer = Writer {
    initialized: false,
    rtt: OptionalCell::empty(),
};

impl Writer {
    /// Indicate that the UART has already been configured, so the panic path
    /// does not reset it in the middle of a transmission.
    pub fn set_initialized(&mut self) {
        self.initialized = true;
    }

    /// Also send panic output to SEGGER RTT.
    pub fn set_rtt_memory(&self, rtt: &'static segger::rtt::SeggerRttMemory<'static>) {
        self.rtt.set(rtt);
    }
}

impl Write for Writer {
    fn write_str(&mut self, s: &str) -> ::core::fmt::Result {
        self.write(s.as_bytes());
        Ok(())
    }
}

impl IoWrite for Writer {
    fn write(&mut self, buf: &[u8]) -> usize {
        // A fresh `Pmc` assumes the 4 MHz reset clock. That is only used to
        // compute the baud rate if the UART was never configured, i.e. for a
        // panic before `main` has switched clocks and set up the console.
        let pmc = sam3x::pmc::Pmc::new();
        let uart = sam3x::uart::Uart::new(&pmc);

        if !self.initialized {
            self.initialized = true;
            let _ = uart.configure(uart::Parameters {
                baud_rate: 115200,
                stop_bits: uart::StopBits::One,
                parity: uart::Parity::None,
                hw_flow_control: false,
                width: uart::Width::Eight,
            });
        }

        for &c in buf {
            uart.send_byte(c);
        }
        uart.flush();
        self.rtt.map(|rtt| rtt.write_sync(buf));
        buf.len()
    }
}

/// The panic message and location, kept in RAM for a debugger.
///
/// The RTT console is a 1 KB ring, and the process dump that follows a panic
/// overwrites the message. Read this with the probe instead, e.g.
/// `dump_image panic.txt <address of PANIC_RECORD> 256` in OpenOCD.
#[no_mangle]
pub static mut PANIC_RECORD: [u8; 256] = [0; 256];

struct PanicRecordWriter {
    position: usize,
}

impl Write for PanicRecordWriter {
    fn write_str(&mut self, s: &str) -> ::core::fmt::Result {
        // ### Safety
        //
        // Only the panic handler writes `PANIC_RECORD`, and the kernel is
        // single threaded and no longer running.
        let record = unsafe { &mut *addr_of_mut!(PANIC_RECORD) };
        for &b in s.as_bytes() {
            if self.position < record.len() - 1 {
                record[self.position] = b;
                self.position += 1;
            }
        }
        Ok(())
    }
}

/// Panic handler.
#[panic_handler]
pub unsafe fn panic_fmt(info: &PanicInfo) -> ! {
    let _ = write!(PanicRecordWriter { position: 0 }, "{}", info);

    // The "L" LED is on PB27 (Arduino pin 13).
    let pin = sam3x::pio::Pin::new(PortId::B, 27);
    let led = &mut led::LedHigh::new(&pin);

    let writer = &mut *addr_of_mut!(WRITER);

    debug::panic_old(
        &mut [led],
        writer,
        info,
        &cortexm3::support::nop,
        crate::PANIC_RESOURCES.get(),
    )
}
