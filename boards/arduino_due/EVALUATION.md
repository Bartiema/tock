Lockdown PoC on the ATSAM3X8E: Evaluation
==========================================

Measured on 2026-10-07 on an Arduino Due (ATSAM3X8E rev. `0x285E0A60`) with a
J-Link over SWD, on the loopback-wired test rig of the sam3_hal_rs project.
Kernel: this tree, release build with LTO. Apps: `test_apps` set `poc`
(keeper, vault, worker). Console over SEGGER RTT; no logic analyzer was
available, so timings come from the CPU cycle counter (DWT) and the RTT.

Summary
-------

| Criterion | Target | Result | Status |
|-----------|--------|--------|--------|
| Stop latency | < 1 ms | 21 us from the trigger's interrupt handler to all untrusted processes terminated (1 789 cycles at 84 MHz). Edge-to-handler latency not measured. | Met for the measured part |
| Full lockdown time | < 10 ms | 0.78 ms to all wipes done, plus 0.15 ms for the clock switch: about 0.93 ms from the handler | Met |
| Wipe completeness | 0 canaries | 0 full or partial canaries anywhere in SRAM after every lock (100 soak locks, plus 32 in earlier soak runs) | Met |
| Region zeroing | every byte 0 | The process-accessible memory and grant region of each terminated process reads 0 (kernel read-back check: 0 failures; SRAM dumps). The kernel's ~1.2 KB of bookkeeping at the top of each region is not wiped. | Met with exception |
| Allowlisted continuity | no missed heartbeat in 10 min | Heartbeat continued without a gap across every lock and unlock observed and through the WFI test; the keeper is never terminated by lockdown. A timed 10-minute heartbeat count was not made. | Partly verified |
| Power profile applied | clocks gated, RC, sleep | Register readback: PIOA/C/D gated, MCK from the 12 MHz RC, PLL and crystal off, FWS 0. Sleep: 96.9% of the time in WFI during a 3 s test, keeper running. | Met (UART clock cannot be gated; WFI not default, see below) |
| Robustness | 100 cycles | 100/100 cycles passed (plus 22/22 and 10/10 in earlier runs); no panic, no leaked canary | Met |
| Unlock | fresh restart, 84 MHz | Vault and worker restart from flash and plant fresh canaries every cycle; MCK back on PLLA at 84 MHz | Met |

What was built
--------------

- `chips/sam3x`: PMC (crystal and PLL, internal RC, peripheral clocks, write
  protection), EEFC wait states, SUPC slow clock, watchdog, PIO (edge
  interrupts, glitch and debounce filters), interrupt-driven UART, RTT alarm.
- `boards/arduino_due`: board, linker layout with a `.kernel_secrets`
  section, RTT console, test apps and bench tools.
- Kernel: `ProcessZeroizeCapability` and `Process::zeroize_memory`, and
  `static_buf_in_section!`.
- `capsules/system/src/lockdown.rs`: `LockdownManager` (trigger, allowlist,
  lock and unlock sequences, lockdown-aware fault policy, console commands).
- Process console: hooks to add commands and refuse starting processes.

Lock sequence timing
--------------------

Cycle counts from the start of the lock handler (the trigger pin's bottom
half), at 84 MHz, from the `lockstat` readings taken across the test
session. (The kernel also keeps min/mean/max over all locks, but the board
was power-cycled after the 100-cycle soak, before those could be read.)

| Step | Min | Max |
|------|-----|-----|
| Untrusted processes terminated (marker B) | 1 743 (20.8 us) | 1 912 (22.8 us) |
| Processes, kernel secrets and stack wiped | 65 316 (0.78 ms) | 66 689 (0.79 ms) |

Zeroing 14 120 bytes of process memory takes about 51 000 cycles (0.61 ms),
`.kernel_secrets` (3 296 bytes) and the unused kernel stack (5 760 bytes)
about 13 000 cycles. The clock switch to the RC oscillator, timed with the
RTT, takes 152 us.

Not measured: the time from the pin edge to the lock handler. It needs the
logic analyzer (markers on pins 6, 7 and 8 are in place). When a process is
running, the pin interrupt preempts it at once; when the kernel is busy, the
handler waits for the current kernel task. Kernel tasks of more than 87 us
were observed (see the UART note below), so the edge-to-handler time can be a
few hundred microseconds in the worst case.

Test matrix
-----------

