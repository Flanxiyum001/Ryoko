use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::WalkBuilder;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::paths::{create_directories_without_symlinks, safe_join};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 5_000;

#[derive(Default)]
pub struct WorkspaceState {
    root: Mutex<Option<PathBuf>>,
}

#[derive(Serialize)]
pub struct WorkspaceInfo {
    pub root: String,
    pub name: String,
}

pub(crate) fn current_root(state: &State<'_, WorkspaceState>) -> Result<PathBuf, String> {
    state
        .root
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .ok_or_else(|| "no workspace is open".to_owned())
}

/// Shows a native folder picker that stays attached to the Ryoko window.
///
/// The dialog plugin only parents its picker on Windows and macOS. On Linux an
/// unparented GTK dialog can open behind the main window or on another
/// workspace, which looks like the button does nothing. Here Linux gets a
/// modal GTK chooser that is transient for the main window instead.
#[tauri::command]
pub async fn pick_workspace_folder(
    window: tauri::WebviewWindow,
    title: Option<String>,
) -> Result<Option<String>, String> {
    let title = title.unwrap_or_else(|| "Open Folder".to_owned());
    #[cfg(target_os = "linux")]
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                let _ = sender.send(linux_pick_folder(&target, &title));
            })
            .map_err(|error| format!("cannot show the folder picker: {error}"))?;
        tauri::async_runtime::spawn_blocking(move || receiver.recv())
            .await
            .map_err(|error| format!("folder picker failed: {error}"))?
            .map_err(|_| "folder picker closed unexpectedly".to_owned())?
    }
    #[cfg(not(target_os = "linux"))]
    {
        let picked = tauri::async_runtime::spawn_blocking(move || {
            window
                .dialog()
                .file()
                .set_parent(&window)
                .set_title(title)
                .blocking_pick_folder()
        })
        .await
        .map_err(|error| format!("folder picker failed: {error}"))?;
        picked
            .map(|path| {
                path.into_path()
                    .map(|path| path.display().to_string())
                    .map_err(|error| error.to_string())
            })
            .transpose()
    }
}

#[cfg(target_os = "linux")]
fn linux_pick_folder(window: &tauri::WebviewWindow, title: &str) -> Result<Option<String>, String> {
    use gtk::prelude::*;

    let parent = window
        .gtk_window()
        .map_err(|error| format!("cannot find the main window: {error}"))?;
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(title),
        Some(&parent),
        gtk::FileChooserAction::SelectFolder,
        &[
            ("_Cancel", gtk::ResponseType::Cancel),
            ("_Open", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let response = dialog.run();
    let path = (response == gtk::ResponseType::Accept)
        .then(|| dialog.filename())
        .flatten()
        .map(|path| path.display().to_string());
    // SAFETY: the dialog is owned here, was created on this (main) thread and
    // is not used after being destroyed.
    unsafe { dialog.destroy() };
    Ok(path)
}

#[tauri::command]
pub fn open_workspace(
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<WorkspaceInfo, String> {
    let root = PathBuf::from(&path)
        .canonicalize()
        .map_err(|error| format!("cannot open workspace: {error}"))?;
    if !root.is_dir() {
        return Err("workspace path is not a directory".to_owned());
    }
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let info = WorkspaceInfo {
        root: root.display().to_string(),
        name,
    };
    *state.root.lock().unwrap_or_else(|error| error.into_inner()) = Some(root);
    Ok(info)
}

#[tauri::command]
pub fn list_files(state: State<'_, WorkspaceState>) -> Result<Vec<String>, String> {
    let root = current_root(&state)?;
    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".ryoko" | "target" | "node_modules")
                )
        });

    let mut files = Vec::new();
    for entry in builder.build().filter_map(Result::ok) {
        if files.len() >= MAX_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(&root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    files.sort();
    Ok(files)
}

fn read_workspace_bytes(path: &str, root: &std::path::Path) -> Result<Vec<u8>, String> {
    let candidate = safe_join(root, path)?;
    let real = candidate
        .canonicalize()
        .map_err(|error| format!("cannot read {path}: {error}"))?;
    if !real.starts_with(root) {
        return Err(format!("path escapes the workspace: {path}"));
    }
    if !real.is_file() {
        return Err(format!("not a regular file: {path}"));
    }
    let metadata = fs::metadata(&real).map_err(|error| format!("cannot read {path}: {error}"))?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("file is larger than 2 MiB: {path}"));
    }
    let mut bytes = Vec::new();
    fs::File::open(&real)
        .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read {path}: {error}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("file is larger than 2 MiB: {path}"));
    }
    Ok(bytes)
}

