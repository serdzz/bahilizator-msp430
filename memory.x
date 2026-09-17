/* MSP430F2618: 116 kB flash and 8 kB SRAM on the device.
 *
 * Only the flash below 0x10000 is here. The rest needs the 20-bit addressing of the MSP430X, which
 * Rust's msp430 target does not have, so about 50 kB is the budget. The original C firmware used
 * the upper region for constants; that is not an option from Rust.
 *
 * NVRAM is five 512-byte flash segments carved out of the bottom of the code region: one for
 * settings, three for the counter journal, one for the event log (added for PORT_AUDIT.md §5 —
 * the port had no persisted event/transaction history at all). They are a region of their own so
 * that the linker cannot place code in them: a segment being erased holds the CPU, and erasing the
 * code that is running would be the last thing the machine did.
 */
MEMORY
{
  RAM     : ORIGIN = 0x1100, LENGTH = 0x2000  /* 0x1100 ..= 0x30FF */
  NVRAM   : ORIGIN = 0x3100, LENGTH = 0x0A00  /* 0x3100 ..= 0x3AFF, five segments */
  ROM     : ORIGIN = 0x3B00, LENGTH = 0xC4C0  /* 0x3B00 ..= 0xFFBF */
  VECTORS : ORIGIN = 0xFFC0, LENGTH = 0x0040  /* 31 interrupt vectors and the reset vector */
}
