//! Finds an IR remote's carrier in ADC samples from a bare photodiode. Shared by the Pico
//! firmware and the tests here, so it runs on a Cortex-M0+: no floats, no allocation.

#![cfg_attr(not(test), no_std)]

mod decode;

pub use decode::{decode, fingerprint, Command, Frame, Protocol, MAX_TIMINGS};

/// ADC samples per second.
pub const SAMPLE_RATE: u32 = 250_000;
/// Samples per block: 64 µs, short against the 200 µs pulses some remotes use, and wide enough in
/// frequency to take 36–40 kHz remotes.
pub const BLOCK: usize = 16;
/// 2·cos(2π·38 kHz / 250 kHz) in Q14.
const COEF: i32 = 18_930;

/// Energy near 38 kHz in one block. Removing the block's average first takes out steady light.
pub fn block_power(samples: &[u16; BLOCK]) -> u64 {
    let sum: u32 = samples.iter().map(|s| u32::from(*s)).sum();
    let mean = (sum / BLOCK as u32) as i32;
    let (mut s1, mut s2) = (0i32, 0i32);
    for sample in samples {
        // Goertzel: a 12-bit input over 32 steps stays well inside i32 after the multiply.
        let s = i32::from(*sample) - mean + ((COEF * s1) >> 14) - s2;
        s2 = s1;
        s1 = s;
    }
    let (a, b) = (i64::from(s1), i64::from(s2));
    let power = a * a + b * b - ((i64::from(COEF) * a * b) >> 14);
    power.max(0) as u64
}

/// Converts a block's power to the carrier's peak height in ADC counts, for reporting.
pub fn amplitude(power: u64) -> u32 {
    isqrt(power) * 2 / BLOCK as u32
}

fn isqrt(n: u64) -> u32 {
    let (mut root, mut bit) = (0u64, 1u64 << 62);
    let mut n = n;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if n >= root + bit {
            n -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root as u32
}

/// Signal level over a stretch of blocks.
#[derive(Debug, Clone)]
pub struct Stats {
    /// Blocks by power: bucket `n` holds powers with `n` significant bits.
    histogram: [u32; 65],
    blocks: u32,
    peak: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Self { histogram: [0; 65], blocks: 0, peak: 0 }
    }
}

impl Stats {
    pub fn add(&mut self, power: u64) {
        self.histogram[(64 - power.leading_zeros()) as usize] += 1;
        self.blocks += 1;
        self.peak = self.peak.max(power);
    }

    /// Strongest carrier seen, in ADC counts.
    pub fn peak(&self) -> u32 {
        amplitude(self.peak)
    }

    /// Typical level with no carrier, in ADC counts, rounded up to a power-of-two bucket. A
    /// remote transmits under half the time, so the lower quarter of blocks is background.
    pub fn floor(&self) -> u32 {
        let mut seen = 0;
        for (bits, count) in self.histogram.iter().enumerate() {
            seen += count;
            if seen * 4 >= self.blocks {
                return amplitude((1u128 << bits).saturating_sub(1).min(u128::from(u64::MAX)) as u64);
            }
        }
        0
    }
}

/// The quietest carrier treated as a signal, in ADC counts. With nothing happening, blocks
/// measured up to about 13 on the real circuit.
const MIN_CARRIER: u32 = 20;
/// A block overlapping the start of a burst reads as carrier, so marks measure this much long.
const MARK_BIAS_US: u32 = 40;
/// Silence this long ends a transmission. Some remotes pause over 13 ms between bursts of one
/// press, and that must count as one transmission, not several buttons.
const FRAME_GAP_US: u32 = 18_000;
/// Longer than any real mark: interference, not a remote.
const MAX_MARK_US: u32 = 15_000;
/// NEC's repeat code only means anything this soon after a command.
const REPEAT_WINDOW_US: u32 = 200_000;

/// Inverse of [`amplitude`].
const fn power_of(counts: u32) -> u64 {
    let root = (counts * BLOCK as u32 / 2) as u64;
    root * root
}

/// A decoded button press from the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    pub command: Command,
    pub repeat: bool,
}

