//! The identifier the app had before it moved to its own domain.
//!
//! The app was `dev.villain.layer` (the dev build `dev.villain.layer.dev`),
//! a domain it never had. macOS keys the config folder and the keychain item
//! by identifier, so the move by itself opened on an empty app: every repo,
//! task and worktree still on disk, none of them listed, and no tokens. The
//! first launch under the new identifier starts from what the old one saved
//! instead (DISK-4).

use std::path::Path;

use tauri::{AppHandle, Manager};

const NOW: &str = "eu.codevillain.villain-layer";
const BEFORE: &str = "dev.villain.layer";

/// The identifier this build had before: the old name, with the same suffix.
pub fn identifier(current: &str) -> Option<String> {
    current.strip_prefix(NOW).map(|rest| format!("{BEFORE}{rest}"))
}

/// Carry the previous identifier's config and tokens over, on the first
/// launch under this one. Before `ConfigStore::load`, which creates the
/// folder this looks for, and after `secrets::use_service`, so the tokens
/// land under this build's own item.
pub fn adopt(app: &AppHandle) {
    let current = app.config().identifier.clone();
    let (Ok(dir), Some(old)) = (app.path().app_config_dir(), identifier(&current)) else {
        return;
    };
    if adopt_config(&dir, &old) {
        if let Err(e) = crate::secrets::adopt(&old) {
            eprintln!("tokens saved under {old} were not carried over: {e}");
        }
    }
}

/// Copy `old`'s config.json into `dir`, this build's config folder, if the
/// folder does not exist yet. Copied, never moved: the old build still opens
/// as it was. Says whether it did, so the tokens follow only then.
///
/// Decided by the folder rather than by config.json: starting over is
/// moving config.json aside, and that must not bring the old one back. Nor
/// may tokens the user has since disconnected.
fn adopt_config(dir: &Path, old: &str) -> bool {
    if dir.exists() {
        return false;
    }
    let Some(from) = dir.parent().map(|d| d.join(old).join("config.json")) else {
        return false;
    };
    if !from.is_file() {
        return false;
    }
    // Through a temp file and a rename, as every config write is (DISK-3):
    // a copy cut short would otherwise be read as unreadable, and set aside.
    let tmp = dir.join("config.json.adopting");
    std::fs::create_dir_all(dir).is_ok()
        && std::fs::copy(&from, &tmp).is_ok()
        && std::fs::rename(&tmp, dir.join("config.json")).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_build_remembers_the_identifier_it_had_before() {
        assert_eq!(identifier("eu.codevillain.villain-layer").as_deref(), Some("dev.villain.layer"));
        assert_eq!(identifier("eu.codevillain.villain-layer.dev").as_deref(), Some("dev.villain.layer.dev"));
        assert_eq!(identifier("com.example.other"), None);
    }

    #[test]
    fn a_first_launch_starts_from_the_config_saved_under_the_old_identifier() {
        let root = std::env::temp_dir().join(format!("vl-previous-{}", uuid::Uuid::new_v4()));
        let old = root.join("dev.villain.layer");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.json"), r#"{"version":2,"tasks":[]}"#).unwrap();

        let dir = root.join("eu.codevillain.villain-layer");
        assert!(adopt_config(&dir, "dev.villain.layer"));
        assert_eq!(std::fs::read_to_string(dir.join("config.json")).unwrap(), r#"{"version":2,"tasks":[]}"#);
        assert!(old.join("config.json").exists(), "copied, not moved");
        assert!(!dir.join("config.json.adopting").exists());

        // The second launch has a folder, and adopts nothing again.
        std::fs::remove_file(dir.join("config.json")).unwrap();
        assert!(!adopt_config(&dir, "dev.villain.layer"), "starting over stays started over");
        assert!(!dir.join("config.json").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_first_launch_with_nothing_before_it_creates_nothing() {
        let root = std::env::temp_dir().join(format!("vl-previous-{}", uuid::Uuid::new_v4()));
        let dir = root.join("eu.codevillain.villain-layer");
        assert!(!adopt_config(&dir, "dev.villain.layer"));
        assert!(!dir.exists(), "ConfigStore::load makes the folder, not this");
        std::fs::remove_dir_all(&root).ok();
    }
}
