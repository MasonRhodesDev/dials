//! Lid / AC / battery discovery. Empty pick = Auto (same defaults as hyprstate).
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const LID_DIR: &str = "/proc/acpi/button/lid";
const SUPPLY_DIR: &str = "/sys/class/power_supply";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SensorPicks {
    #[serde(default)]
    pub lid: String,
    #[serde(default)]
    pub ac: String,
    #[serde(default)]
    pub battery: String,
}

impl SensorPicks {
    pub fn auto(value: &str) -> bool {
        value.is_empty() || value.eq_ignore_ascii_case("auto")
    }
}

pub fn index_of(options: &[SensorOption], pick: &str) -> i32 {
    if SensorPicks::auto(pick) {
        return 0;
    }
    options
        .iter()
        .position(|o| o.id == pick)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

pub fn persist_from_index(options: &[SensorOption], index: i32) -> String {
    let Ok(i) = usize::try_from(index) else {
        return String::new();
    };
    match options.get(i).map(|o| o.id.as_str()) {
        Some("auto") | None => String::new(),
        Some(id) => id.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct SensorOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct SensorReading {
    pub lid_closed: bool,
    pub lid_source: String,
    pub on_ac: Option<bool>,
    pub ac_source: String,
    pub battery_pct: Option<f64>,
    pub battery_source: String,
    pub lid_options: Vec<SensorOption>,
    pub ac_options: Vec<SensorOption>,
    pub battery_options: Vec<SensorOption>,
}

pub fn config_path() -> PathBuf {
    hypr_dir().join("hyprstate-gui.json")
}

pub fn hypr_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hypr")
}

pub fn load_picks() -> SensorPicks {
    let text = fs::read_to_string(config_path()).unwrap_or_default();
    if text.is_empty() {
        return SensorPicks::default();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_picks(picks: &SensorPicks) -> Result<(), String> {
    let dir = hypr_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = config_path();
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(picks).map_err(|e| e.to_string())?;
    fs::write(&tmp, body).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn read(picks: &SensorPicks) -> SensorReading {
    let acpi_lids = list_acpi_lids();
    let logind = logind_lid_closed();
    let supplies = list_supplies();

    let (lid_closed, lid_source) = resolve_lid(&picks.lid, logind, &acpi_lids);
    let (on_ac, ac_source) = resolve_ac(&picks.ac, &supplies);
    let (battery_pct, battery_source) = resolve_battery(&picks.battery, &supplies);

    let mut lid_options = vec![SensorOption {
        id: "auto".into(),
        label: format!("Auto ({lid_source})"),
    }];
    if logind.is_some() {
        lid_options.push(SensorOption {
            id: "logind".into(),
            label: format!(
                "logind ({})",
                if logind == Some(true) {
                    "closed"
                } else {
                    "open"
                }
            ),
        });
    }
    for (name, closed) in &acpi_lids {
        lid_options.push(SensorOption {
            id: name.clone(),
            label: format!("{name} ({})", if *closed { "closed" } else { "open" }),
        });
    }

    let mut ac_options = vec![SensorOption {
        id: "auto".into(),
        label: format!("Auto ({ac_source})"),
    }];
    for s in supplies.iter().filter(|s| s.mains) {
        ac_options.push(SensorOption {
            id: s.name.clone(),
            label: format!(
                "{} ({})",
                s.name,
                if s.online == Some(true) {
                    "online"
                } else {
                    "offline"
                }
            ),
        });
    }

    let mut battery_options = vec![SensorOption {
        id: "auto".into(),
        label: format!("Auto ({battery_source})"),
    }];
    for s in supplies.iter().filter(|s| s.battery) {
        let pct = s
            .capacity
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "n/a".into());
        battery_options.push(SensorOption {
            id: s.name.clone(),
            label: format!("{} ({pct})", s.name),
        });
    }

    SensorReading {
        lid_closed,
        lid_source,
        on_ac,
        ac_source,
        battery_pct,
        battery_source,
        lid_options,
        ac_options,
        battery_options,
    }
}

#[derive(Debug, Clone)]
struct Supply {
    name: String,
    mains: bool,
    battery: bool,
    online: Option<bool>,
    capacity: Option<f64>,
}

fn list_acpi_lids() -> Vec<(String, bool)> {
    let Ok(entries) = fs::read_dir(LID_DIR) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(text) = fs::read_to_string(entry.path().join("state")) else {
            continue;
        };
        out.push((name, text.contains("closed")));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn logind_lid_closed() -> Option<bool> {
    const TTL: Duration = Duration::from_millis(200);
    static CACHE: Mutex<Option<(Instant, Option<bool>)>> = Mutex::new(None);
    let now = Instant::now();
    if let Ok(guard) = CACHE.lock()
        && let Some((at, value)) = *guard
        && now.saturating_duration_since(at) < TTL
    {
        return value;
    }
    let value = logind_lid_query();
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((now, value));
    }
    value
}

fn logind_lid_query() -> Option<bool> {
    let out = std::process::Command::new("busctl")
        .args([
            "--system",
            "get-property",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "LidClosed",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("true") {
        Some(true)
    } else if text.contains("false") {
        Some(false)
    } else {
        None
    }
}

fn list_supplies() -> Vec<Supply> {
    let Ok(entries) = fs::read_dir(SUPPLY_DIR) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let kind = read_trim(&path.join("type"));
        let mains = kind.eq_ignore_ascii_case("Mains") || name.starts_with('A');
        let battery = kind.eq_ignore_ascii_case("Battery");
        if !mains && !battery {
            continue;
        }
        let online = read_trim(&path.join("online"))
            .parse::<u8>()
            .ok()
            .map(|v| v == 1);
        let capacity = read_trim(&path.join("capacity")).parse::<f64>().ok();
        out.push(Supply {
            name,
            mains,
            battery,
            online,
            capacity,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn resolve_lid(pick: &str, logind: Option<bool>, acpi: &[(String, bool)]) -> (bool, String) {
    if !SensorPicks::auto(pick) {
        if pick.eq_ignore_ascii_case("logind") {
            if let Some(closed) = logind {
                return (closed, "logind".into());
            }
        } else if let Some((_, closed)) = acpi.iter().find(|(n, _)| n == pick) {
            return (*closed, pick.to_string());
        }
    }
    if let Some(closed) = logind {
        return (closed, "logind".into());
    }
    if let Some((name, closed)) = acpi.first() {
        return (*closed, name.clone());
    }
    (false, "none".into())
}

/// Same OR as the daemon: a real logind idle/sleep block, or hypridle inhibit locks > 0.
pub fn read_inhibitor() -> (bool, String) {
    const TTL: Duration = Duration::from_millis(200);
    static CACHE: Mutex<Option<(Instant, bool, String)>> = Mutex::new(None);
    let now = Instant::now();
    if let Ok(guard) = CACHE.lock()
        && let Some((at, active, body)) = guard.as_ref()
        && now.saturating_duration_since(*at) < TTL
    {
        return (*active, body.clone());
    }
    let logind_who = logind_idle_who();
    let locks = wayland_inhibit_locks();
    let active = logind_who.is_some() || locks.is_some_and(|n| n > 0);
    let body = inhibitor_body(logind_who.as_deref(), locks);
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((now, active, body.clone()));
    }
    (active, body)
}

fn inhibitor_body(logind_who: Option<&str>, locks: Option<u64>) -> String {
    match (logind_who, locks) {
        (Some(who), Some(n)) if n > 0 => format!("held · hypridle {n} · logind {who}"),
        (Some(who), _) => format!("held · logind {who}"),
        (_, Some(n)) if n > 0 => {
            if n == 1 {
                "held · hypridle 1 lock".into()
            } else {
                format!("held · hypridle {n} locks")
            }
        }
        (_, Some(_)) => "idle · hypridle 0 locks".into(),
        _ => "idle · none".into(),
    }
}

const INHIBIT_BASELINE_WHO: &[&str] = &[
    "ModemManager",
    "NetworkManager",
    "UPower",
    "hypridle",
    "hyprstate",
    "hypr-power",
    "hypr-fsm",
];

fn logind_idle_who() -> Option<String> {
    let out = std::process::Command::new("busctl")
        .args([
            "--json=short",
            "--system",
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "ListInhibitors",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let parsed: BusctlInhibitors = serde_json::from_slice(&out.stdout).ok()?;
    let rows = parsed.data.into_iter().next()?;
    first_real_inhibitor(&rows)
}

#[derive(Deserialize)]
struct BusctlInhibitors {
    data: Vec<Vec<(String, String, String, String, u32, u32)>>,
}

fn first_real_inhibitor(rows: &[(String, String, String, String, u32, u32)]) -> Option<String> {
    for (what, who, _why, mode, _uid, _pid) in rows {
        if mode != "block" {
            continue;
        }
        let idle_or_sleep = what.split(':').any(|c| c == "idle" || c == "sleep");
        if !idle_or_sleep {
            continue;
        }
        if INHIBIT_BASELINE_WHO.contains(&who.as_str()) {
            continue;
        }
        return Some(who.clone());
    }
    None
}

fn wayland_inhibit_locks() -> Option<u64> {
    if let Some(n) = parse_inhibit_locks(&hypridle_journal_tail()) {
        return Some(n);
    }
    parse_inhibit_locks(&hypridle_log_tail())
}

fn hypridle_journal_tail() -> String {
    let out = std::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            "hypridle",
            "-n",
            "40",
            "--output=cat",
            "--no-pager",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

fn hypridle_log_tail() -> String {
    let path = hypr_dir().join("logs/hypridle.log");
    read_tail(&path, 8192).unwrap_or_default()
}

fn read_tail(path: &Path, bytes: u64) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let size = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(size.saturating_sub(bytes)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn parse_inhibit_locks(text: &str) -> Option<u64> {
    let mut latest = None;
    for line in text.lines() {
        let Some(idx) = line.find("Inhibit locks:") else {
            continue;
        };
        let rest = line[idx + "Inhibit locks:".len()..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            latest = Some(n);
        }
    }
    latest
}

fn resolve_ac(pick: &str, supplies: &[Supply]) -> (Option<bool>, String) {
    let mains: Vec<&Supply> = supplies.iter().filter(|s| s.mains).collect();
    if !SensorPicks::auto(pick)
        && let Some(s) = mains.iter().find(|s| s.name == pick)
    {
        return (s.online, s.name.clone());
    }
    if let Some(s) = mains
        .iter()
        .find(|s| s.name.starts_with('A') && s.online.is_some())
    {
        return (s.online, s.name.clone());
    }
    (None, "none".into())
}

fn resolve_battery(pick: &str, supplies: &[Supply]) -> (Option<f64>, String) {
    let bats: Vec<&Supply> = supplies.iter().filter(|s| s.battery).collect();
    if !SensorPicks::auto(pick)
        && let Some(s) = bats.iter().find(|s| s.name == pick)
    {
        return (s.capacity, s.name.clone());
    }
    if let Some(s) = bats.iter().find(|s| s.capacity.is_some()) {
        return (s.capacity, s.name.clone());
    }
    if let Some(s) = bats.first() {
        return (s.capacity, s.name.clone());
    }
    (None, "none".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_lid_prefers_logind() {
        let acpi = vec![("LID0".into(), true)];
        let (closed, src) = resolve_lid("auto", Some(false), &acpi);
        assert!(!closed);
        assert_eq!(src, "logind");
    }

    #[test]
    fn explicit_acpi_lid_overrides_logind() {
        let acpi = vec![("LID0".into(), true)];
        let (closed, src) = resolve_lid("LID0", Some(false), &acpi);
        assert!(closed);
        assert_eq!(src, "LID0");
    }

    #[test]
    fn auto_ac_prefers_name_starting_a() {
        let supplies = vec![
            Supply {
                name: "ADP1".into(),
                mains: true,
                battery: false,
                online: Some(false),
                capacity: None,
            },
            Supply {
                name: "ACAD".into(),
                mains: true,
                battery: false,
                online: Some(true),
                capacity: None,
            },
        ];
        let (on, src) = resolve_ac("auto", &supplies);
        assert_eq!(on, Some(false));
        assert_eq!(src, "ADP1");
    }

    #[test]
    fn auto_ac_ignores_non_a_name() {
        let supplies = vec![Supply {
            name: "Mains".into(),
            mains: true,
            battery: false,
            online: Some(true),
            capacity: None,
        }];
        let (on, src) = resolve_ac("auto", &supplies);
        assert_eq!(on, None);
        assert_eq!(src, "none");
    }

    #[test]
    fn persist_auto_clears_pick() {
        let opts = vec![
            SensorOption {
                id: "auto".into(),
                label: "Auto (logind)".into(),
            },
            SensorOption {
                id: "LID0".into(),
                label: "LID0 (open)".into(),
            },
        ];
        assert_eq!(persist_from_index(&opts, 0), "");
        assert_eq!(persist_from_index(&opts, 1), "LID0");
        assert_eq!(index_of(&opts, ""), 0);
        assert_eq!(index_of(&opts, "LID0"), 1);
    }

    #[test]
    fn parse_inhibit_locks_last_marker_wins() {
        let text = "[LOG] Inhibit locks: 1\n[LOG] Ignoring from onIdled(), inhibit locks: 9\n[LOG] Inhibit locks: 2\n";
        assert_eq!(parse_inhibit_locks(text), Some(2));
        assert_eq!(parse_inhibit_locks("no marker"), None);
    }

    #[test]
    fn logind_skips_delay_baseline_and_lid_switch() {
        let rows = vec![
            (
                "sleep".into(),
                "NetworkManager".into(),
                "networks".into(),
                "delay".into(),
                0,
                1,
            ),
            (
                "handle-lid-switch".into(),
                "hyprstate".into(),
                "grace".into(),
                "block".into(),
                1000,
                2,
            ),
            (
                "idle".into(),
                "firefox".into(),
                "video".into(),
                "block".into(),
                1000,
                3,
            ),
        ];
        assert_eq!(first_real_inhibitor(&rows).as_deref(), Some("firefox"));
    }
}
