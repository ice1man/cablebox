//! The Pico IR receiver: reads decoded remote commands from it over USB serial, and installs
//! new firmware on it. The link and update logic take any `Read + Write`, so they can be
//! tested against a simulated receiver.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use ir_proto::{decode, encode, DeviceMsg, FrameReader, HostMsg, Model, UpdateFault, Version, CHUNK, MAX_FRAME, PROTOCOL};
use sha2::{Digest, Sha256};

/// To be created by a udev rule for our receiver's USB ID, once it has a real one.
pub const DEVICE: &str = "/dev/cablebox-ir";

const POLL: Duration = Duration::from_secs(1);

/// What's known about the connected receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicoInfo {
    pub model: Model,
    pub version: Version,
    /// It speaks the messages this CableBox does; if not, only updating it is possible.
    pub compatible: bool,
    /// Strongest carrier and background level, once reported.
    pub signal: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateError {
    Timeout,
    /// Unplugged, or otherwise unreachable, partway through.
    Gone,
    Device(UpdateFault),
    /// It came back running something else, so the update didn't take.
    WrongVersion(Version),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "The receiver stopped responding."),
            Self::Gone => write!(f, "The receiver was disconnected."),
            Self::Device(UpdateFault::BadImage) => write!(f, "The receiver rejected the firmware file."),
            Self::Device(UpdateFault::BadHash) => write!(f, "The firmware was damaged on the way to the receiver."),
            Self::Device(UpdateFault::Flash) => write!(f, "The receiver couldn't write to its memory."),
            Self::Device(UpdateFault::WrongModel) => write!(f, "That firmware is for a different model of receiver."),
            Self::WrongVersion(v) => write!(f, "The receiver came back running {v}."),
        }
    }
}

/// How an update is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpdateStatus {
    /// Fraction of the image sent.
    Progress(f32),
    Done(Version),
    Failed(UpdateError),
}

/// A firmware image to install.
pub struct Firmware {
    pub model: Model,
    pub version: Version,
    pub image: Vec<u8>,
}

/// Framing on top of a byte stream. A read returning 0 means nothing arrived yet.
struct Link<T> {
    port: T,
    reader: FrameReader,
    inbox: VecDeque<DeviceMsg>,
}

impl<T: Read + Write> Link<T> {
    fn new(port: T) -> Self {
        Self { port, reader: FrameReader::default(), inbox: VecDeque::new() }
    }

    fn send(&mut self, msg: &HostMsg) -> io::Result<()> {
        let mut out = [0; MAX_FRAME];
        let frame = encode(msg, &mut out).ok_or(io::ErrorKind::InvalidInput)?;
        self.port.write_all(frame)?;
        self.port.flush()
    }

    /// The next message, or `None` if nothing arrived for one read's wait.
    fn recv(&mut self) -> io::Result<Option<DeviceMsg>> {
        if let Some(msg) = self.inbox.pop_front() {
            return Ok(Some(msg));
        }
        let mut buf = [0; 64];
        let n = self.port.read(&mut buf)?;
        for byte in &buf[..n] {
            if let Some(msg) = self.reader.push(*byte).and_then(decode) {
                self.inbox.push_back(msg);
            }
        }
        Ok(self.inbox.pop_front())
    }

    /// Waits for a message `want` accepts, passing over remote presses and the like.
    fn expect<R>(&mut self, wait: Duration, want: impl Fn(&DeviceMsg) -> Option<Result<R, UpdateError>>) -> Result<R, UpdateError> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            let msg = self.recv().map_err(|_| UpdateError::Gone)?;
            if let Some(result) = msg.as_ref().and_then(&want) {
                return result;
            }
        }
        Err(UpdateError::Timeout)
    }

    fn expect_ack(&mut self, wait: Duration, offset: u32) -> Result<(), UpdateError> {
        self.expect(wait, |msg| match *msg {
            DeviceMsg::UpdateAck { offset: o } if o == offset => Some(Ok(())),
            DeviceMsg::UpdateError(fault) => Some(Err(UpdateError::Device(fault))),
            _ => None,
        })
    }
}

/// How long each stage of an update may take.
#[derive(Debug, Clone, Copy)]
struct Waits {
    /// Erasing the update partition.
    begin: Duration,
    chunk: Duration,
    /// Checking the hash of the whole image.
    commit: Duration,
    /// Rebooting into the new firmware and appearing on USB again.
    reboot: Duration,
    reopen_every: Duration,
}