#[tauri::command]
pub fn read_file(path: String, state: State<'_, WorkspaceState>) -> Result<String, String> {
    let bytes = read_workspace_bytes(&path, &current_root(&state)?)?;
    let content = String::from_utf8(bytes)
        .map_err(|error| format!("cannot read {path} as UTF-8: {error}"))?;
    validate_editor_text(&content, &path)?;
    Ok(content)
}

#[derive(Serialize)]
pub struct EncodedFilePreview {
    content: String,
    encoding: &'static str,
}

// Read-only by design: never convert an existing file's encoding on save.
#[tauri::command]
pub fn read_encoded_file_preview(
    path: String,
    state: State<'_, WorkspaceState>,
) -> Result<EncodedFilePreview, String> {
    let bytes = read_workspace_bytes(&path, &current_root(&state)?)?;
    decode_editor_preview(&bytes, &path)
}

fn decode_editor_preview(bytes: &[u8], path: &str) -> Result<EncodedFilePreview, String> {
    let unsupported = || format!("binary or unsupported text encoding: {path}");
    // Reject UTF-32 before the overlapping UTF-16 LE BOM.
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err(unsupported());
    }
    let utf16 = if bytes.starts_with(&[0xff, 0xfe]) {
        Some((true, &bytes[2..]))
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some((false, &bytes[2..]))
    } else if bytes.len() >= 8 && bytes.len() & 1 == 0 {
        let pairs = bytes.len() / 2;
        let even_zero = bytes.iter().step_by(2).filter(|&&byte| byte == 0).count();
        let odd_zero = bytes
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|&&byte| byte == 0)
            .count();
        if odd_zero * 4 >= pairs * 3 && even_zero * 8 <= pairs {
            Some((true, bytes))
        } else if even_zero * 4 >= pairs * 3 && odd_zero * 8 <= pairs {
            Some((false, bytes))
        } else {
            None
        }
    } else {
        None
    };
    let (content, encoding) = if let Some((little_endian, data)) = utf16 {
        if data.len() & 1 != 0 {
            return Err(unsupported());
        }
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        (
            String::from_utf16(&units).map_err(|_| unsupported())?,
            if little_endian {
                "UTF-16 LE"
            } else {
                "UTF-16 BE"
            },
        )
    } else {
        // For text-like logs with mixed or legacy bytes, display a best-effort
        // preview; do not pretend to know the encoding or allow saving it.
        let log = path.to_ascii_lowercase().ends_with(".log");
        let ascii = bytes
            .iter()
            .filter(|&&byte| matches!(byte, b'\t' | b'\n' | b'\r' | 0x1b | 0x20..=0x7e))
            .count();
        if !log || bytes.is_empty() || ascii * 4 < bytes.len() * 3 {
            return Err(unsupported());
        }
        (
            String::from_utf8_lossy(bytes).into_owned(),
            "Unknown encoding (lossy)",
        )
    };
    if content
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n' | '\u{1b}'))
    {
        return Err(unsupported());
    }
    Ok(EncodedFilePreview { content, encoding })
}

fn validate_editor_text(content: &str, path: &str) -> Result<(), String> {
    if content.contains('\0') {
        return Err(format!("binary or unsupported text encoding: {path}"));
    }
    Ok(())
}

fn requires_sensitive_write_approval(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let segments = normalized.split('/').collect::<Vec<_>>();
    segments.iter().any(|segment| {
        *segment == ".env"
            || segment.starts_with(".env.")
            || matches!(*segment, ".npmrc" | ".pypirc" | ".ssh" | "credentials")
    }) || segments
        .windows(2)
        .any(|parts| parts == [".github", "workflows"])
}

