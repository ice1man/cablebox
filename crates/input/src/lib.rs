//! Unified remote-control keys. Keyboard, IR (evdev) and HDMI-CEC all map onto [`RemoteKey`].

mod keyboard;
pub mod control;
pub mod learn;
pub mod pico;
pub mod remote;

pub use keyboard::from_key_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteKey {
    Digit(u8),
    /// Minor-channel separator (`5.1`).
    Dot,
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Exit,
    Guide,
    Info,
    Menu,
    ChannelUp,
    ChannelDown,
    /// Jump to the previously watched channel.
    Last,
    Record,
    PlayPause,
    Stop,
    FastForward,
    Rewind,
    VolumeUp,
    VolumeDown,
    Mute,
    Red,
    Green,
    Yellow,
    Blue,
    /// Opens the recordings library.
    Dvr,
    PageUp,
    PageDown,
}

impl RemoteKey {
    /// Every key, in the order they're taught when learning a remote: the keys needed to
    /// operate the menus first.
    pub const ALL: [Self; 39] = {
        use RemoteKey::*;
        [
            Ok, Up, Down, Left, Right, Back, Menu, Guide, Info, Exit, ChannelUp, ChannelDown,
            Digit(1), Digit(2), Digit(3), Digit(4), Digit(5), Digit(6), Digit(7), Digit(8), Digit(9), Digit(0),
            Dot, Last, VolumeUp, VolumeDown, Mute, Record, PlayPause, Stop, FastForward, Rewind, Dvr,
            Red, Green, Yellow, Blue, PageUp, PageDown,
        ]
    };

    /// The canonical name, which [`from_name`](Self::from_name) reads back.
    pub fn name(self) -> &'static str {
        use RemoteKey::*;
        match self {
            Digit(d) => ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"][usize::from(d % 10)],
            Dot => "dot",
            Up => "up",
            Down => "down",
            Left => "left",
            Right => "right",
            Ok => "ok",
            Back => "back",
            Exit => "exit",
            Guide => "guide",
            Info => "info",
            Menu => "menu",
            ChannelUp => "ch+",
            ChannelDown => "ch-",
            Last => "last",
            Record => "record",
            PlayPause => "playpause",
            Stop => "stop",
            FastForward => "ff",
            Rewind => "rew",
            VolumeUp => "vol+",
            VolumeDown => "vol-",
            Mute => "mute",
            Red => "red",
            Green => "green",
            Yellow => "yellow",
            Blue => "blue",
            Dvr => "dvr",
            PageUp => "pageup",
            PageDown => "pagedown",
        }
    }

    /// How the key is named on screen.
    pub fn label(self) -> &'static str {
        use RemoteKey::*;
        match self {
            Digit(_) => self.name(),
            Dot => "Dot (.)",
            Up => "Up",
            Down => "Down",
            Left => "Left",
            Right => "Right",
            Ok => "OK",
            Back => "Back",
            Exit => "Exit",
            Guide => "Guide",
            Info => "Info",
            Menu => "Menu",
            ChannelUp => "Channel up",
            ChannelDown => "Channel down",
            Last => "Last channel",
            Record => "Record",
            PlayPause => "Play/Pause",
            Stop => "Stop",
            FastForward => "Fast forward",
            Rewind => "Rewind",
            VolumeUp => "Volume up",
            VolumeDown => "Volume down",
            Mute => "Mute",
            Red => "Red",
            Green => "Green",
            Yellow => "Yellow",
            Blue => "Blue",
            Dvr => "Recordings",
            PageUp => "Page up",
            PageDown => "Page down",
        }
    }

    /// Parses a key name as used by the control FIFO (`guide`, `ch+`, `7`, `ok`, ...).
    pub fn from_name(name: &str) -> Option<Self> {
        use RemoteKey::*;
        let name = name.trim().to_ascii_lowercase();
        if let [d @ b'0'..=b'9'] = name.as_bytes() {
            return Some(Digit(d - b'0'));
        }
        Some(match name.as_str() {
            "." | "dot" => Dot,
            "up" => Up,
            "down" => Down,
            "left" => Left,
            "right" => Right,
            "ok" | "select" | "enter" => Ok,
            "back" => Back,
            "exit" => Exit,
            "guide" => Guide,
            "info" => Info,
            "menu" => Menu,
            "ch+" | "chup" | "channelup" => ChannelUp,
            "ch-" | "chdown" | "channeldown" => ChannelDown,
            "last" => Last,
            "record" | "rec" => Record,
            "play" | "pause" | "playpause" => PlayPause,
            "stop" => Stop,
            "ff" | "fastforward" => FastForward,
            "rew" | "rewind" => Rewind,
            "vol+" => VolumeUp,
            "vol-" => VolumeDown,
            "mute" => Mute,
            "red" => Red,
            "green" => Green,
            "yellow" => Yellow,
            "blue" => Blue,
            "dvr" | "recordings" => Dvr,
            "pageup" => PageUp,
            "pagedown" => PageDown,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteKey;

    #[test]
    fn names_round_trip_for_every_key() {
        let mut seen = std::collections::HashSet::new();
        for k in RemoteKey::ALL {
            assert_eq!(RemoteKey::from_name(k.name()), Some(k), "{k:?}");
            assert!(seen.insert(k), "{k:?} listed twice");
        }
    }
}
