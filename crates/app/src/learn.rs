//! Teaching CableBox a remote: the steps of the walkthrough, independent of the UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use input::learn::{Bindings, Button};
use input::pico::{Firmware, PicoInfo};
use input::RemoteKey;

/// The menus can't be operated without these, so the walkthrough won't skip them.
const REQUIRED: usize = 6;

/// What to do after a button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Save,
    Discard,
}

/// A run through the buttons. Nothing takes effect until it ends in [`Outcome::Save`].
#[derive(Debug)]
pub struct Learn {
    steps: Vec<RemoteKey>,
    step: usize,
    /// Learning one key replaces that key's earlier buttons; a whole remote adds to them.
    single: bool,
    /// Buttons that can't be used again, and what they do.
    taken: Bindings,
    learned: Vec<(Button, RemoteKey)>,
    error: String,
}

impl Learn {
    /// Walks through every key.
    pub fn remote() -> Self {
        Self { steps: RemoteKey::ALL.to_vec(), step: 0, single: false, taken: Bindings::new(), learned: Vec::new(), error: String::new() }
    }

    /// Learns one key. `current` is the saved table; its other keys keep their buttons.
    pub fn key(key: RemoteKey, current: &Bindings) -> Self {
        let mut taken = current.clone();
        taken.retain(|_, k| *k != key);
        Self { steps: vec![key], step: 0, single: true, taken, learned: Vec::new(), error: String::new() }
    }

    fn button_for(&self, key: RemoteKey) -> Option<Button> {
        self.learned.iter().find(|(_, k)| *k == key).map(|(b, _)| *b)
    }

    fn confirming(&self) -> bool {
        self.step >= self.steps.len()
    }

    /// A button was pressed on the remote.
    pub fn press(&mut self, button: Button) -> Outcome {
        self.error.clear();
        if self.confirming() {
            return match button {
                b if Some(b) == self.button_for(RemoteKey::Ok) => Outcome::Save,
                b if Some(b) == self.button_for(RemoteKey::Back) => Outcome::Discard,
                _ => {
                    self.error = "That button isn't OK or Back.".into();
                    Outcome::Continue
                }
            };
        }
        let key = self.steps[self.step];
        if !self.single && self.step >= REQUIRED && Some(button) == self.button_for(RemoteKey::Ok) {
            self.step += 1;
            return Outcome::Continue;
        }
        let used = self.taken.get(&button).copied().or(self.learned.iter().find(|(b, _)| *b == button).map(|(_, k)| *k));
        if let Some(other) = used {
            self.error = format!("That button is already {}.", other.label());
            return Outcome::Continue;
        }
        self.learned.push((button, key));
        self.step += 1;
        if self.single {
            return Outcome::Save;
        }
        Outcome::Continue
    }

    pub fn title(&self) -> String {
        if self.single {
            format!("Learn {}", self.steps[0].label())
        } else if self.confirming() {
            "Save this remote?".into()
        } else {
            format!("Learn a remote ({} of {})", self.step + 1, self.steps.len())
        }
    }

    pub fn body(&self) -> String {
        let prompt = if self.confirming() {
            "Press OK on the remote to save, or Back to discard.".to_string()
        } else {
            let key = self.steps[self.step].label();
            if !self.single && self.step >= REQUIRED {
                format!("Press {key} on the remote, or OK to skip it.")
            } else {
                format!("Press {key} on the remote.")
            }
        };
        format!("{prompt}\n\n{}", self.error)
    }

    /// Writes what was learned into the saved table.
    pub fn apply(&self, table: &mut BTreeMap<String, String>) {
        if self.single {
            let name = self.steps[0].name();
            table.retain(|_, k| k != name);
        }
        for (button, key) in &self.learned {
            table.insert(button.to_string(), key.name().to_string());
        }
    }
}

/// Where the package installs receiver firmware, one folder per model (`ir1/`).
fn firmware_dir(model: ir_proto::Model) -> PathBuf {
    let root = std::env::var_os("CABLEBOX_FIRMWARE_DIR").map(PathBuf::from).unwrap_or_else(|| "/usr/share/cablebox/firmware".into());
    root.join(model.to_string().to_lowercase())
}

/// The firmware version this package ships for `model`, if any. A model this package doesn't
/// know has no folder, so it's never offered anything.
pub fn bundled_firmware_version(model: ir_proto::Model) -> Option<ir_proto::Version> {
    read_version(&firmware_dir(model))
}

fn read_version(dir: &Path) -> Option<ir_proto::Version> {
    std::fs::read_to_string(dir.join("version")).ok()?.parse().ok()
}

/// Loads the firmware this package ships for `model`.
pub fn bundled_firmware(model: ir_proto::Model) -> std::io::Result<Firmware> {
    let dir = firmware_dir(model);
    let version = read_version(&dir).ok_or(std::io::ErrorKind::InvalidData)?;
    Ok(Firmware { model, version, image: std::fs::read(dir.join("cablebox-ir.bin"))? })
}

/// Whether `bundled` should replace what the receiver runs. A receiver that speaks another
/// protocol is replaced whatever its number, since it can't be used otherwise.
pub fn firmware_update_available(info: &PicoInfo, bundled: Option<ir_proto::Version>) -> bool {
    bundled.is_some_and(|b| b > info.version || !info.compatible)
}

