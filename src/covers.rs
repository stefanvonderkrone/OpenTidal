//! The cover cache: one account's directory of cover files under
//! `$XDG_CACHE_HOME/opentidal/covers/<user id>/`. Rune-free.
//!
//! A cover file is `<artwork id>.cover`. Only [`CoverCache::put`] creates one,
//! through `atomic::replace`, and only after the bytes pass the image check. A
//! file that fails the check later reads as a miss and is removed, so a
//! damaged entry converges to a refetch.
#![cfg_attr(not(test), expect(dead_code, reason = "the app reads the cover cache from unit 9"))]

use std::{
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
};

use crate::catalogue::{ArtworkId, is_id_shaped};

/// The most cover files one account keeps. Only files whose name parses as
/// `<artwork id>.cover` count toward it.
const MAX_COVERS: usize = 4000;

/// The extension of a cover file.
const COVER_EXTENSION: &str = "cover";

/// The cover cache root. An absolute, nonempty `XDG_CACHE_HOME` wins, else
/// `$HOME/.cache`. Pure, the same rule as `auth::token_file_path`.
pub fn cover_root(xdg_cache_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let base = xdg_cache_home
        .map(Path::new)
        .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.filter(|path| !path.as_os_str().is_empty()).map(|path| path.join(".cache")))?;
    Some(base.join("opentidal/covers"))
}

/// One account's cover directory. The field is private, and `for_account` is
/// the only constructor, so a cover cache always names one account.
#[derive(Clone, Debug)]
pub struct CoverCache {
    dir: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CoverError {
    /// The bytes are not a JPEG, a PNG or a WebP image.
    NotAnImage,
    /// The filesystem refused the replace.
    Io(String),
}

impl CoverCache {
    /// `None` for a user id without the id shape, so a user id can never climb
    /// out of the root. Does no I/O.
    pub fn for_account(root: &Path, user_id: &str) -> Option<CoverCache> {
        is_id_shaped(user_id).then(|| CoverCache { dir: root.join(user_id) })
    }

    fn path(&self, id: &ArtworkId) -> PathBuf {
        self.dir.join(format!("{}.{COVER_EXTENSION}", id.as_str()))
    }

    /// The path of a cached cover, when the file exists and starts with an
    /// image signature. A file that fails the check is removed and reads as a
    /// miss. Reads at most 12 bytes.
    pub(crate) fn get(&self, id: &ArtworkId) -> Option<PathBuf> {
        let path = self.path(id);
        let mut head = Vec::with_capacity(12);
        std::fs::File::open(&path).ok()?.take(12).read_to_end(&mut head).ok()?;
        if is_image(&head) {
            return Some(path);
        }
        let _ = std::fs::remove_file(&path);
        None
    }

    /// Put a cover through `atomic::replace`. Bytes that are not an image are
    /// refused before any file exists. Putting the same bytes twice leaves one
    /// identical file. After a put, prune to `MAX_COVERS`.
    pub(crate) fn put(&self, id: &ArtworkId, bytes: &[u8]) -> Result<PathBuf, CoverError> {
        if !is_image(bytes) {
            return Err(CoverError::NotAnImage);
        }
        let path = self.path(id);
        crate::atomic::replace(&path, bytes).map_err(|e| CoverError::Io(e.to_string()))?;
        self.prune(MAX_COVERS);
        Ok(path)
    }

    /// Remove a cover whose decode failed, so the next request fetches it again.
    pub(crate) fn evict(&self, id: &ArtworkId) {
        let _ = std::fs::remove_file(self.path(id));
    }

