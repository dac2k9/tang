//! The log of Tang inside a host. A host has no terminal to show a plugin's log, and the process that a DAW runs plugins in is not one that a person looks at, so what Tang says goes to a file that a person can be asked
//! for: `tang-plugin.log` in the system's directory for temporary files. Every line says which process wrote it (several share the file, and a host may run each plugin in a process of its own).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::SystemTime;

/// The file grows only to this much before it is begun again: it is a log of what the last runs did.
const MAX_BYTES: u64 = 1024 * 1024;

/// Where the log is.
pub fn path() -> PathBuf {
    std::env::temp_dir().join("tang-plugin.log")
}

struct FileLogger {
    file: Mutex<File>,
    process: u32,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        let secs = since_epoch.as_secs() % 86400;
        let (h, m, s, ms) = (secs / 3600, (secs % 3600) / 60, secs % 60, since_epoch.subsec_millis());
        let mut file = self.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writeln!(file, "[{h:02}:{m:02}:{s:02}.{ms:03} {} pid {}] {}", record.level(), self.process, record.args());
    }

    fn flush(&self) {
        let _ = self.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).flush();
    }
}

/// Start logging to the file, once for the process (the second instance of a plugin in a process finds it going), unless something has set a logger already (a host that does is the one to read it from). The level is `RUST_LOG`'s if it says one, and info
/// if not.
pub(super) fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let path = path();
        let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_BYTES);
        let opened = OpenOptions::new().create(true).write(true).append(!too_big).truncate(too_big).open(&path);
        let Ok(file) = opened else { return };
        let logger = FileLogger { file: Mutex::new(file), process: std::process::id() };
        if log::set_boxed_logger(Box::new(logger)).is_ok() {
            log::set_max_level(std::env::var("RUST_LOG").ok().and_then(|level| level.parse().ok()).unwrap_or(log::LevelFilter::Info));
        }
    });
}
