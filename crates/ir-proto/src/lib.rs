//! Messages between CableBox and the Pico IR receiver, shared by both ends. Frames are
//! postcard with a CRC-16, COBS-encoded and ended by a zero byte, so a reader that joins
//! midway or sees a corrupt frame recovers at the next zero.
//!
//! Receivers in the field must stay updatable: never change [`DeviceMsg::Hello`], or the
//! update messages, without a way for old firmware to still be replaced.

#![cfg_attr(not(test), no_std)]

use serde::{Deserialize, Serialize};

/// Bumped when messages change incompatibly.
pub const PROTOCOL: u16 = 1;
/// Firmware bytes in one [`HostMsg::UpdateChunk`].
pub const CHUNK: usize = 256;
/// Longest encoded frame, including its zero byte.
pub const MAX_FRAME: usize = CHUNK + 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Nec,
    Rc5,
    Rc6,
    Sony,
    Samsung,
    /// Not a known protocol: `address` and `command` are a fingerprint of the pulse pattern,
    /// which is all learning a button needs.
    Pulses,
}

impl Protocol {
    pub const ALL: [Self; 6] = [Self::Nec, Self::Rc5, Self::Rc6, Self::Sony, Self::Samsung, Self::Pulses];

    pub fn name(self) -> &'static str {
        match self {
            Self::Nec => "nec",
            Self::Rc5 => "rc5",
            Self::Rc6 => "rc6",
            Self::Sony => "sony",
            Self::Samsung => "samsung",
            Self::Pulses => "pulses",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// Which hardware a receiver is. Firmware is built for exactly one model and is never
/// installed on another, however its version compares. Numbers are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Model(pub u16);

impl Model {
    /// Pico with a bare 2-leg photodiode, sampled by the ADC.
    pub const IR1: Self = Self(1);
}

impl core::fmt::Display for Model {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "IR{}", self.0)
    }
}

/// Firmware version. Ordered, so the newest compares greatest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl core::str::FromStr for Version {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let mut parts = s.trim().split('.').map(|p| p.parse::<u16>().map_err(|_| ()));
        let mut next = || parts.next().ok_or(())?;
        let v = Version { major: next()?, minor: next()?, patch: next()? };
        if parts.next().is_some() {
            return Err(());
        }
        Ok(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateFault {
    /// The image doesn't fit, or a chunk arrived out of place.
    BadImage,
    /// The received image doesn't match the announced hash.
    BadHash,
    Flash,
    /// The firmware was built for another model.
    WrongModel,
}

/// From the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceMsg {
    /// Answers [`HostMsg::GetInfo`] and [`HostMsg::Confirm`]. `trial`: this firmware was just
    /// installed and rolls back unless confirmed.
    Hello { protocol: u16, model: Model, version: Version, trial: bool },
    Ir { protocol: Protocol, address: u16, command: u16, repeat: bool },
    /// Strongest carrier and background level lately, in ADC counts.
    Signal { peak: u16, noise: u16 },
    /// Everything before `offset` is stored. Answers the update messages.
    UpdateAck { offset: u32 },
    UpdateError(UpdateFault),
    /// DIAGNOSTIC: part of a frame that didn't decode, in µs, starting at timing `index` of
    /// `total`. Also the average sample block length.
    Timings { index: u8, total: u8, block_us: u16, values: [u16; 12] },
    /// DIAGNOSTIC: raw ADC samples from a capture, `seq` counting groups within one capture.
    Raw { seq: u16, values: [u16; 12] },
}

/// From CableBox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMsg<'a> {
    GetInfo,
    GetSignal,
    /// The receiver refuses a `model` other than its own, before erasing anything.
    UpdateBegin { model: Model, size: u32, sha256: [u8; 32] },
    UpdateChunk { offset: u32, data: &'a [u8] },
    /// Checks the hash, acknowledges, then reboots into the new firmware.
    UpdateCommit,
    /// Keeps the running firmware; without this a trial rolls back.
    Confirm,
    /// DIAGNOSTIC: restart into BOOTSEL, to flash a new build.
    EnterBootsel,
}

/// The receiver's side of an image transfer: which chunks to store and when it's complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    pub size: u32,
    pub sha256: [u8; 32],
    pub received: u32,
}

/// What to do with an arriving chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkAction {
    Store,
    /// CableBox didn't hear the last acknowledgement and sent it again; don't store it twice.
    AckAgain,
}