    /// Remove temporary files a crashed replace left behind. Runs on the worker
    /// before a batch, never on the startup path.
    pub(crate) fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.file_name().to_string_lossy().ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Remove the oldest valid covers past `limit`. A name that does not parse
    /// as `<artwork id>.cover`, such as a temporary file or a stray file, is not
    /// counted and not removed, so an invalid entry can never push out a valid
    /// one.
    fn prune(&self, limit: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut covers: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|entry| is_cover_name(&entry.file_name()))
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        if covers.len() <= limit {
            return;
        }
        covers.sort();
        let excess = covers.len() - limit;
        for (_, path) in covers.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Whether a file name is `<artwork id>.cover`.
fn is_cover_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.strip_suffix(COVER_EXTENSION)
        .and_then(|stem| stem.strip_suffix('.'))
        .is_some_and(is_id_shaped)
}

/// JPEG `FF D8 FF`, PNG `89 50 4E 47 0D 0A 1A 0A`, or WebP `RIFF....WEBP`.
fn is_image(head: &[u8]) -> bool {
    head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || (head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::catalogue::{AlbumRow, Catalogue, CatalogueRows};

    /// An artwork id, minted the only way one can be: by a catalogue.
    pub(crate) fn artwork(id: &str) -> ArtworkId {
        let mut cat = Catalogue::seed();
        cat.absorb(CatalogueRows {
            albums: vec![AlbumRow {
                id: "cover-test-album".into(),
                title: "Cover test".into(),
                artist: "seed-artist-0".into(),
                cover: id.into(),
            }],
            ..CatalogueRows::default()
        });
        cat.album(&cat.album_id("cover-test-album").unwrap()).unwrap().cover.clone().expect("not an id")
    }

    /// The solid magenta PNG the cover scene and the pixel check use.
    pub(crate) fn magenta() -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/covers/magenta.png", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn cache(root: &Path) -> CoverCache {
        CoverCache::for_account(root, "173210380").unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn the_cover_cache_lives_under_xdg_cache_home() {
        assert_eq!(
            cover_root(Some(OsStr::new("/xdg/cache")), Some(Path::new("/home/ada"))),
            Some(PathBuf::from("/xdg/cache/opentidal/covers"))
        );
    }

    /// A relative or empty XDG value is not a base directory, so the root falls
    /// back to `~/.cache`.
    #[test]
    fn without_xdg_cache_home_the_cover_cache_falls_back_to_dot_cache() {
        let home = Some(Path::new("/home/ada"));
        let fallback = Some(PathBuf::from("/home/ada/.cache/opentidal/covers"));
        assert_eq!(cover_root(None, home), fallback);
        assert_eq!(cover_root(Some(OsStr::new("")), home), fallback);
        assert_eq!(cover_root(Some(OsStr::new("relative/cache")), home), fallback);
        assert_eq!(cover_root(None, None), None);
        assert_eq!(cover_root(None, Some(Path::new(""))), None);
    }

    /// A user id that is not a single safe path component gets no cache.
    #[test]
    fn a_user_id_cannot_leave_the_cover_root() {
        let root = Path::new("/covers");
        for bad in ["..", "a/b", "", "/etc", &"9".repeat(65), "a b"] {
            assert!(CoverCache::for_account(root, bad).is_none(), "{bad:?} got a cache");
        }
        assert_eq!(CoverCache::for_account(root, "173210380").unwrap().dir, root.join("173210380"));
    }

    /// The bytes a put lands are the bytes a later run reads, through a cache
    /// built again from the same root and user id.
    #[test]
    fn a_put_cover_survives_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let id = artwork("251380836-large");
        let path = cache(root.path()).put(&id, &magenta()).unwrap();

        let restarted = cache(root.path());
        assert_eq!(restarted.get(&id), Some(path.clone()));
        assert_eq!(std::fs::read(&path).unwrap(), magenta());
    }

    /// A put that cannot rename leaves neither a cover nor a temporary file. A
    /// directory at the cover path makes the rename fail after the bytes are
    /// written.
    #[test]
    fn a_failed_put_leaves_no_partial_cover() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        std::fs::create_dir_all(cache.path(&id)).unwrap();

        assert!(matches!(cache.put(&id, &magenta()), Err(CoverError::Io(_))));
        assert_eq!(names(&cache.dir), ["251380836-large.cover"], "a failed put left a file behind");
        assert!(cache.path(&id).is_dir(), "the failed put replaced the blocking directory");
        assert_eq!(cache.get(&id), None);
    }