const WAITS: Waits = Waits {
    begin: Duration::from_secs(15),
    chunk: Duration::from_secs(5),
    commit: Duration::from_secs(10),
    reboot: Duration::from_secs(30),
    reopen_every: Duration::from_millis(250),
};

/// Refuses firmware unless the receiver has said it's the model the firmware was built for.
fn check_model(info: Option<PicoInfo>, firmware: &Firmware) -> Result<(), UpdateError> {
    match info {
        Some(info) if info.model == firmware.model => Ok(()),
        _ => Err(UpdateError::Device(UpdateFault::WrongModel)),
    }
}

/// Installs `firmware`. Until the final confirm, the receiver keeps or returns to its old
/// firmware if anything goes wrong, so every failure here leaves it working.
fn install<T: Read + Write>(
    mut link: Link<T>,
    firmware: &Firmware,
    waits: Waits,
    mut reopen: impl FnMut() -> io::Result<T>,
    mut progress: impl FnMut(f32),
) -> Result<Link<T>, UpdateError> {
    let image = &firmware.image;
    let size = image.len() as u32;
    let gone = |_| UpdateError::Gone;

    link.send(&HostMsg::UpdateBegin { model: firmware.model, size, sha256: Sha256::digest(image).into() }).map_err(gone)?;
    link.expect_ack(waits.begin, 0)?;
    for (i, data) in image.chunks(CHUNK).enumerate() {
        let offset = (i * CHUNK) as u32;
        link.send(&HostMsg::UpdateChunk { offset, data }).map_err(gone)?;
        let next = offset + data.len() as u32;
        // A lost frame only delays things: ask once more before giving up.
        match link.expect_ack(waits.chunk, next) {
            Err(UpdateError::Timeout) => {
                link.send(&HostMsg::UpdateChunk { offset, data }).map_err(gone)?;
                link.expect_ack(waits.chunk, next)?;
            }
            result => result?,
        }
        progress(next as f32 / size.max(1) as f32);
    }
    link.send(&HostMsg::UpdateCommit).map_err(gone)?;
    link.expect_ack(waits.commit, size)?;
    drop(link);

    let deadline = Instant::now() + waits.reboot;
    let hello = |msg: &DeviceMsg| match *msg {
        DeviceMsg::Hello { model, version, trial, .. } => Some(Ok((model, version, trial))),
        _ => None,
    };
    let mut link = loop {
        let attempt = (|| {
            let mut link = Link::new(reopen().map_err(gone)?);
            link.send(&HostMsg::GetInfo).map_err(gone)?;
            let (model, version, trial) = link.expect(waits.reopen_every, hello)?;
            // The old firmware may still answer for a moment before it reboots.
            if model != firmware.model || version != firmware.version || !trial {
                return Err(UpdateError::WrongVersion(version));
            }
            Ok::<_, UpdateError>(link)
        })();
        match attempt {
            Ok(found) => break found,
            Err(_) if Instant::now() < deadline => thread::sleep(waits.reopen_every),
            Err(e) => return Err(e),
        }
    };
    link.send(&HostMsg::Confirm).map_err(gone)?;
    // Answers to requests sent while it rebooted can still be queued, saying it's on trial.
    link.expect(waits.chunk, |msg| matches!(msg, DeviceMsg::Hello { trial: false, .. }).then_some(Ok(())))?;
    Ok(link)
}

/// The serial port, in raw mode with reads that give up after 100 ms.
struct Port {
    file: File,
    path: PathBuf,
}

impl Port {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32).open(path)?;
        let mut tio = rustix::termios::tcgetattr(&file)?;
        tio.make_raw();
        tio.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 0;
        tio.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 1;
        rustix::termios::tcsetattr(&file, rustix::termios::OptionalActions::Now, &tio)?;
        Ok(Self { file, path: path.to_path_buf() })
    }
}

impl Read for Port {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read(buf)?;
        // A hung-up tty reads as empty, same as a timeout; udev removes the link on unplug.
        if n == 0 && !self.path.exists() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        Ok(n)
    }
}

