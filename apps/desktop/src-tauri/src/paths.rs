use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Joins a client-supplied relative path onto the workspace root.
///
/// Lexical checks here are the first gate; filesystem commands additionally
/// canonicalise the result so symlinks cannot escape the workspace.
pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let trimmed = relative.trim();
    if trimmed.is_empty() {
        return Err("file path is empty".to_owned());
    }
    if trimmed.contains('\0') {
        return Err("file path contains a NUL byte".to_owned());
    }
    // Apply one cross-platform policy even when developing on Unix. Windows
    // drive-relative paths and backslashes must never become alternate escapes.
    if trimmed.contains('\\') || trimmed.contains(':') {
        return Err("cross-platform unsafe file path".to_owned());
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(format!("absolute paths are not allowed: {trimmed}"));
    }
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".ryoko") {
                    return Err("workspace metadata cannot be edited".to_owned());
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("unsafe file path: {trimmed}"));
            }
        }
    }
    let joined = root.join(candidate);
    if !joined.starts_with(root) {
        return Err(format!("path escapes the workspace: {trimmed}"));
    }
    Ok(joined)
}

/// Creates the parent directories one component at a time, refusing to follow
/// an existing symlink inside the workspace. `create_dir_all` would follow a
/// symlinked component before the canonical check could reject it.
pub fn create_directories_without_symlinks(root: &Path, parent: &Path) -> Result<(), String> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| "path escapes the workspace".to_owned())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to write through a symlink: {}",
                    current.display()
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("not a directory: {}", current.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|error| format!("cannot create {}: {error}", current.display()))?;
            }
            Err(error) => {
                return Err(format!("cannot inspect {}: {error}", current.display()));
            }
        }
    }
    Ok(())
}

/// Strips Windows extended-length verbatim prefixes (`\\?\` and `\\?\UNC\`)
/// before passing canonicalized paths to external shells, language servers,
/// debuggers, or build tools that reject `\\?\` paths.
pub fn host_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| raw.strip_prefix(r"\\?\unc\"))
    {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        let bytes = stripped.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/')
        {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("ryoko-paths-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn host_path_strips_windows_verbatim_prefixes() {
        assert_eq!(
            host_path(Path::new(r"\\?\C:\Users\dev\project")),
            PathBuf::from(r"C:\Users\dev\project")
        );
        assert_eq!(
            host_path(Path::new(r"\\?\UNC\files\share\project")),
            PathBuf::from(r"\\files\share\project")
        );
        assert_eq!(
            host_path(Path::new("/home/dev/project")),
            PathBuf::from("/home/dev/project")
        );
    }

    #[test]
    fn accepts_nested_relative_paths() {
        let root = Path::new("/repo");
        assert_eq!(
            safe_join(root, "src/main.rs").unwrap(),
            Path::new("/repo/src/main.rs")
        );
        assert_eq!(
            safe_join(root, "./src/lib.rs").unwrap(),
            Path::new("/repo/src/lib.rs")
        );
    }

    #[test]
    fn rejects_parent_traversal_and_absolute_paths() {
        let root = Path::new("/repo");
        assert!(safe_join(root, "../escape.rs").is_err());
        assert!(safe_join(root, "src/../../escape.rs").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "  ").is_err());
    }

    #[test]
    fn rejects_workspace_metadata_and_cross_platform_escapes() {
        let root = Path::new("/repo");
        for path in [
            ".git/config",
            "src/.ryoko/session.json",
            "src/.GiT/hooks/post-commit",
            "src\\..\\secrets.txt",
            "C:relative.txt",
            "src/C:config.txt",
        ] {
            assert!(
                safe_join(root, path).is_err(),
                "unexpectedly accepted {path}"
            );
        }
        assert!(safe_join(root, "src/.gitignore").is_ok());
        assert!(safe_join(root, "src/.env.example").is_ok());
    }

    #[test]
    fn rejects_embedded_nul_bytes() {
        assert!(safe_join(Path::new("/repo"), "src/a\0b.rs").is_err());
    }

    #[test]
    fn creates_nested_directories_under_the_root() {
        let root = temp_root("create");
        let parent = root.join("src/nested/deep");
        create_directories_without_symlinks(&root, &parent).unwrap();
        assert!(parent.is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_directories_outside_the_root() {
        let root = temp_root("outside");
        let outside = temp_root("outside-target");
        let error = create_directories_without_symlinks(&root, &outside).unwrap_err();
        assert!(error.contains("escapes the workspace"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_follow_a_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink");
        let outside = temp_root("symlink-target");
        symlink(&outside, root.join("link")).unwrap();
        let error =
            create_directories_without_symlinks(&root, &root.join("link/nested")).unwrap_err();
        assert!(error.contains("symlink"));
        assert!(!outside.join("nested").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
