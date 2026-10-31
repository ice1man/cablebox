//! Bench tool: talks to a Pico receiver through CableBox's own receiver code.
//!
//!     cargo run -p input --example pico_tool -- /dev/ttyACM0 [SECONDS]
//!     cargo run -p input --example pico_tool -- /dev/ttyACM0 SECONDS FIRMWARE.bin VERSION [MODEL]
//!
//! Prints what it reports for SECONDS. With a firmware file, installs it first.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use input::pico::{serve, Firmware, PicoHost, PicoInfo, UpdateStatus};
use ir_proto::{Model, Protocol};

struct Tool {
    info: Mutex<Option<PicoInfo>>,
    update: Mutex<Option<Firmware>>,
    started: Instant,
    seconds: u64,
    updating: Mutex<bool>,
}

impl Tool {
    fn stamp(&self) -> String {
        format!("{:7.2}s", self.started.elapsed().as_secs_f32())
    }
}

impl PicoHost for Tool {
    fn set_info(&self, info: Option<PicoInfo>) {
        let mut current = self.info.lock().unwrap();
        if *current != info {
            match info {
                Some(i) => println!("{} {} firmware {} compatible={} signal={:?}", self.stamp(), i.model, i.version, i.compatible, i.signal),
                None => println!("{} no receiver info", self.stamp()),
            }
        }
        *current = info;
    }

    fn info(&self) -> Option<PicoInfo> {
        // Runs often enough to end the bench run on time.
        if self.started.elapsed() > Duration::from_secs(self.seconds) && !*self.updating.lock().unwrap() {
            std::process::exit(0);
        }
        *self.info.lock().unwrap()
    }

    fn command(&self, protocol: Protocol, address: u16, command: u16) {
        println!("{} BUTTON {} address={address:#06x} command={command:#06x}", self.stamp(), protocol.name());
    }

    fn timings(&self, index: u8, total: u8, block_us: u16, values: &[u16]) {
        println!("{} UNDECODED block={block_us}us [{index}/{total}] {values:?}", self.stamp());
    }

    fn raw(&self, seq: u16, values: &[u16]) {
        println!("RAW {seq} {}", values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" "));
    }

    fn next_update(&self) -> Option<Firmware> {
        // Wait until the receiver has identified itself, as CableBox does.
        self.info.lock().unwrap().as_ref()?;
        let firmware = self.update.lock().unwrap().take();
        if firmware.is_some() {
            *self.updating.lock().unwrap() = true;
        }
        firmware
    }

    fn update_status(&self, status: UpdateStatus) {
        match status {
            UpdateStatus::Progress(p) if (p * 100.0) as u32 % 25 != 0 => {}
            UpdateStatus::Progress(p) => println!("{} update {:.0}%", self.stamp(), p * 100.0),
            done => {
                *self.updating.lock().unwrap() = false;
                println!("{} update result: {done:?}", self.stamp());
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("device path");
    if args.get(1).is_some_and(|a| a == "bootsel") {
        input::pico::enter_bootsel(Path::new(path)).expect("sending");
        return;
    }
    let seconds = args.get(1).map_or(10, |s| s.parse().expect("seconds"));
    let update = args.get(2).map(|file| Firmware {
        model: args.get(4).map_or(Model::IR1, |m| Model(m.parse().expect("model number"))),
        version: args.get(3).expect("version").parse().expect("version"),
        image: std::fs::read(file).expect("reading firmware"),
    });
    let tool = Tool { info: Mutex::default(), update: Mutex::new(update), started: Instant::now(), seconds, updating: Mutex::new(false) };
    let result = serve(Path::new(path), &tool);
    println!("{} ended: {result:?}", tool.stamp());
}
