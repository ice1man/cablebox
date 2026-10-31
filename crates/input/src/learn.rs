//! Learned remote buttons: how a physical button is identified, and turning a device's raw
//! events into presses. No device access here, so all of it can be tested with plain values.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use evdev::KeyCode;
use ir_proto::Protocol;

use crate::RemoteKey;

/// A scancode seen again this soon is the same button still being held. IR has no release.
const HOLD_GAP: Duration = Duration::from_millis(300);
/// Same timing as rc-core's key autorepeat, so learned buttons feel like built-in ones.
const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_PERIOD: Duration = Duration::from_millis(125);

/// One physical button on a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// Scancode from an rc-core receiver, reported even when no keymap entry exists.
    Scan(u32),
    /// Key code from any other remote, such as a USB HID one.
    Key(u16),
    /// Command decoded by the Pico receiver. A toggle bit isn't part of a button's identity.
    Ir { protocol: Protocol, address: u16, command: u16 },
}

impl fmt::Display for Button {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Button::Scan(s) => write!(f, "scan:{s:08x}"),
            Button::Key(k) => write!(f, "key:{:?}", KeyCode::new(*k)),
            Button::Ir { protocol, address, command } => write!(f, "ir:{}:{address:02x}:{command:02x}", protocol.name()),
        }
    }
}

impl FromStr for Button {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s.split_once(':').ok_or(())? {
            ("scan", hex) => u32::from_str_radix(hex, 16).map(Button::Scan).map_err(|_| ()),
            ("key", name) => name.parse::<KeyCode>().map(|k| Button::Key(k.code())).map_err(|_| ()),
            ("ir", rest) => {
                let mut parts = rest.split(':');
                let protocol = parts.next().and_then(Protocol::from_name).ok_or(())?;
                let mut hex = || u16::from_str_radix(parts.next().ok_or(())?, 16).map_err(|_| ());
                let (address, command) = (hex()?, hex()?);
                if parts.next().is_some() {
                    return Err(());
                }
                Ok(Button::Ir { protocol, address, command })
            }
            _ => Err(()),
        }
    }
}

pub type Bindings = HashMap<Button, RemoteKey>;

/// One event from a remote, reduced to what matters here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raw {
    Scan(u32),
    /// `value`: 1 press, 2 autorepeat, 0 release.
    Key { code: u16, value: i32 },
    /// End of a frame: everything since the last one happened together.
    Sync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    pub button: Button,
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    button: Button,
    pressed: Instant,
    seen: Instant,
    emitted: Instant,
}

/// Follows a button through a hold, for sources that keep resending it with no release.
#[derive(Debug, Default)]
pub struct HoldTimer {
    held: Option<Held>,
}

impl HoldTimer {
    /// The button was seen. `Some(repeat)` when it should act now.
    pub fn seen(&mut self, button: Button, now: Instant) -> Option<bool> {
        match &mut self.held {
            Some(h) if h.button == button && now.duration_since(h.seen) <= HOLD_GAP => {
                h.seen = now;
                let due = now.duration_since(h.pressed) >= REPEAT_DELAY && now.duration_since(h.emitted) >= REPEAT_PERIOD;
                due.then(|| {
                    h.emitted = now;
                    true
                })
            }
            _ => {
                self.held = Some(Held { button, pressed: now, seen: now, emitted: now });
                Some(false)
            }
        }
    }
}

/// Turns one device's raw events into presses.
///
/// A learned scancode wins over any key the kernel also reports for it, including that key's
/// autorepeats, so one button never acts twice. Anything else comes through as the kernel's
/// key, which keeps existing remotes exactly as they were.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Only rc-core devices have stable scancodes worth using.
    rc: bool,
    scan: Option<u32>,
    keys: Vec<(u16, i32)>,
    held: HoldTimer,
    suppressed: Option<u16>,
}

impl Decoder {
    pub fn new(rc: bool) -> Self {
        Self { rc, ..Self::default() }
    }

    /// Feeds one event. `learned` says whether a button should be taken over by its scancode.
    pub fn feed(&mut self, raw: Raw, now: Instant, learned: impl Fn(Button) -> bool) -> Vec<Press> {
        match raw {
            Raw::Scan(s) => {
                if self.rc {
                    self.scan = Some(s);
                }
                Vec::new()
            }
            Raw::Key { code, value } => {
                self.keys.push((code, value));
                Vec::new()
            }
            Raw::Sync => self.frame(now, learned),
        }
    }

