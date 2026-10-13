//! `~/.config/lmtt/config.toml`, read and written without losing the user's
//! comments or unknown keys (lmtt's own `Config::save` drops both, which is
//! why hypr-de-theme avoids it too). Values live under dotted section paths
//! (`general`, `theme_profiles.light`).
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

pub fn config_path() -> Option<PathBuf> {
    let dirs = xdg_paths::ConfigDirs::from_env().ok()?;
    Some(dirs.config_dir("lmtt").join("config.toml"))
}

/// lmtt-config refreshes `~/.config/lmtt/config-schema.json` on every run;
/// a packaged copy under a data dir (`lmtt/config-schema.json`) is the
/// fallback for a box that never ran it.
pub fn schema_path() -> Option<PathBuf> {
    let dirs = xdg_paths::ConfigDirs::from_env().ok()?;
    let user = dirs.config_dir("lmtt").join("config-schema.json");
    if user.is_file() {
        return Some(user);
    }
    let xdg = std::env::var("XDG_DATA_DIRS").ok();
    crate::entries::data_dirs(dirs.data_home(), xdg.as_deref())
        .into_iter()
        .map(|d| d.join("lmtt/config-schema.json"))
        .find(|p| p.is_file())
}

/// Missing file → empty document (lmtt itself runs on defaults then).
pub fn load(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn table<'a>(doc: &'a DocumentMut, section: &str) -> Option<&'a Table> {
    let mut t = doc.as_table();
    for part in section.split('.') {
        t = t.get(part)?.as_table()?;
    }
    Some(t)
}

pub fn get(doc: &DocumentMut, section: &str, id: &str) -> Option<Value> {
    table(doc, section)?.get(id)?.as_value().cloned()
}

/// Create the section tables as needed; a new table is written explicitly
/// so the file stays readable, an existing one keeps its layout.
pub fn set(doc: &mut DocumentMut, section: &str, id: &str, value: Value) {
    let mut t = doc.as_table_mut();
    for part in section.split('.') {
        if !t.contains_table(part) {
            let mut fresh = Table::new();
            fresh.set_implicit(false);
            t.insert(part, Item::Table(fresh));
        }
        t = t[part].as_table_mut().expect("just inserted a table");
    }
    t.insert(id, Item::Value(value));
}

/// Temp file and rename, like power.conf.
pub fn save(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("toml.tmp-{}", std::process::id()));
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_unknown_keys() {
        let text = "# mine\n[general]\nwallpaper = \"~/w.png\" # keep\nscheme_type = \"scheme-tonal-spot\"\n\n[theme_profiles.dark]\ngtk_theme = \"Adwaita-dark\"\nextra = 1\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        assert_eq!(
            get(&doc, "general", "scheme_type").and_then(|v| v.as_str().map(str::to_string)),
            Some("scheme-tonal-spot".into())
        );
        assert_eq!(
            get(&doc, "theme_profiles.dark", "gtk_theme")
                .and_then(|v| v.as_str().map(str::to_string)),
            Some("Adwaita-dark".into())
        );
        assert!(get(&doc, "theme_profiles.light", "gtk_theme").is_none());
        set(
            &mut doc,
            "general",
            "scheme_type",
            Value::from("scheme-vibrant"),
        );
        set(
            &mut doc,
            "theme_profiles.light",
            "cursor_size",
            Value::from(32),
        );
        set(
            &mut doc,
            "theme_profiles.dark",
            "window_blur",
            Value::from(true),
        );
        let out = doc.to_string();
        assert!(out.starts_with("# mine\n"), "{out}");
        assert!(out.contains("wallpaper = \"~/w.png\" # keep\n"), "{out}");
        assert!(out.contains("scheme_type = \"scheme-vibrant\""), "{out}");
        assert!(out.contains("extra = 1"), "{out}");
        assert!(
            out.contains("[theme_profiles.light]\ncursor_size = 32"),
            "{out}"
        );
        assert!(out.contains("window_blur = true"), "{out}");
    }

    #[test]
    fn missing_file_is_an_empty_document_and_save_round_trips() {
        let dir = std::env::temp_dir().join(format!("dials-appearance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut doc = load(&path).unwrap();
        assert!(doc.to_string().is_empty());
        set(&mut doc, "general", "use_matugen", Value::from(false));
        save(&path, &doc).unwrap();
        let again = load(&path).unwrap();
        assert_eq!(
            get(&again, "general", "use_matugen").and_then(|v| v.as_bool()),
            Some(false)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
