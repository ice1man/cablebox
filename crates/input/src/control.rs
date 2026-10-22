//! Line-based control FIFO for automation and debugging over SSH:
//! `echo guide > $CABLEBOX_CONTROL` (one key name per line, see [`RemoteKey::from_name`]).

use std::ffi::CString;
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::thread;

use crate::RemoteKey;

extern "C" {
    fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
}

pub fn spawn(path: PathBuf, on_key: impl Fn(RemoteKey) + Send + 'static) {
    let _ = std::fs::remove_file(&path);
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return };
    // SAFETY: valid NUL-terminated path.
    if unsafe { mkfifo(c.as_ptr(), 0o600) } != 0 {
        tracing::warn!("cannot create control fifo {}", path.display());
        return;
    }
    tracing::info!("control fifo at {}", path.display());
    let _ = thread::Builder::new().name("control".into()).spawn(move || loop {
        // Blocks until a writer opens the fifo; EOF when it closes.
        let Ok(f) = std::fs::File::open(&path) else { return };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            for word in line.split_whitespace() {
                match RemoteKey::from_name(word) {
                    Some(k) => on_key(k),
                    None => tracing::warn!("control: unknown key {word:?}"),
                }
            }
        }
    });
}
