// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! UART self-test in local loopback mode.
//!
//! Sends a message through the UART with its transmitter connected to its
//! receiver internally, and checks that the same bytes arrive. This exercises
//! the interrupt-driven transmit and receive paths without external wiring.
//! Results are reported with `debug!`, so the console must not be the UART.
//!
//! The UART has no receive FIFO: at 115200 baud a byte is lost if the kernel
//! does not service the interrupt within one character time (87 us). Long
//! kernel operations, such as the RTT console copying a large buffer, exceed
//! that. The test therefore starts after the boot output has drained, runs
//! several rounds and reports how many passed.

use core::cell::Cell;

use kernel::ErrorCode;
use kernel::debug;
use kernel::hil::time::{Alarm, AlarmClient, ConvertTicks};
use kernel::hil::uart::{self, Configure, Receive, Transmit};
use kernel::utilities::cells::TakeCell;

const MESSAGE: &[u8] = b"Tock on the SAM3X: UART loopback";
pub const LEN: usize = MESSAGE.len();
const ROUNDS: u32 = 10;
const START_DELAY_MS: u32 = 2000;
const ROUND_INTERVAL_MS: u32 = 1000;

pub struct UartLoopbackTest<A: Alarm<'static> + 'static> {
    uart: &'static sam3x::uart::Uart<'static>,
    alarm: &'static A,
    tx: TakeCell<'static, [u8]>,
    rx: TakeCell<'static, [u8]>,
    round: Cell<u32>,
    passed: Cell<u32>,
}

impl<A: Alarm<'static> + 'static> UartLoopbackTest<A> {
    pub fn new(
        uart: &'static sam3x::uart::Uart<'static>,
        alarm: &'static A,
        tx: &'static mut [u8; LEN],
        rx: &'static mut [u8; LEN],
    ) -> Self {
        Self {
            uart,
            alarm,
            tx: TakeCell::new(tx),
            rx: TakeCell::new(rx),
            round: Cell::new(0),
            passed: Cell::new(0),
        }
    }

    /// Configure the UART and schedule the first round.
    pub fn start(&'static self) {
        self.uart.set_transmit_client(self);
        self.uart.set_receive_client(self);
        self.alarm.set_alarm_client(self);
        let result = self.uart.configure(uart::Parameters {
            baud_rate: 115200,
            width: uart::Width::Eight,
            parity: uart::Parity::None,
            stop_bits: uart::StopBits::One,
            hw_flow_control: false,
        });
        if let Err(e) = result {
            debug!("uart loopback: FAIL, configure returned {:?}", e);
            return;
        }
        self.alarm
            .set_alarm(self.alarm.now(), self.alarm.ticks_from_ms(START_DELAY_MS));
    }

    fn run_round(&self) {
        self.uart.set_local_loopback(true);
        if let Some(rx) = self.rx.take() {
            if let Err((e, buf)) = self.uart.receive_buffer(rx, LEN) {
                self.rx.replace(buf);
                debug!("uart loopback: FAIL, receive_buffer returned {:?}", e);
                return;
            }
        }
        if let Some(tx) = self.tx.take() {
            tx.copy_from_slice(MESSAGE);
            if let Err((e, buf)) = self.uart.transmit_buffer(tx, LEN) {
                self.tx.replace(buf);
                debug!("uart loopback: FAIL, transmit_buffer returned {:?}", e);
            }
        }
    }

    fn round_done(&self, ok: bool) {
        self.uart.set_local_loopback(false);
        if ok {
            self.passed.set(self.passed.get() + 1);
        }
        let round = self.round.get() + 1;
        self.round.set(round);
        if round < ROUNDS {
            self.alarm.set_alarm(
                self.alarm.now(),
                self.alarm.ticks_from_ms(ROUND_INTERVAL_MS),
            );
        } else {
            let passed = self.passed.get();
            debug!(
                "uart loopback: {} ({}/{} rounds passed)",
                if passed == ROUNDS { "PASS" } else { "FAIL" },
                passed,
                ROUNDS
            );
        }
    }
}

impl<A: Alarm<'static> + 'static> AlarmClient for UartLoopbackTest<A> {
    fn alarm(&self) {
        self.run_round();
    }
}

impl<A: Alarm<'static> + 'static> uart::TransmitClient for UartLoopbackTest<A> {
    fn transmitted_buffer(
        &self,
        buffer: &'static mut [u8],
        len: usize,
        rval: Result<(), ErrorCode>,
    ) {
        if rval.is_err() || len != LEN {
            debug!("uart loopback: transmitted {} bytes, {:?}", len, rval);
        }
        self.tx.replace(buffer);
    }
}

impl<A: Alarm<'static> + 'static> uart::ReceiveClient for UartLoopbackTest<A> {
    fn received_buffer(
        &self,
        buffer: &'static mut [u8],
        len: usize,
        rval: Result<(), ErrorCode>,
        error: uart::Error,
    ) {
        let ok = rval.is_ok() && len == LEN && &buffer[..len] == MESSAGE;
        if !ok {
            debug!(
                "uart loopback: round {} failed after {} bytes: {:?} {:?}",
                self.round.get(),
                len,
                rval,
                error
            );
        }
        self.rx.replace(buffer);
        self.round_done(ok);
    }
}
