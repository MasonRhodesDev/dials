//! hyprstate-gui — Displays configurator (MVP).
mod canvas;
mod hyprctl;
mod profiles_io;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use monitor_profiles::{
    ConnectedOutput, Mode, Monitor, Profile, ResolvedOutput, match_in_signature, resolve, select,
};
use profiles_io::{ListedProfile, load_merged, write_atomic};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

slint::include_modules!();

struct EditorState {
    listed: ListedProfile,
    profile: Profile,
    selected: usize,
    /// Logical positions mirrored from profile resolve (mutable while editing).
    positions: Vec<(i32, i32)>,
    drag_origin: Option<(i32, i32)>,
    canvas_scale: f32,
}

struct AppState {
    listed: Vec<ListedProfile>,
    live: Vec<hyprctl::HyprMonitor>,
    connected: Vec<ConnectedOutput>,
    signature: Vec<String>,
    active_name: Option<String>,
    editor: Option<EditorState>,
    save_wait: Option<Instant>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let state = Rc::new(RefCell::new(AppState {
        listed: Vec::new(),
        live: Vec::new(),
        connected: Vec::new(),
        signature: Vec::new(),
        active_name: None,
        editor: None,
        save_wait: None,
    }));

    refresh_list(&ui, &state);

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_profile(move |idx| {
            let ui = ui_weak.unwrap();
            open_editor(&ui, &state, idx as usize);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_back_to_list(move || {
            let ui = ui_weak.unwrap();
            state.borrow_mut().editor = None;
            ui.set_show_editor(false);
            refresh_list(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_capture_current(move || {
            let ui = ui_weak.unwrap();
            capture_current(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_profile(move || {
            let ui = ui_weak.unwrap();
            save_editor(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_select_monitor(move |idx| {
            let ui = ui_weak.unwrap();
            select_monitor(&ui, &state, idx as usize);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_center_to_neighbor(move || {
            let ui = ui_weak.unwrap();
            center_to_neighbor(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_insp_mode_changed(move |idx| {
            let ui = ui_weak.unwrap();
            set_mode(&ui, &state, idx as usize);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_insp_scale_changed(move |text| {
            let ui = ui_weak.unwrap();
            if let Ok(s) = text.as_str().parse::<f64>() {
                set_scale(&ui, &state, s);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_insp_rotate_changed(move |idx| {
            let ui = ui_weak.unwrap();
            set_rotate(&ui, &state, idx as u8);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_insp_pos_changed(move |xs, ys| {
            let ui = ui_weak.unwrap();
            let Ok(x) = xs.as_str().parse::<i32>() else {
                return;
            };
            let Ok(y) = ys.as_str().parse::<i32>() else {
                return;
            };
            set_pos(&ui, &state, x, y);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_insp_enabled_changed(move |en| {
            let ui = ui_weak.unwrap();
            set_enabled(&ui, &state, en);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_drag(move |idx, dx, dy| {
            let ui = ui_weak.unwrap();
            drag_monitor(&ui, &state, idx as usize, dx, dy);
        });
    }

    // Poll after save for session convergence.
    let ui_weak = ui.as_weak();
    let state_poll = state.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let mut st = state_poll.borrow_mut();
        let Some(started) = st.save_wait else {
            return;
        };
        if started.elapsed() > Duration::from_secs(8) {
            st.save_wait = None;
            ui.set_status_text("Saved; session hasn’t picked it up yet.".into());
            return;
        }
        if session_matches_editor(&st) {
            st.save_wait = None;
            ui.set_status_text("Session updated.".into());
        }
    });
    std::mem::forget(timer);

    ui.run()?;
    Ok(())
}

fn refresh_list(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let live = hyprctl::monitors().unwrap_or_default();
    let connected = hyprctl::connected(&live);
    let signature = hyprctl::signature(&live);
    let (listed, _diags) = load_merged();
    let profiles_only: Vec<Profile> = listed.iter().map(|l| l.profile.clone()).collect();
    let active = select(&signature, &profiles_only).map(|p| p.name.clone());

    ui.set_live_signature(hyprctl::humanize_signature(&live).into());

    let rows: Vec<ProfileRow> = listed
        .iter()
        .map(|l| ProfileRow {
            name: l.profile.name.clone().into(),
            source: l.source.as_str().into(),
            matched: profile_matches_sig(&l.profile, &signature),
            active: active.as_deref() == Some(l.profile.name.as_str()),
        })
        .collect();

    ui.set_profiles(ModelRc::new(VecModel::from(rows)));
    let mut st = state.borrow_mut();
    st.listed = listed;
    st.live = live;
    st.connected = connected;
    st.signature = signature;
    st.active_name = active;
}

fn profile_matches_sig(profile: &Profile, signature: &[String]) -> bool {
    !profile.matches.is_empty()
        && profile
            .matches
            .iter()
            .all(|m| match_in_signature(m, signature))
}

fn open_editor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    let mut st = state.borrow_mut();
    let Some(listed) = st.listed.get(idx).cloned() else {
        return;
    };
    let resolved = resolve(&listed.profile, &st.connected);
    let positions: Vec<(i32, i32)> = listed
        .profile
        .monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            m.position
                .or_else(|| resolved.outputs.get(i).map(|o| o.position))
                .unwrap_or((0, 0))
        })
        .collect();

    let badge = if st.active_name.as_deref() == Some(listed.profile.name.as_str()) {
        "Matched · Active"
    } else if profile_matches_sig(&listed.profile, &st.signature) {
        "Matched"
    } else {
        "Not matched to live desk"
    };

    ui.set_show_editor(true);
    ui.set_editor_name(listed.profile.name.clone().into());
    ui.set_editor_source(listed.source.as_str().into());
    ui.set_editor_badge(badge.into());
    ui.set_editor_dirty(false);
    ui.set_status_text("".into());
    ui.set_insp_match(listed.profile.matches.join("\n").into());

    st.editor = Some(EditorState {
        listed: listed.clone(),
        profile: listed.profile,
        selected: 0,
        positions,
        drag_origin: None,
        canvas_scale: 1.0,
    });
    drop(st);
    push_canvas(ui, state);
    select_monitor(ui, state, 0);
}

fn editor_outputs(st: &AppState) -> Vec<(ResolvedOutput, String, bool)> {
    let Some(ed) = &st.editor else {
        return Vec::new();
    };
    let mut profile = ed.profile.clone();
    for (m, pos) in profile.monitors.iter_mut().zip(ed.positions.iter()) {
        m.position = Some(*pos);
    }
    let resolved = resolve(&profile, &st.connected);
    profile
        .monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let connected = st
                .connected
                .iter()
                .any(|c| selects(&m.output, c));
            let out = resolved
                .outputs
                .iter()
                .find(|o| o.selector == m.output)
                .cloned()
                .unwrap_or(ResolvedOutput {
                    name: m.output.clone(),
                    selector: m.output.clone(),
                    mode: m.mode,
                    position: ed.positions.get(i).copied().unwrap_or((0, 0)),
                    scale: m.scale,
                    transform: m.transform,
                    enabled: m.enabled,
                });
            let label = short_label(&m.output);
            (out, label, !connected)
        })
        .collect()
}

fn selects(selector: &str, o: &ConnectedOutput) -> bool {
    match selector.strip_prefix("desc:") {
        Some(d) => o.description.starts_with(d.trim()),
        None => o.name == selector,
    }
}

fn short_label(output: &str) -> String {
    output
        .strip_prefix("desc:")
        .unwrap_or(output)
        .split_whitespace()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_canvas(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let mut st = state.borrow_mut();
    let outs = editor_outputs(&st);
    let cw = 720.0;
    let ch = 520.0;
    let scale = canvas::canvas_scale(&outs, cw, ch);
    if let Some(ed) = st.editor.as_mut() {
        ed.canvas_scale = scale;
    }
    let selected = st.editor.as_ref().map(|e| e.selected).unwrap_or(0);
    let drawn = canvas::layout_drawn(&outs, cw, ch);
    let model: Vec<CanvasMonitor> = drawn
        .into_iter()
        .map(|d| CanvasMonitor {
            label: d.label.into(),
            x: d.x,
            y: d.y,
            w: d.w,
            h: d.h,
            selected: d.index == selected,
            ghost: d.ghost,
        })
        .collect();
    ui.set_canvas_monitors(ModelRc::new(VecModel::from(model)));

    let warnings: Vec<String> = {
        let Some(ed) = &st.editor else {
            ui.set_warnings("".into());
            return;
        };
        let mut profile = ed.profile.clone();
        for (m, pos) in profile.monitors.iter_mut().zip(ed.positions.iter()) {
            m.position = Some(*pos);
        }
        let r = resolve(&profile, &st.connected);
        let mut w = r.warnings;
        for u in r.unmatched {
            w.push(format!("no output for {u}"));
        }
        for m in &ed.profile.monitors {
            if let Some(mode) = m.mode {
                let snapped = monitor_profiles::valid_scale(mode.width, mode.height, m.scale);
                if (snapped - m.scale).abs() > 0.001 {
                    w.push(format!(
                        "{}: scale {} snaps to {snapped}",
                        short_label(&m.output),
                        m.scale
                    ));
                }
            }
        }
        w
    };
    ui.set_warnings(warnings.join(" · ").into());
}

fn select_monitor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        if idx >= ed.profile.monitors.len() {
            return;
        }
        ed.selected = idx;
        ed.drag_origin = None;
    }
    push_canvas(ui, state);
    push_inspector(ui, state);
}

fn push_inspector(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let Some(ed) = &st.editor else {
        return;
    };
    let i = ed.selected;
    let m = &ed.profile.monitors[i];
    let (x, y) = ed.positions[i];
    ui.set_insp_label(short_label(&m.output).into());
    ui.set_insp_scale(format!("{}", m.scale).into());
    if let Some(mode) = m.mode {
        let snapped = monitor_profiles::valid_scale(mode.width, mode.height, m.scale);
        if (snapped - m.scale).abs() > 0.001 {
            ui.set_insp_snap(format!("snaps to {snapped}").into());
        } else {
            ui.set_insp_snap("".into());
        }
    } else {
        ui.set_insp_snap("".into());
    }
    ui.set_insp_rotate_index(i32::from(m.transform.min(3)));
    ui.set_insp_pos_x(x.to_string().into());
    ui.set_insp_pos_y(y.to_string().into());
    ui.set_insp_enabled(m.enabled);

    let modes = modes_for(&st.live, &m.output);
    let current = m.mode.map(|md| md.to_string()).unwrap_or_default();
    let mut idx = modes.iter().position(|t| t == &current).unwrap_or(0);
    if modes.is_empty() {
        let fallback = if current.is_empty() {
            "preferred".to_string()
        } else {
            current
        };
        ui.set_insp_modes(ModelRc::new(VecModel::from(vec![SharedString::from(
            fallback,
        )])));
        idx = 0;
    } else {
        ui.set_insp_modes(ModelRc::new(VecModel::from(
            modes
                .iter()
                .map(|t| SharedString::from(t.as_str()))
                .collect::<Vec<_>>(),
        )));
    }
    ui.set_insp_mode_index(idx as i32);
}

fn modes_for(live: &[hyprctl::HyprMonitor], selector: &str) -> Vec<String> {
    let desc = selector.strip_prefix("desc:").unwrap_or(selector);
    live.iter()
        .find(|m| m.name == selector || m.description.starts_with(desc))
        .map(|m| {
            let mut v = m.available_modes.clone();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default()
}

fn mark_dirty(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        for (m, pos) in ed.profile.monitors.iter_mut().zip(ed.positions.iter()) {
            m.position = Some(*pos);
        }
    }
    ui.set_editor_dirty(true);
    push_canvas(ui, state);
    push_inspector(ui, state);
}

fn set_mode(ui: &AppWindow, state: &Rc<RefCell<AppState>>, mode_idx: usize) {
    let modes = {
        let st = state.borrow();
        let Some(ed) = &st.editor else {
            return;
        };
        modes_for(&st.live, &ed.profile.monitors[ed.selected].output)
    };
    let text = modes.get(mode_idx).cloned().unwrap_or_default();
    // Hyprland modes look like 3840x2160@60.00Hz — strip Hz for Mode::parse
    let cleaned = text.replace("Hz", "");
    if let Some(mode) = Mode::parse(&cleaned) {
        let mut st = state.borrow_mut();
        if let Some(ed) = st.editor.as_mut() {
            ed.profile.monitors[ed.selected].mode = Some(mode);
        }
    }
    mark_dirty(ui, state);
}

fn set_scale(ui: &AppWindow, state: &Rc<RefCell<AppState>>, scale: f64) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.profile.monitors[ed.selected].scale = scale.max(0.1);
    }
    mark_dirty(ui, state);
}

fn set_rotate(ui: &AppWindow, state: &Rc<RefCell<AppState>>, t: u8) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.profile.monitors[ed.selected].transform = t.min(3);
    }
    mark_dirty(ui, state);
}

fn set_pos(ui: &AppWindow, state: &Rc<RefCell<AppState>>, x: i32, y: i32) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        let i = ed.selected;
        ed.positions[i] = (x, y);
        ed.drag_origin = None;
    }
    mark_dirty(ui, state);
}

fn set_enabled(ui: &AppWindow, state: &Rc<RefCell<AppState>>, en: bool) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.profile.monitors[ed.selected].enabled = en;
    }
    mark_dirty(ui, state);
}

fn drag_monitor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, idx: usize, dx: f32, dy: f32) {
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        if idx >= ed.positions.len() {
            return;
        }
        if ed.drag_origin.is_none() {
            ed.drag_origin = Some(ed.positions[idx]);
            ed.selected = idx;
        }
        let origin = ed.drag_origin.unwrap();
        let scale = ed.canvas_scale.max(0.01);
        let nx = origin.0 + (dx / scale) as i32;
        let ny = origin.1 + (dy / scale) as i32;
        ed.positions[idx] = (nx, ny);
    }
    ui.set_editor_dirty(true);
    push_canvas(ui, state);
    // Update pos fields without resetting drag_origin
    let st = state.borrow();
    if let Some(ed) = &st.editor {
        let (x, y) = ed.positions[ed.selected];
        ui.set_insp_pos_x(x.to_string().into());
        ui.set_insp_pos_y(y.to_string().into());
    }
}

fn center_to_neighbor(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        if ed.profile.monitors.len() < 2 {
            return;
        }
        let i = ed.selected;
        let j = if i == 0 { 1 } else { 0 };
        let outs = {
            // temporary resolve sizes
            let mut profile = ed.profile.clone();
            for (m, pos) in profile.monitors.iter_mut().zip(ed.positions.iter()) {
                m.position = Some(*pos);
            }
            // use canvas helpers
            profile
        };
        let _ = outs;
        let size_i = {
            let m = &ed.profile.monitors[i];
            let ro = ResolvedOutput {
                name: String::new(),
                selector: String::new(),
                mode: m.mode,
                position: ed.positions[i],
                scale: m.scale,
                transform: m.transform,
                enabled: true,
            };
            canvas::logical_size(&ro)
        };
        let size_j = {
            let m = &ed.profile.monitors[j];
            let ro = ResolvedOutput {
                name: String::new(),
                selector: String::new(),
                mode: m.mode,
                position: ed.positions[j],
                scale: m.scale,
                transform: m.transform,
                enabled: true,
            };
            canvas::logical_size(&ro)
        };
        let (jx, jy) = ed.positions[j];
        // Center i vertically relative to j (keep i's x)
        let (ix, _) = ed.positions[i];
        let cy = jy + (size_j.1 - size_i.1) / 2;
        ed.positions[i] = (ix, cy);
        ed.drag_origin = None;
        let _ = jx;
    }
    mark_dirty(ui, state);
}

fn save_editor(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let path;
    let profile;
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        for (m, pos) in ed.profile.monitors.iter_mut().zip(ed.positions.iter()) {
            m.position = Some(*pos);
        }
        path = ed.listed.path.clone();
        profile = ed.profile.clone();
    }
    match write_atomic(&path, &profile) {
        Ok(()) => {
            ui.set_editor_dirty(false);
            ui.set_status_text("Waiting for session…".into());
            state.borrow_mut().save_wait = Some(Instant::now());
            // refresh listed copy
            if let Some(ed) = state.borrow_mut().editor.as_mut() {
                ed.listed.profile = profile;
            }
        }
        Err(e) => ui.set_status_text(format!("Save failed: {e}").into()),
    }
}

fn session_matches_editor(st: &AppState) -> bool {
    let Some(ed) = &st.editor else {
        return false;
    };
    let Ok(live) = hyprctl::monitors() else {
        return false;
    };
    for (m, pos) in ed.profile.monitors.iter().zip(ed.positions.iter()) {
        let desc = m.output.strip_prefix("desc:").unwrap_or(&m.output);
        let Some(hm) = live
            .iter()
            .find(|h| h.name == m.output || h.description.starts_with(desc))
        else {
            continue;
        };
        if hm.x != pos.0 || hm.y != pos.1 {
            return false;
        }
    }
    true
}

fn capture_current(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    if st.live.is_empty() {
        ui.set_status_text("No live monitors to capture.".into());
        return;
    }
    let name = format!(
        "captured-{}",
        chrono_lite_stamp()
    );
    let mut monitors = Vec::new();
    let mut matches = Vec::new();
    for h in &st.live {
        let selector = if h.description.is_empty() {
            h.name.clone()
        } else {
            // serial-free prefix: make + model words (drop last token if looks like serial)
            let desc = strip_serial_suffix(&h.description);
            format!("desc:{desc}")
        };
        matches.push(selector.clone());
        let mode = Mode {
            width: h.width,
            height: h.height,
            refresh: h.refresh_rate.round(),
        };
        monitors.push(Monitor {
            output: selector,
            mode: Some(mode),
            scale: h.scale,
            position: Some((h.x, h.y)),
            transform: h.transform % 4,
            enabled: true,
        });
    }
    let profile = Profile {
        name: name.clone(),
        description: "Captured from live Hyprland layout.".into(),
        matches,
        edp: monitor_profiles::EdpPolicy::Auto,
        gpu: monitor_profiles::GpuPref::Auto,
        hooks: vec![],
        priority: monitors.len() as i64,
        monitors,
        workspaces: vec![],
    };
    let path = profiles_io::capture_path(&name);
    drop(st);
    match write_atomic(&path, &profile) {
        Ok(()) => {
            ui.set_status_text(format!("Captured {}", path.display()).into());
            refresh_list(ui, state);
            let idx = state
                .borrow()
                .listed
                .iter()
                .position(|l| l.profile.name == name);
            if let Some(i) = idx {
                open_editor(ui, state, i);
            }
        }
        Err(e) => ui.set_status_text(format!("Capture failed: {e}").into()),
    }
}

fn strip_serial_suffix(desc: &str) -> String {
    let parts: Vec<&str> = desc.split_whitespace().collect();
    if parts.len() >= 2 {
        let last = parts[parts.len() - 1];
        // Heuristic: serial-like tokens are long alnum without spaces
        if last.len() >= 5 && last.chars().all(|c| c.is_ascii_alphanumeric()) {
            return parts[..parts.len() - 1].join(" ");
        }
    }
    desc.to_string()
}

fn chrono_lite_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}
