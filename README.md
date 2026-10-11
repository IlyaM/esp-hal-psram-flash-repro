# esp-hal: flash reads return 0xff after octal PSRAM use when the flash's Quad Enable bit is clear (ESP32-S3)

## What the program does

1. Prints the flash's JEDEC ID and status registers, the IO_MUX registers of
   GPIO27 and GPIO28, and the flash clock.
2. Writes one word of all ones through `esp_hal::flash`. That changes no bit
   of the flash, but the driver unlocks the flash first. Prints the same
   again.
3. Sets up the PSRAM with `Psram::new(peripherals.PSRAM, Default::default())`.
   Prints the same again.
4. From RAM, with interrupts off: writes 256 KiB of PSRAM and reads it back,
   then reads a 124 KiB table in flash and counts the bytes that are wrong.
5. Puts the IO_MUX registers of GPIO27 and GPIO28 back to what they were
   before step 3, so that it can print, and prints the counts.

It uses esp-hal `main` at `bdcc3b087` (2026-10-09), and no wiring.

## Running

Needs the `esp` toolchain (espup), espflash, and an ESP32-S3 board with octal
PSRAM (an ESP32-S3R8, or a module ending in `R8`).

```sh
cargo run --release
```

The fault needs a flash whose Quad Enable bit is clear:

- Flash with espflash's default flash mode (DIO). In QIO mode the bootloader
  sets the bit.
- The bit is non-volatile. On flash that keeps it in status register 1 bit 6
  (Macronix, ISSI), step 2 clears it, and it stays clear until a status
  register write sets it again, which a bootloader in QIO mode does.
  Elsewhere it is status register 2 bit 1, which step 2 leaves alone.
- The first line of output shows both registers.

## Expected

Both counts are 0, and the flash write leaves the status registers as they
were.

## What was measured

ESP32-S3R8 with Macronix flash (`c2 20 18`), three boots of each build, with
a build that differs from this program in two ways: the figures went to
memory and were read with a debug probe instead of being printed, and step 4
ran 8 times per boot.

| Build | Flash status 1 | Flash read after PSRAM use |
| --- | --- | --- |
| without step 2 (Quad Enable stays set) | `0x40` | 0 wrong, 24 rounds of 24 |
| this program | `0x40`, `0x00` after step 2 and on later boots | 7 wrong, 24 rounds of 24 |

- The wrong bytes are always the first 8 of the table, read as `0xff` (7
  counted: one is `0xff` in the table).
- The PSRAM read back right in every round.
- GPIO27 and GPIO28 read `0x1700` (GPIO, pulled up) before step 3 and
  `0x0f00` (SPIHD and SPIWP) after, in both builds.
- The flash clock read 80 MHz before and after: the probe's flasher wrote an
  80 MHz image header.

Not measured: this exact program started from power-on and read over serial.

## Why

- The octal PSRAM's DQ3 and DQ2 are on GPIO27 and GPIO28, which are also the
  flash's HOLD# and WP#. PSRAM init gives both pads to the memory controller
  ([esp32s3.rs](https://github.com/esp-rs/esp-hal/blob/bdcc3b0872b8affbafa8f3bab79f398d07a49b5a/esp-hal/src/psram/esp32s3.rs#L813-L816)),
  and a flash with Quad Enable clear still obeys them.
- The flash driver unlocks the flash with the ROM's `esp_rom_spiflash_unlock`
  ([flash/mod.rs](https://github.com/esp-rs/esp-hal/blob/bdcc3b0872b8affbafa8f3bab79f398d07a49b5a/esp-hal/src/flash/mod.rs#L792-L801)),
  which clears Quad Enable where it is status register 1 bit 6.
