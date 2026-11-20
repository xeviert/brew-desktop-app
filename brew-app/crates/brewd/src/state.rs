//! `brewd`'s state files: small JSON, replaced whole.
//!
//! Written to a sibling and renamed over the old one, after an `fsync`, so a power cut
//! leaves either the old file or the new one — never half of either. Unlike a journal,
//! a state file is rewritten, so a torn write would lose all of it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

/// `None` when the file doesn't exist. One that doesn't read is logged and treated as
/// missing: the desktop re-pushes its watches within a poll, and an alert book started
/// over re-raises what still holds.
pub fn load<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e, "unreadable state file, starting over");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_value_loads_back_and_a_missing_or_torn_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        assert_eq!(load::<Vec<u32>>(&path).unwrap(), None);

        save(&path, &vec![1u32, 2]).unwrap();
        save(&path, &vec![3u32]).unwrap();
        assert_eq!(load::<Vec<u32>>(&path).unwrap(), Some(vec![3]));
        assert!(!path.with_extension("json.tmp").exists());

        fs::write(&path, "[3,").unwrap();
        assert_eq!(load::<Vec<u32>>(&path).unwrap(), None);
    }
}
