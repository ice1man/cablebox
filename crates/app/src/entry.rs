//! Cable-box style channel-number entry: digits accumulate in an on-screen buffer and
//! tune on OK, after a pause, or as soon as no longer number is possible.

use std::time::{Duration, Instant};

use tvh::ChannelNumber;

pub const TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug, Default)]
pub struct ChannelEntry {
    major: String,
    minor: Option<String>,
    last_input: Option<Instant>,
    /// Width of the longest major number in the lineup (auto-commit threshold).
    max_major: usize,
    max_minor: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    /// Still typing; show this text.
    Pending(String),
    /// Tune to this number now.
    Commit(ChannelNumber),
}

impl ChannelEntry {
    /// `max_major`/`max_minor` are digit widths derived from the lineup.
    pub fn set_limits(&mut self, max_major: usize, max_minor: usize) {
        self.max_major = max_major.clamp(1, 5);
        self.max_minor = max_minor.min(3);
    }

    /// What to show on screen; empty when not typing.
    pub fn display_text(&self) -> String {
        if self.is_active() {
            self.display()
        } else {
            String::new()
        }
    }

    pub fn is_active(&self) -> bool {
        self.last_input.is_some()
    }

    pub fn digit(&mut self, d: u8, now: Instant) -> Entry {
        debug_assert!(d < 10);
        let c = char::from(b'0' + d);
        match &mut self.minor {
            Some(minor) => {
                minor.push(c);
                if self.max_minor > 0 && minor.len() >= self.max_minor {
                    return self.commit().map_or_else(|| Entry::Pending(String::new()), Entry::Commit);
                }
            }
            None => {
                // A leading zero never starts a number; ignore it like real boxes do.
                if self.major.is_empty() && d == 0 && self.max_major > 1 {
                    return Entry::Pending(self.display());
                }
                self.major.push(c);
                if self.major.len() >= self.max_major && self.max_minor == 0 {
                    return self.commit().map_or_else(|| Entry::Pending(String::new()), Entry::Commit);
                }
            }
        }
        self.last_input = Some(now);
        Entry::Pending(self.display())
    }

    /// Starts the minor part (`5.` → `5.1`). Ignored without a major number or if already typing a minor.
    pub fn dot(&mut self, now: Instant) -> Option<String> {
        if self.major.is_empty() || self.minor.is_some() {
            return None;
        }
        self.minor = Some(String::new());
        self.last_input = Some(now);
        Some(self.display())
    }

    /// Commits whatever has been typed.
    pub fn commit(&mut self) -> Option<ChannelNumber> {
        let text = match &self.minor {
            Some(m) if !m.is_empty() => format!("{}.{}", self.major, m),
            _ => self.major.clone(),
        };
        self.cancel();
        if text.is_empty() {
            None
        } else {
            ChannelNumber::parse(&text)
        }
    }

    /// Commits if the user paused long enough.
    pub fn tick(&mut self, now: Instant) -> Option<ChannelNumber> {
        match self.last_input {
            Some(t) if now.duration_since(t) >= TIMEOUT => self.commit(),
            _ => None,
        }
    }

    /// Removes the last typed character (Back while typing). Returns the new display, empty if cleared.
    pub fn backspace(&mut self, now: Instant) -> String {
        match &mut self.minor {
            Some(m) if !m.is_empty() => {
                m.pop();
            }
            Some(_) => self.minor = None,
            None => {
                self.major.pop();
            }
        }
        if self.major.is_empty() {
            self.cancel();
            return String::new();
        }
        self.last_input = Some(now);
        self.display()
    }

    pub fn cancel(&mut self) {
        self.major.clear();
        self.minor = None;
        self.last_input = None;
    }

    /// `"1 2 _"` while typing the major number, `"5.1"` once a minor part is started.
    fn display(&self) -> String {
        match &self.minor {
            Some(m) => format!("{}.{}{}", self.major, m, "_".repeat(self.max_minor.saturating_sub(m.len()).min(1))),
            None => {
                let mut cells: Vec<String> = self.major.chars().map(String::from).collect();
                while cells.len() < self.max_major {
                    cells.push("_".into());
                }
                cells.join(" ")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(max_major: usize, max_minor: usize) -> ChannelEntry {
        let mut e = ChannelEntry::default();
        e.set_limits(max_major, max_minor);
        e
    }

    #[test]
    fn auto_commits_at_full_width() {
        let t = Instant::now();
        let mut e = entry(3, 0);
        assert_eq!(e.digit(1, t), Entry::Pending("1 _ _".into()));
        assert_eq!(e.digit(2, t), Entry::Pending("1 2 _".into()));
        assert_eq!(e.digit(5, t), Entry::Commit(ChannelNumber::new(125, 0)));
        assert!(!e.is_active());
    }

    #[test]
    fn commits_after_timeout() {
        let t = Instant::now();
        let mut e = entry(3, 0);
        e.digit(7, t);
        assert_eq!(e.tick(t + Duration::from_millis(1999)), None);
        assert_eq!(e.tick(t + TIMEOUT), Some(ChannelNumber::new(7, 0)));
        assert_eq!(e.tick(t + TIMEOUT * 2), None);
    }

    #[test]
    fn timeout_restarts_on_each_digit() {
        let t = Instant::now();
        let mut e = entry(4, 0);
        e.digit(1, t);
        e.digit(2, t + Duration::from_millis(1500));
        assert_eq!(e.tick(t + Duration::from_millis(2500)), None);
        assert_eq!(e.tick(t + Duration::from_millis(3500)), Some(ChannelNumber::new(12, 0)));
    }

    #[test]
    fn ok_commits_early() {
        let t = Instant::now();
        let mut e = entry(3, 0);
        e.digit(4, t);
        assert_eq!(e.commit(), Some(ChannelNumber::new(4, 0)));
        assert_eq!(e.commit(), None);
    }

    #[test]
    fn minor_numbers() {
        let t = Instant::now();
        let mut e = entry(2, 1);
        // With minors in the lineup, a full major doesn't auto-commit.
        assert_eq!(e.digit(1, t), Entry::Pending("1 _".into()));
        assert_eq!(e.digit(3, t), Entry::Pending("1 3".into()));
        assert_eq!(e.dot(t), Some("13._".into()));
        assert_eq!(e.digit(2, t), Entry::Commit(ChannelNumber::new(13, 2)));
    }

    #[test]
    fn dot_without_major_is_ignored() {
        let t = Instant::now();
        let mut e = entry(2, 1);
        assert_eq!(e.dot(t), None);
        assert!(!e.is_active());
    }

    #[test]
    fn leading_zero_ignored() {
        let t = Instant::now();
        let mut e = entry(3, 0);
        assert_eq!(e.digit(0, t), Entry::Pending("_ _ _".into()));
        assert!(!e.is_active());
        e.digit(0, t);
        e.digit(9, t);
        assert_eq!(e.commit(), Some(ChannelNumber::new(9, 0)));
    }

    #[test]
    fn backspace() {
        let t = Instant::now();
        let mut e = entry(3, 0);
        e.digit(1, t);
        e.digit(2, t);
        assert_eq!(e.backspace(t), "1 _ _");
        assert_eq!(e.backspace(t), "");
        assert!(!e.is_active());
    }
}