    fn frame(&mut self, now: Instant, learned: impl Fn(Button) -> bool) -> Vec<Press> {
        let mut out = Vec::new();
        let keys = std::mem::take(&mut self.keys);
        // The kernel releases the previous key in the same frame as the next button's scancode.
        for &(code, value) in &keys {
            if value == 0 && self.suppressed == Some(code) {
                self.suppressed = None;
            }
        }
        if let Some(scan) = self.scan.take() {
            let repeat = self.held.seen(Button::Scan(scan), now);
            if learned(Button::Scan(scan)) {
                if let Some(&(code, _)) = keys.iter().find(|k| k.1 == 1) {
                    self.suppressed = Some(code);
                }
                if let Some(repeat) = repeat {
                    out.push(Press { button: Button::Scan(scan), repeat });
                }
            }
        }
        for (code, value) in keys {
            if value != 0 && self.suppressed != Some(code) {
                out.push(Press { button: Button::Key(code), repeat: value == 2 });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: u16 = KeyCode::KEY_OK.code();

    /// Sends a frame `ms` after the start and returns the presses it produced.
    fn frame(d: &mut Decoder, t0: Instant, ms: u64, events: &[Raw], learned: &[u32]) -> Vec<Press> {
        let now = t0 + Duration::from_millis(ms);
        let is_learned = |b: Button| matches!(b, Button::Scan(s) if learned.contains(&s));
        let mut out = Vec::new();
        for e in events.iter().copied().chain([Raw::Sync]) {
            out.extend(d.feed(e, now, is_learned));
        }
        out
    }

    fn scan(s: u32, repeat: bool) -> Press {
        Press { button: Button::Scan(s), repeat }
    }

    #[test]
    fn button_text_round_trips() {
        for b in [Button::Scan(0x800f0422), Button::Scan(0), Button::Key(OK)] {
            assert_eq!(b.to_string().parse(), Ok(b), "{b}");
        }
        assert_eq!(Button::Scan(0x800f0422).to_string(), "scan:800f0422");
        assert_eq!(Button::Key(OK).to_string(), "key:KEY_OK");
        let ir = Button::Ir { protocol: Protocol::Rc6, address: 0x800f, command: 0x22 };
        assert_eq!(ir.to_string(), "ir:rc6:800f:22");
        assert_eq!(ir.to_string().parse(), Ok(ir));
        for bad in ["", "scan", "scan:xyz", "key:NOT_A_KEY", "other:1", "ir:rc6:1", "ir:nope:1:2", "ir:nec:1:2:3"] {
            assert!(bad.parse::<Button>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_tap_acts_once_even_when_the_remote_sends_two_frames() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7)], &[7]), [scan(7, false)]);
        assert_eq!(frame(&mut d, t, 110, &[Raw::Scan(7)], &[7]), []);
    }

    #[test]
    fn a_hold_repeats_after_the_delay_at_the_kernels_rate() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        let mut acted = Vec::new();
        for ms in (0..=1000).step_by(100) {
            if !frame(&mut d, t, ms, &[Raw::Scan(7)], &[7]).is_empty() {
                acted.push(ms);
            }
        }
        // 100 ms frames can't repeat faster than every other frame at a 125 ms period.
        assert_eq!(acted, [0, 500, 700, 900]);
    }

    #[test]
    fn a_gap_or_another_button_is_a_new_press() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7)], &[7, 8]), [scan(7, false)]);
        assert_eq!(frame(&mut d, t, 400, &[Raw::Scan(7)], &[7, 8]), [scan(7, false)]);
        assert_eq!(frame(&mut d, t, 450, &[Raw::Scan(8)], &[7, 8]), [scan(8, false)]);
    }

    #[test]
    fn an_unlearned_scancode_with_no_key_does_nothing() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7)], &[]), []);
    }

    #[test]
    fn an_unlearned_button_keeps_the_kernels_key() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        let press = Press { button: Button::Key(OK), repeat: false };
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7), Raw::Key { code: OK, value: 1 }], &[]), [press]);
        let repeat = Press { button: Button::Key(OK), repeat: true };
        assert_eq!(frame(&mut d, t, 500, &[Raw::Key { code: OK, value: 2 }], &[]), [repeat]);
        assert_eq!(frame(&mut d, t, 700, &[Raw::Key { code: OK, value: 0 }], &[]), []);
    }

    #[test]
    fn a_learned_scancode_silences_its_kernel_key_until_release() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        let down = [Raw::Scan(7), Raw::Key { code: OK, value: 1 }];
        assert_eq!(frame(&mut d, t, 0, &down, &[7]), [scan(7, false)]);
        assert_eq!(frame(&mut d, t, 500, &[Raw::Key { code: OK, value: 2 }], &[7]), []);
        assert_eq!(frame(&mut d, t, 700, &[Raw::Key { code: OK, value: 0 }], &[7]), []);
        // Released: the key works again if the scancode is no longer learned.
        let press = Press { button: Button::Key(OK), repeat: false };
        assert_eq!(frame(&mut d, t, 1000, &[Raw::Key { code: OK, value: 1 }], &[7]), [press]);
    }

    #[test]
    fn the_next_button_releases_the_previous_ones_key_in_one_frame() {
        let (t, mut d) = (Instant::now(), Decoder::new(true));
        let up = KeyCode::KEY_UP.code();
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7), Raw::Key { code: OK, value: 1 }], &[7]), [scan(7, false)]);
        let next = [Raw::Key { code: OK, value: 0 }, Raw::Scan(8), Raw::Key { code: up, value: 1 }];
        assert_eq!(frame(&mut d, t, 100, &next, &[7]), [Press { button: Button::Key(up), repeat: false }]);
    }

    #[test]
    fn scancodes_are_ignored_on_other_devices() {
        // USB keyboards and HID remotes report HID usages as MSC_SCAN too.
        let (t, mut d) = (Instant::now(), Decoder::new(false));
        let press = Press { button: Button::Key(OK), repeat: false };
        assert_eq!(frame(&mut d, t, 0, &[Raw::Scan(7), Raw::Key { code: OK, value: 1 }], &[7]), [press]);
    }
}
