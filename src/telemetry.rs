//! Listen for hyprstate daemon telemetry (NDJSON on a Unix socket).
//! The daemon connects as a client; this module is the server.
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

/// Live Help snapshot from the last telemetry frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpLive {
    pub have_frame: bool,
    pub kind: String,
    pub event: String,
    pub to: String,
    pub lid_closed: bool,
    pub ext_mon_count: u32,
    pub inhibitor: bool,
    pub on_ac: bool,
    pub low_battery: bool,
    pub power_base: String,
    pub desired_profile: String,
    pub applied_profile: String,
    pub active_profile: String,
}

#[derive(Debug, Deserialize)]
struct WireFrame {
    kind: String,
    event: String,
    to: String,
    ctx: WireCtx,
}

#[derive(Debug, Deserialize)]
struct WireCtx {
    lid_closed: bool,
    ext_mon_count: u32,
    inhibitor: bool,
    on_ac: bool,
    #[serde(default)]
    low_battery: bool,
    #[serde(default)]
    power_base: String,
    #[serde(default)]
    desired_profile: String,
    #[serde(default)]
    applied_profile: String,
    #[serde(default)]
    active_profile: String,
}

impl HelpLive {
    fn from_wire(f: WireFrame) -> Self {
        Self {
            have_frame: true,
            kind: f.kind,
            event: f.event,
            to: f.to,
            lid_closed: f.ctx.lid_closed,
            ext_mon_count: f.ctx.ext_mon_count,
            inhibitor: f.ctx.inhibitor,
            on_ac: f.ctx.on_ac,
            low_battery: f.ctx.low_battery,
            power_base: f.ctx.power_base,
            desired_profile: f.ctx.desired_profile,
            applied_profile: f.ctx.applied_profile,
            active_profile: f.ctx.active_profile,
        }
    }
}

pub fn sock_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hyprstate-telemetry.sock")
}

/// Parse one NDJSON line into HelpLive.
pub fn parse_line(line: &str) -> Option<HelpLive> {
    let f: WireFrame = serde_json::from_str(line.trim()).ok()?;
    Some(HelpLive::from_wire(f))
}

/// Bind the telemetry socket and forward frames on `tx`.
/// Returns immediately after spawning the listener thread.
pub fn spawn(tx: mpsc::Sender<HelpLive>) {
    thread::Builder::new()
        .name("hyprstate-telem".into())
        .spawn(move || listen_loop(tx))
        .expect("spawn telemetry listener");
}

fn listen_loop(tx: mpsc::Sender<HelpLive>) {
    let path = sock_path();
    loop {
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(_) => {
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let _ = listener.set_nonblocking(false);
        while let Ok((stream, _)) = listener.accept() {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                let Some(live) = parse_line(&line) else {
                    continue;
                };
                if tx.send(live).is_err() {
                    return;
                }
            }
        }
        // Listener died or bind lost — retry.
        let _ = std::fs::remove_file(&path);
        thread::sleep(Duration::from_millis(500));
    }
}

/// Build match-row lighting from daemon active_profile + local match list.
pub fn display_rows(
    matches: &[(String, String, String, bool)],
    active_profile: &str,
) -> Vec<(String, String, String, bool)> {
    if active_profile.is_empty() {
        return matches
            .iter()
            .map(|(id, c, l, _)| (id.clone(), c.clone(), l.clone(), false))
            .collect();
    }
    let mut out: Vec<(String, String, String, bool)> = matches
        .iter()
        .map(|(id, _c, l, _)| {
            let name = l.split(" · ").next().unwrap_or(l.as_str());
            let selected = name == active_profile;
            let caption = if selected {
                "selected".to_string()
            } else {
                "also matches".to_string()
            };
            (id.clone(), caption, l.clone(), selected)
        })
        .collect();
    let any = out.iter().any(|r| r.3);
    if !any {
        out.insert(
            0,
            (
                "m-active".into(),
                "selected".into(),
                format!("{active_profile} · from daemon"),
                true,
            ),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_expanded_frame() {
        let line = r#"{"ts":1,"kind":"transition","from":"LID_OPEN","event":"LidClose","to":"COUNTDOWN","screen":"SCREEN_ACTIVE","ctx":{"lid_closed":true,"ext_mon_count":0,"inhibitor":false,"locked":false,"on_ac":true,"low_battery":false,"power_base":"ac","desired_profile":"balanced","applied_profile":"balanced","active_profile":"dual"},"effectors":["start_grace_timer"]}"#;
        let live = parse_line(line).unwrap();
        assert!(live.have_frame);
        assert_eq!(live.to, "COUNTDOWN");
        assert!(live.lid_closed);
        assert_eq!(live.power_base, "ac");
        assert_eq!(live.active_profile, "dual");
    }

    #[test]
    fn display_rows_lights_active_profile() {
        let matches = [
            (
                "m0".into(),
                "selected".into(),
                "two · 2 prefixes · priority 2".into(),
                true,
            ),
            (
                "m1".into(),
                "also matches".into(),
                "one · 1 prefixes · priority 1".into(),
                false,
            ),
        ];
        let rows = display_rows(&matches, "one");
        assert!(rows.iter().any(|r| r.3 && r.2.starts_with("one")));
        assert!(!rows.iter().any(|r| r.3 && r.2.starts_with("two")));
    }
}
