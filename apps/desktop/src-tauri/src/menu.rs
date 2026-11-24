//! Native application menu for macOS.
//!
//! macOS apps are expected to put File / Edit / View / Run / Help in the
//! system menu bar, so on macOS the renderer hides its in-window menu bar and
//! this module installs a native one instead. Windows and Linux keep the
//! in-window menu bar drawn by `ui/src/menu_bar.ts`.
//!
//! Custom items carry a workbench command id. Choosing one emits
//! `ryoko://menu` with that id; the renderer runs it only if the id is a
//! registered command. Clipboard and undo items are the platform's predefined
//! items so copy, paste and undo keep working in every text field.

// The data below is only used by the macOS build and the tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(target_os = "macos")]
use tauri::{
    AppHandle, Emitter, Runtime,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
};

/// Event the renderer listens to; the payload is a command id.
pub const MENU_EVENT: &str = "ryoko://menu";

/// One entry of a native submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A workbench command: id, label, optional accelerator.
    Command(&'static str, &'static str, Option<&'static str>),
    Separator,
    Undo,
    Redo,
    Cut,
    CopyText,
    Paste,
    SelectAll,
}

use Entry::{Command, CopyText, Cut, Paste, Redo, SelectAll, Separator, Undo};

/// The native menus. Command ids must match `MENUS` in `ui/src/menu_bar.ts`
/// (checked by a test below). Accelerators are the default keybindings.
pub const MENUS: &[(&str, &[Entry])] = &[
    (
        "File",
        &[
            Command("workspace.openFolder", "Open Folder…", Some("CmdOrCtrl+O")),
            Command("workbench.quickOpen", "Go to File…", Some("CmdOrCtrl+P")),
            Separator,
            Command("editor.save", "Save", Some("CmdOrCtrl+S")),
            Command("editor.close", "Close Editor", Some("CmdOrCtrl+W")),
            Separator,
            Command("workbench.settings", "Settings…", Some("CmdOrCtrl+,")),
        ],
    ),
    (
        "Edit",
        &[
            Undo,
            Redo,
            Separator,
            Cut,
            CopyText,
            Paste,
            SelectAll,
            Separator,
            Command("editor.find", "Find", None),
            Command("editor.replace", "Replace", None),
            Command(
                "workbench.search",
                "Find in Files",
                Some("CmdOrCtrl+Shift+F"),
            ),
            Command(
                "workbench.replace",
                "Replace in Files",
                Some("CmdOrCtrl+Shift+R"),
            ),
            Separator,
            Command(
                "editor.inlineEdit",
                "Inline Edit with Agent",
                Some("CmdOrCtrl+K"),
            ),
        ],
    ),
    (
        "View",
        &[
            Command(
                "workbench.commandPalette",
                "Command Palette…",
                Some("CmdOrCtrl+Shift+P"),
            ),
            Separator,
            Command("view.explorer", "Explorer", None),
            Command(
                "workbench.changes",
                "Source Control Changes",
                Some("CmdOrCtrl+Shift+G"),
            ),
            Command("view.mission", "Mission Control", None),
            Command("view.problems", "Problems", None),
            Command("view.extensions", "Extensions", None),
            Separator,
            Command(
                "view.toggleSidebar",
                "Toggle Primary Side Bar",
                Some("CmdOrCtrl+B"),
            ),
            Command(
                "view.toggleInspector",
                "Toggle Mission Control Side Bar",
                None,
            ),
            Command("view.togglePanel", "Toggle Panel", Some("CmdOrCtrl+J")),
            Command("view.toggleMinimap", "Toggle Minimap", None),
            Command("view.resetLayout", "Reset Layout", None),
            Separator,
            Command("theme.pick", "Color Theme…", None),
        ],
    ),
    (
        "Run",
        &[
            Command("run.activeFile", "Run Active File", None),
            Separator,
            Command(
                "workbench.debug",
                "Toggle Debug Panel",
                Some("CmdOrCtrl+Shift+D"),
            ),
            Command("terminal.show", "Show Terminal", None),
        ],
    ),
    (
        "Help",
        &[
            Command("help.welcome", "Welcome", None),
            Command("help.reportBug", "Report a Bug…", None),
            Command("help.keyboardShortcuts", "Keyboard Shortcuts", None),
            Separator,
            Command("help.checkForUpdates", "Check for Updates…", None),
        ],
    ),
];

/// Every command id in the native menus, in order.
pub fn command_ids() -> Vec<&'static str> {
    MENUS
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .filter_map(|entry| match entry {
            Command(id, _, _) => Some(*id),
            _ => None,
        })
        .collect()
}

/// Builds the macOS menu: the application menu followed by `MENUS`.
#[cfg(target_os = "macos")]
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let app_menu = Submenu::with_items(
        app,
        "Ryoko",
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let menu = Menu::with_items(app, &[&app_menu])?;
    for (title, entries) in MENUS {
        let submenu = Submenu::new(app, *title, true)?;
        for entry in *entries {
            match *entry {
                Command(id, label, accelerator) => {
                    submenu.append(&MenuItem::with_id(app, id, label, true, accelerator)?)?
                }
                Separator => submenu.append(&PredefinedMenuItem::separator(app)?)?,
                Undo => submenu.append(&PredefinedMenuItem::undo(app, None)?)?,
                Redo => submenu.append(&PredefinedMenuItem::redo(app, None)?)?,
                Cut => submenu.append(&PredefinedMenuItem::cut(app, None)?)?,
                CopyText => submenu.append(&PredefinedMenuItem::copy(app, None)?)?,
                Paste => submenu.append(&PredefinedMenuItem::paste(app, None)?)?,
                SelectAll => submenu.append(&PredefinedMenuItem::select_all(app, None)?)?,
            }
        }
        if *title == "Help" {
            // macOS adds its menu search field to a submenu titled "Help".
            submenu.set_as_help_menu_for_nsapp()?;
        }
        menu.append(&submenu)?;
    }
    Ok(menu)
}

/// Forwards a native menu choice to the renderer, but only for known ids.
#[cfg(target_os = "macos")]
pub fn forward<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if command_ids().contains(&id) {
        let _ = app.emit(MENU_EVENT, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extracts `{ command: '...' }` ids from the renderer's menu definition.
    fn renderer_menu_ids() -> Vec<String> {
        let source = include_str!("../../ui/src/menu_bar.ts");
        let start = source
            .find("export const MENUS")
            .expect("MENUS in menu_bar.ts");
        let end = source[start..]
            .find("\n];")
            .map(|offset| start + offset)
            .expect("end of MENUS");
        source[start..end]
            .split("{ command: '")
            .skip(1)
            .map(|rest| rest[..rest.find('\'').expect("closing quote")].to_string())
            .collect()
    }

    #[test]
    fn native_menu_matches_renderer_menu() {
        let native: Vec<String> = command_ids().into_iter().map(str::to_string).collect();
        // Native Undo/Redo are the platform's predefined items instead.
        let renderer: Vec<String> = renderer_menu_ids()
            .into_iter()
            .filter(|id| id != "editor.undo" && id != "editor.redo")
            .collect();
        assert!(!native.is_empty());
        assert_eq!(native, renderer);
    }

    #[test]
    fn command_ids_are_unique_and_well_formed() {
        let ids = command_ids();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
        for id in ids {
            assert!(id.contains('.') && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
        }
    }
}
