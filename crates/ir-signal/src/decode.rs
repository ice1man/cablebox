//! Turns one frame of IR timings into a command. Timings come from carrier detection in
//! 128 µs blocks, so they're coarse: every comparison here allows for that.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Nec,
    Rc5,
    Rc6,
    Sony,
    Samsung,
    /// Not a known protocol; see [`fingerprint`].
    Pulses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub protocol: Protocol,
    pub address: u16,
    pub command: u16,
}

/// What a frame turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Command(Command),
    /// NEC's short "still held" code, with no command of its own.
    NecRepeat,
}

/// Enough timings for the longest frame here, RC6 with 32 bits at every level change.
pub const MAX_TIMINGS: usize = 96;

/// Fewer pulses than this is more likely interference than a remote.
const MIN_PULSES: usize = 8;
/// Pulse-to-pulse times are compared in steps this big, coarse enough to absorb the timing error
/// of detecting the carrier in blocks.
const STEP_US: u32 = 250;

/// Identifies a transmission by its pulse pattern, for remotes with no decoder here. A remote that
/// changes its signal on every press (a toggle bit) would look like several buttons.
pub fn fingerprint(frame: &[u32]) -> Option<Command> {
    if frame.len() / 2 + 1 < MIN_PULSES {
        return None;
    }
    let mut hash = 0x811c_9dc5u32; // FNV-1a
    for period in frame.chunks_exact(2) {
        let steps = (period[0] + period[1] + STEP_US / 2) / STEP_US;
        for byte in (steps as u16).to_le_bytes() {
            hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        }
    }
    Some(Command { protocol: Protocol::Pulses, address: (hash >> 16) as u16, command: hash as u16 })
}

fn near(value: u32, target: u32) -> bool {
    value.abs_diff(target) <= (target / 3).max(200)
}

/// `frame` alternates mark, space, mark… starting and ending on a mark.
pub fn decode(frame: &[u32]) -> Option<Frame> {
    let (&lead, rest) = frame.split_first()?;
    let gap = *rest.first()?;
    if near(lead, 9000) {
        if near(gap, 2250) && frame.len() <= 3 {
            return Some(Frame::NecRepeat);
        }
        return (near(gap, 4500)).then(|| nec(&frame[2..], Protocol::Nec)).flatten();
    }
    if near(lead, 4500) && near(gap, 4500) {
        return nec(&frame[2..], Protocol::Samsung);
    }
    if lead > 2000 {
        return sony(frame).or_else(|| rc6(frame));
    }
    rc5(frame)
}

/// Pulse-distance data: a short mark, then a short space for 0 or a long one for 1.
fn nec(data: &[u32], protocol: Protocol) -> Option<Frame> {
    if data.len() != 65 {
        return None;
    }
    let mut bits = 0u32;
    for i in 0..32 {
        let (mark, space) = (data[i * 2], data[i * 2 + 1]);
        if !near(mark, 562) {
            return None;
        }
        if near(space, 1687) {
            bits |= 1 << i;
        } else if !near(space, 562) {
            return None;
        }
    }
    let [a, b, c, d] = bits.to_le_bytes();
    let address = u16::from_le_bytes([a, b]);
    // The last byte normally checks the third; if not, the remote uses both as its command.
    let command = if c == !d { u16::from(c) } else { u16::from_le_bytes([c, d]) };
    Some(Frame::Command(Command { protocol, address, command }))
}

fn sony(frame: &[u32]) -> Option<Frame> {
    if !near(frame[0], 2400) || frame.len() % 2 == 0 {
        return None;
    }
    let bits = (frame.len() - 1) / 2;
    if !matches!(bits, 12 | 15 | 20) {
        return None;
    }
    let mut value = 0u32;
    for i in 0..bits {
        if !near(frame[1 + i * 2], 600) {
            return None;
        }
        let mark = frame[2 + i * 2];
        // Sharper than `near`, since these two are only one 600 µs step apart.
        if mark.abs_diff(1200) < mark.abs_diff(600) {
            value |= 1 << i;
        } else if !near(mark, 600) {
            return None;
        }
    }
    let (address, command) = ((value >> 7) as u16, (value & 0x7f) as u16);
    Some(Frame::Command(Command { protocol: Protocol::Sony, address, command }))
}

/// Expands timings into equal time units of level, `first` being the first timing's level.
/// Returns how many were written.
fn levels(timings: &[u32], unit: u32, first: bool, out: &mut [bool]) -> Option<usize> {
    let (mut level, mut at) = (first, 0);
    for &t in timings {
        let units = ((t + unit / 2) / unit) as usize;
        if !(1..=4).contains(&units) || at + units > out.len() {
            return None;
        }
        out[at..at + units].fill(level);
        at += units;
        level = !level;
    }
    Some(at)
}

fn rc5(frame: &[u32]) -> Option<Frame> {
    let mut l = [false; 28];
    // The first half of the start bit is a space, which can't be seen.
    let n = levels(frame, 889, true, &mut l[1..])? + 1;
    // A final 0 bit ends in an unseen space too.
    if n != 28 && n != 27 {
        return None;
    }
    let mut value = 0u16;
    for pair in l.chunks_exact(2) {
        if pair[0] == pair[1] {
            return None;
        }
        value = value << 1 | u16::from(pair[1]);
    }
    if value >> 13 != 1 {
        return None;
    }
    // The second start bit, inverted, extends the command to 7 bits (RC5X).
    let command = (value & 0x3f) | u16::from((value >> 12) & 1 == 0) << 6;
    Some(Frame::Command(Command { protocol: Protocol::Rc5, address: (value >> 6) & 0x1f, command }))
}

fn rc6(frame: &[u32]) -> Option<Frame> {
    if !near(frame[0], 2666) || !near(frame[1], 889) {
        return None;
    }
    let mut l = [false; 2 * (5 + 32) + 2];
    let n = levels(&frame[2..], 444, true, &mut l)?;
    // The last half is an unseen space when the last bit is 1, which makes the count odd.
    let n = n + n % 2;
    let (mut at, mut mode, mut value, mut bits) = (0, 0u8, 0u32, 0);
    let mut field = 0;
    while at < n {
        // Field 4 is the toggle ("trailer"), twice as long and not part of the identity.
        let width = if field == 4 { 2 } else { 1 };
        if at + 2 * width > n || l[at] == l[at + width] || l[at] != l[at + width - 1] {
            return None;
        }
        let bit = l[at];
        match field {
            0 if !bit => return None,
            1..=3 => mode = mode << 1 | u8::from(bit),
            5.. => {
                if bits == 32 {
                    return None;
                }
                value = value << 1 | u32::from(bit);
                bits += 1;
            }
            _ => {}
        }
        at += 2 * width;
        field += 1;
    }
    let (address, command) = match (mode, bits) {
        (0, 16) => ((value >> 8) as u16, (value & 0xff) as u16),
        // Mode 6A's long form puts another toggle in bit 15, as Windows Media Center remotes do.
        (6, 32) => ((value >> 16) as u16, (value & 0x7fff) as u16),
        (6, 20 | 24) => ((value >> 8) as u16, (value & 0xff) as u16),
        _ => return None,
    };
    Some(Frame::Command(Command { protocol: Protocol::Rc6, address, command }))
}
