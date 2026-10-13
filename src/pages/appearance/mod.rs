//! Appearance: lmtt's settings, rendered from the config schema lmtt-config
//! draws in the terminal (`schema`), edited in place without touching the
//! user's comments (`config`), and applied with `lmtt switch <current mode>`
//! so every lmtt module re-renders. Mode switches at once. The wallpaper
//! itself belongs to the wallpaper module (Displays); this page only shows it.
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use toml_edit::{DocumentMut, Value};

use crate::{AppState, AppWindow, SettingsField, Swatch};
use schema::{Field, Kind, Options};

pub mod config;
pub mod schema;

/// Token keys shown in the palette panel, in order.
const SWATCHES: &[&str] = &[
    "primary",
    "on_primary",
    "primary_container",
    "secondary",
    "tertiary",
    "surface",
    "surface_container",
    "error",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    Apply,
    Mode(String),
}

#[derive(Default)]
pub struct State {
    fields: Vec<Field>,
    doc: Option<DocumentMut>,
    config_path: Option<PathBuf>,
    /// Edits not yet written, by `<section>.<key>`.
    pending: BTreeMap<String, Value>,
    /// Provider option lists, enumerated once per reload.
    providers: HashMap<String, Vec<String>>,
    mode: String,
    error: Option<String>,
    /// In-flight lmtt job (0 = idle); one at a time.
    job: u64,
    job_seq: u64,
    job_kind: Option<Job>,
}

pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_set_mode(move |mode| {
            let ui = ui_weak.unwrap();
            set_mode(&ui, &state, mode.as_str());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_field_text(move |id, text| {
            let ui = ui_weak.unwrap();
            field_text(&ui, &state, id.as_str(), text.as_str());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_field_toggle(move |id, on| {
            let ui = ui_weak.unwrap();
            field_toggle(&ui, &state, id.as_str(), on);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_field_select(move |id, index| {
            let ui = ui_weak.unwrap();
            field_select(&ui, &state, id.as_str(), index);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_apply(move || {
            let ui = ui_weak.unwrap();
            apply(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_revert(move || {
            let ui = ui_weak.unwrap();
            state.borrow_mut().appearance.pending.clear();
            push(&ui, &state);
            ui.set_appearance_status("Reverted to config.toml.".into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_appearance_job_done(move |job, ok, msg| {
            let ui = ui_weak.unwrap();
            finish_job(&ui, &state, job as u64, ok, msg.to_string());
        });
    }
    reload(ui, state);
}

/// The Appearance section was opened: pick up outside edits unless the
/// user has unapplied ones.
pub fn enter(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if state.borrow().appearance.pending.is_empty() {
        reload(ui, state);
    } else {
        push(ui, state);
    }
}

fn lmtt_available() -> bool {
    crate::entries::on_path("lmtt")
}

/// `lmtt status` → `dark`|`light` (lmtt's own theme_state).
fn current_mode() -> Option<String> {
    let out = Command::new("lmtt")
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

/// lmtt-config's option providers, from lmtt-core itself.
fn provider_options(name: &str) -> Vec<String> {
    use lmtt_core::theme_detection as td;
    match name {
        "lmtt_gtk_themes" => td::detect_gtk_themes(),
        "lmtt_icon_themes" => td::detect_icon_themes(),
        "lmtt_cursor_themes" => td::detect_cursor_themes(),
        "lmtt_fonts" => td::detect_fonts(),
        "lmtt_vscode_themes" => td::detect_vscode_themes(),
        "lmtt_neovim_colorschemes" => td::detect_neovim_colorschemes(),
        _ => Vec::new(),
    }
}

/// Re-read schema, config and providers; keeps nothing pending.
fn reload(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let mut error = None;
    let fields = match config::schema_path() {
        Some(p) => match std::fs::read_to_string(&p)
            .map_err(|e| format!("{}: {e}", p.display()))
            .and_then(|t| schema::parse(&t))
        {
            Ok(f) => f,
            Err(e) => {
                error = Some(e);
                Vec::new()
            }
        },
        None => {
            error = Some(
                "lmtt's settings schema was not found: run lmtt-config once, or install an lmtt that ships lmtt/config-schema.json."
                    .into(),
            );
            Vec::new()
        }
    };
    let config_path = config::config_path();
    let doc = match &config_path {
        Some(p) => match config::load(p) {
            Ok(d) => Some(d),
            Err(e) => {
                error.get_or_insert(e);
                None
            }
        },
        None => None,
    };
    let mut providers: HashMap<String, Vec<String>> = HashMap::new();
    for f in &fields {
        if let Options::Provider(p) = &f.options
            && !providers.contains_key(p)
        {
            providers.insert(p.clone(), provider_options(p));
        }
    }
    let mode = current_mode().unwrap_or_else(|| slint_kit::load_tokens().mode.to_ascii_lowercase());
    {
        let mut st = state.borrow_mut();
        let a = &mut st.appearance;
        a.fields = fields;
        a.doc = doc;
        a.config_path = config_path;
        a.providers = providers;
        a.pending.clear();
        a.mode = mode;
        a.error = error;
    }
    push(ui, state);
}

fn json_to_toml(kind: Kind, v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match kind {
            Kind::Float => Value::from(n.as_f64().unwrap_or(0.0)),
            _ => match n.as_i64() {
                Some(i) => Value::from(i),
                None => Value::from(n.as_f64().unwrap_or(0.0)),
            },
        },
        _ => match kind {
            Kind::Bool => Value::from(false),
            Kind::Number => Value::from(0),
            Kind::Float => Value::from(0.0),
            _ => Value::from(""),
        },
    }
}

/// Pending edit, else the file, else the schema default.
fn value_of(a: &State, f: &Field) -> Value {
    if let Some(v) = a.pending.get(&f.key()) {
        return v.clone();
    }
    if let Some(doc) = &a.doc
        && let Some(v) = config::get(doc, &f.section, &f.id)
    {
        return v;
    }
    json_to_toml(f.kind, &f.default)
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        Value::Integer(i) => i.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        other => other.to_string().trim().to_string(),
    }
}

fn row_for(a: &State, f: &Field) -> SettingsField {
    let v = value_of(a, f);
    let key = f.key();
    let dirty = a.pending.contains_key(&key);
    let kind = f.kind.tag();
    let (value, checked, options, index) = match f.kind {
        Kind::Bool => (String::new(), v.as_bool().unwrap_or(false), Vec::new(), 0),
        Kind::Enum => {
            let current = value_text(&v);
            let mut opts: Vec<String> = match &f.options {
                Options::Static(l) => l.clone(),
                Options::Provider(p) => a.providers.get(p).cloned().unwrap_or_default(),
                Options::None => Vec::new(),
            };
            // A value that is not installed stays selectable rather than
            // silently becoming the first option.
            if !opts.iter().any(|o| *o == current) {
                opts.insert(0, current.clone());
            }
            let index = opts.iter().position(|o| *o == current).unwrap_or(0);
            (current, false, opts, index)
        }
        Kind::Number | Kind::Float | Kind::Path | Kind::Text => {
            (value_text(&v), false, Vec::new(), 0)
        }
    };
    SettingsField {
        id: key.into(),
        section: f.section_title.clone().into(),
        subsection: f.subsection.clone().into(),
        label: f.label.clone().into(),
        description: f.description.clone().into(),
        kind: kind.into(),
        value: value.into(),
        checked,
        options: ModelRc::new(VecModel::from(
            options
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )),
        index: index as i32,
        dirty,
    }
}

fn wallpaper_text() -> String {
    lmtt_core::Config::load()
        .map(|c| c.general.wallpaper)
        .unwrap_or_default()
}

/// Rebuild every row (not while the user types: see `field_text`).
fn push(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let a = &st.appearance;
    let rows: Vec<SettingsField> = a.fields.iter().map(|f| row_for(a, f)).collect();
    ui.set_appearance_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_appearance_mode(a.mode.clone().into());
    ui.set_appearance_dirty(!a.pending.is_empty());
    let available = lmtt_available();
    ui.set_appearance_available(available);
    ui.set_appearance_wallpaper(wallpaper_text().into());
    if let Some(e) = &a.error {
        ui.set_appearance_status(e.clone().into());
    } else if !available {
        ui.set_appearance_status(
            "lmtt is not installed – settings can be edited but not applied.".into(),
        );
    }
    drop(st);
    refresh_palette(ui);
}

fn refresh_palette(ui: &AppWindow) {
    let tokens = slint_kit::load_tokens();
    let swatches: Vec<Swatch> = SWATCHES
        .iter()
        .map(|k| Swatch {
            name: k.replace('_', " ").into(),
            color: tokens.get(k),
        })
        .collect();
    ui.set_appearance_swatches(ModelRc::new(VecModel::from(swatches)));
}

fn field_by_key<'a>(a: &'a State, key: &str) -> Option<&'a Field> {
    a.fields.iter().find(|f| f.key() == key)
}

/// Update one row in place (combo boxes and checkboxes; never a line edit
/// mid-typing, which would move the caret).
fn patch_row(ui: &AppWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let st = state.borrow();
    let a = &st.appearance;
    let Some(f) = field_by_key(a, key) else {
        return;
    };
    let rows = ui.get_appearance_fields();
    for i in 0..rows.row_count() {
        if rows.row_data(i).is_some_and(|r| r.id.as_str() == key) {
            rows.set_row_data(i, row_for(a, f));
            break;
        }
    }
    ui.set_appearance_dirty(!a.pending.is_empty());
}

fn field_text(ui: &AppWindow, state: &Rc<RefCell<AppState>>, key: &str, text: &str) {
    let parsed = {
        let st = state.borrow();
        let Some(f) = field_by_key(&st.appearance, key) else {
            return;
        };
        match f.kind {
            Kind::Number => match text.trim().parse::<i64>() {
                Ok(i) => Some(Value::from(i)),
                Err(_) => None,
            },
            Kind::Float => match text.trim().parse::<f64>() {
                Ok(x) => Some(Value::from(x)),
                Err(_) => None,
            },
            _ => Some(Value::from(text)),
        }
    };
    match parsed {
        Some(v) => {
            state
                .borrow_mut()
                .appearance
                .pending
                .insert(key.to_string(), v);
            ui.set_appearance_dirty(true);
            ui.set_appearance_status("".into());
        }
        None => ui.set_appearance_status(format!("{key}: needs a number.").into()),
    }
}

fn field_toggle(ui: &AppWindow, state: &Rc<RefCell<AppState>>, key: &str, on: bool) {
    state
        .borrow_mut()
        .appearance
        .pending
        .insert(key.to_string(), Value::from(on));
    patch_row(ui, state, key);
}

fn field_select(ui: &AppWindow, state: &Rc<RefCell<AppState>>, key: &str, index: i32) {
    let chosen = {
        let rows = ui.get_appearance_fields();
        (0..rows.row_count())
            .filter_map(|i| rows.row_data(i))
            .find(|r| r.id.as_str() == key)
            .and_then(|r| r.options.row_data(index.max(0) as usize))
            .map(|s| s.to_string())
    };
    let Some(chosen) = chosen else {
        return;
    };
    state
        .borrow_mut()
        .appearance
        .pending
        .insert(key.to_string(), Value::from(chosen));
    patch_row(ui, state, key);
}

/// Claim the single job slot; None while another job runs.
fn begin_job(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    kind: Job,
    status: &str,
) -> Option<u64> {
    let mut st = state.borrow_mut();
    let a = &mut st.appearance;
    if a.job != 0 {
        return None;
    }
    a.job_seq += 1;
    a.job = a.job_seq;
    a.job_kind = Some(kind);
    ui.set_appearance_busy(true);
    ui.set_appearance_status(status.into());
    Some(a.job)
}

/// `lmtt switch <mode> --no-notify` on a worker thread.
fn start_switch(ui: &AppWindow, job_id: u64, mode: String) {
    let ui_weak = ui.as_weak();
    std::thread::Builder::new()
        .name("appearance-apply".into())
        .spawn(move || {
            let out = Command::new("lmtt")
                .args(["switch", mode.as_str(), "--no-notify"])
                .stdin(Stdio::null())
                .output();
            let (ok, msg) = match out {
                Ok(o) if o.status.success() => (true, String::new()),
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    (
                        false,
                        if err.is_empty() {
                            format!("lmtt switch failed ({})", o.status)
                        } else {
                            err
                        },
                    )
                }
                Err(e) => (false, format!("lmtt: {e}")),
            };
            ui_weak
                .upgrade_in_event_loop(move |ui| {
                    ui.invoke_appearance_job_done(job_id as i32, ok, msg.into())
                })
                .ok();
        })
        .ok();
}

/// Write the pending edits into config.toml, then re-render.
fn apply(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let (path, doc, mode) = {
        let st = state.borrow();
        let a = &st.appearance;
        if a.pending.is_empty() {
            return;
        }
        let (Some(path), Some(doc)) = (a.config_path.clone(), a.doc.clone()) else {
            ui.set_appearance_status(
                "No config.toml location: XDG config dirs unavailable.".into(),
            );
            return;
        };
        let mut doc = doc;
        for (key, v) in &a.pending {
            if let Some(f) = field_by_key(a, key) {
                config::set(&mut doc, &f.section, &f.id, v.clone());
            }
        }
        (path, doc, a.mode.clone())
    };
    if let Err(e) = config::save(&path, &doc) {
        ui.set_appearance_status(e.into());
        return;
    }
    let Some(job) = begin_job(
        ui,
        state,
        Job::Apply,
        "Wrote config.toml; re-rendering with lmtt…",
    ) else {
        return;
    };
    start_switch(ui, job, mode);
}

fn set_mode(ui: &AppWindow, state: &Rc<RefCell<AppState>>, mode: &str) {
    if mode != "dark" && mode != "light" {
        return;
    }
    if state.borrow().appearance.mode == mode {
        return;
    }
    let Some(job) = begin_job(ui, state, Job::Mode(mode.to_string()), "Switching…") else {
        return;
    };
    start_switch(ui, job, mode.to_string());
}

fn finish_job(ui: &AppWindow, state: &Rc<RefCell<AppState>>, job_id: u64, ok: bool, msg: String) {
    let kind = {
        let mut st = state.borrow_mut();
        let a = &mut st.appearance;
        if a.job != job_id {
            return;
        }
        a.job = 0;
        let kind = a.job_kind.take();
        if ok && let Some(Job::Mode(m)) = &kind {
            a.mode = m.clone();
        }
        if ok && kind == Some(Job::Apply) {
            a.pending.clear();
        }
        kind
    };
    ui.set_appearance_busy(false);
    reload(ui, state);
    ui.set_appearance_status(
        match (ok, kind) {
            (true, Some(Job::Apply)) => {
                "Applied: config.toml written and every lmtt module re-rendered.".to_string()
            }
            (true, Some(Job::Mode(m))) => format!("Switched to {m}."),
            (true, None) => String::new(),
            (false, _) => msg,
        }
        .into(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: Kind, default: serde_json::Value, options: Options) -> Field {
        Field {
            section: "general".into(),
            section_title: "General".into(),
            subsection: "Basic".into(),
            id: "k".into(),
            label: "K".into(),
            description: String::new(),
            kind,
            default,
            options,
        }
    }

    #[test]
    fn rows_show_pending_then_file_then_default() {
        let mut a = State::default();
        let f = field(Kind::Number, serde_json::json!(10), Options::None);
        assert_eq!(row_for(&a, &f).value.as_str(), "10");
        a.doc = Some("[general]\nk = 42\n".parse().unwrap());
        assert_eq!(row_for(&a, &f).value.as_str(), "42");
        a.pending.insert("general.k".into(), Value::from(7));
        let row = row_for(&a, &f);
        assert_eq!(row.value.as_str(), "7");
        assert!(row.dirty);
    }

    #[test]
    fn enum_rows_keep_an_uninstalled_value_selectable() {
        let mut a = State::default();
        a.providers.insert(
            "lmtt_gtk_themes".into(),
            vec!["Adwaita".into(), "Breeze".into()],
        );
        let f = field(
            Kind::Enum,
            serde_json::json!("Adwaita"),
            Options::Provider("lmtt_gtk_themes".into()),
        );
        let row = row_for(&a, &f);
        assert_eq!(row.index, 0);
        assert_eq!(row.options.row_count(), 2);
        a.doc = Some("[general]\nk = \"Nordic\"\n".parse().unwrap());
        let row = row_for(&a, &f);
        assert_eq!(row.index, 0);
        assert_eq!(row.options.row_data(0).unwrap().as_str(), "Nordic");
        assert_eq!(row.options.row_count(), 3);
    }

    #[test]
    fn bool_and_float_rows() {
        let a = State::default();
        let b = field(Kind::Bool, serde_json::json!(true), Options::None);
        let row = row_for(&a, &b);
        assert_eq!(row.kind.as_str(), "bool");
        assert!(row.checked);
        let x = field(Kind::Float, serde_json::json!(0.95), Options::None);
        let row = row_for(&a, &x);
        assert_eq!(row.kind.as_str(), "number");
        assert_eq!(row.value.as_str(), "0.95");
    }
}
