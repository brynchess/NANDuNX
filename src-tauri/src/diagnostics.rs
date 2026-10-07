use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

/// A single process owns this append-only diagnostic file. No caller may pass
/// key material here.
pub fn init_diagnostics() -> io::Result<PathBuf> {
    let directory = std::env::current_exe()?
        .parent()
        .ok_or_else(|| io::Error::other("executable directory unavailable"))?
        .join("logs");
    fs::create_dir_all(&directory)?;
    let name = std::env::current_exe()?
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let path = directory.join(format!("{name}-{}.log", std::process::id()));
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let _ = LOG.set(Mutex::new(file));
    diagnostic("process started");
    Ok(path)
}

pub fn diagnostic(event: &str) {
    let Some(log) = LOG.get() else { return };
    let Ok(mut file) = log.lock() else { return };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let _ = writeln!(file, "{timestamp} pid={} {event}", std::process::id());
    let _ = file.flush();
}
