//! Runs first on the Pico: applies a pending firmware update, or rolls back one that was
//! never confirmed, then starts the application. Interrupted swaps resume on the next boot.
//!
//! It leaves the clocks as the boot ROM set them, so the application starts from the same
//! state as if it had been flashed on its own.

#![no_std]
#![no_main]

use core::cell::RefCell;

use cortex_m_rt::{entry, exception};
use embassy_boot_rp::{BootLoader, BootLoaderConfig, WatchdogFlash};
use embassy_rp::flash::FLASH_BASE;
use embassy_rp::pac;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

const FLASH_SIZE: usize = 2 * 1024 * 1024;
/// The watchdog counts microseconds from the reference clock, in MHz. The boot ROM leaves it on
/// the ring oscillator, about this fast; only rough timing is needed.
const REF_MHZ: u32 = 6;

// DIAGNOSTIC: watchdog scratch registers survive resets. Too many starts without the application
// reaching USB drops to BOOTSEL, so the next build can be flashed without replugging.
const BOOT_COUNT_MAGIC: u32 = 0xB007_0000;
const MAX_FAILED_BOOTS: u32 = 3;
/// PICOBOOT is switched off if startup reached at least this stage (1 handoff, 2 main, 3 clocks).
const PROBE_STAGE: u32 = match option_env!("PROBE_STAGE") {
    Some(s) => (s.as_bytes()[0] - b'0') as u32,
    None => 3,
};

fn count_boot() {
    let scratch = pac::WATCHDOG.scratch0();
    let value = scratch.read();
    let count = if value & 0xFFFF_0000 == BOOT_COUNT_MAGIC { (value & 0xFFFF) + 1 } else { 1 };
    if count > MAX_FAILED_BOOTS {
        scratch.write(|w| *w = 0);
        // With the PICOBOOT interface off if the application got past its clock setup, so
        // `lsusb -v` shows how far it reached.
        let past_clocks = pac::WATCHDOG.scratch1().read() >= PROBE_STAGE;
        pac::WATCHDOG.scratch1().write(|w| *w = 0);
        embassy_rp::rom_data::reset_to_usb_boot(0, if past_clocks { 2 } else { 0 });
    }
    scratch.write(|w| *w = BOOT_COUNT_MAGIC | count);
}

#[entry]
fn main() -> ! {
    // A reset doesn't clear the SIO, so a crash inside a critical section could leave this lock
    // held. The application clears it in its own start code; this has to, before using it.
    pac::SIO.spinlock(31).write_value(1);
    count_boot();
    let p = unsafe { embassy_rp::Peripherals::steal() };
    pac::WATCHDOG.tick().write(|w| {
        w.set_cycles(REF_MHZ as u16);
        w.set_enable(true);
    });
    let flash = WatchdogFlash::<FLASH_SIZE>::start(p.FLASH, p.WATCHDOG, Duration::from_secs(8));
    let flash = Mutex::new(RefCell::new(flash));
    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active = config.active.offset();
    let loader: BootLoader = BootLoader::prepare(config);
    pac::WATCHDOG.scratch1().write(|w| *w = 1); // DIAGNOSTIC: stage
    unsafe { loader.load(FLASH_BASE as u32 + active) }
}

#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".HardFault.user"))]
unsafe extern "C" fn HardFault() {
    cortex_m::peripheral::SCB::sys_reset();
}

#[exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}
