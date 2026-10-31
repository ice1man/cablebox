//! IR1 receiver firmware: a Pico with a bare photodiode on GP26. Finds a remote's signal in
//! the ADC samples and passes its commands to CableBox over USB serial. See README.md.

#![no_std]
#![no_main]

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, Ordering};

use cortex_m::peripheral::SCB;
use embassy_boot_rp::{AlignedBuffer, BlockingFirmwareUpdater, FirmwareUpdaterConfig, State as BootState};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_futures::yield_now;
use embassy_rp::adc::{self, Adc, Channel as AdcChannel};
use embassy_rp::flash::{Blocking, Flash, WRITE_SIZE};
use embassy_rp::gpio::{Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, FLASH, USB};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, usb, Peri};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, Receiver as UsbRx, Sender as UsbTx, State};
use embassy_usb::{Builder, Config};
use ir_proto::{decode, encode, ChunkAction, DeviceMsg, FrameReader, HostMsg, Model, Protocol, Transfer, UpdateFault, Version, MAX_FRAME, PROTOCOL};
use ir_signal::{block_power, Receiver, Stats, BLOCK, SAMPLE_RATE};
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

/// This hardware. Another circuit needs its own number and its own build.
const MODEL: Model = Model::IR1;
const VERSION: Version = Version { major: number(env!("CARGO_PKG_VERSION_MAJOR")), minor: number(env!("CARGO_PKG_VERSION_MINOR")), patch: number(env!("CARGO_PKG_VERSION_PATCH")) };

const FLASH_SIZE: usize = 2 * 1024 * 1024;
/// Size of the active partition; a bigger image couldn't run.
const MAX_IMAGE: u32 = 128 * 1024;
/// New firmware has this long to be confirmed, or it's rolled back. CableBox confirms well
/// within this, once it has seen the firmware working.
const TRIAL: Duration = Duration::from_secs(60);
const WATCHDOG: Duration = Duration::from_secs(8);
/// The ADC runs from 48 MHz; this divider gives [`SAMPLE_RATE`].
const ADC_DIV: u16 = (48_000_000 / SAMPLE_RATE - 1) as u16;
const STATS_EVERY: Duration = Duration::from_millis(250);
/// Blocks the LED stays lit after a command: about a tenth of a second.
const LED_BLOCKS: u32 = 1200;

const fn number(s: &str) -> u16 {
    let (bytes, mut value, mut i) = (s.as_bytes(), 0, 0);
    while i < bytes.len() {
        value = value * 10 + (bytes[i] - b'0') as u16;
        i += 1;
    }
    value
}