/// Follows the signal block by block, and reports each command as its frame ends.
pub struct Receiver {
    /// Background power, followed while no carrier is present.
    noise: u64,
    carrier: bool,
    /// Strongest block of the current burst; the burst ends well below it.
    burst_peak: u64,
    run_us: u32,
    timings: [u32; MAX_TIMINGS],
    count: usize,
    overflow: bool,
    last: Option<Command>,
    since_last_us: u32,
    /// DIAGNOSTIC: length of the last frame that didn't decode, until taken.
    undecoded: usize,
    kept: [u32; MAX_TIMINGS],
}

impl Default for Receiver {
    fn default() -> Self {
        Self {
            noise: 0,
            carrier: false,
            burst_peak: 0,
            run_us: 0,
            timings: [0; MAX_TIMINGS],
            count: 0,
            overflow: false,
            last: None,
            since_last_us: 0,
            undecoded: 0,
            kept: [0; MAX_TIMINGS],
        }
    }
}

impl Receiver {
    /// Adds one block: its power, and the time it covers.
    pub fn block(&mut self, power: u64, us: u32) -> Option<Received> {
        self.since_last_us = self.since_last_us.saturating_add(us);
        self.run_us = self.run_us.saturating_add(us);
        if self.carrier {
            self.burst_peak = self.burst_peak.max(power);
            if power < (self.burst_peak / 16).max(self.noise * 4).max(power_of(MIN_CARRIER) / 4) {
                let mark = self.run_us.saturating_sub(us + MARK_BIAS_US);
                self.record(mark);
                self.carrier = false;
                self.run_us = us;
            } else if self.run_us > MAX_MARK_US {
                // Something bright and steady at this frequency: treat it as background.
                self.noise = power;
                self.carrier = false;
                self.reset_frame();
                self.run_us = 0;
            }
            return None;
        }
        if power >= (self.noise * 16).max(power_of(MIN_CARRIER)) {
            if self.count > 0 {
                self.record(self.run_us.saturating_sub(us) + MARK_BIAS_US);
            }
            self.carrier = true;
            self.burst_peak = power;
            self.run_us = us;
            return None;
        }
        // A slow average, so a burst's edge can't lift the threshold much.
        self.noise = self.noise - self.noise / 256 + power / 256;
        if self.count > 0 && self.run_us >= FRAME_GAP_US {
            return self.finish();
        }
        None
    }

    /// DIAGNOSTIC: the last frame that didn't decode.
    pub fn take_undecoded(&mut self) -> Option<&[u32]> {
        let n = core::mem::take(&mut self.undecoded);
        (n > 0).then(|| &self.kept[..n])
    }

    fn record(&mut self, us: u32) {
        match self.timings.get_mut(self.count) {
            Some(slot) => {
                *slot = us;
                self.count += 1;
            }
            None => self.overflow = true,
        }
    }

    fn reset_frame(&mut self) {
        self.count = 0;
        self.overflow = false;
    }

