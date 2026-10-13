//! hyprctl monitors -j → connected outputs + modes.
use monitor_profiles::ConnectedOutput;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct HyprMonitor {
    pub name: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub transform: u8,
    pub scale: f64,
    #[serde(rename = "availableModes", default)]
    pub available_modes: Vec<String>,
    #[serde(rename = "refreshRate", default)]
    pub refresh_rate: f64,
}

pub fn monitors() -> Result<Vec<HyprMonitor>, String> {
    let out = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .map_err(|e| format!("hyprctl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "hyprctl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("hyprctl json: {e}"))
}

pub fn connected(monitors: &[HyprMonitor]) -> Vec<ConnectedOutput> {
    monitors
        .iter()
        .map(|m| ConnectedOutput {
            name: m.name.clone(),
            description: if m.description.is_empty() {
                m.name.clone()
            } else {
                m.description.clone()
            },
        })
        .collect()
}

pub fn signature(monitors: &[HyprMonitor]) -> Vec<String> {
    monitors
        .iter()
        .map(|m| {
            if m.description.is_empty() {
                m.name.clone()
            } else {
                m.description.clone()
            }
        })
        .collect()
}

pub fn humanize_signature(monitors: &[HyprMonitor]) -> String {
    if monitors.is_empty() {
        return "Live: (no monitors)".into();
    }
    let parts: Vec<String> = monitors
        .iter()
        .map(|m| {
            let label = m
                .description
                .split_whitespace()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" ");
            if label.is_empty() {
                m.name.clone()
            } else {
                format!("{label} ({})", m.name)
            }
        })
        .collect();
    format!("Live: {}", parts.join(" + "))
}
