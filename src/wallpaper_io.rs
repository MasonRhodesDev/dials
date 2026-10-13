//! Background (wallpaper) page IO.
//!
//! Reads come from the appearance-profiles registry, the file contract vigil
//! and lmtt share. Writes go only through the `lmtt` CLI: lmtt owns the
//! registry and publishes the greeter-readable snapshot, so dials never
//! edits the TOML itself and never calls hypr-DE's wrapper script (hypr-DE
//! depends on dials; calling back would invert that).
//!
//! Convention kept here: the output at logical (0,0) — the origin — *is*
//! the global wallpaper. Choosing on the origin writes `[background]` and
//! removes any override the origin had, so the two never drift.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use appearance_profiles::{OutputIdentity, Registry, Source};

/// A connected output as hyprctl reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub connector: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
}

/// What one output resolves to in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: Option<PathBuf>,
    /// Layer the path came from: user | system | packaged | none.
    pub layer: &'static str,
    /// The path came from a per-output rule (any layer), not `[background]`.
    pub is_override: bool,
    /// Keys in the *user* profile's `[output.*]` that match this output;
    /// what `lmtt wallpaper unset` has to remove.
    pub override_keys: Vec<String>,
}

impl Default for Resolved {
    fn default() -> Self {
        Self {
            path: None,
            layer: "none",
            is_override: false,
            override_keys: Vec::new(),
        }
    }
}

/// The page's model: the origin/global row plus every other output.
#[derive(Debug, Clone, Default)]
pub struct Current {
    /// None when no outputs are known (no hyprctl).
    pub origin: Option<OutputInfo>,
    /// Resolved for the origin's identity (or `"default"` without outputs).
    pub global: Resolved,
    pub others: Vec<(OutputInfo, Resolved)>,
    /// Every `[output.*]` key in the user profile; switching per-display off
    /// removes them all.
    pub all_override_keys: Vec<String>,
    pub error: Option<String>,
}

/// One round of lmtt writes: an optional set (selector None = global), then
/// unsets, then a `switch` in the current mode so the palette follows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LmttJob {
    pub set: Option<(PathBuf, Option<String>)>,
    pub unset: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// Mode was not `dark`/`light`; nothing was run (a bare `lmtt switch`
    /// would toggle the theme).
    BadMode,
    Set(String),
    Unset(String),
    Switch(String),
    /// The installed lmtt predates `wallpaper unset`.
    TooOld,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMode => write!(f, "theme mode unknown; not switching"),
            Self::Set(e) => write!(f, "{e}"),
            Self::Unset(e) => write!(f, "{e}"),
            Self::Switch(e) => write!(f, "Wallpaper set; theme re-render failed: {e}"),
            Self::TooOld => write!(
                f,
                "The installed lmtt is too old for per-display changes (needs lmtt wallpaper unset)"
            ),
        }
    }
}

/// The origin output: at (0,0), else the top-left-most. None when empty.
pub fn origin_of(outputs: &[OutputInfo]) -> Option<usize> {
    if let Some(i) = outputs.iter().position(|o| o.x == 0 && o.y == 0) {
        return Some(i);
    }
    outputs
        .iter()
        .enumerate()
        .min_by_key(|(_, o)| (o.x, o.y))
        .map(|(i, _)| i)
}

/// Registry key dials writes for an output: the full hyprctl description
/// (stable across ports; lmtt's own convention), else the connector.
pub fn selector_for(connector: &str, description: &str) -> String {
    if description.is_empty() {
        connector.to_string()
    } else {
        format!("desc:{description}")
    }
}

fn identity_for(output: &OutputInfo) -> OutputIdentity {
    let description = (!output.description.is_empty()).then(|| output.description.clone());
    OutputIdentity::new(output.connector.clone(), description)
}

fn layer_of(source: Source) -> (&'static str, bool) {
    match source {
        Source::UserOutput => ("user", true),
        Source::User => ("user", false),
        Source::SystemOutput => ("system", true),
        Source::System => ("system", false),
        Source::PackagedOutput => ("packaged", true),
        Source::Packaged => ("packaged", false),
        Source::Runtime => ("runtime", false),
        Source::Default => ("none", false),
    }
}

/// User-profile keys that match this output, in the order resolve tries them.
fn override_keys_for(registry: &Registry, output: &OutputInfo) -> Vec<String> {
    let Some(user) = registry.user.as_ref() else {
        return Vec::new();
    };
    let mut keys = vec![output.connector.clone()];
    if !output.description.is_empty() {
        keys.push(format!("desc:{}", output.description));
    }
    keys.retain(|k| user.output.contains_key(k));
    keys
}