    #[test]
    fn putting_a_cover_twice_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        cache.put(&id, &magenta()).unwrap();
        let before = (names(&cache.dir), std::fs::read(cache.path(&id)).unwrap());
        cache.put(&id, &magenta()).unwrap();
        assert_eq!((names(&cache.dir), std::fs::read(cache.path(&id)).unwrap()), before);
    }

    /// An HTML error page or an empty body never becomes a cover file.
    #[test]
    fn bytes_that_are_not_an_image_are_never_put() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        for bad in [&b"<!doctype html><title>404</title>"[..], b"", b"\x89PN", b"RIFF0000WAVE"] {
            assert_eq!(cache.put(&id, bad), Err(CoverError::NotAnImage));
        }
        assert!(names(&cache.dir).is_empty(), "refused bytes left a file");
        assert!(is_image(b"\xFF\xD8\xFF\xE0"));
        assert!(is_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "));
    }

    /// A cover file damaged on disk reads as a miss and is removed, so the next
    /// batch fetches it again.
    #[test]
    fn a_damaged_cover_reads_as_a_miss() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        let path = cache.put(&id, &magenta()).unwrap();
        std::fs::write(&path, b"truncated").unwrap();

        assert_eq!(cache.get(&id), None);
        assert!(!path.exists(), "the damaged cover stayed on disk");
    }

    /// Stray files never count toward the limit, so they cannot push out a
    /// valid cover. They are not removed by prune either.
    #[test]
    fn an_invalid_cache_entry_does_not_count_against_the_limit() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let kept = artwork("kept");
        cache.put(&kept, &magenta()).unwrap();
        for stray in ["notes.txt", "a b.cover", ".cover", "x.cover.1.tmp", "y.jpg"] {
            std::fs::write(cache.dir.join(stray), b"stray").unwrap();
        }

        cache.prune(1);
        assert!(cache.get(&kept).is_some(), "a stray file pushed out a valid cover");
        assert_eq!(names(&cache.dir).len(), 6, "prune removed a file it does not own");

        let newer = artwork("newer");
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.put(&newer, &magenta()).unwrap();
        cache.prune(1);
        assert_eq!(cache.get(&kept), None, "the oldest valid cover was not pruned");
        assert!(cache.get(&newer).is_some());
    }

    /// A cover whose decode failed is evicted, so the next request fetches it.
    #[test]
    fn an_evicted_cover_is_a_miss() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        cache.put(&id, &magenta()).unwrap();
        cache.evict(&id);
        assert_eq!(cache.get(&id), None);
        assert!(names(&cache.dir).is_empty());
    }

    /// The sweep removes temporary files a crashed replace left, and nothing
    /// else.
    #[test]
    fn a_leftover_temporary_file_is_swept() {
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let id = artwork("251380836-large");
        cache.put(&id, &magenta()).unwrap();
        std::fs::write(cache.dir.join("251380836-large.cover.4242.tmp"), b"half").unwrap();

        cache.sweep();
        assert_eq!(names(&cache.dir), ["251380836-large.cover"]);
    }

    /// Two accounts on one root have two directories. A cover put for one is a
    /// miss for the other.
    #[test]
    fn one_account_cannot_read_another_accounts_covers() {
        let root = tempfile::tempdir().unwrap();
        let a = CoverCache::for_account(root.path(), "account-a").unwrap();
        let b = CoverCache::for_account(root.path(), "account-b").unwrap();
        let id = artwork("playlist-cover-1");
        a.put(&id, &magenta()).unwrap();

        assert!(a.get(&id).is_some());
        assert_eq!(b.get(&id), None, "account B read account A's cover");
        assert!(names(&b.dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_cover_cache_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let cache = cache(root.path());
        let path = cache.put(&artwork("251380836-large"), &magenta()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&cache.dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
