//! ESP32-S3, octal PSRAM, flash with Quad Enable clear: flash reads return
//! `0xff` after the PSRAM is used. See README.md.

#![no_std]
#![no_main]

use core::ptr;

use esp_backtrace as _;
use esp_hal::{
    flash::{Config, Flash},
    main,
    psram::Psram,
    ram,
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

/// Read through the flash mapping. Larger than the data cache, so reading it
/// in order goes to the flash.
static TABLE: [u8; TABLE_LEN] = table();
const TABLE_LEN: usize = 124 * 1024;

const PSRAM_WORDS: usize = 256 * 1024 / 4;

/// IO_MUX registers of the pads the flash reads as HOLD# and WP#, and the
/// octal PSRAM uses as DQ3 and DQ2.
const IO_MUX_GPIO27: *mut u32 = 0x6000_9070 as *mut u32;
const IO_MUX_GPIO28: *mut u32 = 0x6000_9074 as *mut u32;
/// SPI0 is the controller the cache reads the flash through: its flash clock
/// divider, and the clock that is divided.
const SPI0_CLOCK: *const u32 = 0x6000_3014 as *const u32;
const SPI0_CORE_CLK_SEL: *const u32 = 0x6000_30ec as *const u32;

const READ_STATUS_1: u8 = 0x05;
const READ_STATUS_2: u8 = 0x35;

unsafe extern "C" {
    fn esp_rom_spiflash_read_user_cmd(status: *mut u32, cmd: u8) -> i32;
    fn Cache_Suspend_ICache() -> u32;
    fn Cache_Resume_ICache(autoload: u32);
    fn Cache_Suspend_DCache() -> u32;
    fn Cache_Resume_DCache(autoload: u32);
}

/// A hash of the index. A table that compresses well can time out a debug
/// probe that sends the image compressed.
const fn expected(i: usize) -> u8 {
    let mut x = (i as u32).wrapping_mul(0x9e37_79b1);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    (x >> 24) as u8
}

const fn table() -> [u8; TABLE_LEN] {
    let mut table = [0; TABLE_LEN];
    let mut i = 0;
    while i < TABLE_LEN {
        table[i] = expected(i);
        i += 1;
    }
    table
}

/// Bytes of the table's first `len` that read wrong, and how many of those
/// read `0xff`.
#[ram]
fn check_table(table: *const u8, len: usize) -> (usize, usize) {
    let mut wrong = 0;
    let mut ones = 0;
    let mut i = 0;
    while i < len {
        // SAFETY: inside `TABLE`.
        let byte = unsafe { table.add(i).read_volatile() };
        if byte != expected(i) {
            wrong += 1;
            if byte == 0xff {
                ones += 1;
            }
        }
        i += 1;
    }
    (wrong, ones)
}

struct Outcome {
    psram_wrong: usize,
    flash_wrong: usize,
    flash_ones: usize,
}

/// Write and read back the PSRAM, read the flash, then give the two pads back
/// to the flash. Runs from RAM with interrupts off, so nothing is fetched
/// from the flash while it may misread.
#[ram]
fn use_psram_then_read_flash(psram: *mut u32, table: *const u8, pads: [u32; 2]) -> Outcome {
    let mut i = 0;
    while i < PSRAM_WORDS {
        // SAFETY: inside the mapped PSRAM.
        unsafe { psram.add(i).write_volatile(!(i as u32)) };
        i += 1;
    }
    let mut psram_wrong = 0;
    let mut i = 0;
    while i < PSRAM_WORDS {
        // SAFETY: as above.
        if unsafe { psram.add(i).read_volatile() } != !(i as u32) {
            psram_wrong += 1;
        }
        i += 1;
    }

    let (flash_wrong, flash_ones) = check_table(table, TABLE_LEN);

    // So that the code that prints the result can be fetched. The PSRAM is
    // unusable from here on.
    // SAFETY: the values these registers held before the PSRAM was set up.
    unsafe {
        IO_MUX_GPIO27.write_volatile(pads[0]);
        IO_MUX_GPIO28.write_volatile(pads[1]);
    }

    Outcome {
        psram_wrong,
        flash_wrong,
        flash_ones,
    }
}

/// Read one of the flash's status registers.
#[ram]
fn flash_status(cmd: u8) -> u8 {
    critical_section::with(|_| {
        let mut status = 0;
        // SAFETY: a ROM one-byte read over SPI1, with both caches suspended
        // and resumed with the values the suspension returned.
        unsafe {
            let icache = Cache_Suspend_ICache();
            let dcache = Cache_Suspend_DCache();
            esp_rom_spiflash_read_user_cmd(&mut status, cmd);
            Cache_Resume_DCache(dcache);
            Cache_Resume_ICache(icache);
        }
        status as u8
    })
}

/// The flash clock SPI0 is set to, in MHz.
fn flash_mhz() -> u32 {
    // SAFETY: readable registers.
    let (clock, select) = unsafe {
        (
            SPI0_CLOCK.read_volatile(),
            SPI0_CORE_CLK_SEL.read_volatile(),
        )
    };
    let core = [80, 120, 160, 240][(select & 3) as usize];
    // Bit 31 is "no division"; bits 16 to 23 hold the divider minus one.
    if clock >> 31 == 1 {
        core
    } else {
        core / (((clock >> 16) & 0xff) + 1)
    }
}

fn print_state(when: &str) {
    // SAFETY: readable registers.
    let (gpio27, gpio28) =
        unsafe { (IO_MUX_GPIO27.read_volatile(), IO_MUX_GPIO28.read_volatile()) };
    println!(
        "{}: flash status 1 {:#04x}, status 2 {:#04x}; IO_MUX GPIO27 {:#06x}, GPIO28 {:#06x}; flash clock {} MHz",
        when,
        flash_status(READ_STATUS_1),
        flash_status(READ_STATUS_2),
        gpio27,
        gpio28,
        flash_mhz(),
    );
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let table = ptr::addr_of!(TABLE).cast::<u8>();

    let mut flash = Flash::new(peripherals.FLASH, Config::default()).unwrap();
    println!("flash JEDEC ID {:#08x}", flash.chip_info().chip_id);
    print_state("at start");
    let (wrong, _) = check_table(table, TABLE_LEN);
    println!("flash read, {} bytes: {} wrong", TABLE_LEN, wrong);

    // All ones, which changes no bit of a NOR flash. The driver unlocks the
    // flash before its first write.
    // SAFETY: programs nothing.
    unsafe { flash.write(0x9000, &[u32::MAX]) }.unwrap();
    print_state("after a flash write");

    // SAFETY: readable registers.
    let pads = unsafe { [IO_MUX_GPIO27.read_volatile(), IO_MUX_GPIO28.read_volatile()] };
    let psram = Psram::new(peripherals.PSRAM, Default::default());
    let (start, size) = psram.raw_parts();
    println!("PSRAM: {} bytes at {:p}", size, start);
    print_state("after PSRAM init");
    assert!(size >= PSRAM_WORDS * 4, "this needs a board with PSRAM");

    let outcome = critical_section::with(|_| use_psram_then_read_flash(start.cast(), table, pads));
    println!(
        "PSRAM written and read back, {} words: {} wrong",
        PSRAM_WORDS, outcome.psram_wrong
    );
    println!(
        "flash read after that, {} bytes: {} wrong, {} of them 0xff",
        TABLE_LEN, outcome.flash_wrong, outcome.flash_ones
    );

    loop {}
}
