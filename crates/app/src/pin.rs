//! Optional settings PIN: checks typed PINs and locks out guessing from the remote.

use std::time::{Duration, Instant};

pub const LEN: usize = 4;
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    Ok,
    Wrong { remaining: u32 },
    /// Too many wrong guesses; try again after this long.
    Locked(Duration),
}

#[derive(Debug, Default)]
pub struct PinGate {
    failures: u32,
    locked_until: Option<Instant>,
}

impl PinGate {
    /// Remaining lockout, if any.
    pub fn locked(&self, now: Instant) -> Option<Duration> {
        self.locked_until.filter(|&u| u > now).map(|u| u - now)
    }

    pub fn check(&mut self, expected: &str, entered: &str, now: Instant) -> Check {
        if let Some(left) = self.locked(now) {
            return Check::Locked(left);
        }
        if entered == expected {
            self.failures = 0;
            self.locked_until = None;
            return Check::Ok;
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            self.failures = 0;
            self.locked_until = Some(now + LOCKOUT);
            return Check::Locked(LOCKOUT);
        }
        Check::Wrong { remaining: MAX_FAILURES - self.failures }
    }
}

/// `"●  ●  ○  ○"` for two typed digits.
pub fn mask(entered: &str) -> String {
    (0..LEN).map(|i| if i < entered.len() { "●" } else { "○" }).collect::<Vec<_>>().join("  ")
}

pub fn is_valid(pin: &str) -> bool {
    pin.len() == LEN && pin.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_pin_resets_failures() {
        let t = Instant::now();
        let mut g = PinGate::default();
        assert_eq!(g.check("1234", "0000", t), Check::Wrong { remaining: 4 });
        assert_eq!(g.check("1234", "1234", t), Check::Ok);
        assert_eq!(g.check("1234", "0000", t), Check::Wrong { remaining: 4 });
    }

    #[test]
    fn locks_out_after_five_failures() {
        let t = Instant::now();
        let mut g = PinGate::default();
        for left in (1..5).rev() {
            assert_eq!(g.check("1234", "9999", t), Check::Wrong { remaining: left });
        }
        assert_eq!(g.check("1234", "9999", t), Check::Locked(LOCKOUT));
        // Even the right PIN is refused while locked.
        assert!(matches!(g.check("1234", "1234", t + Duration::from_secs(30)), Check::Locked(_)));
        assert_eq!(g.check("1234", "1234", t + LOCKOUT), Check::Ok);
    }

    #[test]
    fn masks_and_validates() {
        assert_eq!(mask("12"), "●  ●  ○  ○");
        assert_eq!(mask(""), "○  ○  ○  ○");
        assert!(is_valid("0420"));
        assert!(!is_valid("123"));
        assert!(!is_valid("12a4"));
    }
}
