use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use thiserror::Error;
use zeroize::Zeroize;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum PrivateFileError {
    #[error("refusing to use symlink at {0}")]
    Symlink(PathBuf),
    #[error("path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_error(path: &Path, source: io::Error) -> PrivateFileError {
    PrivateFileError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Creates a private directory and repairs its mode to `0700`.
pub fn ensure_private_dir(path: &Path) -> Result<(), PrivateFileError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(PrivateFileError::Symlink(path.to_path_buf()));
        }
        if !metadata.is_dir() {
            return Err(PrivateFileError::NotRegularFile(path.to_path_buf()));
        }
    }
    fs::create_dir_all(path).map_err(|error| io_error(path, error))?;
    #[cfg(unix)]
    fs::set_permissions(path, Permissions::from_mode(0o700))
        .map_err(|error| io_error(path, error))?;
    Ok(())
}

/// Atomically writes a secret-bearing file with mode `0600`.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), PrivateFileError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    ensure_private_dir(parent)?;

    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(PrivateFileError::Symlink(path.to_path_buf()));
        }
        if !metadata.is_file() {
            return Err(PrivateFileError::NotRegularFile(path.to_path_buf()));
        }
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("private");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{sequence}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| io_error(&temporary, error))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, error));
    }
    drop(file);

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(path, error));
    }
    #[cfg(unix)]
    fs::set_permissions(path, Permissions::from_mode(0o600))
        .map_err(|error| io_error(path, error))?;
    sync_directory(parent)?;
    Ok(())
}

/// Reads a regular private file without following its final symlink and with
/// an allocation bound. Existing files are repaired to mode `0600`.
pub fn read_private_file_bounded(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, PrivateFileError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(PrivateFileError::Symlink(path.to_path_buf()));
        }
        if !metadata.is_file() {
            return Err(PrivateFileError::NotRegularFile(path.to_path_buf()));
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(|error| io_error(path, error))?;
    let metadata = file.metadata().map_err(|error| io_error(path, error))?;
    if !metadata.is_file() {
        return Err(PrivateFileError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "private file exceeds its size limit",
            ),
        ));
    }
    #[cfg(unix)]
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|error| io_error(path, error))?;

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if let Err(error) = file.take(maximum_bytes as u64 + 1).read_to_end(&mut bytes) {
        bytes.zeroize();
        return Err(io_error(path, error));
    }
    if bytes.len() > maximum_bytes {
        bytes.zeroize();
        return Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "private file exceeds its size limit",
            ),
        ));
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), PrivateFileError> {
    let directory = File::open(path).map_err(|error| io_error(path, error))?;
    directory.sync_all().map_err(|error| io_error(path, error))
}

pub fn private_file_mode(path: &Path) -> Result<Option<u32>, PrivateFileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(path, error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(PrivateFileError::Symlink(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        Ok(Some(metadata.permissions().mode() & 0o777))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "woof-secure-file-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn bounded_private_reads_repair_mode_and_reject_oversize_files() {
        let directory = temporary_directory("bounded");
        let path = directory.join("state.json");
        fs::write(&path, b"private").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        assert_eq!(read_private_file_bounded(&path, 7).unwrap(), b"private");
        #[cfg(unix)]
        assert_eq!(private_file_mode(&path).unwrap(), Some(0o600));
        assert!(read_private_file_bounded(&path, 6).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_private_reads_never_follow_final_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = temporary_directory("symlink");
        let target = directory.join("target");
        let link = directory.join("link");
        fs::write(&target, b"private").unwrap();
        symlink(&target, &link).unwrap();
        assert!(matches!(
            read_private_file_bounded(&link, 64),
            Err(PrivateFileError::Symlink(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
