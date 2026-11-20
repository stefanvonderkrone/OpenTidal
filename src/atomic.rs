//! The one safe way to replace a file. The token file and the cover cache both
//! go through [`replace`], so a reader sees the old file or the whole new one,
//! never a part, and a failure leaves no temporary file behind. Rune-free.

use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

/// Replace `path` with `bytes`. The parent is created with mode 0700 and the
/// file with mode 0600. The bytes go to `<path>.<pid>.tmp`, opened with
/// `create_new`, synced, and renamed over `path`. Then the parent is synced, so
/// the new name survives a crash. A failure at any step removes the temporary
/// file.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| std::io::Error::other("the file has no parent"))?;
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    private_directory(&mut directory);
    directory.create(parent)?;
    let temporary = temporary_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    private_file(&mut options);
    let mut file = match options.open(&temporary) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A stale temporary file from a crash is safe to remove. Creating
            // with create_new refuses an existing symlink.
            std::fs::remove_file(&temporary)?;
            options.open(&temporary)?
        }
        Err(error) => return Err(error),
    };
    let mut temporary = TemporaryFile::new(temporary);
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary.path, path)?;
    temporary.disarm();
    sync_directory(parent)
}

/// The temporary name [`replace`] writes before the rename.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(name)
}

/// A temporary file. It removes itself unless the rename succeeds, so a failed
/// replace leaves no half-written file for the next launch to find.
struct TemporaryFile {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Make the renamed directory entry durable. `sync_all` on the file does not
/// make its name in the directory survive a crash.
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// No equivalent on other platforms. The rename still completes.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn private_directory(directory: &mut std::fs::DirBuilder) {
    use std::os::unix::fs::DirBuilderExt;
    directory.mode(0o700);
}

#[cfg(not(unix))]
fn private_directory(_directory: &mut std::fs::DirBuilder) {}

#[cfg(unix)]
fn private_file(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_file(_options: &mut std::fs::OpenOptions) {}
