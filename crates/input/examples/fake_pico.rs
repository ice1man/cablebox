//! A simulated Pico receiver on a pseudo-terminal, for working on CableBox's Settings pages with
//! no hardware. Run CableBox in a window with CABLEBOX_PRETEND_BOX=1 and
//! CABLEBOX_PICO_DEVICE=LINK.
//!
//!     cargo run -p input --example fake_pico -- LINK [MODEL] [VERSION] [UPDATED_VERSION]
//!
//! Commands on stdin: `ir nec 4 18` (a button), `fail hash`, `fail vanish`, `fail none`, `quit`.

use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ir_proto::{decode, encode, DeviceMsg, FrameReader, HostMsg, Model, Protocol, Transfer, UpdateFault, Version, MAX_FRAME, PROTOCOL};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcgetattr, tcsetattr, OptionalActions};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq)]
enum Fail {
    None,
    Hash,
    Vanish,
}

struct State {
    model: Model,
    version: Version,
    updated: Version,
    trial: bool,
    fail: Fail,
    transfer: Option<Transfer>,
    received: Vec<u8>,
}

fn send(out: &Mutex<File>, msg: &DeviceMsg) {
    let mut buf = [0; MAX_FRAME];
    if let Some(frame) = encode(msg, &mut buf) {
        let _ = out.lock().unwrap().write_all(frame);
    }
}

fn hello(s: &State) -> DeviceMsg {
    DeviceMsg::Hello { protocol: PROTOCOL, model: s.model, version: s.version, trial: s.trial }
}

fn handle(msg: HostMsg, state: &Mutex<State>, out: &Mutex<File>, link: &Path) {
    let mut s = state.lock().unwrap();
    let reply = match msg {
        HostMsg::GetInfo => hello(&s),
        HostMsg::GetSignal => DeviceMsg::Signal { peak: 42, noise: 3 },
        HostMsg::UpdateBegin { model, size, sha256 } => match Transfer::begin(s.model, model, size, sha256, 128 * 1024) {
            Ok(t) => {
                s.transfer = Some(t);
                s.received.clear();
                DeviceMsg::UpdateAck { offset: 0 }
            }
            Err(fault) => DeviceMsg::UpdateError(fault),
        },
        HostMsg::UpdateChunk { offset, data } => {
            // Slow enough for the progress bar to be seen.
            thread::sleep(Duration::from_millis(5));
            if s.fail == Fail::Vanish && offset > 2048 {
                println!("vanishing");
                let _ = std::fs::remove_file(link);
                return;
            }
            let Some(mut t) = s.transfer else { return send(out, &DeviceMsg::UpdateError(UpdateFault::BadImage)) };
            match t.chunk(offset, data.len() as u32) {
                Ok(ir_proto::ChunkAction::Store) => {
                    s.received.extend_from_slice(data);
                    t.received += data.len() as u32;
                    s.transfer = Some(t);
                    DeviceMsg::UpdateAck { offset: t.received }
                }
                Ok(ir_proto::ChunkAction::AckAgain) => DeviceMsg::UpdateAck { offset: t.received },
                Err(fault) => DeviceMsg::UpdateError(fault),
            }
        }
        HostMsg::UpdateCommit => {
            let Some(t) = s.transfer.filter(|t| t.complete()) else { return send(out, &DeviceMsg::UpdateError(UpdateFault::BadImage)) };
            let digest: [u8; 32] = Sha256::digest(&s.received).into();
            if digest != t.sha256 || s.fail == Fail::Hash {
                DeviceMsg::UpdateError(UpdateFault::BadHash)
            } else {
                send(out, &DeviceMsg::UpdateAck { offset: t.size });
                s.version = s.updated;
                s.trial = true;
                println!("rebooting into {}", s.version);
                drop(s);
                thread::sleep(Duration::from_secs(1));
                return;
            }
        }
        HostMsg::Confirm => {
            s.trial = false;
            println!("confirmed {}", s.version);
            hello(&s)
        }
        HostMsg::EnterBootsel => return,
    };
    drop(s);
    send(out, &reply);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let link = PathBuf::from(args.first().expect("link path"));
    let model = Model(args.get(1).map_or(1, |m| m.parse().expect("model")));
    let version: Version = args.get(2).map_or("0.1.0", |v| v).parse().expect("version");
    let updated: Version = args.get(3).map_or("0.2.0", |v| v).parse().expect("updated version");

    let master: OwnedFd = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let pts = PathBuf::from(name.to_str().unwrap());
    // Held open so CableBox reconnecting after a "reboot" doesn't hang up the terminal.
    let slave = File::options().read(true).write(true).open(&pts).unwrap();
    let mut tio = tcgetattr(slave.as_fd()).unwrap();
    tio.make_raw();
    tcsetattr(slave.as_fd(), OptionalActions::Now, &tio).unwrap();

    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&pts, &link).unwrap();
    println!("receiver {model} firmware {version} at {}", link.display());

    let master = File::from(master);
    let out = Arc::new(Mutex::new(master.try_clone().unwrap()));
    let state = Arc::new(Mutex::new(State { model, version, updated, trial: false, fail: Fail::None, transfer: None, received: Vec::new() }));

    {
        let (state, out, link) = (state.clone(), out.clone(), link.clone());
        thread::spawn(move || {
            let mut master = master;
            let mut reader = FrameReader::default();
            let mut buf = [0; 512];
            loop {
                let Ok(n) = master.read(&mut buf) else {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                };
                for byte in &buf[..n] {
                    if let Some(msg) = reader.push(*byte).and_then(decode::<HostMsg>) {
                        handle(msg, &state, &out, &link);
                    }
                }
            }
        });
    }

    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["ir", protocol, address, command] => {
                let msg = DeviceMsg::Ir {
                    protocol: Protocol::from_name(protocol).expect("protocol"),
                    address: address.parse().expect("address"),
                    command: command.parse().expect("command"),
                    repeat: false,
                };
                send(&out, &msg);
            }
            ["fail", "hash"] => state.lock().unwrap().fail = Fail::Hash,
            ["fail", "vanish"] => state.lock().unwrap().fail = Fail::Vanish,
            ["fail", "none"] => state.lock().unwrap().fail = Fail::None,
            ["link"] => {
                let _ = std::os::unix::fs::symlink(&pts, &link);
            }
            ["quit"] => break,
            other => println!("unknown command: {other:?}"),
        }
    }
    let _ = std::fs::remove_file(&link);
}