    fn finish(&mut self) -> Option<Received> {
        let timings = &self.timings[..self.count];
        // A long transmission is identified by its beginning, which is what always arrives whole.
        let frame = if self.overflow { None } else { decode(timings) }.or_else(|| fingerprint(timings).map(Frame::Command));
        if frame.is_none() {
            self.kept = self.timings;
            self.undecoded = self.count;
        }
        self.reset_frame();
        match frame? {
            Frame::Command(command) => {
                let repeat = self.last == Some(command) && self.since_last_us <= REPEAT_WINDOW_US;
                self.last = Some(command);
                self.since_last_us = 0;
                Some(Received { command, repeat })
            }
            Frame::NecRepeat if self.since_last_us <= REPEAT_WINDOW_US => {
                self.since_last_us = 0;
                self.last.map(|command| Received { command, repeat: true })
            }
            Frame::NecRepeat => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const MID: f64 = 1000.0;

    /// A sine of `height` counts at `hz`, on a steady level, starting at `phase`.
    fn sine(hz: f64, height: f64, phase: f64) -> [u16; BLOCK] {
        core::array::from_fn(|i| {
            (MID + height * (TAU * hz * i as f64 / f64::from(SAMPLE_RATE) + phase).sin()) as u16
        })
    }

    #[test]
    fn measures_a_carrier_about_at_its_height_at_any_phase() {
        for phase in [0.0, 1.0, 2.0, 3.0, 4.0, 5.0] {
            let found = amplitude(block_power(&sine(38_000.0, 100.0, phase)));
            assert!((80..=120).contains(&found), "phase {phase}: {found}");
        }
    }

    #[test]
    fn takes_remotes_from_36_to_40_khz() {
        for hz in [36_000.0, 40_000.0] {
            let found = amplitude(block_power(&sine(hz, 100.0, 0.5)));
            assert!(found >= 50, "{hz} Hz: {found}");
        }
    }

    #[test]
    fn ignores_steady_light() {
        assert_eq!(block_power(&[1000; BLOCK]), 0);
    }

    #[test]
    fn rejects_slow_flicker() {
        // Mains lamps at 100 Hz, and an LED driver well below the carrier.
        for hz in [100.0, 1_000.0, 5_000.0] {
            for phase in [0.0, 1.5, 3.0] {
                let found = amplitude(block_power(&sine(hz, 100.0, phase)));
                assert!(found <= 15, "{hz} Hz: {found}");
            }
        }
    }

    #[test]
    fn full_scale_input_does_not_overflow() {
        let square = core::array::from_fn(|i| if (i * 38 / 125) % 2 == 0 { 4095 } else { 0 });
        assert!(amplitude(block_power(&square)) > 1000);
    }

    // ---- the whole path: light on the diode, through blocks and timings, to a command.

    /// Builds the ADC samples for a remote's transmission.
    struct Light {
        hz: f64,
        height: f64,
        /// Noise height in counts.
        noise: f64,
        /// Slow flicker added to the light: height and frequency.
        flicker: (f64, f64),
        t: f64,
        rng: u32,
    }

    impl Light {
        fn new(hz: f64, height: f64) -> Self {
            Self { hz, height, noise: 2.0, flicker: (0.0, 100.0), t: 0.0, rng: 0x1234_5678 }
        }

        fn random(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 17;
            self.rng ^= self.rng << 5;
            f64::from(self.rng) / f64::from(u32::MAX) * 2.0 - 1.0
        }

        /// Runs alternating mark/space timings through a receiver, ending with a long silence.
        fn play(&mut self, timings: &[u32], rx: &mut Receiver) -> Vec<Received> {
            // Edges: time each run ends.
            let mut runs = Vec::new();
            let mut at = 0.0;
            for (i, us) in timings.iter().enumerate() {
                at += f64::from(*us) / 1e6;
                runs.push((at, i % 2 == 0));
            }
            let total = at + 0.02;
            let start = self.t;
            let dt = 1.0 / f64::from(SAMPLE_RATE);
            let mut out = Vec::new();
            while self.t - start < total {
                let mut block = [0u16; BLOCK];
                for slot in &mut block {
                    let local = self.t - start;
                    let on = runs.iter().find(|r| local < r.0).is_some_and(|r| r.1);
                    let carrier = if on { self.height * (TAU * self.hz * self.t).sin() } else { 0.0 };
                    let flicker = self.flicker.0 * (TAU * self.flicker.1 * self.t).sin();
                    let value = MID + carrier + flicker + self.noise * self.random();
                    *slot = value.clamp(0.0, 4095.0) as u16;
                    self.t += dt;
                }
                let us = (BLOCK as u32 * 1_000_000) / SAMPLE_RATE;
                out.extend(rx.block(block_power(&block), us));
            }
            out
        }
    }

    fn pulse_distance(lead: (u32, u32), bits: u32) -> Vec<u32> {
        let mut t = vec![lead.0, lead.1];
        for i in 0..32 {
            t.extend([562, if bits >> i & 1 == 1 { 1687 } else { 562 }]);
        }
        t.push(562);
        t
    }

    /// Timings from a list of half-unit levels.
    fn from_levels(levels: &[bool], unit: u32, mut out: Vec<u32>) -> Vec<u32> {
        let mut run = 0;
        for (i, level) in levels.iter().enumerate() {
            run += unit;
            if levels.get(i + 1) != Some(level) {
                out.push(run);
                run = 0;
            }
        }
        // A frame ends on a mark; a trailing space can't be seen.
        if levels.last() == Some(&false) {
            out.pop();
        }
        out
    }

    fn rc5_frame(address: u32, command: u32, toggle: bool) -> Vec<u32> {
        let value = 0b11u32 << 12 | u32::from(toggle) << 11 | address << 6 | command;
        let mut levels = Vec::new();
        for i in (0..14).rev() {
            let bit = value >> i & 1 == 1;
            levels.extend([!bit, bit]);
        }
        // The opening space isn't part of what's seen.
        from_levels(&levels[1..], 889, Vec::new())
    }

    fn rc6_frame(mode: u32, data: u32, bits: u32, toggle: bool) -> Vec<u32> {
        let mut levels = Vec::new();
        let mut push = |bit: bool, width: usize| {
            levels.extend(std::iter::repeat_n(bit, width));
            levels.extend(std::iter::repeat_n(!bit, width));
        };
        push(true, 1);
        for i in (0..3).rev() {
            push(mode >> i & 1 == 1, 1);
        }
        push(toggle, 2);
        for i in (0..bits).rev() {
            push(data >> i & 1 == 1, 1);
        }
        from_levels(&levels, 444, vec![2666, 889])
    }

    fn sony_frame(address: u32, command: u32, address_bits: u32) -> Vec<u32> {
        let value = address << 7 | command;
        let mut t = vec![2400];
        for i in 0..7 + address_bits {
            t.extend([600, if value >> i & 1 == 1 { 1200 } else { 600 }]);
        }
        t
    }

    fn nec_bits(address: u32, command: u32) -> u32 {
        address | (address ^ 0xff) << 8 | command << 16 | (command ^ 0xff) << 24
    }

    /// Every protocol with the carrier it really uses.
    fn cases() -> Vec<(&'static str, f64, Vec<u32>, Command)> {
        let cmd = |protocol, address, command| Command { protocol, address, command };
        vec![
            ("nec", 38_000.0, pulse_distance((9000, 4500), nec_bits(0x04, 0x12)), cmd(Protocol::Nec, 0xfb04, 0x12)),
            ("samsung", 38_000.0, pulse_distance((4500, 4500), nec_bits(0x07, 0x02)), cmd(Protocol::Samsung, 0xf807, 0x02)),
            ("rc5", 36_000.0, rc5_frame(0x05, 0x2a, false), cmd(Protocol::Rc5, 0x05, 0x2a)),
            ("rc5 ending in 0", 36_000.0, rc5_frame(0x05, 0x2a & !1, false), cmd(Protocol::Rc5, 0x05, 0x2a & !1)),
            ("rc6 mode 0", 36_000.0, rc6_frame(0, 0x04_55, 16, false), cmd(Protocol::Rc6, 0x04, 0x55)),
            ("rc6 mode 6a", 36_000.0, rc6_frame(6, 0x800f_0422, 32, false), cmd(Protocol::Rc6, 0x800f, 0x0422)),
            ("sony 12", 40_000.0, sony_frame(0x01, 0x15, 5), cmd(Protocol::Sony, 0x01, 0x15)),
            ("sony 20", 40_000.0, sony_frame(0x1a5, 0x33, 13), cmd(Protocol::Sony, 0x1a5, 0x33)),
        ]
    }

    #[test]
    fn decodes_every_protocol_at_strong_and_weak_levels() {
        for (name, hz, timings, want) in cases() {
            for height in [450.0, 60.0, 25.0] {
                let mut rx = Receiver::default();
                let got = Light::new(hz, height).play(&timings, &mut rx);
                assert_eq!(got, [Received { command: want, repeat: false }], "{name} at {height}");
            }
        }
    }

    #[test]
    fn decodes_under_lamp_and_led_flicker() {
        for (name, hz, timings, want) in cases() {
            for flicker in [(300.0, 100.0), (100.0, 1000.0)] {
                let mut light = Light::new(hz, 200.0);
                light.flicker = flicker;
                let got = light.play(&timings, &mut Receiver::default());
                assert_eq!(got, [Received { command: want, repeat: false }], "{name} with {flicker:?}");
            }
        }
    }

    #[test]
    fn noise_and_flicker_alone_decode_to_nothing() {
        let mut light = Light::new(38_000.0, 0.0);
        light.noise = 4.0;
        light.flicker = (500.0, 100.0);
        let mut rx = Receiver::default();
        assert_eq!(light.play(&[2_000_000], &mut rx), []);
    }

    /// The pattern recorded from a real cable remote: short marks on a 0.5 ms grid, in three
    /// bursts split by gaps of 8–13 ms.
    fn cable_remote() -> Vec<u32> {
        let periods = [
            1488, 1520, 448, 1056, 1488, 448, 2032, 2496, 1536, 2496, 1504, 448, 2016, 1504, 2496, 8528, 12992, 11984,
            2496, 1504, 4000, 1488, 1504, 1520, 1472, 6512, 4000, 4000, 2496,
        ];
        let mut t = Vec::new();
        for p in periods {
            t.extend([230, p - 230]);
        }
        t.push(230);
        t
    }

    #[test]
    fn an_unknown_protocol_gets_one_stable_fingerprint_per_press() {
        let mut first = None;
        for (height, noise) in [(40.0, 2.0), (130.0, 2.0), (400.0, 4.0), (40.0, 4.0)] {
            let mut light = Light::new(38_000.0, height);
            light.noise = noise;
            let got = light.play(&cable_remote(), &mut Receiver::default());
            assert_eq!(got.len(), 1, "one press, one command, at height {height}");
            assert_eq!(got[0].command.protocol, Protocol::Pulses);
            assert_eq!(*first.get_or_insert(got[0].command), got[0].command, "at height {height}");
        }
        // Another button differs.
        let mut other = cable_remote();
        other[3] += 1000;
        let got = Light::new(38_000.0, 130.0).play(&other, &mut Receiver::default());
        assert_ne!(Some(got[0].command), first);
    }

    #[test]
    fn a_few_stray_pulses_are_not_a_button() {
        let got = Light::new(38_000.0, 200.0).play(&[230, 1000, 230, 2000, 230], &mut Receiver::default());
        assert_eq!(got, []);
    }

    #[test]
    fn a_toggle_bit_does_not_change_the_command() {
        let press = |timings: Vec<u32>| Light::new(36_000.0, 200.0).play(&timings, &mut Receiver::default())[0].command;
        assert_eq!(press(rc5_frame(5, 9, false)), press(rc5_frame(5, 9, true)));
        assert_eq!(press(rc6_frame(0, 0x0455, 16, false)), press(rc6_frame(0, 0x0455, 16, true)));
        assert_eq!(press(rc6_frame(6, 0x800f_0422, 32, false)), press(rc6_frame(6, 0x800f_8422, 32, false)));
    }

    #[test]
    fn nec_repeat_codes_repeat_the_last_command_only_soon_after_it() {
        let mut rx = Receiver::default();
        let mut light = Light::new(38_000.0, 200.0);
        let repeat_code = [9000, 2250, 562];
        // With nothing before it, a repeat code means nothing.
        assert_eq!(light.play(&repeat_code, &mut rx), []);
        let frame = pulse_distance((9000, 4500), nec_bits(0x04, 0x12));
        let first = light.play(&frame, &mut rx);
        assert_eq!(first[0].repeat, false);
        let again = light.play(&repeat_code, &mut rx);
        assert_eq!(again, [Received { command: first[0].command, repeat: true }]);
        // Long after: ignored.
        light.play(&[300_000], &mut rx);
        assert_eq!(light.play(&repeat_code, &mut rx), []);
    }

    #[test]
    fn a_frame_too_long_for_any_protocol_is_dropped() {
        let timings: Vec<u32> = vec![600; 301];
        let mut rx = Receiver::default();
        let got = Light::new(38_000.0, 200.0).play(&timings, &mut rx);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].command.protocol, Protocol::Pulses);
        // And the next real one still works.
        let (_, hz, timings, want) = cases().remove(0);
        assert_eq!(Light::new(hz, 200.0).play(&timings, &mut rx)[0].command, want);
    }

    #[test]
    fn a_steady_interfering_tone_is_learned_as_background() {
        let mut rx = Receiver::default();
        let mut light = Light::new(38_000.0, 300.0);
        assert_eq!(light.play(&[100_000], &mut rx), []);
    }

    #[test]
    fn stats_separate_a_carrier_from_the_background() {
        let mut stats = Stats::default();
        for i in 0..100 {
            let height = if i % 3 == 0 { 200.0 } else { 2.0 };
            stats.add(block_power(&sine(38_000.0, height, 0.5)));
        }
        assert!((160..=240).contains(&stats.peak()), "peak {}", stats.peak());
        assert!(stats.floor() < 10, "floor {}", stats.floor());
        assert_eq!(Stats::default().peak(), 0);
    }
}
