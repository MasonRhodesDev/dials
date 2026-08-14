//! Load/save `~/.config/hypr/power.conf` via hyprstate-fsm parse.
use std::fs;
use std::path::PathBuf;

use hyprstate_fsm::power::{PowerPolicy, parse_power_policy};
use hyprstate_fsm::profiles::parse_directive;

use crate::sensors::hypr_dir;

pub fn power_conf_path() -> PathBuf {
    hypr_dir().join("power.conf")
}

pub fn power_override_path() -> PathBuf {
    hypr_dir().join("power-override")
}

pub fn load() -> (PowerPolicy, u8, Vec<String>) {
    match fs::read_to_string(power_conf_path()) {
        Ok(text) => parse_power_policy(&text),
        Err(_) => (
            PowerPolicy::default(),
            hyprstate_fsm::power::DEFAULT_BATTERY_LOW_PCT,
            Vec::new(),
        ),
    }
}

pub fn save(policy: &PowerPolicy, low_pct: u8) -> Result<(), String> {
    let path = power_conf_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let body = render(&existing, policy, low_pct);
    let tmp = path.with_extension("conf.tmp");
    fs::write(&tmp, body).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn override_profile() -> Option<String> {
    let text = fs::read_to_string(power_override_path()).ok()?;
    let word = text.split_whitespace().next()?;
    word.parse::<hyprstate_fsm::power::PowerProfile>()
        .ok()
        .map(|p| p.as_str().to_string())
}

/// `hyprstate power set <profile>|auto` or `hyprstate power cycle`.
/// Daemon stamps the override file; the GUI does not write it.
pub fn apply_override(action: &str) -> Result<(), String> {
    let mut cmd = std::process::Command::new("hyprstate");
    cmd.arg("power");
    if action == "cycle" {
        cmd.arg("cycle");
    } else {
        cmd.args(["set", action]);
    }
    let out = cmd.output().map_err(|e| format!("hyprstate: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let msg = err.trim();
    if msg.is_empty() {
        Err("hyprstate power failed".into())
    } else {
        Err(msg.to_string())
    }
}

pub fn applied_profile() -> Option<String> {
    let text = fs::read_to_string("/var/lib/hyprstate/profile").ok()?;
    let word = text.split_whitespace().next()?;
    word.parse::<hyprstate_fsm::power::PowerProfile>()
        .ok()
        .map(|p| p.as_str().to_string())
}

fn render(existing: &str, policy: &PowerPolicy, low_pct: u8) -> String {
    let mut kept = Vec::new();
    for line in existing.lines() {
        if let Some((key, _)) = parse_directive(line, true)
            && matches!(
                key,
                "docked-ac" | "ac" | "battery" | "battery-low" | "battery-low-percent"
            )
        {
            continue;
        }
        kept.push(line);
    }
    let mut out = String::new();
    if !existing.is_empty() && kept.iter().all(|l| l.is_empty()) {
        // fall through to directives only
    } else {
        for line in &kept {
            out.push_str(line);
            out.push('\n');
        }
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "#@ docked-ac = {}\n#@ ac = {}\n#@ battery = {}\n#@ battery-low = {}\n#@ battery-low-percent = {low_pct}\n",
        policy.docked_ac.as_str(),
        policy.ac.as_str(),
        policy.battery.as_str(),
        policy.battery_low.as_str(),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprstate_fsm::power::PowerProfile;

    #[test]
    fn render_replaces_known_keys_keeps_comments() {
        let existing = "# keep me\n#@ ac = performance\n#@ other = x\n";
        let mut policy = PowerPolicy::default();
        policy.ac = PowerProfile::PowerSaver;
        let out = render(existing, &policy, 20);
        assert!(out.contains("# keep me"));
        assert!(out.contains("#@ other = x"));
        assert!(out.contains("#@ ac = power-saver"));
        assert!(out.contains("#@ battery-low-percent = 20"));
        assert!(!out.contains("#@ ac = performance"));
    }
}