impl Transfer {
    /// Starts a transfer, if the image is for this model and fits in `capacity` bytes.
    pub fn begin(ours: Model, model: Model, size: u32, sha256: [u8; 32], capacity: u32) -> Result<Self, UpdateFault> {
        if model != ours {
            return Err(UpdateFault::WrongModel);
        }
        if size == 0 || size > capacity {
            return Err(UpdateFault::BadImage);
        }
        Ok(Self { size, sha256, received: 0 })
    }

    pub fn chunk(&self, offset: u32, len: u32) -> Result<ChunkAction, UpdateFault> {
        if len == 0 || len as usize > CHUNK || offset.checked_add(len).is_none_or(|end| end > self.size) {
            return Err(UpdateFault::BadImage);
        }
        if offset == self.received {
            return Ok(ChunkAction::Store);
        }
        if offset + len == self.received {
            return Ok(ChunkAction::AckAgain);
        }
        Err(UpdateFault::BadImage)
    }

    pub fn complete(&self) -> bool {
        self.received == self.size
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Encodes `msg` as one frame into `out`, returning its bytes.
pub fn encode<'a, T: Serialize>(msg: &T, out: &'a mut [u8; MAX_FRAME]) -> Option<&'a [u8]> {
    let mut raw = [0u8; MAX_FRAME];
    let len = postcard::to_slice(msg, &mut raw[..MAX_FRAME - 8]).ok()?.len();
    let crc = crc16(&raw[..len]).to_le_bytes();
    raw[len..len + 2].copy_from_slice(&crc);
    let raw = &raw[..len + 2];

    // COBS: each code byte says how far to the next zero.
    let (mut at, mut code_at, mut code) = (1, 0, 1u8);
    for &byte in raw {
        if byte != 0 {
            out[at] = byte;
            at += 1;
            code += 1;
        }
        if byte == 0 || code == 0xff {
            out[code_at] = code;
            code_at = at;
            at += 1;
            code = 1;
        }
    }
    out[code_at] = code;
    out[at] = 0;
    Some(&out[..=at])
}

/// Collects bytes into frames.
pub struct FrameReader {
    buf: [u8; MAX_FRAME],
    len: usize,
    overflow: bool,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self { buf: [0; MAX_FRAME], len: 0, overflow: false }
    }
}

impl FrameReader {
    /// Adds a byte. At the end of a good frame, returns its message bytes for
    /// [`postcard::from_bytes`] (see [`decode`]). Bad frames are dropped.
    pub fn push(&mut self, byte: u8) -> Option<&[u8]> {
        if byte != 0 {
            match self.buf.get_mut(self.len) {
                Some(slot) => {
                    *slot = byte;
                    self.len += 1;
                }
                None => self.overflow = true,
            }
            return None;
        }
        let (len, overflow) = (self.len, self.overflow);
        self.len = 0;
        self.overflow = false;
        if overflow {
            return None;
        }
        let decoded = uncobs(&mut self.buf[..len])?;
        let body = decoded.checked_sub(2)?;
        let crc = u16::from_le_bytes([self.buf[body], self.buf[body + 1]]);
        (crc16(&self.buf[..body]) == crc).then_some(&self.buf[..body])
    }
}

/// Decodes COBS in place, returning the decoded length.
fn uncobs(buf: &mut [u8]) -> Option<usize> {
    let (mut read, mut write) = (0, 0);
    while read < buf.len() {
        let code = usize::from(buf[read]);
        if code == 0 || read + code > buf.len() {
            return None;
        }
        buf.copy_within(read + 1..read + code, write);
        write += code - 1;
        read += code;
        if code != 0xff && read < buf.len() {
            buf[write] = 0;
            write += 1;
        }
    }
    Some(write)
}

