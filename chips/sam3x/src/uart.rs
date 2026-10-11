// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2026.

//! Universal Asynchronous Receiver Transmitter (UART).
//!
//! The SAM3X has one two-pin UART (URXD on PA8, UTXD on PA9, peripheral A),
//! which the Arduino Due connects to its USB-serial bridge. This driver is
//! interrupt driven, one byte at a time. It does not use the PDC, so no
//! peripheral writes to memory on its own.
//!
//! The UART only supports 8 data bits and 1 stop bit, without flow control.

use core::cell::Cell;

use kernel::ErrorCode;
use kernel::deferred_call::{DeferredCall, DeferredCallClient};
use kernel::hil::uart;
use kernel::utilities::StaticRef;
use kernel::utilities::cells::{OptionalCell, TakeCell};
use kernel::utilities::registers::interfaces::{ReadWriteable, Readable, Writeable};
use kernel::utilities::registers::{ReadOnly, ReadWrite, WriteOnly, register_bitfields};

use crate::pmc::{PeripheralId, Pmc};

#[repr(C)]
struct UartRegisters {
    /// Control Register.
    cr: WriteOnly<u32, Control::Register>,
    /// Mode Register.
    mr: ReadWrite<u32, Mode::Register>,
    /// Interrupt Enable Register.
    ier: WriteOnly<u32, Status::Register>,
    /// Interrupt Disable Register.
    idr: WriteOnly<u32, Status::Register>,
    /// Interrupt Mask Register.
    imr: ReadOnly<u32, Status::Register>,
    /// Status Register.
    sr: ReadOnly<u32, Status::Register>,
    /// Receive Holding Register.
    rhr: ReadOnly<u32>,
    /// Transmit Holding Register.
    thr: WriteOnly<u32>,
    /// Baud Rate Generator Register.
    brgr: ReadWrite<u32>,
}

register_bitfields![u32,
    Control [
        RSTRX OFFSET(2) NUMBITS(1) [],
        RSTTX OFFSET(3) NUMBITS(1) [],
        RXEN OFFSET(4) NUMBITS(1) [],
        RXDIS OFFSET(5) NUMBITS(1) [],
        TXEN OFFSET(6) NUMBITS(1) [],
        TXDIS OFFSET(7) NUMBITS(1) [],
        RSTSTA OFFSET(8) NUMBITS(1) []
    ],
    Mode [
        PAR OFFSET(9) NUMBITS(3) [
            Even = 0,
            Odd = 1,
            Space = 2,
            Mark = 3,
            No = 4
        ],
        CHMODE OFFSET(14) NUMBITS(2) [
            Normal = 0,
            AutomaticEcho = 1,
            LocalLoopback = 2,
            RemoteLoopback = 3
        ]
    ],
    Status [
        RXRDY OFFSET(0) NUMBITS(1) [],
        TXRDY OFFSET(1) NUMBITS(1) [],
        ENDRX OFFSET(3) NUMBITS(1) [],
        ENDTX OFFSET(4) NUMBITS(1) [],
        OVRE OFFSET(5) NUMBITS(1) [],
        FRAME OFFSET(6) NUMBITS(1) [],
        PARE OFFSET(7) NUMBITS(1) [],
        TXEMPTY OFFSET(9) NUMBITS(1) [],
        TXBUFE OFFSET(11) NUMBITS(1) [],
        RXBUFF OFFSET(12) NUMBITS(1) []
    ]
];

// ### Safety
//
// 0x400E0800 is the base address of the UART register block in the SAM3X
// memory map (datasheet section 7). The register layout matches
// `UartRegisters`, and the block is valid for the lifetime of the program.
const UART_BASE: StaticRef<UartRegisters> =
    unsafe { StaticRef::new(0x400E_0800 as *const UartRegisters) };

pub struct Uart<'a> {
    registers: StaticRef<UartRegisters>,
    pmc: &'a Pmc,

    tx_client: OptionalCell<&'a dyn uart::TransmitClient>,
    tx_buffer: TakeCell<'static, [u8]>,
    tx_len: Cell<usize>,
    tx_position: Cell<usize>,
    tx_abort: Cell<bool>,

    rx_client: OptionalCell<&'a dyn uart::ReceiveClient>,
    rx_buffer: TakeCell<'static, [u8]>,
    rx_len: Cell<usize>,
    rx_position: Cell<usize>,
    rx_abort: Cell<bool>,

    /// Baud rate of the last configuration, to recompute the divisor when the
    /// master clock changes.
    baud_rate: Cell<u32>,

    deferred_call: DeferredCall,
}

