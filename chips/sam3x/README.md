SAM3X Chip Crate
================

Tock support for the Microchip (Atmel) SAM3X family (Cortex-M3), written for
the ATSAM3X8E on the Arduino Due.

Supported peripherals:

| Module | Peripheral                         | Notes                                              |
|--------|------------------------------------|----------------------------------------------------|
| `wdt`  | Watchdog                           | Disable only. The mode register is write-once.     |
| `eefc` | Flash controllers (both banks)     | Wait states only.                                  |
| `supc` | Supply controller                  | Select the 32.768 kHz crystal as slow clock.       |
| `pmc`  | Power management controller        | Crystal and PLLA to MCK; peripheral clock gating.  |
| `pio`  | GPIO (PIOA to PIOD)                | `gpio::InterruptPin`, edge selection, debouncing.  |
| `uart` | UART (PA8/PA9)                     | `uart::Uart`, interrupt driven, no PDC.            |
| `rtt`  | Real-time timer                    | `time::Alarm` at 32.768 kHz from the slow clock.   |

Not supported: USART, SPI, TWI, ADC, DAC, timer counters, DMA/PDC, USB, CAN,
EMAC, and the low-power modes beyond Sleep.

Behaviour found on hardware:

- `Chip::sleep` spins by default instead of executing WFI: a SAM3X core
  sleeping in WFI makes the debug port answer WAIT until it wakes, so the
  probe loses access. Enable WFI with `Sam3x::set_wfi_sleep(true)`.
- The RTT alarm disables its interrupt in the handler and re-enables it only
  once the status register has cleared (two slow clock cycles after a read).
  Re-enabling it earlier makes it fire continuously and starves the UART.
- The PIO latches input changes in its interrupt status register even for
  pins whose interrupt is disabled. Enabling a pin's interrupt drops its stale
  flag; flags of other pins read at that moment are dispatched from a deferred
  call.
- `Pmc` disables PMC write protection before writing, since earlier software
  may have left it on.

Boards must, in this order: disable the watchdog, set VTOR to `BASE_VECTORS`,
switch clocks with `Pmc::use_crystal_pll`, then create
`Sam3xDefaultPeripherals`. See `boards/arduino_due`.