bind_interrupts!(struct Irqs {
    ADC_IRQ_FIFO => adc::InterruptHandler;
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

/// Commands waiting to go out. If CableBox isn't reading, older presses aren't worth keeping.
static OUTBOX: Channel<CriticalSectionRawMutex, DeviceMsg, 24> = Channel::new();
/// Strongest carrier since CableBox last asked, and the latest background level.
static SIGNAL: embassy_sync::blocking_mutex::CriticalSectionMutex<Cell<(u16, u16)>> = embassy_sync::blocking_mutex::CriticalSectionMutex::new(Cell::new((0, 0)));
static TRIAL_RUNNING: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
async fn run_usb(mut device: embassy_usb::UsbDevice<'static, usb::Driver<'static, USB>>) {
    device.run().await
}

fn protocol(p: ir_signal::Protocol) -> Protocol {
    match p {
        ir_signal::Protocol::Nec => Protocol::Nec,
        ir_signal::Protocol::Rc5 => Protocol::Rc5,
        ir_signal::Protocol::Rc6 => Protocol::Rc6,
        ir_signal::Protocol::Sony => Protocol::Sony,
        ir_signal::Protocol::Samsung => Protocol::Samsung,
        ir_signal::Protocol::Pulses => Protocol::Pulses,
    }
}

/// Samples the diode for ever. Also the watchdog's keeper: if this ever stops, the board
/// resets, and unconfirmed new firmware rolls back.
#[embassy_executor::task]
async fn sample(mut adc: Adc<'static, adc::Async>, mut diode: AdcChannel<'static>, mut dma: dma::Channel<'static>, mut led: Output<'static>, mut watchdog: Watchdog) {
    let mut samples = [0u16; BLOCK];
    let mut receiver = Receiver::default();
    let mut stats = Stats::default();
    let mut stats_at = Instant::now() + STATS_EVERY;
    let mut led_left = 0u32;
    let mut last = Instant::now();
    let (mut block_sum, mut block_count) = (0u32, 0u32);
    loop {
        watchdog.feed(WATCHDOG);
        if adc.read_many(&mut diode, &mut samples, ADC_DIV, &mut dma).await.is_err() {
            continue;
        }
        let now = Instant::now();
        // Measured, since the moments between blocks are part of the signal's timing too.
        let us = now.duration_since(last).as_micros() as u32;
        last = now;
        let power = block_power(&samples);
        stats.add(power);
        if let Some(rx) = receiver.block(power, us) {
            let c = rx.command;
            let _ = OUTBOX.try_send(DeviceMsg::Ir { protocol: protocol(c.protocol), address: c.address, command: c.command, repeat: rx.repeat });
            led.set_high();
            led_left = LED_BLOCKS;
        }
        // DIAGNOSTIC: report frames that didn't decode, and how long blocks really take.
        block_sum += us;
        block_count += 1;
        if let Some(timings) = receiver.take_undecoded() {
            let block_us = (block_sum / block_count.max(1)) as u16;
            let total = timings.len() as u8;
            for (i, part) in timings.chunks(12).enumerate() {
                let mut values = [0u16; 12];
                for (slot, t) in values.iter_mut().zip(part) {
                    *slot = (*t).min(65535) as u16;
                }
                let _ = OUTBOX.try_send(DeviceMsg::Timings { index: (i * 12) as u8, total, block_us, values });
            }
            block_sum = 0;
            block_count = 0;
        }
        if led_left > 0 {
            led_left -= 1;
            if led_left == 0 {
                led.set_low();
            }
        }
        if now >= stats_at {
            stats_at = now + STATS_EVERY;
            SIGNAL.lock(|s| {
                let (peak, _) = s.get();
                s.set((peak.max(stats.peak().min(65535) as u16), stats.floor().min(65535) as u16));
            });
            stats = Stats::default();
        }
        // Don't let a long run of work here starve USB.
        yield_now().await;
    }
}

/// Gives unconfirmed firmware a limited time.
#[embassy_executor::task]
async fn trial_timer() {
    Timer::after(TRIAL).await;
    if TRIAL_RUNNING.load(Ordering::Relaxed) {
        SCB::sys_reset();
    }
}

async fn send(tx: &mut UsbTx<'static, usb::Driver<'static, USB>>, msg: &DeviceMsg) {
    let mut out = [0; MAX_FRAME];
    let Some(frame) = encode(msg, &mut out) else { return };
    // Every device message is shorter than a USB packet. Nobody reading must not stall us.
    let _ = with_timeout(Duration::from_millis(200), tx.write_packet(frame)).await;
}

/// Serves CableBox over USB.
#[embassy_executor::task]
async fn link(mut tx: UsbTx<'static, usb::Driver<'static, USB>>, mut rx: UsbRx<'static, usb::Driver<'static, USB>>, flash: Peri<'static, FLASH>) {
    let flash: Flash<'_, FLASH, Blocking, FLASH_SIZE> = Flash::new_blocking(flash);
    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let mut updater = BlockingFirmwareUpdater::new(FirmwareUpdaterConfig::from_linkerfile_blocking(&flash, &flash), &mut aligned.0);

    let mut trial = false;
    match updater.get_state() {
        Ok(BootState::Swap) => {
            trial = true;
            TRIAL_RUNNING.store(true, Ordering::Relaxed);
        }
        // Coming back from a rollback counts as booted, or further updates would be refused.
        Ok(BootState::Revert) => {
            let _ = updater.mark_booted();
        }
        _ => {}
    }

    let mut reader = FrameReader::default();
    let mut transfer: Option<Transfer> = None;
    let mut buf = [0u8; 64];
    loop {
        rx.wait_connection().await;
        embassy_rp::pac::WATCHDOG.scratch0().write(|w| *w = 0); // DIAGNOSTIC: USB works, stop counting boots
        loop {
            let n = match select(rx.read_packet(&mut buf), OUTBOX.receive()).await {
                Either::First(Ok(n)) => n,
                Either::First(Err(_)) => break,
                Either::Second(msg) => {
                    if tx.dtr() {
                        send(&mut tx, &msg).await;
                    }
                    continue;
                }
            };
            for &byte in &buf[..n] {
                let Some(msg) = reader.push(byte).and_then(decode::<HostMsg>) else { continue };
                let reply = match msg {
                    HostMsg::GetInfo => DeviceMsg::Hello { protocol: PROTOCOL, model: MODEL, version: VERSION, trial },
                    HostMsg::GetSignal => {
                        let (peak, noise) = SIGNAL.lock(|s| {
                            let v = s.get();
                            s.set((0, v.1));
                            v
                        });
                        DeviceMsg::Signal { peak, noise }
                    }
                    HostMsg::UpdateBegin { model, size, sha256 } => match Transfer::begin(MODEL, model, size, sha256, MAX_IMAGE) {
                        Ok(t) => {
                            transfer = Some(t);
                            DeviceMsg::UpdateAck { offset: 0 }
                        }
                        Err(fault) => DeviceMsg::UpdateError(fault),
                    },
                    HostMsg::UpdateChunk { offset, data } => match transfer.as_mut() {
                        None => DeviceMsg::UpdateError(UpdateFault::BadImage),
                        Some(t) => match t.chunk(offset, data.len() as u32) {
                            Ok(ChunkAction::AckAgain) => DeviceMsg::UpdateAck { offset: t.received },
                            Ok(ChunkAction::Store) => match updater.write_firmware(offset as usize, data) {
                                Ok(()) => {
                                    t.received += data.len() as u32;
                                    DeviceMsg::UpdateAck { offset: t.received }
                                }
                                Err(_) => DeviceMsg::UpdateError(UpdateFault::Flash),
                            },
                            Err(fault) => DeviceMsg::UpdateError(fault),
                        },
                    },
                    HostMsg::UpdateCommit => match transfer.filter(|t| t.complete()) {
                        None => DeviceMsg::UpdateError(UpdateFault::BadImage),
                        Some(t) => {
                            // What's checked is what's in flash, not what arrived.
                            let mut hash = Sha256::new();
                            let (mut at, mut page, mut good) = (0, [0u8; 256], true);
                            while at < t.size {
                                let len = (t.size - at).min(256) as usize;
                                good &= updater.read_dfu(at, &mut page[..len]).is_ok();
                                hash.update(&page[..len]);
                                at += len as u32;
                                yield_now().await; // keeps sampling, so the watchdog is fed
                            }
                            let digest: [u8; 32] = hash.finalize().into();
                            if !good || digest != t.sha256 {
                                DeviceMsg::UpdateError(UpdateFault::BadHash)
                            } else if updater.mark_updated().is_err() {
                                DeviceMsg::UpdateError(UpdateFault::Flash)
                            } else {
                                send(&mut tx, &DeviceMsg::UpdateAck { offset: t.size }).await;
                                Timer::after_millis(100).await;
                                SCB::sys_reset();
                            }
                        }
                    },
                    HostMsg::EnterBootsel => {
                        Timer::after_millis(50).await;
                        embassy_rp::rom_data::reset_to_usb_boot(0, 0);
                        continue;
                    }
                    HostMsg::Confirm => {
                        if updater.mark_booted().is_ok() {
                            trial = false;
                            TRIAL_RUNNING.store(false, Ordering::Relaxed);
                        }
                        DeviceMsg::Hello { protocol: PROTOCOL, model: MODEL, version: VERSION, trial }
                    }
                };
                send(&mut tx, &reply).await;
            }
        }
    }
}

// DIAGNOSTIC: a crash is written here, in RAM clear of everything else, and survives the reset.
const CRASH: *mut u32 = 0x2002_0000 as *mut u32;
const CRASH_MAGIC: u32 = 0xC0DE_DEAD;
const CRASH_MAX: usize = 400;

struct CrashWriter(usize);

impl core::fmt::Write for CrashWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if self.0 < CRASH_MAX {
                unsafe { (CRASH.add(2) as *mut u8).add(self.0).write_volatile(b) };
                self.0 += 1;
            }
        }
        unsafe { CRASH.add(1).write_volatile(self.0 as u32) };
        Ok(())
    }
}

