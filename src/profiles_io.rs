//! Load/save profile TOML from system + user dirs.
use std::fs;
use std::path::{Path, PathBuf};

use monitor_profiles::{Profile, from_toml, load_dir, to_toml};

pub fn system_dir() -> PathBuf {
    PathBuf::from("/etc/monitor-profiles")
}

pub fn user_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hypr/profiles")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    System,
    User,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
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

    let (system, sd) = load_dir(&system_dir());
    for d in sd {
        diags.push(format!("{}: {}", d.source, d.message));
    }
    for p in system {
        if out.iter().any(|l| l.profile.name == p.name) {
            continue;
        }
        let path = system_dir().join(format!("{}.toml", p.name));
        out.push(ListedProfile {
            profile: p,
            source: Source::System,
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

pub fn system_writable() -> bool {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(system_dir().join(".write-test"))
        .and_then(|f| {
            drop(f);
            fs::remove_file(system_dir().join(".write-test"))
        })
        .is_ok()
}

pub fn capture_path(name: &str) -> PathBuf {
    if system_writable() {
        system_dir().join(format!("{name}.toml"))
    } else {
        user_dir().join(format!("{name}.toml"))
    }
}

pub fn parse_file(path: &Path, name: &str) -> Result<Profile, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    from_toml(name, &text)
        .map(|(p, _)| p)
        .map_err(|e| e.to_string())
}
