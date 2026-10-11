Arduino Due
===========

The [Arduino Due](https://docs.arduino.cc/hardware/due/) has an ATSAM3X8E
(Cortex-M3, 512 KB flash, 96 KB SRAM) running at 84 MHz from a 12 MHz crystal.

## Memory layout

| Region        | Address      | Size   | Use                                   |
|---------------|--------------|--------|---------------------------------------|
| Flash bank 0  | `0x00080000` | 256 KB | Kernel                                |
| Flash bank 1  | `0x000C0000` | 256 KB | Apps (TBFs back to back)              |
| SRAM          | `0x20070000` | 96 KB  | Kernel stack and data, then app RAM   |

SRAM0 (64 KB) is mirrored at `0x20070000`, directly below SRAM1 (32 KB) at
`0x20080000`, so the kernel uses both as one region.

## Peripherals

| Function       | Pin                          | Driver                     |
|----------------|------------------------------|----------------------------|
| Console        | SEGGER RTT (default) or UART | See "Console" below        |
| LED            | PB27 (pin 13, "L")           | LED driver, `debug_gpio!(0, ...)` |
| Button         | PB25 (pin 2), pull-up        | Short to GND to press      |
| Tamper loop    | PB26 (pin 22), pull-up       | Lockdown trigger, see below |
| GPIO           | Pins 3, 5-9, 11, 12, 22-29   | GPIO driver, by pin number |
| Alarm          | RTT, 32.768 kHz              | Alarm driver               |

## Tamper lockdown

The board implements the lockdown proof of concept
(`capsules/system/src/lockdown.rs`, `src/lockdown.rs`, `src/power.rs`).

- **Trigger:** a normally closed tamper loop from Arduino pin 22 (PB26) to
  GND; PB26 has a pull-up and the glitch filter. Opening the loop (rising
  edge) locks at once. Closing it unlocks after it has stayed closed for
  500 ms (`UNLOCK_HOLD_MS`). If the loop is open at boot, the board starts
  locked, before any untrusted process runs.
- **Allowlist:** `ALLOWLIST` in `src/main.rs`, by TBF name (`due_keeper`).
- **Lock:** terminates every other process, zeroes its memory
  (`Process::zeroize_memory`), zeroes `.kernel_secrets` (the console, UART
  mux, RTT and debug writer data buffers) and the unused kernel stack, then
  switches to the lockdown power profile: 12 MHz RC oscillator, PLL and
  crystal off, PIOA/C/D clocks gated, and optionally WFI (`SLEEP_IN_LOCKDOWN`,
  or `wfi on` in the process console). See "Debugging notes" before enabling
  WFI on a bench.
- **Unlock:** restores the 84 MHz profile and restarts the terminated
  processes from flash.
- **While locked:** a faulting process is stopped, terminated and wiped, and
  restarts only on unlock; the process console refuses `start` and `boot`
  for processes not on the allowlist.
- **Process console:** `lockdown`, `unlock`, `lockstat` (state, counts and
  timing in CPU cycles), `wfi on|off`, and `wfi test <ms>`, which idles in
  WFI for that long and then reports the time asleep (measured with the RTT,
  since the CPU cycle counter keeps running in Sleep mode).
- **Markers:** pin 7 (PC23) is high during the lock sequence, pin 8 (PC22)
  toggles once untrusted processes are stopped, pin 6 (PC24) is high while
  sleeping in WFI.

On the loopback test rig, pin 23 (PA14) is wired to pin 22, so the probe can
simulate the loop with `tools/tamper.sh open|close|status`.
`tools/lockdown_soak.py` locks and unlocks repeatedly, scanning SRAM for
canaries after each lock and checking the vault restarted after each unlock.

## Console

`CONSOLE` in `src/main.rs` selects where the console, `debug!()` and the
process console go:

- `ConsoleDevice::SeggerRtt` (default): through the debug probe. Run
  `make rtt` and connect with `nc localhost 9090`, or use
  `tools/rtt_capture.sh` and `tools/rtt_cmd.sh`. With this setting the board
  runs a UART self-test in local loopback mode two seconds after boot.
- `ConsoleDevice::Uart`: the UART on PA8/PA9 (pins 0/1), which the 16U2
  bridges to the programming USB port, 115200 8N1.

Panic output always goes to both.

The UART has no receive FIFO. At 115200 baud a received byte is lost if the
kernel takes longer than one character time (87 us) to service the
interrupt, which long kernel operations can exceed.

## Debugging notes

- The kernel idles by spinning, not in WFI: on the SAM3X a core sleeping in
  WFI makes the debug port answer WAIT until it wakes, which breaks RTT,
  flashing and GDB. Measured in lockdown (12 MHz RC), the core sleeps about
  97% of the time even with the 10 ms keep-alive wake-up, and the probe cannot
  attach. The keep-alive still helps recovery: repeated raw AP writes of
  SYSRESETREQ get through during a wake-up. Otherwise press RESET; WFI is off
  after reset.
- The flash targets end with a reset of processor and peripherals through
  the reset controller, since a reset through the core leaves peripheral
  state such as enabled interrupts in place.
- Software that ran before the kernel (for example a debugger's flash
  algorithm) can leave PMC write protection on; the PMC driver turns it off.

## Building

```
make
```

## Flashing with a debug probe

Connect the probe to the 10-pin debug header. The default is a J-Link over
SWD; for others, set `OPENOCD_INTERFACE` (and `OPENOCD_TRANSPORT`):

```
make flash OPENOCD_INTERFACE=interface/cmsis-dap.cfg
```

`make flash` also sets GPNVM bit 1, so the chip boots from flash rather than
the ROM bootloader.

Apps are TBF files written back to back from the start of flash bank 1. The
bank is erased first:

```
make flash-apps APPS="path/to/a.tbf path/to/b.tbf"
```

Other targets:

- `make dump-sram`: halt and save all 96 KB of SRAM to `sram.bin`.
- `make debug-server`: start OpenOCD for GDB on port 3333.
- `make erase-apps`: erase flash bank 1.
- `make reset`: reset processor and peripherals.
- `make rtt`: serve the RTT console on TCP port 9090.

## Test apps

`test_apps/` holds libtock-rs apps. `test_apps/build.sh` builds a set of them
for fixed addresses (each app's flash slot follows the previous TBF; RAM slots
are 16 KB from `0x20078000`) and writes `test_apps/build/<set>/apps.bin`.

| Set    | Apps                       | Purpose                                         |
|--------|----------------------------|-------------------------------------------------|
| `poc`  | keeper, vault, worker      | Lockdown PoC: allowlisted app, secrets, load    |
| `mpu`  | keeper, intruder           | The intruder writes into the keeper's RAM; the MPU must stop it |
| `gpio` | keeper, gpio_test          | PIO levels, edges, button and debounce          |

- **keeper**: LED and console heartbeat once a second.
- **vault**: 32-byte canaries in its stack (S), `.data` (D), heap (H) and a
  buffer it shares with the console capsule every second (C).
- **worker**: CPU-bound loop with continuous console output.
- **gpio_test** needs loopback wires D25 to D26 and D24 to D2, and drives D24
  and D25 as outputs. Only run it right after a full reset (`make reset` or
  any flash target), so the other end of each wire is an input; on a shared
  rig, check first that nothing else drives those pins.

```
test_apps/build.sh ~/libtock-rs poc
make flash-apps APPS=test_apps/build/poc/apps.bin
tools/rtt_capture.sh 10
make canary-scan APPS=test_apps/build/poc/apps.bin SCAN_ARGS="--expect-in due_vault:S,D,H,C"
```

`tools/canary_scan.py` dumps SRAM through the probe and reports every full or
partial copy of a vault canary, by kernel buffer or process.

Debug markers for a logic analyzer: `debug_gpio!(1, ...)` drives pin 7 (PC23)
and `debug_gpio!(2, ...)` pin 8 (PC22).

## Flashing without a probe

`make program` uses `bossac` and the ROM bootloader over the programming USB
port. Press ERASE, then RESET, first. This only writes the kernel.