impl<'a> Uart<'a> {
    pub fn new(pmc: &'a Pmc) -> Self {
        Self {
            registers: UART_BASE,
            pmc,
            tx_client: OptionalCell::empty(),
            tx_buffer: TakeCell::empty(),
            tx_len: Cell::new(0),
            tx_position: Cell::new(0),
            tx_abort: Cell::new(false),
            rx_client: OptionalCell::empty(),
            rx_buffer: TakeCell::empty(),
            rx_len: Cell::new(0),
            rx_position: Cell::new(0),
            rx_abort: Cell::new(false),
            baud_rate: Cell::new(0),
            deferred_call: DeferredCall::new(),
        }
    }

    /// Connect the transmitter to the receiver inside the UART (local
    /// loopback), for self-tests. UTXD stays high while enabled.
    pub fn set_local_loopback(&self, enable: bool) {
        if enable {
            self.registers.mr.modify(Mode::CHMODE::LocalLoopback);
        } else {
            self.registers.mr.modify(Mode::CHMODE::Normal);
        }
    }

    /// Recompute the baud rate divisor for a new master clock frequency. Call
    /// after changing the master clock while the UART is configured.
    pub fn clock_changed(&self) {
        let baud = self.baud_rate.get();
        if baud != 0 {
            let divisor = 16 * baud;
            let cd = (self.pmc.master_clock_hz() + divisor / 2) / divisor;
            self.registers.brgr.set(cd.clamp(1, 0xFFFF));
        }
    }

    /// Blocking write of one byte, for panic output.
    pub fn send_byte(&self, byte: u8) {
        while !self.registers.sr.is_set(Status::TXRDY) {}
        self.registers.thr.set(byte as u32);
    }

    /// Block until the last byte has left the shift register.
    pub fn flush(&self) {
        while !self.registers.sr.is_set(Status::TXEMPTY) {}
    }

    pub fn handle_interrupt(&self) {
        let status = self.registers.sr.extract();
        let mask = self.registers.imr.extract();

        if mask.is_set(Status::TXRDY) && status.is_set(Status::TXRDY) {
            self.handle_tx_ready();
        }

        let errors =
            status.matches_any(&[Status::OVRE::SET, Status::FRAME::SET, Status::PARE::SET]);
        if mask.is_set(Status::RXRDY) && (status.is_set(Status::RXRDY) || errors) {
            let error = if status.is_set(Status::OVRE) {
                uart::Error::OverrunError
            } else if status.is_set(Status::FRAME) {
                uart::Error::FramingError
            } else if status.is_set(Status::PARE) {
                uart::Error::ParityError
            } else {
                uart::Error::None
            };
            if errors {
                self.registers.cr.write(Control::RSTSTA::SET);
            }
            let byte = status
                .is_set(Status::RXRDY)
                .then(|| self.registers.rhr.get() as u8);
            self.handle_rx(byte, error);
        }
    }

    fn handle_tx_ready(&self) {
        let position = self.tx_position.get();
        if position < self.tx_len.get() {
            self.tx_buffer.map(|buf| {
                self.registers.thr.set(buf[position] as u32);
            });
            self.tx_position.set(position + 1);
        } else {
            self.registers.idr.write(Status::TXRDY::SET);
            self.tx_buffer.take().map(|buf| {
                self.tx_client
                    .map(|client| client.transmitted_buffer(buf, self.tx_len.get(), Ok(())));
            });
        }
    }

    fn handle_rx(&self, byte: Option<u8>, error: uart::Error) {
        let mut position = self.rx_position.get();
        if let Some(byte) = byte {
            self.rx_buffer.map(|buf| {
                if let Some(slot) = buf.get_mut(position) {
                    *slot = byte;
                    position += 1;
                }
            });
            self.rx_position.set(position);
        }

        let done = position >= self.rx_len.get();
        if done || error != uart::Error::None {
            self.disable_rx_interrupts();
            let rval = if error == uart::Error::None {
                Ok(())
            } else {
                Err(ErrorCode::FAIL)
            };
            self.rx_buffer.take().map(|buf| {
                self.rx_client
                    .map(|client| client.received_buffer(buf, position, rval, error));
            });
        }
    }

    fn enable_rx_interrupts(&self) {
        self.registers
            .ier
            .write(Status::RXRDY::SET + Status::OVRE::SET + Status::FRAME::SET + Status::PARE::SET);
    }

    fn disable_rx_interrupts(&self) {
        self.registers
            .idr
            .write(Status::RXRDY::SET + Status::OVRE::SET + Status::FRAME::SET + Status::PARE::SET);
    }
}

impl DeferredCallClient for Uart<'_> {
    fn register(&'static self) {
        self.deferred_call.register(self);
    }

    fn handle_deferred_call(&self) {
        if self.tx_abort.take() {
            self.tx_buffer.take().map(|buf| {
                self.tx_client.map(|client| {
                    client.transmitted_buffer(buf, self.tx_position.get(), Err(ErrorCode::CANCEL))
                });
            });
        }
        if self.rx_abort.take() {
            self.rx_buffer.take().map(|buf| {
                self.rx_client.map(|client| {
                    client.received_buffer(
                        buf,
                        self.rx_position.get(),
                        Err(ErrorCode::CANCEL),
                        uart::Error::Aborted,
                    )
                });
            });
        }
    }
}