fn record_crash(args: core::fmt::Arguments) -> ! {
    use core::fmt::Write;
    let _ = CrashWriter(0).write_fmt(args);
    unsafe { CRASH.write_volatile(CRASH_MAGIC) };
    SCB::sys_reset();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    record_crash(format_args!("PANIC {info}"));
}

#[cortex_m_rt::exception]
unsafe fn HardFault(ef: &cortex_m_rt::ExceptionFrame) -> ! {
    record_crash(format_args!("HARDFAULT pc={:#x} lr={:#x}", ef.pc(), ef.lr()));
}

/// Takes the crash left by the last run, if any.
fn take_crash(out: &mut [u8; CRASH_MAX]) -> Option<usize> {
    unsafe {
        if CRASH.read_volatile() != CRASH_MAGIC {
            return None;
        }
        CRASH.write_volatile(0);
        let len = (CRASH.add(1).read_volatile() as usize).min(CRASH_MAX);
        for (i, slot) in out[..len].iter_mut().enumerate() {
            *slot = (CRASH.add(2) as *const u8).add(i).read_volatile();
        }
        Some(len)
    }
}

/// Only USB and the crash report, so whatever crashed can't crash again.
async fn report_crash(mut tx: UsbTx<'static, usb::Driver<'static, USB>>, text: &[u8], mut watchdog: Watchdog) -> ! {
    loop {
        watchdog.feed(WATCHDOG);
        if tx.dtr() {
            embassy_rp::pac::WATCHDOG.scratch0().write(|w| *w = 0);
            for chunk in text.chunks(32) {
                let _ = with_timeout(Duration::from_millis(200), tx.write_packet(chunk)).await;
            }
            let _ = with_timeout(Duration::from_millis(200), tx.write_packet(b"\r\n")).await;
        }
        Timer::after_millis(500).await;
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    embassy_rp::pac::WATCHDOG.scratch1().write(|w| *w = 2); // DIAGNOSTIC: stage
    static EXECUTOR: StaticCell<embassy_executor::Executor> = StaticCell::new();
    EXECUTOR.init(embassy_executor::Executor::new()).run(|spawner| spawner.spawn(start(spawner).unwrap()))
}

#[embassy_executor::task]
async fn start(spawner: Spawner) {
    embassy_rp::pac::WATCHDOG.scratch1().write(|w| *w = 3); // DIAGNOSTIC: stage
    let mut crash = [0u8; CRASH_MAX];
    let crashed = take_crash(&mut crash);
    let p = embassy_rp::init(Default::default());

    // DIAGNOSTIC: tells the bootloader, and the LED tells you, that clock setup finished.
    embassy_rp::pac::WATCHDOG.scratch1().write(|w| *w = 4);
    let led = Output::new(p.PIN_25, Level::High);

    // The bootloader left the watchdog running; a hang from here on resets the board.
    let mut watchdog = Watchdog::new(p.WATCHDOG);
    watchdog.start(WATCHDOG);

    let driver = usb::Driver::new(p.USB, Irqs);
    let mut config = Config::new(0x1209, 0x0001); // pid.codes test ID, until a real one is assigned
    config.manufacturer = Some("CableBox");
    config.product = Some("IR receiver IR1");

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 64]> = StaticCell::new();
    static CONTROL: StaticCell<[u8; 64]> = StaticCell::new();
    static CDC: StaticCell<State> = StaticCell::new();
    let mut builder = Builder::new(driver, config, CONFIG_DESC.init([0; 256]), BOS_DESC.init([0; 64]), &mut [], CONTROL.init([0; 64]));
    let serial = CdcAcmClass::new(&mut builder, CDC.init(State::new()), 64);
    let (tx, rx) = serial.split();

    spawner.spawn(run_usb(builder.build()).unwrap());
    if let Some(len) = crashed {
        report_crash(tx, &crash[..len], watchdog).await;
    }
    spawner.spawn(link(tx, rx, p.FLASH).unwrap());
    spawner.spawn(trial_timer().unwrap());

    let adc = Adc::new(p.ADC, Irqs, adc::Config::default());
    let diode = AdcChannel::new_pin(p.PIN_26, Pull::None);
    let dma = dma::Channel::new(p.DMA_CH0, Irqs);
    spawner.spawn(sample(adc, diode, dma, led, watchdog).unwrap());
}