| Scenario | How | Result |
|----------|-----|--------|
| Vault running / blocked in a syscall | Soak: the vault spends most time waiting on an alarm, the worker computing | No canary after any lock |
| Console transfer in flight | Soak: the worker writes to the console continuously, the vault shares a buffer with the console every second | Console and RTT buffers wiped (`.kernel_secrets`), no canary |
| Kernel busy | Lock while the console is flooded and the process console has queued output | Lockdown completes; found and fixed a process console panic (below) |
| Switch bounce | `tools/bounce_test.sh`: edges 5 to 300 ms apart, faster than the 500 ms unlock hold | One unlock and one lock per settled level |
| Unlock | Soak | Restart every cycle |
| Boot with loop open | Reset with the loop open | Boots locked; untrusted processes terminated and wiped before they run |
| Fault during lockdown | `fault due_keeper` while locked | Keeper stopped, terminated, 6 960 bytes zeroed; restarted on unlock |
| Spoofed name | The vault packaged as `due_keeper` | Survives lockdown with its canaries, as expected: the name-based allowlist is a PoC limit |
| Optimised build | Release build with LTO, disassembly | Word store loops in `zeroize_memory` and the kernel secrets wipe, and the read-back loop, are present |
| Soak | `tools/lockdown_soak.py`, 100 cycles | 100/100 cycles passed (plus 22/22 and 10/10 in earlier runs); no panic, no leaked canary |

Findings and deviations from the plan
-------------------------------------

1. **Trigger debouncing.** The PIO debounce filter delays the input by up to
   1.5 periods (15 ms at the 10 ms needed for a switch), which breaks the 1 ms
   stop target. The trigger uses the glitch filter instead, locks on the
   first rising edge (repeated locks do nothing) and unlocks only after the
   loop has stayed closed for 500 ms.
2. **Kernel bookkeeping is not wiped.** The top of each process's memory
   region holds the kernel's process structure, upcall queue and grant
   pointer table (about 1.2 KB). They stay in use, so `zeroize_memory` leaves
   them; it resets the saved registers. No canary was found there.
3. **WFI and the debugger.** With WFI enabled, the SAM3X debug port answers
   WAIT while the core sleeps. In lockdown the core sleeps 97% of the time,
   so the probe cannot attach even with a 10 ms keep-alive wake-up. WFI is
   therefore off by default (`SLEEP_IN_LOCKDOWN`), and sleep was verified with
   a time-limited test (`wfi test`). The CPU cycle counter keeps running in
   Sleep mode, so sleep time is measured with the RTT.
4. **UART clock.** The UART peripheral clock cannot be gated: PMC_PCSR0 bit 8
   stays set after a write to PMC_PCDR0, consistent with Table 9-1.
5. **UART baud rate on the RC oscillator.** At 12 MHz the nearest divisor for
   115200 baud is 7%. The RC itself measured 12.006 MHz. A UART console in
   lockdown needs a lower baud rate (38400 is within 2.3%).
6. **UART without a FIFO.** At 115200 baud the UART loses a byte if the
   kernel takes more than 87 us to service it. Copying a large buffer into the
   RTT console exceeds that. The PDC would avoid it, but the plan excludes DMA.
7. **Process console panic (upstream bug, fixed).** `write_byte` overflowed
   the 300-byte queue when the console was flooded and input kept arriving,
   panicking the kernel.
8. **RTT console input stalls (open).** Input is not processed while another
   process floods the console, and in lockdown it stops being processed
   about 10 s after boot, although the RTT driver keeps consuming the bytes.
   Likely cause: when a second receiver appears, the UART mux aborts the
   ongoing read and waits for its callback, but `SeggerRtt::receive_abort`
   returns `Ok(())` without one, so the mux waits forever. Not fixed yet.
9. **Stale SRAM.** SRAM keeps its contents across resets, and app memory not
   given to any process was never cleared. The board now zeroes all app
   memory at boot.
10. **Not done:** Wait and Backup modes, current measurement (out of scope),
    edge latency (needs the analyzer), Phase 6 (custom board).

Reproducing
-----------

```
make -C boards/arduino_due flash
boards/arduino_due/test_apps/build.sh <libtock-rs> poc
make -C boards/arduino_due flash-apps APPS=test_apps/build/poc/apps.bin
cd boards/arduino_due
tools/tamper.sh close            # unlock (the rig boots with the loop open)
tools/lockdown_soak.py --elf ../../target/thumbv7m-none-eabi/release/arduino_due.elf \
    --apps test_apps/build/poc/apps.bin --cycles 100
tools/rtt_cmd.sh 5 lockstat
```
