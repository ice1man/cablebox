use crate::RemoteKey::{self, *};

// Private-use code points Slint (and many toolkits) use for special keys.
const BACKSPACE: char = '\u{8}';
const TAB: char = '\t';
const RETURN: char = '\n';
const ESCAPE: char = '\u{1b}';
const UP: char = '\u{F700}';
const DOWN: char = '\u{F701}';
const LEFT: char = '\u{F702}';
const RIGHT: char = '\u{F703}';
const F1: char = '\u{F704}';
const F2: char = '\u{F705}';
const F3: char = '\u{F706}';
const F4: char = '\u{F707}';
const F5: char = '\u{F708}';
const HOME: char = '\u{F729}';
const END: char = '\u{F72B}';
const PAGE_UP: char = '\u{F72C}';
const PAGE_DOWN: char = '\u{F72D}';

/// Maps the text of a toolkit key event to a remote key.
///
/// Layout for keyboard use: digits and `.` for channels, arrows/Enter/Esc to navigate,
/// `g` guide, `i` info, `m` menu, `d` recordings, `r` record, PgUp/PgDn channel,
/// Backspace last channel, Space play/pause, `[`/`]` rewind/ff, `-`/`=` volume, F1–F4 colours.
pub fn from_key_text(text: &str) -> Option<RemoteKey> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match c {
        '0'..='9' => Digit(c as u8 - b'0'),
        '.' => Dot,
        UP => Up,
        DOWN => Down,
        LEFT => Left,
        RIGHT => Right,
        RETURN | '\r' => Ok,
        ESCAPE => Back,
        TAB => Info,
        BACKSPACE => Last,
        PAGE_UP => ChannelUp,
        PAGE_DOWN => ChannelDown,
        HOME => PageUp,
        END => PageDown,
        F1 => Red,
        F2 => Green,
        F3 => Yellow,
        F4 => Blue,
        F5 => Menu,
        ' ' => PlayPause,
        '[' => Rewind,
        ']' => FastForward,
        '-' => VolumeDown,
        '=' | '+' => VolumeUp,
        _ => match c.to_ascii_lowercase() {
            'g' => Guide,
            'i' => Info,
            'm' => Menu,
            'd' => Dvr,
            'r' => Record,
            's' => Stop,
            'l' => Last,
            'x' => Mute,
            'q' => Exit,
            'f' => Yellow,
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_keys() {
        assert_eq!(from_key_text("7"), Some(Digit(7)));
        assert_eq!(from_key_text("."), Some(Dot));
        assert_eq!(from_key_text("\u{F700}"), Some(Up));
        assert_eq!(from_key_text("\n"), Some(Ok));
        assert_eq!(from_key_text("G"), Some(Guide));
        assert_eq!(from_key_text("\u{F72C}"), Some(ChannelUp));
        assert_eq!(from_key_text("ab"), None);
        assert_eq!(from_key_text(""), None);
        assert_eq!(from_key_text("z"), None);
    }
}
