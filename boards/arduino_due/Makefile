# Licensed under the Apache License, Version 2.0 or the MIT License.
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Copyright Tock Contributors 2026.

# Makefile for building the Tock kernel for the Arduino Due.

include ../Makefile.common

OPENOCD ?= openocd
# Debug probe configuration. Override this for your probe, for example
#   make flash OPENOCD_INTERFACE="interface/cmsis-dap.cfg"
OPENOCD_INTERFACE ?= interface/jlink.cfg
OPENOCD_TRANSPORT ?= swd
# Without the probe's reset line wired to NRST, reset through the core
# (SYSRESETREQ) instead. Set to "srst_only" if NRST is wired.
OPENOCD_RESET_CONFIG ?= none
OPENOCD_CMD = $(OPENOCD) -f $(OPENOCD_INTERFACE) -c "transport select $(OPENOCD_TRANSPORT)" \
	-f target/at91sam3ax_8x.cfg -c "reset_config $(OPENOCD_RESET_CONFIG)"
RTT_PORT ?= 9090
# Reset processor and peripherals through the reset controller (RSTC_CR with
# PROCRST and PERRST). A reset through the core only resets the processor, so
# peripheral state such as enabled interrupts would survive into the kernel.
RESET_ALL = mww 0x400E1A00 0xA5000005

# Apps are flashed to the start of flash bank 1.
APPS_ADDRESS = 0x000C0000
SRAM_ADDRESS = 0x20070000
SRAM_SIZE = 0x18000

# Space separated list of .tbf files to flash, e.g.
#   make flash-apps APPS="keeper.tbf vault.tbf"
APPS ?=

BOSSAC ?= bossac
BOSSAC_PORT ?= ttyACM0

.PHONY: install
install: flash

# Write the kernel to flash bank 0 and set GPNVM bit 1, so the chip boots from
# flash instead of the ROM bootloader.
.PHONY: flash
flash: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/$(PLATFORM).elf
	$(OPENOCD_CMD) -c "init; reset halt; flash write_image erase $<; verify_image $<; at91sam3 gpnvm set 1; $(RESET_ALL); shutdown"

.PHONY: flash-debug
flash-debug: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/debug/$(PLATFORM).elf
	$(OPENOCD_CMD) -c "init; reset halt; flash write_image erase $<; verify_image $<; at91sam3 gpnvm set 1; $(RESET_ALL); shutdown"

# Erase all of flash bank 1 so no stale app survives, then write the TBFs back
# to back from its start.
.PHONY: flash-apps
flash-apps: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/apps.bin
	$(OPENOCD_CMD) -c "init; reset halt; flash erase_sector 1 0 last; flash write_image $< $(APPS_ADDRESS) bin; verify_image $< $(APPS_ADDRESS) bin; $(RESET_ALL); shutdown"

# Always rebuilt, so a changed APPS list is picked up.
.PHONY: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/apps.bin
$(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/apps.bin: $(APPS)
	$(if $(APPS),,$(error Set APPS to a list of .tbf files))
	mkdir -p $(dir $@)
	cat $(APPS) > $@

.PHONY: erase-apps
erase-apps:
	$(OPENOCD_CMD) -c "init; reset halt; flash erase_sector 1 0 last; $(RESET_ALL); shutdown"

# Dump SRAM and scan it for the vault app's canaries. Pass the flashed
# apps.bin as APPS to name process slots, and checks as SCAN_ARGS, e.g.
#   make canary-scan APPS=test_apps/build/poc/apps.bin \
#       SCAN_ARGS="--expect-in due_vault:S,D,H,C"
.PHONY: canary-scan
canary-scan: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/$(PLATFORM).elf
	python3 tools/canary_scan.py sram.bin --dump --elf $< \
		--interface $(OPENOCD_INTERFACE) --transport $(OPENOCD_TRANSPORT) \
		$(if $(APPS),--apps $(APPS)) $(SCAN_ARGS)

# Reset processor and peripherals.
.PHONY: reset
reset:
	$(OPENOCD_CMD) -c "init; $(RESET_ALL); shutdown"

# Halt the core and save all 96 KB of SRAM to sram.bin.
.PHONY: dump-sram
dump-sram:
	$(OPENOCD_CMD) -c "init; halt; dump_image sram.bin $(SRAM_ADDRESS) $(SRAM_SIZE); resume; shutdown"

# Serve the SEGGER RTT console on TCP port $(RTT_PORT); connect with
#   nc localhost $(RTT_PORT)
# The kernel must have booted, so the RTT control block exists in RAM. The
# search starts above the kernel stack (the first 0x2000 bytes of RAM, see
# `stack_size!` in main.rs): a stale copy of the control block is left there
# while it is initialized, and OpenOCD would otherwise attach to that.
RTT_SEARCH_START = 0x20072000
RTT_SEARCH_SIZE = 0x16000
.PHONY: rtt
rtt:
	$(OPENOCD_CMD) -c "init; rtt setup $(RTT_SEARCH_START) $(RTT_SEARCH_SIZE) \"SEGGER RTT\"; rtt start; rtt server start $(RTT_PORT) 0"

# Start an OpenOCD GDB server. Connect with
#   arm-none-eabi-gdb $(PLATFORM).elf -ex "target extended-remote :3333"
.PHONY: debug-server
debug-server:
	$(OPENOCD_CMD) -c "init; reset halt"

# Fallback without a probe: the ROM bootloader over the programming USB port.
# Press ERASE, then RESET, before running this.
.PHONY: program
program: $(TOCK_ROOT_DIRECTORY)target/$(TARGET)/release/$(PLATFORM).bin
	$(BOSSAC) --port=$(BOSSAC_PORT) -U false -e -w -v -b $< -R
