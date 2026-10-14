//! Single instance per state directory (ADR-0016): an exclusive lock on an open
//! file handle. The OS releases it on every kind of exit, so a stale lock
//! cannot exist and there is no PID file to trust.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another devstack instance owns {0}")]
    Held(PathBuf),
    #[error("cannot open {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Held for the lifetime of the value.
#[derive(Debug)]
pub struct StateLock {
    _file: File,
    pub path: PathBuf,
}

impl StateLock {
    pub fn acquire(state_dir: &Path) -> Result<Self, LockError> {
        let path = state_dir.join("lock");
        let io = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        std::fs::create_dir_all(state_dir).map_err(io)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::Held(path)),
            Err(TryLockError::Error(e)) => return Err(io(e)),
        }
        // Informational only; the lock is the truth.
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        Ok(Self { _file: file, path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let first = StateLock::acquire(dir.path()).unwrap();
        assert!(matches!(
            StateLock::acquire(dir.path()),
            Err(LockError::Held(_))
        ));
        drop(first);
        StateLock::acquire(dir.path()).unwrap();
    }
}