impl Write for Port {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// What the reader needs from whoever runs it.
pub trait PicoHost {
    fn set_info(&self, info: Option<PicoInfo>);
    fn info(&self) -> Option<PicoInfo>;
    fn command(&self, protocol: ir_proto::Protocol, address: u16, command: u16);
    fn next_update(&self) -> Option<Firmware>;
    fn update_status(&self, status: UpdateStatus);
    /// DIAGNOSTIC: part of a frame that didn't decode.
    fn timings(&self, _index: u8, _total: u8, _block_us: u16, _values: &[u16]) {}
    /// DIAGNOSTIC: raw samples.
    fn raw(&self, _seq: u16, _values: &[u16]) {}
}

/// DIAGNOSTIC: restarts the receiver into BOOTSEL.
pub fn enter_bootsel(path: &Path) -> io::Result<()> {
    Link::new(Port::open(path)?).send(&HostMsg::EnterBootsel)
}

/// Serves the receiver at `path` until it's unplugged.
pub fn serve(path: &Path, host: &dyn PicoHost) -> io::Result<()> {
    let mut link = Link::new(Port::open(path)?);
    let mut poll_at = Instant::now();
    let result = loop {
        if Instant::now() >= poll_at {
            poll_at = Instant::now() + POLL;
            let ask = if host.info().is_some_and(|i| i.compatible) { HostMsg::GetSignal } else { HostMsg::GetInfo };
            if let Err(e) = link.send(&ask) {
                break Err(e);
            }
        }
        if let Some(firmware) = host.next_update() {
            if let Err(e) = check_model(host.info(), &firmware) {
                tracing::warn!("refusing receiver update: {e}");
                host.update_status(UpdateStatus::Failed(e));
                continue;
            }
            let reopen = || Port::open(path);
            let status = |p| host.update_status(UpdateStatus::Progress(p));
            match install(link, &firmware, WAITS, reopen, status) {
                Ok(next) => {
                    link = next;
                    host.set_info(Some(PicoInfo { model: firmware.model, version: firmware.version, compatible: true, signal: None }));
                    host.update_status(UpdateStatus::Done(firmware.version));
                }
                Err(e) => {
                    tracing::warn!("receiver update failed: {e}");
                    host.update_status(UpdateStatus::Failed(e));
                    // The old link is gone with the attempt; start again, or end if unplugged.
                    link = match Port::open(path) {
                        Result::Ok(port) => Link::new(port),
                        Err(e) => break Err(e),
                    };
                    host.set_info(None);
                }
            }
            continue;
        }
        match link.recv() {
            Result::Ok(Some(msg)) => handle(msg, host),
            Result::Ok(None) => {}
            Err(e) => break Err(e),
        }
    };
    host.set_info(None);
    result
}

fn handle(msg: DeviceMsg, host: &dyn PicoHost) {
    match msg {
        DeviceMsg::Hello { protocol, model, version, .. } => {
            let signal = host.info().and_then(|i| i.signal);
            host.set_info(Some(PicoInfo { model, version, compatible: protocol == PROTOCOL, signal }));
        }
        DeviceMsg::Ir { protocol, address, command, .. } => {
            if host.info().is_some_and(|i| i.compatible) {
                host.command(protocol, address, command);
            }
        }
        DeviceMsg::Signal { peak, noise } => {
            if let Some(info) = host.info() {
                host.set_info(Some(PicoInfo { signal: Some((peak, noise)), ..info }));
            }
        }
        DeviceMsg::Timings { index, total, block_us, values } => host.timings(index, total, block_us, &values),
        DeviceMsg::Raw { seq, values } => host.raw(seq, &values),
        DeviceMsg::UpdateAck { .. } | DeviceMsg::UpdateError(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const OLD: Version = Version { major: 0, minor: 1, patch: 0 };
    const NEW: Version = Version { major: 0, minor: 2, patch: 0 };

    const FAST: Waits = Waits {
        begin: Duration::from_millis(30),
        chunk: Duration::from_millis(30),
        commit: Duration::from_millis(30),
        reboot: Duration::from_millis(60),
        reopen_every: Duration::from_millis(5),
    };

    /// Ways for the simulated receiver to misbehave.
    #[derive(Clone, Copy, Default)]
    struct Faults {
        lose_first_chunk: bool,
        corrupt_hash: bool,
        vanish_at: Option<u32>,
        reject_begin: bool,
        /// Boots back into the old firmware, as after a rollback.
        rolls_back: bool,
        never_returns: bool,
        ignores_confirm: bool,
        /// Still running the old firmware the first time it's reached after the commit.
        slow_reboot: bool,
        /// A later model of receiver.
        other_model: bool,
        /// Writing to flash fails from this offset.
        flash_fails_at: Option<u32>,
        /// Reading fails from this offset while writing still succeeds, as a real port does
        /// when it's unplugged.
        reads_fail_at: Option<u32>,
        /// Answers queued during the reboot arrive ahead of the reply to the confirm.
        stale_hello: bool,
    }

    #[derive(Default)]
    struct Sim {
        faults: Faults,
        received: Vec<u8>,
        hash: [u8; 32],
        trial: bool,
        running_new: bool,
        confirmed: bool,
        rebooted: bool,
        gone: bool,
        lost: bool,
        outbox: VecDeque<u8>,
        noise_sent: bool,
        reads_gone: bool,
        chunks_after_fault: u32,
    }

    impl Sim {
        fn model(&self) -> Model {
            if self.faults.other_model { Model(2) } else { Model::IR1 }
        }

        fn send(&mut self, msg: DeviceMsg) {
            let mut out = [0; MAX_FRAME];
            self.outbox.extend(encode(&msg, &mut out).unwrap());
        }

        fn on(&mut self, msg: HostMsg) {
            match msg {
                HostMsg::GetInfo => {
                    let version = if self.running_new { NEW } else { OLD };
                    self.send(DeviceMsg::Hello { protocol: PROTOCOL, model: self.model(), version, trial: self.trial });
                }
                HostMsg::GetSignal => self.send(DeviceMsg::Signal { peak: 1, noise: 0 }),
                HostMsg::EnterBootsel => {}
                HostMsg::UpdateBegin { model, sha256, .. } => {
                    if model != self.model() {
                        return self.send(DeviceMsg::UpdateError(UpdateFault::WrongModel));
                    }
                    if self.faults.reject_begin {
                        return self.send(DeviceMsg::UpdateError(UpdateFault::BadImage));
                    }
                    self.hash = sha256;
                    self.received.clear();
                    self.send(DeviceMsg::UpdateAck { offset: 0 });
                }
                HostMsg::UpdateChunk { offset, data } => {
                    if self.faults.flash_fails_at.is_some_and(|at| offset >= at) {
                        self.chunks_after_fault += 1;
                        return self.send(DeviceMsg::UpdateError(UpdateFault::Flash));
                    }
                    if self.faults.reads_fail_at.is_some_and(|at| offset >= at) {
                        self.chunks_after_fault += 1;
                        self.reads_gone = true;
                        return;
                    }
                    if self.faults.vanish_at.is_some_and(|at| offset >= at) {
                        self.gone = true;
                        return;
                    }
                    if self.faults.lose_first_chunk && !self.lost {
                        self.lost = true;
                        return;
                    }
                    assert_eq!(offset as usize, self.received.len());
                    self.received.extend_from_slice(data);
                    if !self.noise_sent {
                        // A remote press mid-update must not confuse things.
                        self.noise_sent = true;
                        self.send(DeviceMsg::Ir { protocol: ir_proto::Protocol::Nec, address: 1, command: 2, repeat: false });
                    }
                    self.send(DeviceMsg::UpdateAck { offset: self.received.len() as u32 });
                }
                HostMsg::UpdateCommit => {
                    let mut hash: [u8; 32] = Sha256::digest(&self.received).into();
                    if self.faults.corrupt_hash {
                        hash[0] ^= 1;
                    }
                    if hash != self.hash {
                        return self.send(DeviceMsg::UpdateError(UpdateFault::BadHash));
                    }
                    self.send(DeviceMsg::UpdateAck { offset: self.received.len() as u32 });
                    self.rebooted = true;
                }
                HostMsg::Confirm => {
                    if self.faults.stale_hello {
                        self.send(DeviceMsg::Hello { protocol: PROTOCOL, model: self.model(), version: NEW, trial: true });
                        self.send(DeviceMsg::Hello { protocol: PROTOCOL, model: self.model(), version: NEW, trial: true });
                    }
                    if !self.faults.ignores_confirm {
                        self.trial = false;
                        self.confirmed = true;
                    }
                    self.send(DeviceMsg::Hello { protocol: PROTOCOL, model: self.model(), version: NEW, trial: self.trial });
                }
            }
        }
    }

    #[derive(Clone)]
    struct FakePort {
        sim: Rc<RefCell<Sim>>,
        reader: Rc<RefCell<FrameReader>>,
    }

    impl Read for FakePort {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut sim = self.sim.borrow_mut();
            if sim.gone || sim.reads_gone {
                return Err(io::ErrorKind::NotConnected.into());
            }
            let n = buf.len().min(sim.outbox.len());
            for slot in &mut buf[..n] {
                *slot = sim.outbox.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for FakePort {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut sim = self.sim.borrow_mut();
            if sim.gone {
                return Err(io::ErrorKind::NotConnected.into());
            }
            let mut reader = self.reader.borrow_mut();
            for byte in buf {
                if let Some(msg) = reader.push(*byte).and_then(decode::<HostMsg>) {
                    sim.on(msg);
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn firmware() -> Firmware {
        Firmware { model: Model::IR1, version: NEW, image: (0..CHUNK * 3 + 10).map(|i| (i % 251) as u8).collect() }
    }

    fn run(faults: Faults) -> (Result<(), UpdateError>, Rc<RefCell<Sim>>, Vec<f32>) {
        let sim = Rc::new(RefCell::new(Sim { faults, ..Sim::default() }));
        let port = |sim: &Rc<RefCell<Sim>>| FakePort { sim: sim.clone(), reader: Rc::default() };
        let reopen = {
            let (sim, port) = (sim.clone(), port);
            move || {
                let mut s = sim.borrow_mut();
                if !s.rebooted || s.faults.never_returns {
                    return Err(io::ErrorKind::NotFound.into());
                }
                s.outbox.clear();
                if s.faults.slow_reboot && !s.lost {
                    s.lost = true;
                    drop(s);
                    return Ok(port(&sim));
                }
                s.running_new = !s.faults.rolls_back;
                s.trial = !s.faults.rolls_back && !s.confirmed;
                drop(s);
                Ok(port(&sim))
            }
        };
        let mut seen = Vec::new();
        let result = install(Link::new(port(&sim)), &firmware(), FAST, reopen, |p| seen.push(p)).map(|_| ());
        (result, sim, seen)
    }

    #[test]
    fn installs_and_confirms() {
        let (result, sim, progress) = run(Faults::default());
        assert_eq!(result, Ok(()));
        let sim = sim.borrow();
        assert_eq!(sim.received, firmware().image);
        assert!(sim.confirmed);
        assert_eq!(progress.len(), 4);
        assert_eq!(progress.last(), Some(&1.0));
        assert!(progress.windows(2).all(|p| p[0] < p[1]));
    }

    #[test]
    fn the_old_firmware_answering_before_its_reboot_is_not_a_failure() {
        let (result, sim, _) = run(Faults { slow_reboot: true, ..Faults::default() });
        assert_eq!(result, Ok(()));
        assert!(sim.borrow().confirmed);
    }

    #[test]
    fn another_model_refuses_the_firmware_and_nothing_is_sent() {
        let (result, sim, progress) = run(Faults { other_model: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Device(UpdateFault::WrongModel)));
        assert!(sim.borrow().received.is_empty());
        assert!(!sim.borrow().rebooted);
        assert!(progress.is_empty());
    }

    #[test]
    fn cablebox_refuses_another_model_or_an_unidentified_receiver() {
        let info = |model| PicoInfo { model, version: OLD, compatible: true, signal: None };
        assert_eq!(check_model(Some(info(Model::IR1)), &firmware()), Ok(()));
        // Even when the firmware's version is newer.
        let refused = Err(UpdateError::Device(UpdateFault::WrongModel));
        assert_eq!(check_model(Some(info(Model(2))), &firmware()), refused);
        assert_eq!(check_model(None, &firmware()), refused);
    }

    #[test]
    fn a_lost_chunk_is_sent_again() {
        let (result, sim, _) = run(Faults { lose_first_chunk: true, ..Faults::default() });
        assert_eq!(result, Ok(()));
        assert_eq!(sim.borrow().received, firmware().image);
    }

    #[test]
    fn a_wrong_hash_never_reboots() {
        let (result, sim, _) = run(Faults { corrupt_hash: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Device(UpdateFault::BadHash)));
        assert!(!sim.borrow().rebooted);
    }

    #[test]
    fn a_rejected_image_stops_at_once() {
        let (result, sim, _) = run(Faults { reject_begin: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Device(UpdateFault::BadImage)));
        assert!(sim.borrow().received.is_empty());
    }

    #[test]
    fn vanishing_midway_is_reported_and_not_confirmed() {
        let (result, sim, _) = run(Faults { vanish_at: Some(CHUNK as u32), ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Gone));
        assert!(!sim.borrow().confirmed);
    }

    #[test]
    fn a_flash_failure_stops_the_update_at_once() {
        let (result, sim, progress) = run(Faults { flash_fails_at: Some(CHUNK as u32), ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Device(UpdateFault::Flash)));
        assert_eq!(sim.borrow().chunks_after_fault, 1, "nothing more is sent");
        assert!(!sim.borrow().rebooted);
        assert_eq!(progress.len(), 1);
    }

    #[test]
    fn unplugging_stops_the_update_even_while_writes_still_succeed() {
        let (result, sim, _) = run(Faults { reads_fail_at: Some(CHUNK as u32), ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Gone));
        assert_eq!(sim.borrow().chunks_after_fault, 1, "nothing more is sent");
    }

    #[test]
    fn stale_answers_from_the_reboot_are_not_taken_as_the_confirm_failing() {
        let (result, sim, _) = run(Faults { stale_hello: true, ..Faults::default() });
        assert_eq!(result, Ok(()));
        assert!(sim.borrow().confirmed);
    }

    #[test]
    fn no_return_after_reboot_fails_without_confirming() {
        let (result, sim, _) = run(Faults { never_returns: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Gone));
        assert!(!sim.borrow().confirmed);
    }

    #[test]
    fn a_rollback_is_noticed_and_not_confirmed() {
        let (result, sim, _) = run(Faults { rolls_back: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::WrongVersion(OLD)));
        assert!(!sim.borrow().confirmed);
    }

    #[test]
    fn a_confirm_that_does_not_stick_is_a_failure() {
        let (result, _, _) = run(Faults { ignores_confirm: true, ..Faults::default() });
        assert_eq!(result, Err(UpdateError::Timeout));
    }

    /// The real port code, on a pseudo-terminal standing in for USB serial.
    #[test]
    fn the_port_is_raw_times_out_and_carries_frames() {
        use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let path = PathBuf::from(name.to_str().unwrap());
        let mut link = Link::new(Port::open(&path).unwrap());
        let mut master = File::from(master);

        // Nothing to read: gives up after about VTIME rather than blocking.
        let start = Instant::now();
        assert_eq!(link.recv().unwrap(), None);
        assert!(start.elapsed() < Duration::from_secs(2));

        // Raw: zero bytes, newlines and control characters all survive.
        let msg = DeviceMsg::Ir { protocol: ir_proto::Protocol::Nec, address: 0x0a0d, command: 0x0304, repeat: false };
        let mut out = [0; MAX_FRAME];
        master.write_all(encode(&msg, &mut out).unwrap()).unwrap();
        assert_eq!(link.expect(Duration::from_secs(2), |m| Some(Ok(*m))), Ok(msg));

        // And the other way.
        link.send(&HostMsg::GetInfo).unwrap();
        let mut reader = FrameReader::default();
        let mut buf = [0; 64];
        let n = master.read(&mut buf).unwrap();
        let body = buf[..n].iter().find_map(|b| reader.push(*b).map(<[u8]>::to_vec)).unwrap();
        assert_eq!(decode::<HostMsg>(&body), Some(HostMsg::GetInfo));
    }

    #[derive(Default)]
    struct FakeHost {
        info: RefCell<Option<PicoInfo>>,
        commands: RefCell<Vec<(u16, u16)>>,
    }

    impl PicoHost for FakeHost {
        fn set_info(&self, info: Option<PicoInfo>) {
            *self.info.borrow_mut() = info;
        }
        fn info(&self) -> Option<PicoInfo> {
            *self.info.borrow()
        }
        fn command(&self, _: ir_proto::Protocol, address: u16, command: u16) {
            self.commands.borrow_mut().push((address, command));
        }
        fn next_update(&self) -> Option<Firmware> {
            None
        }
        fn update_status(&self, _: UpdateStatus) {}
    }

    #[test]
    fn a_receiver_with_another_protocol_can_only_be_updated() {
        let host = FakeHost::default();
        let ir = DeviceMsg::Ir { protocol: ir_proto::Protocol::Nec, address: 1, command: 2, repeat: false };
        handle(ir, &host); // nothing known yet
        handle(DeviceMsg::Hello { protocol: PROTOCOL + 1, model: Model::IR1, version: OLD, trial: false }, &host);
        assert_eq!(host.info().map(|i| i.compatible), Some(false));
        handle(ir, &host);
        assert!(host.commands.borrow().is_empty());
        handle(DeviceMsg::Hello { protocol: PROTOCOL, model: Model::IR1, version: OLD, trial: false }, &host);
        assert_eq!(host.info().map(|i| i.model), Some(Model::IR1));
        handle(ir, &host);
        assert_eq!(*host.commands.borrow(), [(1, 2)]);
        handle(DeviceMsg::Signal { peak: 450, noise: 3 }, &host);
        assert_eq!(host.info().and_then(|i| i.signal), Some((450, 3)));
    }
}
