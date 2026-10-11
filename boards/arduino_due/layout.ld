/* Licensed under the Apache License, Version 2.0 or the MIT License. */
/* SPDX-License-Identifier: Apache-2.0 OR MIT                         */
/* Copyright Tock Contributors 2026.                                  */

INCLUDE ./chip_layout.ld
INCLUDE tock_kernel_layout.ld

/* Kernel buffers that may hold copies of process data (console, UART mux,
 * RTT and debug writer buffers), collected so lockdown can wipe them as one
 * range. Not initialized at boot: buffers are written before use. Placed
 * between the kernel's data and BSS, below application memory.
 */
SECTIONS
{
    .kernel_secrets (NOLOAD) :
    {
        . = ALIGN(4);
        _skernel_secrets = .;
        KEEP(*(.kernel_secrets .kernel_secrets.*))
        . = ALIGN(4);
        _ekernel_secrets = .;
    } > ram
}
INSERT AFTER .relocate;
