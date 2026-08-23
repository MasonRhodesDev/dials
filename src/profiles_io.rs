//! Load/save profile TOML from shared + user dirs.
use std::fs;
use std::path::{Path, PathBuf};

use monitor_profiles::{Profile, load_dir, to_toml};

pub fn shared_dir() -> PathBuf {
    PathBuf::from("/etc/monitor-profiles")
}

pub fn user_dir() -> PathBuf {
    match xdg_paths::ConfigDirs::from_env() {
        Ok(dirs) => dirs.config_dir("hypr").join("profiles"),
        Err(_) => PathBuf::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `/etc/monitor-profiles` (greeter-shared)
    Shared,
    /// `~/.config/hypr/profiles` (per-user; wins on name)
    User,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListedProfile {
    pub profile: Profile,
    pub source: Source,
    pub path: PathBuf,
}

/// User overrides win on name collision (same as hyprstate).
pub fn load_merged() -> (Vec<ListedProfile>, Vec<String>) {
    let mut out = Vec::new();
    let mut diags = Vec::new();

    let (user, ud) = load_dir(&user_dir());
    for d in ud {
        diags.push(format!("{}: {}", d.source, d.message));
    }
    for p in user {
        let path = user_dir().join(format!("{}.toml", p.name));
        out.push(ListedProfile {
            profile: p,
            source: Source::User,
            path,
        });
    }

    let (shared, sd) = load_dir(&shared_dir());
    for d in sd {
        diags.push(format!("{}: {}", d.source, d.message));
    }
    for p in shared {
        if out.iter().any(|l| l.profile.name == p.name) {
            continue;
        }
        let path = shared_dir().join(format!("{}.toml", p.name));
        out.push(ListedProfile {
            profile: p,
            source: Source::Shared,
            path,
        });
    }

    out.sort_by(|a, b| a.profile.name.cmp(&b.profile.name));
    (out, diags)
}

pub fn write_atomic(path: &Path, profile: &Profile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = to_toml(profile);
    let tmp = path.with_extension(format!(
        "toml.tmp-{}",
        std::process::id()
    ));
    fs::write(&tmp, &content).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn shared_writable() -> bool {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(shared_dir().join(".write-test"))
        .and_then(|f| {
            drop(f);
            fs::remove_file(shared_dir().join(".write-test"))
        })
        .is_ok()
}

/// New profiles default to the user dir (no perms surprises).
pub fn user_profile_path(name: &str) -> PathBuf {
    user_dir().join(format!("{name}.toml"))
}

pub fn shared_profile_path(name: &str) -> PathBuf {
    shared_dir().join(format!("{name}.toml"))
}

pub fn remove_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