fn resolve_for(registry: &Registry, output: &OutputInfo) -> Resolved {
    let r = registry.resolve(&identity_for(output), None);
    let (layer, is_override) = layer_of(r.path_source);
    Resolved {
        path: r.path,
        layer,
        is_override,
        override_keys: override_keys_for(registry, output),
    }
}

/// Pure classification of a loaded registry against the live outputs.
pub fn classify(registry: &Registry, outputs: &[OutputInfo]) -> Current {
    let origin_idx = origin_of(outputs);
    let global = match origin_idx {
        Some(i) => resolve_for(registry, &outputs[i]),
        None => {
            let r = registry.resolve(&OutputIdentity::new("default", None), None);
            let (layer, is_override) = layer_of(r.path_source);
            Resolved {
                path: r.path,
                layer,
                is_override,
                override_keys: Vec::new(),
            }
        }
    };
    let others = outputs
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != origin_idx)
        .map(|(_, o)| (o.clone(), resolve_for(registry, o)))
        .collect();
    let all_override_keys = registry
        .user
        .as_ref()
        .map(|p| p.output.keys().cloned().collect())
        .unwrap_or_default();
    Current {
        origin: origin_idx.map(|i| outputs[i].clone()),
        global,
        others,
        all_override_keys,
        error: None,
    }
}

/// Load the current user's registry and classify. A broken layer is reported
/// in `error` with every row unresolved; the page still renders.
pub fn current(outputs: &[OutputInfo]) -> Current {
    match Registry::load_current_user() {
        Ok(registry) => classify(&registry, outputs),
        Err(e) => {
            let origin_idx = origin_of(outputs);
            Current {
                origin: origin_idx.map(|i| outputs[i].clone()),
                global: Resolved::default(),
                others: outputs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| Some(*i) != origin_idx)
                    .map(|(_, o)| (o.clone(), Resolved::default()))
                    .collect(),
                all_override_keys: Vec::new(),
                error: Some(format!("appearance profile: {e}")),
            }
        }
    }
}

pub fn lmtt_available() -> bool {
    crate::entries::on_path("lmtt")
}

/// `lmtt status` → the current theme mode (`dark`|`light`). lmtt's own
/// `theme_state`, the same line hypr-de-set-wallpaper reads.
pub fn current_mode_with(lmtt: &Path) -> Option<String> {
    let out = Command::new(lmtt)
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Current theme:"))
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| m == "dark" || m == "light")
}

/// Run one job: `wallpaper set`, then each `wallpaper unset`, then
/// `switch <mode> --no-notify`. Refuses to run at all without a known mode.
pub fn run_job_with(lmtt: &Path, job: &LmttJob, mode: &str) -> Result<(), ApplyError> {
    if mode != "dark" && mode != "light" {
        return Err(ApplyError::BadMode);
    }
    if job.set.is_none() && job.unset.is_empty() {
        return Ok(());
    }
    if let Some((path, selector)) = &job.set {
        let mut args: Vec<OsString> = vec!["wallpaper".into(), "set".into(), path.into()];
        if let Some(selector) = selector {
            args.push("--output".into());
            args.push(selector.into());
        }
        run(lmtt, &args).map_err(ApplyError::Set)?;
    }
    for key in &job.unset {
        let args: Vec<OsString> = vec![
            "wallpaper".into(),
            "unset".into(),
            "--output".into(),
            key.into(),
        ];
        run(lmtt, &args).map_err(|e| {
            if e.contains("unrecognized subcommand") {
                ApplyError::TooOld
            } else {
                ApplyError::Unset(e)
            }
        })?;
    }
    let args: Vec<OsString> = vec!["switch".into(), mode.into(), "--no-notify".into()];
    run(lmtt, &args).map_err(ApplyError::Switch)
}

/// Trimmed stderr on failure, like `power_io::apply_override`.
fn run(lmtt: &Path, args: &[OsString]) -> Result<(), String> {
    let out = Command::new(lmtt)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", lmtt.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.is_empty() {
        Err(format!(
            "lmtt {} failed ({})",
            args[0].to_string_lossy(),
            out.status
        ))
    } else {
        Err(err)
    }
}

