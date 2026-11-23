//! Capability-relative writes: no path component is reopened through an
//! ambient absolute path after it has been checked.
use std::io::Write;
use std::path::Path;

use cap_std::fs::{Dir, OpenOptions};
use uuid::Uuid;

pub(crate) fn write(root: &Path, relative: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let parent = relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent != Path::new(".") {
        dir.create_dir_all(parent)?;
    }
    // The opened directory remains the authority even if an attacker swaps a
    // parent symlink after this point. A temporary file is renamed over the
    // destination, so a final-component symlink is never followed for writes.
    let parent_dir = dir.open_dir(parent)?;
    let name = relative.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing file name")
    })?;
    let temporary = format!(".ryoko-write-{}", Uuid::new_v4());
    let result = (|| {
        let mut file =
            parent_dir.open_with(&temporary, OpenOptions::new().write(true).create_new(true))?;
        file.write_all(content)?;
        file.sync_all()?;
        parent_dir.rename(&temporary, &parent_dir, name)
    })();
    if result.is_err() {
        let _ = parent_dir.remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn swapped_parent_never_writes_outside_root() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        let target = Path::new("src/file.rs");
        for _ in 0..100 {
            let _ = std::fs::remove_file(root.path().join("src"));
            let _ = std::fs::remove_dir_all(root.path().join("src"));
            symlink(outside.path(), root.path().join("src")).unwrap();
            assert!(write(root.path(), target, b"unsafe").is_err());
            std::fs::remove_file(root.path().join("src")).unwrap();
            std::fs::create_dir(root.path().join("src")).unwrap();
            write(root.path(), target, b"safe").unwrap();
        }
        assert!(!outside.path().join("file.rs").exists());
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_is_replaced_without_following_it() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "original").unwrap();
        symlink(&victim, root.path().join("link")).unwrap();
        write(root.path(), Path::new("link"), b"replacement").unwrap();
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(root.path().join("link")).unwrap(),
            "replacement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_swap_during_write_cannot_escape_capability() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = root.path().join("src");
        let attacker = outside.path().to_path_buf();
        let switch = std::thread::spawn(move || {
            for _ in 0..500 {
                let _ = std::fs::remove_file(&parent);
                let _ = std::fs::remove_dir(&parent);
                let _ = symlink(&attacker, &parent);
                let _ = std::fs::remove_file(&parent);
                let _ = std::fs::create_dir(&parent);
            }
        });
        for _ in 0..500 {
            let _ = write(root.path(), Path::new("src/escaped.rs"), b"payload");
        }
        switch.join().unwrap();
        assert!(!outside.path().join("escaped.rs").exists());
    }
}
