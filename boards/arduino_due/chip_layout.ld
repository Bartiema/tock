/* Licensed under the Apache License, Version 2.0 or the MIT License. */
/* SPDX-License-Identifier: Apache-2.0 OR MIT                         */
/* Copyright Tock Contributors 2026.                                  */

/* Memory layout for the ATSAM3X8E
 *
 * Flash is 512 KB in two 256 KB banks. The kernel uses bank 0, apps use
 * bank 1.
 *
 * SRAM is 64 KB (SRAM0) plus 32 KB (SRAM1). SRAM0 is mirrored at 0x20070000,
 * directly below SRAM1 at 0x20080000, so the two banks form one contiguous
 * 96 KB region starting at 0x20070000. The kernel's RAM comes first, followed
 * by process RAM.
 */

MEMORY
{
  rom (rx)  : ORIGIN = 0x00080000, LENGTH = 256K
  prog (rx) : ORIGIN = 0x000C0000, LENGTH = 256K
  ram (rwx) : ORIGIN = 0x20070000, LENGTH = 96K
}

PAGE_SIZE = 256;