/// Parses a frame's message bytes.
pub fn decode<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Option<T> {
    postcard::from_bytes(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(bytes: &[u8]) -> Vec<DeviceMsg> {
        let mut reader = FrameReader::default();
        bytes.iter().filter_map(|b| reader.push(*b).and_then(decode)).collect()
    }

    fn frame(msg: &DeviceMsg) -> Vec<u8> {
        let mut out = [0; MAX_FRAME];
        encode(msg, &mut out).unwrap().to_vec()
    }

    const IR: DeviceMsg = DeviceMsg::Ir { protocol: Protocol::Rc6, address: 0x800f, command: 0x0422, repeat: true };

    #[test]
    fn device_messages_round_trip() {
        let version = Version { major: 1, minor: 2, patch: 300 };
        for msg in [
            DeviceMsg::Hello { protocol: PROTOCOL, model: Model::IR1, version, trial: true },
            IR,
            DeviceMsg::Signal { peak: 459, noise: 0 },
            DeviceMsg::UpdateAck { offset: 0 },
            DeviceMsg::UpdateError(UpdateFault::BadHash),
        ] {
            let bytes = frame(&msg);
            assert_eq!(bytes.iter().filter(|b| **b == 0).count(), 1, "only the end is zero");
            assert_eq!(frames(&bytes), [msg]);
        }
    }

    #[test]
    fn a_full_chunk_fits_and_round_trips_for_any_bytes() {
        for fill in [0u8, 1, 0xff] {
            let data = [fill; CHUNK];
            let msg = HostMsg::UpdateChunk { offset: u32::MAX, data: &data };
            let mut out = [0; MAX_FRAME];
            let bytes = encode(&msg, &mut out).unwrap();
            let mut reader = FrameReader::default();
            let found: Vec<_> = bytes.iter().filter_map(|b| reader.push(*b).map(|body| body.to_vec())).collect();
            assert_eq!(found.len(), 1);
            assert_eq!(decode::<HostMsg>(&found[0]), Some(msg));
        }
    }

    #[test]
    fn long_runs_with_no_zeros_cross_cobs_blocks() {
        let data: Vec<u8> = (0..CHUNK).map(|i| (i % 255 + 1) as u8).collect();
        let msg = HostMsg::UpdateChunk { offset: 1, data: &data };
        let mut out = [0; MAX_FRAME];
        let bytes = encode(&msg, &mut out).unwrap();
        let mut reader = FrameReader::default();
        let body = bytes.iter().find_map(|b| reader.push(*b).map(|b| b.to_vec())).unwrap();
        assert_eq!(decode::<HostMsg>(&body), Some(msg));
    }

    #[test]
    fn corrupt_frames_are_dropped_and_the_next_one_survives() {
        let good = frame(&IR);
        let mut flipped = good.clone();
        flipped[2] ^= 0x10;
        assert_eq!(frames(&flipped), []);
        assert_eq!(frames(&[flipped, good.clone()].concat()), [IR]);
        // Joining midway through a frame.
        assert_eq!(frames(&[&good[3..], &good[..]].concat()), [IR]);
        // Text from a build that doesn't speak this protocol.
        assert_eq!(frames(&[b"peak=3 floor=0\r\n".as_slice(), &[0], &good].concat()), [IR]);
    }

    #[test]
    fn an_endless_stream_with_no_zero_recovers() {
        let junk = vec![7u8; MAX_FRAME * 3];
        assert_eq!(frames(&[junk, vec![0], frame(&IR)].concat()), [IR]);
    }

    #[test]
    fn versions_parse_and_order() {
        let v = |s: &str| s.parse::<Version>();
        assert_eq!(v("1.2.3\n"), Ok(Version { major: 1, minor: 2, patch: 3 }));
        assert_eq!(v("1.2.3").unwrap().to_string(), "1.2.3");
        assert!(v("1.10.0").unwrap() > v("1.9.9").unwrap());
        for bad in ["", "1.2", "1.2.3.4", "a.b.c"] {
            assert!(v(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_transfer_checks_model_size_and_order() {
        let begin = |model, size| Transfer::begin(Model::IR1, model, size, [0; 32], 1000);
        assert_eq!(begin(Model(2), 100), Err(UpdateFault::WrongModel));
        assert_eq!(begin(Model::IR1, 0), Err(UpdateFault::BadImage));
        assert_eq!(begin(Model::IR1, 1001), Err(UpdateFault::BadImage));
        let mut t = begin(Model::IR1, 300).unwrap();
        assert_eq!(t.chunk(0, 256), Ok(ChunkAction::Store));
        t.received = 256;
        assert!(!t.complete());
        assert_eq!(t.chunk(0, 256), Ok(ChunkAction::AckAgain));
        assert_eq!(t.chunk(512, 10), Err(UpdateFault::BadImage));
        assert_eq!(t.chunk(256, 256), Err(UpdateFault::BadImage));
        assert_eq!(t.chunk(u32::MAX, 10), Err(UpdateFault::BadImage));
        assert_eq!(t.chunk(256, 44), Ok(ChunkAction::Store));
        t.received = 300;
        assert!(t.complete());
    }

    #[test]
    fn models_are_named_from_their_number() {
        assert_eq!(Model::IR1.to_string(), "IR1");
        assert_eq!(Model(2).to_string(), "IR2");
    }

    #[test]
    fn protocol_names_round_trip() {
        for p in Protocol::ALL {
            assert_eq!(Protocol::from_name(p.name()), Some(p));
        }
    }
}
