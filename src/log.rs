use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use time::OffsetDateTime;

const MAX_BYTES: u64 = 512 * 1024;

struct FileLogger {
    path: PathBuf,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let stamp = format!(
            "{:02}:{:02}:{:02}.{:03}",
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        );
        let line = format!(
            "[{stamp} {:<5} {}] {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        self.append(&line);
    }

    fn flush(&self) {}
}

impl FileLogger {
    fn append(&self, line: &str) {
        let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        if f.metadata().map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            drop(f);
            let _ = std::fs::write(&self.path, b"");
            if let Ok(mut f) = OpenOptions::new().append(true).open(&self.path) {
                let _ = writeln!(f, "-- log truncated at {MAX_BYTES} bytes --");
            }
            return;
        }
        let _ = f.write_all(line.as_bytes());
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

pub fn init(dir: &Path) -> PathBuf {
    let path = dir.join("ytkew.log");
    let _ = LOGGER.set(FileLogger { path: path.clone() });
    if let Some(l) = LOGGER.get() {
        let _ = log::set_logger(l);
        log::set_max_level(log::LevelFilter::Info);
    }
    path
}

pub fn path() -> Option<&'static Path> {
    LOGGER.get().map(|l| l.path())
}