/// Decode and downscale for a row preview. Worker thread only: a 4K source
/// takes real time and memory; the UI thread gets `(w, h, rgba)` back.
pub fn decode_preview(path: &Path, max_edge: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let thumb = img.thumbnail(max_edge, max_edge).to_rgba8();
    let (w, h) = thumb.dimensions();
    Ok((w, h, thumb.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appearance_profiles::{Background, Profile};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DIR_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dials-wallpaper-{}-{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn out(connector: &str, description: &str, x: i32, y: i32) -> OutputInfo {
        OutputInfo {
            connector: connector.into(),
            description: description.into(),
            x,
            y,
        }
    }

    fn bg(path: &str) -> Background {
        Background {
            path: Some(PathBuf::from(path)),
            fit: None,
        }
    }

    /// A stub `lmtt` that logs one argument per line and `--` per call.
    /// Boundaries are provable, so a path with a space is a real test.
    fn stub(dir: &Path, prelude: &str) -> (PathBuf, PathBuf) {
        let log = dir.join("argv.log");
        let exe = dir.join("lmtt");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\n{prelude}\nprintf '%s\\n' \"$@\" >> '{log}'\nprintf -- '--\\n' >> '{log}'\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        (exe, log)
    }

    fn calls(log: &Path) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let mut calls = Vec::new();
        let mut cur = Vec::new();
        for line in text.lines() {
            if line == "--" {
                calls.push(std::mem::take(&mut cur));
            } else {
                cur.push(line.to_string());
            }
        }
        calls
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn origin_is_the_output_at_zero() {
        let outs = [out("DP-2", "", 1920, 0), out("DP-1", "", 0, 0)];
        assert_eq!(origin_of(&outs), Some(1));
        let outs = [out("DP-2", "", 1920, 0), out("DP-1", "", 100, 50)];
        assert_eq!(origin_of(&outs), Some(1));
        assert_eq!(origin_of(&[]), None);
    }

    #[test]
    fn selector_prefers_full_description() {
        assert_eq!(
            selector_for("DP-1", "Dell U2720Q ABC12345"),
            "desc:Dell U2720Q ABC12345"
        );
        assert_eq!(selector_for("DP-1", ""), "DP-1");
    }

    #[test]
    fn classify_reports_origin_global_and_overrides() {
        let mut output = BTreeMap::new();
        output.insert("desc:X".to_string(), bg("/w/x.png"));
        output.insert("DP-1".to_string(), bg("/w/stray.png"));
        let profile = Profile {
            version: 1,
            background: bg("/w/global.png"),
            output,
        };
        let registry = Registry {
            user: Some(profile),
            ..Default::default()
        };
        let outs = [
            out("DP-3", "Third", 3840, 0),
            out("DP-1", "Dell", 0, 0),
            out("DP-2", "X", 1920, 0),
        ];
        let cur = classify(&registry, &outs);
        assert_eq!(
            cur.origin.as_ref().map(|o| o.connector.as_str()),
            Some("DP-1")
        );
        // The stray case: the origin carries its own override.
        assert_eq!(cur.global.path.as_deref(), Some(Path::new("/w/stray.png")));
        assert!(cur.global.is_override);
        assert_eq!(cur.global.layer, "user");
        assert_eq!(cur.global.override_keys, vec!["DP-1"]);
        let by_name = |n: &str| {
            cur.others
                .iter()
                .find(|(o, _)| o.connector == n)
                .map(|(_, r)| r.clone())
                .unwrap()
        };
        let x = by_name("DP-2");
        assert_eq!(x.path.as_deref(), Some(Path::new("/w/x.png")));
        assert!(x.is_override);
        assert_eq!(x.override_keys, vec!["desc:X"]);
        let third = by_name("DP-3");
        assert_eq!(third.path.as_deref(), Some(Path::new("/w/global.png")));
        assert!(!third.is_override);
        assert!(third.override_keys.is_empty());
        assert_eq!(cur.all_override_keys, vec!["DP-1", "desc:X"]);

        let empty = classify(&Registry::default(), &outs);
        assert_eq!(empty.global.layer, "none");
        assert!(empty.global.path.is_none());

        let none = classify(&registry, &[]);
        assert!(none.origin.is_none());
        assert_eq!(
            none.global.path.as_deref(),
            Some(Path::new("/w/global.png"))
        );
        assert!(!none.global.is_override);
    }

    #[test]
    fn job_records_exact_argv() {
        let dir = tmp();
        let (exe, log) = stub(&dir, "");
        let global = LmttJob {
            set: Some((PathBuf::from("/x/my wall.png"), None)),
            unset: vec!["DP-1".into()],
        };
        run_job_with(&exe, &global, "dark").unwrap();
        assert_eq!(
            calls(&log),
            vec![
                argv(&["wallpaper", "set", "/x/my wall.png"]),
                argv(&["wallpaper", "unset", "--output", "DP-1"]),
                argv(&["switch", "dark", "--no-notify"]),
            ]
        );

        std::fs::remove_file(&log).unwrap();
        let output = LmttJob {
            set: Some((
                PathBuf::from("/x/my wall.png"),
                Some("desc:Dell U2720Q".into()),
            )),
            unset: vec![],
        };
        run_job_with(&exe, &output, "light").unwrap();
        assert_eq!(
            calls(&log),
            vec![
                argv(&[
                    "wallpaper",
                    "set",
                    "/x/my wall.png",
                    "--output",
                    "desc:Dell U2720Q"
                ]),
                argv(&["switch", "light", "--no-notify"]),
            ]
        );

        std::fs::remove_file(&log).unwrap();
        let clear = LmttJob {
            set: None,
            unset: vec!["DP-2".into(), "desc:Dell U2720Q".into()],
        };
        run_job_with(&exe, &clear, "dark").unwrap();
        let got = calls(&log);
        assert_eq!(
            got,
            vec![
                argv(&["wallpaper", "unset", "--output", "DP-2"]),
                argv(&["wallpaper", "unset", "--output", "desc:Dell U2720Q"]),
                argv(&["switch", "dark", "--no-notify"]),
            ]
        );
        assert!(got.iter().all(|c| c != &argv(&["switch"])));

        std::fs::remove_file(&log).unwrap();
        assert_eq!(
            run_job_with(&exe, &global, "auto"),
            Err(ApplyError::BadMode)
        );
        assert!(!log.exists(), "BadMode must run nothing");
        assert_eq!(run_job_with(&exe, &LmttJob::default(), "dark"), Ok(()));
        assert!(!log.exists(), "an empty job runs nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn current_mode_parses_status() {
        let dir = tmp();
        let (exe, _) = stub(
            &dir,
            "[ \"$1\" = status ] && { echo 'Wallpaper: /w.png'; echo 'Current theme: Light'; exit 0; }",
        );
        assert_eq!(current_mode_with(&exe).as_deref(), Some("light"));
        let (exe, _) = stub(
            &dir,
            "[ \"$1\" = status ] && { echo 'Current theme: auto'; exit 0; }",
        );
        assert_eq!(current_mode_with(&exe), None);
        let (exe, _) = stub(&dir, "[ \"$1\" = status ] && { echo garbage; exit 0; }");
        assert_eq!(current_mode_with(&exe), None);
        assert_eq!(current_mode_with(&dir.join("missing")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failures_surface_trimmed_stderr() {
        let dir = tmp();
        let job = LmttJob {
            set: Some((PathBuf::from("/x/a.png"), None)),
            unset: vec!["DP-1".into()],
        };
        let (exe, _) = stub(&dir, "echo ' boom ' >&2; exit 1");
        assert_eq!(
            run_job_with(&exe, &job, "dark"),
            Err(ApplyError::Set("boom".into()))
        );

        let (exe, log) = stub(
            &dir,
            "[ \"$1\" = switch ] && { echo 'no palette' >&2; exit 1; }",
        );
        std::fs::remove_file(&log).ok();
        assert_eq!(
            run_job_with(&exe, &job, "dark"),
            Err(ApplyError::Switch("no palette".into()))
        );
        assert_eq!(
            calls(&log).len(),
            2,
            "set and unset ran before the switch failed"
        );

        let (exe, _) = stub(
            &dir,
            "[ \"$2\" = unset ] && { echo \"error: unrecognized subcommand 'unset'\" >&2; exit 2; }",
        );
        assert_eq!(run_job_with(&exe, &job, "dark"), Err(ApplyError::TooOld));

        let (exe, _) = stub(&dir, "exit 3");
        assert_eq!(
            run_job_with(&exe, &job, "dark"),
            Err(ApplyError::Set(
                "lmtt wallpaper failed (exit status: 3)".into()
            ))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decode_preview_downsizes() {
        let dir = tmp();
        let src = dir.join("wide.png");
        image::RgbaImage::new(640, 360).save(&src).unwrap();
        let (w, h, bytes) = decode_preview(&src, 320).unwrap();
        assert_eq!((w, h), (320, 180));
        assert_eq!(bytes.len(), 320 * 180 * 4);
        assert!(decode_preview(&dir.join("missing.png"), 320).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