impl uart::Configure for Uart<'_> {
    fn configure(&self, params: uart::Parameters) -> Result<(), ErrorCode> {
        if params.width != uart::Width::Eight
            || params.stop_bits != uart::StopBits::One
            || params.hw_flow_control
        {
            return Err(ErrorCode::NOSUPPORT);
        }
        if params.baud_rate == 0 {
            return Err(ErrorCode::INVAL);
        }
        // Baud rate = MCK / (16 * CD), rounded to the nearest divisor.
        let divisor = 16 * params.baud_rate;
        let cd = (self.pmc.master_clock_hz() + divisor / 2) / divisor;
        if cd == 0 || cd > 0xFFFF {
            return Err(ErrorCode::INVAL);
        }

        self.pmc.enable_peripheral_clock(PeripheralId::Uart);

        let regs = &*self.registers;
        regs.idr.set(0xFFFF_FFFF);
        regs.cr.write(
            Control::RSTRX::SET
                + Control::RSTTX::SET
                + Control::RXDIS::SET
                + Control::TXDIS::SET
                + Control::RSTSTA::SET,
        );
        let parity = match params.parity {
            uart::Parity::None => Mode::PAR::No,
            uart::Parity::Odd => Mode::PAR::Odd,
            uart::Parity::Even => Mode::PAR::Even,
        };
        regs.mr.write(parity + Mode::CHMODE::Normal);
        regs.brgr.set(cd);
        self.baud_rate.set(params.baud_rate);
        regs.cr.write(Control::RXEN::SET + Control::TXEN::SET);

        // Resume interrupts for operations that were in progress.
        if self.tx_buffer.is_some() {
            regs.ier.write(Status::TXRDY::SET);
        }
        if self.rx_buffer.is_some() {
            self.enable_rx_interrupts();
        }
        Ok(())
    }
}

impl<'a> uart::Transmit<'a> for Uart<'a> {
    fn set_transmit_client(&self, client: &'a dyn uart::TransmitClient) {
        self.tx_client.set(client);
    }

    fn transmit_buffer(
        &self,
        tx_buffer: &'static mut [u8],
        tx_len: usize,
    ) -> Result<(), (ErrorCode, &'static mut [u8])> {
        if self.tx_buffer.is_some() || self.tx_abort.get() {
            return Err((ErrorCode::BUSY, tx_buffer));
        }
        if tx_len == 0 || tx_len > tx_buffer.len() {
            return Err((ErrorCode::SIZE, tx_buffer));
        }
        self.tx_len.set(tx_len);
        self.tx_position.set(0);
        self.tx_buffer.replace(tx_buffer);
        // The transmit holding register is empty when idle, so this interrupt
        // fires right away and sends the first byte.
        self.registers.ier.write(Status::TXRDY::SET);
        Ok(())
    }

    fn transmit_word(&self, _word: u32) -> Result<(), ErrorCode> {
        Err(ErrorCode::NOSUPPORT)
    }

    fn transmit_abort(&self) -> Result<(), ErrorCode> {
        if self.tx_buffer.is_none() {
            return Ok(());
        }
        self.registers.idr.write(Status::TXRDY::SET);
        self.tx_abort.set(true);
        self.deferred_call.set();
        Err(ErrorCode::BUSY)
    }
}

impl<'a> uart::Receive<'a> for Uart<'a> {
    fn set_receive_client(&self, client: &'a dyn uart::ReceiveClient) {
        self.rx_client.set(client);
    }

    fn receive_buffer(
        &self,
        rx_buffer: &'static mut [u8],
        rx_len: usize,
    ) -> Result<(), (ErrorCode, &'static mut [u8])> {
        if self.rx_buffer.is_some() || self.rx_abort.get() {
            return Err((ErrorCode::BUSY, rx_buffer));
        }
        if rx_len == 0 || rx_len > rx_buffer.len() {
            return Err((ErrorCode::SIZE, rx_buffer));
        }
        self.rx_len.set(rx_len);
        self.rx_position.set(0);
        self.rx_buffer.replace(rx_buffer);
        self.enable_rx_interrupts();
        Ok(())
    }

    fn receive_word(&self) -> Result<(), ErrorCode> {
        Err(ErrorCode::NOSUPPORT)
    }

    fn receive_abort(&self) -> Result<(), ErrorCode> {
        if self.rx_buffer.is_none() {
            return Ok(());
        }
        self.disable_rx_interrupts();
        self.rx_abort.set(true);
        self.deferred_call.set();
        Err(ErrorCode::BUSY)
    }
}