/// Sensitive files require an OS-owned confirmation. A renderer/agent cannot
/// mint a permission token or bypass this dialog by calling the IPC command.
#[tauri::command]
pub async fn write_file(
    path: String,
    content: String,
    expected_content: String,
    state: State<'_, WorkspaceState>,
    app: AppHandle,
) -> Result<(), String> {
    let root = current_root(&state)?;
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("refusing to write more than 2 MiB: {path}"));
    }
    if expected_content.len() as u64 > MAX_FILE_BYTES {
        return Err("expected file version exceeds 2 MiB".to_owned());
    }
    let candidate = safe_join(&root, &path)?;
    if requires_sensitive_write_approval(&path) {
        let description = format!(
            "Allow writing sensitive project file '{path}' in '{}'? Review its contents first.\
             \n\nDeny unless you explicitly intended this change.",
            root.display()
        );
        // Blocking native dialogs must never run on Tauri's main event thread.
        let approved = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .message(description)
                .title("Ryoko — sensitive file permission")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Approve once".to_owned(),
                    "Deny".to_owned(),
                ))
                .blocking_show()
        })
        .await
        .map_err(|_| "could not display permission dialog".to_owned())?;
        if !approved {
            return Err(format!("sensitive file write denied by user: {path}"));
        }
        if current_root(&state)? != root {
            return Err("workspace changed while awaiting permission; write cancelled".to_owned());
        }
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("invalid file path: {path}"))?;
    create_directories_without_symlinks(&root, parent)?;
    let real_parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", parent.display()))?;
    if !real_parent.starts_with(&root) {
        return Err(format!("path escapes the workspace: {path}"));
    }
    let file_name = candidate
        .file_name()
        .ok_or_else(|| format!("invalid file path: {path}"))?;
    let target = real_parent.join(file_name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err(format!("refusing to write through a symlink: {path}"));
        }
        if !metadata.is_file() {
            return Err(format!("target is not a regular file: {path}"));
        }
    }

    // Never silently replace edits made outside Ryoko since the file was
    // opened. This is a best-effort optimistic-concurrency guard; robust
    // multi-process atomic compare-and-swap needs OS-specific primitives.
    let current = fs::read_to_string(&target)
        .map_err(|error| format!("cannot verify current file {path}: {error}"))?;
    validate_editor_text(&current, &path)?;
    validate_editor_text(&content, &path)?;
    if current != expected_content {
        return Err(format!(
            "file changed on disk since it was opened; reload or compare before saving: {path}"
        ));
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = real_parent.join(format!(".ryoko-save-{}-{stamp}.tmp", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        // Do not follow a pre-existing symlink or clobber a sibling temp file
        // if timestamps collide across saves or a malicious process races us.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot save {path}: {error}"));
    }
    // Avoid the destructive delete-before-rename fallback on Windows: a failed
    // rename must never remove the existing document. Windows native atomic
    // replacement will need a dedicated implementation for full parity.
    fs::rename(&temp, &target).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot save {path}: {error}")
    })
}

#[cfg(test)]
mod permission_tests {
    use super::{decode_editor_preview, requires_sensitive_write_approval, validate_editor_text};

    #[test]
    fn rejects_binary_content_without_rejecting_unknown_text_files() {
        assert!(validate_editor_text("example\n", "file.unknown").is_ok());
        assert!(validate_editor_text("\0binary", "image.bin").is_err());
    }

    #[test]
    fn previews_utf16_logs_with_and_without_bom() {
        let text = "server started\r\n";
        for little_endian in [true, false] {
            let bytes: Vec<u8> = text
                .encode_utf16()
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect();
            let mut bom = if little_endian {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            bom.extend_from_slice(&bytes);
            for data in [&bytes, &bom] {
                let preview = decode_editor_preview(data, ".server.log").unwrap();
                assert_eq!(preview.content, text);
                assert_eq!(
                    preview.encoding,
                    if little_endian {
                        "UTF-16 LE"
                    } else {
                        "UTF-16 BE"
                    }
                );
            }
        }
    }

    #[test]
    fn lossy_preview_is_limited_to_text_like_logs() {
        let bytes = b"server: started \xff successfully\n";
        let preview = decode_editor_preview(bytes, ".server.log").unwrap();
        assert!(preview.content.contains('\u{fffd}'));
        assert!(preview.encoding.contains("lossy"));
        assert!(decode_editor_preview(bytes, "main.rs").is_err());
        assert!(decode_editor_preview(b"server started\0\xff\n", "binary.log").is_err());
        assert!(decode_editor_preview(&[0xff, 0xfe, 0, 0], "utf32.log").is_err());
    }

    #[test]
    fn prompts_for_environment_and_ci_files() {
        for path in [
            ".env",
            ".env.local",
            "apps/api/.env.production",
            ".github/workflows/build.yml",
            r".github\workflows\desktop.yml",
            ".NPMRC",
            ".ssh/id_ed25519",
        ] {
            assert!(requires_sensitive_write_approval(path), "{path}");
        }
    }

    #[test]
    fn ordinary_source_edits_need_no_extra_prompt() {
        for path in ["src/main.rs", "apps/site/index.ts", ".github/README.md"] {
            assert!(!requires_sensitive_write_approval(path), "{path}");
        }
    }
}