/// Reads the saved table. Bad entries are skipped, so a hand-edited file can't stop startup.
pub fn parse_table(table: &BTreeMap<String, String>) -> Bindings {
    table
        .iter()
        .filter_map(|(button, key)| match (button.parse::<Button>(), RemoteKey::from_name(key)) {
            (Ok(b), Some(k)) => Some((b, k)),
            _ => {
                tracing::warn!("ignoring remote button in config: {button} = {key}");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: Button = Button::Scan(100);
    const BACK: Button = Button::Scan(105);

    /// Past the required keys, using scancodes 100.. for them.
    fn past_required() -> Learn {
        let mut l = Learn::remote();
        for i in 0..REQUIRED as u32 {
            assert_eq!(l.press(Button::Scan(100 + i)), Outcome::Continue);
        }
        l
    }

    fn finish(l: &mut Learn) {
        while !l.confirming() {
            l.press(OK);
        }
    }

    #[test]
    fn required_keys_cant_be_skipped() {
        let mut l = Learn::remote();
        assert_eq!(l.press(OK), Outcome::Continue);
        assert_eq!(l.press(OK), Outcome::Continue); // on Up: OK is not a skip here
        assert_eq!(l.step, 1);
        assert!(l.body().contains("already OK"), "{}", l.body());
        assert!(l.body().contains("Press Up"), "{}", l.body());
        assert_eq!(l.press(Button::Scan(101)), Outcome::Continue);
        assert_eq!(l.step, 2);
        assert!(!l.body().contains("already"), "error clears");
    }

    #[test]
    fn optional_keys_skip_with_ok_and_reject_duplicates() {
        let mut l = past_required();
        assert!(l.body().contains("Press Menu"), "{}", l.body());
        assert!(l.body().contains("OK to skip"));
        assert_eq!(l.press(OK), Outcome::Continue);
        assert!(l.button_for(RemoteKey::Menu).is_none());
        assert_eq!(l.press(Button::Scan(200)), Outcome::Continue); // Guide
        assert_eq!(l.button_for(RemoteKey::Guide), Some(Button::Scan(200)));
        let step = l.step;
        assert_eq!(l.press(Button::Scan(200)), Outcome::Continue);
        assert_eq!(l.step, step);
        assert!(l.body().contains("already Guide"), "{}", l.body());
    }

    #[test]
    fn the_new_ok_saves_and_the_new_back_discards() {
        let mut l = past_required();
        finish(&mut l);
        assert_eq!(l.title(), "Save this remote?");
        assert_eq!(l.press(Button::Scan(999)), Outcome::Continue);
        assert!(l.body().contains("isn't OK or Back"));
        assert_eq!(l.press(BACK), Outcome::Discard);
        assert_eq!(l.press(OK), Outcome::Save);
    }

    #[test]
    fn a_whole_remote_adds_to_the_table() {
        let mut l = past_required();
        finish(&mut l);
        let mut table = BTreeMap::from([("scan:00000001".to_string(), "ok".to_string())]);
        l.apply(&mut table);
        assert_eq!(table.get("scan:00000001").unwrap(), "ok"); // another remote's OK still works
        assert_eq!(table.get("scan:00000064").unwrap(), "ok");
        assert_eq!(table.len(), 1 + REQUIRED);
    }

    #[test]
    fn one_key_replaces_that_keys_buttons_and_respects_the_others() {
        let table = BTreeMap::from([
            ("scan:00000001".to_string(), "ok".to_string()),
            ("scan:00000002".to_string(), "up".to_string()),
        ]);
        let mut l = Learn::key(RemoteKey::Ok, &parse_table(&table));
        assert_eq!(l.press(Button::Scan(2)), Outcome::Continue);
        assert!(l.body().contains("already Up"), "{}", l.body());
        assert_eq!(l.press(Button::Scan(1)), Outcome::Save); // its own old button is fine
        let mut table = table;
        l.apply(&mut table);
        assert_eq!(table.len(), 2);
        assert_eq!(table.get("scan:00000001").unwrap(), "ok");
        assert_eq!(table.get("scan:00000002").unwrap(), "up");
    }

    #[test]
    fn firmware_folders_are_per_model() {
        assert!(firmware_dir(ir_proto::Model::IR1).ends_with("ir1"));
        assert!(firmware_dir(ir_proto::Model(2)).ends_with("ir2"));
        assert_eq!(read_version(Path::new("/nonexistent/ir2")), None);
    }

    #[test]
    fn firmware_is_offered_only_when_newer_or_needed() {
        let v = |patch| ir_proto::Version { major: 1, minor: 0, patch };
        let info = PicoInfo { model: ir_proto::Model::IR1, version: v(5), compatible: true, signal: None };
        assert!(firmware_update_available(&info, Some(v(6))));
        assert!(!firmware_update_available(&info, Some(v(5))));
        assert!(!firmware_update_available(&info, Some(v(4))));
        assert!(!firmware_update_available(&info, None));
        let old = PicoInfo { compatible: false, ..info };
        assert!(firmware_update_available(&old, Some(v(4))));
        assert!(!firmware_update_available(&old, None));
    }

    #[test]
    fn bad_table_entries_are_skipped() {
        let table = BTreeMap::from([
            ("scan:0000000a".to_string(), "guide".to_string()),
            ("scan:zz".to_string(), "ok".to_string()),
            ("scan:0000000b".to_string(), "not-a-key".to_string()),
        ]);
        let parsed = parse_table(&table);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get(&Button::Scan(10)), Some(&RemoteKey::Guide));
    }
}
