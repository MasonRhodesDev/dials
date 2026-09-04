//! dials — desktop settings. Displays and power pages edit hyprstate
//! intent; the More page launches other settings tools from their XDG
//! desktop entries (see `entries`).
mod align;
mod canvas;
mod entries;
mod help_graph;
mod hyprctl;
mod power_io;
mod profiles_io;
mod sensors;
mod telemetry;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex, Weak as SyncWeak,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use hyprstate_fsm::power::{PowerPolicy, PowerProfile};
use monitor_profiles::{
    ConnectedOutput, Mode, Monitor, Profile, ResolvedOutput, match_in_signature, resolve, select,
};
use profiles_io::{ListedProfile, load_merged, write_atomic};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use slint_kit::{ThemeBridge, apply_theme};

slint::include_modules!();

struct EditorState {
    listed: ListedProfile,
    profile: Profile,
    selected: usize,
    /// Logical positions mirrored from profile resolve (mutable while editing).
    positions: Vec<(i32, i32)>,
    drag_origin: Option<(i32, i32)>,
    /// Canvas-local pointer at drag start (survives tile moves).
    drag_press_px: Option<(f32, f32)>,
    /// Active pan/zoom transform (fit-all on open; user may zoom/pan).
    view: canvas::CanvasView,
    /// After user zoom/pan, resize won't auto re-fit.
    view_custom: bool,
    /// Middle-button / empty-space pan gesture.
    panning: bool,
    pan_last: Option<(f32, f32)>,
    /// Logical snap guides (mapped to canvas in `push_canvas`).
    active_guides: Vec<align::Guide>,
    /// Parallel to `profile.monitors`: live outputs the loaded profile had no
    /// entry for. Appended on open so they can be placed; cleared on save.
    unsaved: Vec<bool>,
}

struct AppState {
    listed: Vec<ListedProfile>,
    live: Vec<hyprctl::HyprMonitor>,
    connected: Vec<ConnectedOutput>,
    signature: Vec<String>,
    active_name: Option<String>,
    editor: Option<EditorState>,
    save_wait: Option<Instant>,
    save_sequence: u64,
    canvas_model: Rc<VecModel<CanvasMonitor>>,
    canvas_w: f32,
    canvas_h: f32,
    sensor_picks: sensors::SensorPicks,
    help_live: telemetry::HelpLive,
    /// External settings tools (More page), in display order.
    entries: Vec<entries::Entry>,
}

#[derive(Default)]
struct TelemetryWake {
    latest: Mutex<Option<telemetry::HelpLive>>,
    pending: AtomicBool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--entries") => return print_entries(),
        Some("--help" | "-h") => {
            println!(
                "usage: dials [--entries]\n  --entries  list the settings tools the More page would show, then exit"
            );
            return Ok(());
        }
        Some(other) => return Err(format!("unknown argument {other}; see --help").into()),
    }
    let ui = AppWindow::new()?;
    let bridge = ThemeBridge::attach(ui.as_weak(), |ui, tokens| {
        apply_theme!(ui.global::<Theme>(), tokens);
        ui.invoke_sync_palette();
    })?;
    std::mem::forget(bridge);

    let canvas_model = Rc::new(VecModel::from(Vec::<CanvasMonitor>::new()));
    ui.set_canvas_monitors(ModelRc::from(canvas_model.clone()));
    let state = Rc::new(RefCell::new(AppState {
        listed: Vec::new(),
        live: Vec::new(),
        connected: Vec::new(),
        signature: Vec::new(),
        active_name: None,
        editor: None,
        save_wait: None,
        save_sequence: 0,
        canvas_model,
        canvas_w: 720.0,
        canvas_h: 520.0,
        sensor_picks: sensors::load_picks(),
        help_live: telemetry::HelpLive::default(),
        entries: Vec::new(),
    }));

    refresh_list(&ui, &state);
    open_current_desk(&ui, &state);

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
        ui.on_open_current_desk(move || {
            let ui = ui_weak.unwrap();
            refresh_list(&ui, &state);
            open_current_desk(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_profile(move || {
            let ui = ui_weak.unwrap();
            save_editor(&ui, &state);
            if state.borrow().save_wait.is_some() {
                let sequence = state.borrow().save_sequence;
                schedule_save_convergence(ui.as_weak(), state.clone(), sequence);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_promote_to_shared(move || {
            let ui = ui_weak.unwrap();
            promote_to_shared(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rename_profile(move |name| {
            let ui = ui_weak.unwrap();
            rename_profile(&ui, &state, name);
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
        ui.on_align_to_neighbor(move |op| {
            let ui = ui_weak.unwrap();
            align_to_neighbor(&ui, &state, op);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_set_as_origin(move || {
            let ui = ui_weak.unwrap();
            set_as_origin(&ui, &state);
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
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_drag_end(move || {
            if let Some(ui) = ui_weak.upgrade() {
                end_drag(&ui, &state);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_resized(move |w, h| {
            let ui = ui_weak.unwrap();
            {
                let mut st = state.borrow_mut();
                st.canvas_w = w;
                st.canvas_h = h;
            }
            let should_fit = state
                .borrow()
                .editor
                .as_ref()
                .is_some_and(|e| !e.view_custom);
            if should_fit {
                let view = {
                    let st = state.borrow();
                    let outs = editor_outputs(&st);
                    canvas::fit_view(&outs, w.max(32.0), h.max(32.0))
                };
                if let Some(ed) = state.borrow_mut().editor.as_mut() {
                    ed.view = view;
                }
            }
            if state.borrow().editor.is_some() {
                push_canvas(&ui, &state);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_zoom(move |delta_y, cx, cy| {
            let ui = ui_weak.unwrap();
            canvas_zoom(&ui, &state, delta_y, cx, cy);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_zoom_step(move |factor| {
            let ui = ui_weak.unwrap();
            let (cx, cy) = {
                let st = state.borrow();
                (st.canvas_w * 0.5, st.canvas_h * 0.5)
            };
            let factor = if factor > 0.0 { 1.15 } else { 1.0 / 1.15 };
            canvas_zoom_factor(&ui, &state, factor, cx, cy);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_fit(move || {
            let ui = ui_weak.unwrap();
            canvas_fit(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_pan(move |dx, dy| {
            let ui = ui_weak.unwrap();
            canvas_pan(&ui, &state, dx, dy);
        });
    }
    {
        let state = state.clone();
        ui.on_canvas_pan_begin(move |x, y| {
            if let Some(ed) = state.borrow_mut().editor.as_mut() {
                ed.panning = true;
                ed.pan_last = Some((x, y));
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_pan_move(move |x, y| {
            let ui = ui_weak.unwrap();
            let (dx, dy) = {
                let mut st = state.borrow_mut();
                let Some(ed) = st.editor.as_mut() else {
                    return;
                };
                if !ed.panning {
                    return;
                }
                let (lx, ly) = ed.pan_last.unwrap_or((x, y));
                ed.pan_last = Some((x, y));
                (x - lx, y - ly)
            };
            canvas_pan(&ui, &state, dx, dy);
        });
    }
    {
        let state = state.clone();
        ui.on_canvas_pan_end(move || {
            if let Some(ed) = state.borrow_mut().editor.as_mut() {
                ed.panning = false;
                ed.pan_last = None;
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_policy_changed(move || {
            let ui = ui_weak.unwrap();
            save_power_policy(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_low_percent_changed(move |text| {
            let ui = ui_weak.unwrap();
            ui.set_low_percent(text);
            save_power_policy(&ui, &state);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sensor_lid_changed(move |i| {
            let ui = ui_weak.unwrap();
            set_sensor_pick(&ui, &state, SensorKind::Lid, i);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sensor_ac_changed(move |i| {
            let ui = ui_weak.unwrap();
            set_sensor_pick(&ui, &state, SensorKind::Ac, i);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sensor_battery_changed(move |i| {
            let ui = ui_weak.unwrap();
            set_sensor_pick(&ui, &state, SensorKind::Battery, i);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_override_set(move |profile| {
            let ui = ui_weak.unwrap();
            run_override(&ui, &state, profile.as_str());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_override_clear(move || {
            let ui = ui_weak.unwrap();
            run_override(&ui, &state, "auto");
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_override_cycle(move || {
            let ui = ui_weak.unwrap();
            run_override(&ui, &state, "cycle");
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_section_changed(move |section| {
            let ui = ui_weak.unwrap();
            if section == 1 {
                refresh_power(&ui, &state);
            }
            if section == 2 {
                apply_help_from_live(&ui, &state);
            }
            if section == 3 {
                refresh_entries(&ui, &state);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_launch_entry(move |idx| {
            let ui = ui_weak.unwrap();
            let result = state
                .borrow()
                .entries
                .get(idx as usize)
                .map(entries::launch)
                .unwrap_or(Err("no such entry".into()));
            match result {
                Ok(()) => ui.set_status_text("".into()),
                Err(e) => ui.set_status_text(format!("Could not launch {e}").into()),
            }
        });
    }
    refresh_entries(&ui, &state);

    load_policy_ui(&ui);
    refresh_power(&ui, &state);
    apply_help_from_live(&ui, &state);

    // Telemetry is edge-triggered: the worker stores only the newest frame and
    // wakes Slint once. With no frame, input, animation, or one-shot deadline,
    // winit can block indefinitely.
    let telem_wake = Arc::new(TelemetryWake::default());
    {
        let telem_wake = telem_wake.clone();
        let state = state.clone();
        let ui_weak = ui.as_weak();
        ui.on_telemetry_wake(move || {
            let ui = ui_weak.unwrap();
            telem_wake.pending.store(false, Ordering::Release);
            let Some(frame) = telem_wake.latest.lock().expect("telemetry slot").take() else {
                return;
            };
            state.borrow_mut().help_live = frame;
            apply_help_from_live(&ui, &state);
            if ui.get_section() == 1 {
                refresh_power(&ui, &state);
            }
        });
    }
    let telem_weak = Arc::downgrade(&telem_wake);
    let ui_weak = ui.as_weak();
    telemetry::spawn(move |frame| queue_telemetry_frame(&telem_weak, &ui_weak, frame));

    ui.run()?;
    Ok(())
}

fn queue_telemetry_frame(
    slot: &SyncWeak<TelemetryWake>,
    ui: &slint::Weak<AppWindow>,
    frame: telemetry::HelpLive,
) -> bool {
    let Some(slot) = slot.upgrade() else {
        return false;
    };
    *slot.latest.lock().expect("telemetry slot") = Some(frame);
    if !slot.pending.swap(true, Ordering::AcqRel) {
        let slot_for_error = slot.clone();
        if ui
            .upgrade_in_event_loop(move |ui| ui.invoke_telemetry_wake())
            .is_err()
        {
            slot_for_error.pending.store(false, Ordering::Release);
            return false;
        }
    }
    true
}

fn schedule_save_convergence(
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<AppState>>,
    sequence: u64,
) {
    slint::Timer::single_shot(Duration::from_millis(500), move || {
        let Some(ui_handle) = ui.upgrade() else {
            return;
        };
        let mut st = state.borrow_mut();
        if st.save_sequence != sequence {
            return;
        }
        let Some(started) = st.save_wait else {
            return;
        };
        if started.elapsed() > Duration::from_secs(8) {
            st.save_wait = None;
            ui_handle.set_status_text("Saved; session hasn’t picked it up yet.".into());
            return;
        }
        if session_matches_editor(&st) {
            st.save_wait = None;
            ui_handle.set_status_text("Session updated.".into());
            return;
        }
        drop(st);
        schedule_save_convergence(ui, state, sequence);
    });
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

fn open_current_desk(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let active_idx = {
        let st = state.borrow();
        st.active_name.as_ref().and_then(|name| {
            st.listed
                .iter()
                .position(|l| l.profile.name.as_str() == name.as_str())
        })
    };
    if let Some(idx) = active_idx {
        open_editor(ui, state, idx);
        return;
    }
    open_seeded_current_desk(ui, state);
}

fn open_editor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    let listed = {
        let st = state.borrow();
        st.listed.get(idx).cloned()
    };
    let Some(listed) = listed else {
        return;
    };
    let badge = {
        let st = state.borrow();
        if st.active_name.as_deref() == Some(listed.profile.name.as_str()) {
            "Current desk · Active"
        } else if profile_matches_sig(&listed.profile, &st.signature) {
            "Matched to live desk"
        } else {
            "Historical"
        }
    };
    open_listed(ui, state, listed, false, badge);
}

fn open_seeded_current_desk(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    if st.live.is_empty() {
        drop(st);
        ui.set_status_text("No live monitors to edit.".into());
        ui.set_show_editor(false);
        return;
    }
    let name = seed_name_from_live(&st.live);
    if let Some(idx) = st
        .listed
        .iter()
        .position(|l| l.profile.name.as_str() == name.as_str())
    {
        drop(st);
        open_editor(ui, state, idx);
        return;
    }
    let (profile, path, source) = seed_profile_from_live(&st.live, &name);
    let listed = ListedProfile {
        profile,
        source,
        path,
    };
    drop(st);
    open_listed(ui, state, listed, true, "Current desk · Unsaved");
}

fn seed_name_from_live(live: &[hyprctl::HyprMonitor]) -> String {
    let parts: Vec<String> = live
        .iter()
        .map(|h| {
            let raw = if h.description.is_empty() {
                h.name.clone()
            } else {
                strip_serial_suffix(&h.description)
            };
            sanitize_profile_name(&raw)
        })
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        "live-desk".into()
    } else {
        parts.join("-and-")
    }
}

fn sanitize_profile_name(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in raw.chars() {
        let ok = c.is_ascii_alphanumeric();
        if ok {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Profile entry mirroring a live output's current mode/scale/position.
fn monitor_from_live(h: &hyprctl::HyprMonitor) -> Monitor {
    let output = if h.description.is_empty() {
        h.name.clone()
    } else {
        let desc = strip_serial_suffix(&h.description);
        format!("desc:{desc}")
    };
    Monitor {
        output,
        mode: Some(Mode {
            width: h.width,
            height: h.height,
            refresh: h.refresh_rate.round(),
        }),
        scale: h.scale,
        position: Some((h.x, h.y)),
        transform: h.transform % 4,
        enabled: true,
    }
}

/// Live outputs no entry in `profile` selects — newly plugged monitors the
/// saved profile doesn't know about yet.
fn live_missing_from_profile<'a>(
    profile: &Profile,
    live: &'a [hyprctl::HyprMonitor],
    connected: &[ConnectedOutput],
) -> Vec<&'a hyprctl::HyprMonitor> {
    live.iter()
        .zip(connected.iter())
        .filter(|(_, c)| !profile.monitors.iter().any(|m| selects(&m.output, c)))
        .map(|(h, _)| h)
        .collect()
}

fn seed_profile_from_live(
    live: &[hyprctl::HyprMonitor],
    name: &str,
) -> (Profile, std::path::PathBuf, profiles_io::Source) {
    let monitors: Vec<Monitor> = live.iter().map(monitor_from_live).collect();
    let matches = monitors.iter().map(|m| m.output.clone()).collect();
    let profile = Profile {
        name: name.to_string(),
        description: "Current desk (seeded from live Hyprland layout).".into(),
        matches,
        edp: monitor_profiles::EdpPolicy::Auto,
        gpu: monitor_profiles::GpuPref::Auto,
        hooks: vec![],
        priority: monitors.len() as i64,
        monitors,
        workspaces: vec![],
    };
    let path = profiles_io::user_profile_path(name);
    (profile, path, profiles_io::Source::User)
}

fn open_listed(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    listed: ListedProfile,
    dirty: bool,
    badge: &str,
) {
    let mut st = state.borrow_mut();
    let resolved = resolve(&listed.profile, &st.connected);
    let mut positions: Vec<(i32, i32)> = listed
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
    let mut profile = listed.profile.clone();
    let mut unsaved = vec![false; profile.monitors.len()];
    let added: Vec<Monitor> = live_missing_from_profile(&profile, &st.live, &st.connected)
        .into_iter()
        .map(monitor_from_live)
        .collect();
    for m in added {
        positions.push(m.position.unwrap_or((0, 0)));
        unsaved.push(true);
        profile.monitors.push(m);
    }
    let added_count = unsaved.iter().filter(|u| **u).count();
    let dirty = dirty || added_count > 0;

    st.editor = Some(EditorState {
        listed: listed.clone(),
        profile,
        selected: 0,
        positions,
        unsaved,
        drag_origin: None,
        drag_press_px: None,
        view: canvas::CanvasView::default(),
        view_custom: false,
        panning: false,
        pan_last: None,
        active_guides: Vec::new(),
    });
    drop(st);

    ui.set_show_editor(true);
    ui.set_selected_index(0);
    ui.set_editor_name(listed.profile.name.clone().into());
    ui.set_editor_source(listed.source.as_str().into());
    ui.set_editor_badge(badge.into());
    ui.set_editor_dirty(dirty);
    ui.set_renaming_profile(false);
    ui.set_can_promote_shared(
        listed.source == profiles_io::Source::User && profiles_io::shared_writable(),
    );
    ui.set_status_text(
        match added_count {
            0 => String::new(),
            1 => "1 monitor not in this profile yet — save to keep it.".to_string(),
            n => format!("{n} monitors not in this profile yet — save to keep them."),
        }
        .into(),
    );
    ui.set_insp_match(listed.profile.matches.join("\n").into());

    {
        let view = {
            let st = state.borrow();
            let outs = editor_outputs(&st);
            canvas::fit_view(&outs, st.canvas_w.max(32.0), st.canvas_h.max(32.0))
        };
        if let Some(ed) = state.borrow_mut().editor.as_mut() {
            ed.view = view;
            ed.view_custom = false;
        }
    }
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
            let connected = st.connected.iter().any(|c| selects(&m.output, c));
            let pos = ed.positions.get(i).copied().unwrap_or((0, 0));
            // Prefer live resolve for connector name, but always paint with the
            // editor's mode/scale/transform/position so bounds match the profile.
            let mut out = resolved
                .outputs
                .iter()
                .find(|o| o.selector == m.output)
                .cloned()
                .unwrap_or(ResolvedOutput {
                    name: m.output.clone(),
                    selector: m.output.clone(),
                    mode: m.mode,
                    position: pos,
                    scale: m.scale,
                    transform: m.transform,
                    enabled: m.enabled,
                });
            out.mode = m.mode;
            out.scale = m.scale;
            out.transform = m.transform;
            out.position = pos;
            out.enabled = m.enabled;
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
    let st = state.borrow_mut();
    let outs = editor_outputs(&st);
    let view = st.editor.as_ref().map(|e| e.view).unwrap_or_default();
    let drawn = canvas::layout_drawn(&outs, view);
    let selected = st.editor.as_ref().map(|e| e.selected).unwrap_or(0);
    let unsaved: Vec<bool> = st
        .editor
        .as_ref()
        .map(|e| e.unsaved.clone())
        .unwrap_or_default();
    let model = st.canvas_model.clone();
    let zoom_pct = (view.scale * 100.0).round() as i32;
    let rows: Vec<CanvasMonitor> = drawn
        .into_iter()
        .map(|d| CanvasMonitor {
            label: d.label.into(),
            x: d.x,
            y: d.y,
            w: d.w,
            h: d.h,
            selected: d.index == selected,
            ghost: d.ghost,
            origin: d.origin,
            unsaved: unsaved.get(d.index).copied().unwrap_or(false),
        })
        .collect();

    if model.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            model.set_row_data(i, row);
        }
    } else {
        model.set_vec(rows);
    }
    ui.set_canvas_zoom_label(format!("{zoom_pct}%").into());

    let guides = st
        .editor
        .as_ref()
        .map(|e| e.active_guides.clone())
        .unwrap_or_default();
    let guide_rows: Vec<CanvasGuide> = guides
        .iter()
        .map(|g| match *g {
            align::Guide::Vertical(lx) => CanvasGuide {
                vertical: true,
                pos: canvas::map_x(&view, lx),
            },
            align::Guide::Horizontal(ly) => CanvasGuide {
                vertical: false,
                pos: canvas::map_y(&view, ly),
            },
        })
        .collect();
    ui.set_canvas_guides(ModelRc::new(VecModel::from(guide_rows)));

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

fn canvas_zoom(ui: &AppWindow, state: &Rc<RefCell<AppState>>, delta_y: f32, cx: f32, cy: f32) {
    canvas_zoom_factor(ui, state, canvas::zoom_factor_from_wheel(delta_y), cx, cy);
}

fn canvas_zoom_factor(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    factor: f32,
    cx: f32,
    cy: f32,
) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.view = canvas::zoom_at(ed.view, factor, cx, cy);
        ed.view_custom = true;
    }
    push_canvas(ui, state);
}

fn canvas_fit(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let view = {
        let st = state.borrow();
        let outs = editor_outputs(&st);
        canvas::fit_view(&outs, st.canvas_w.max(32.0), st.canvas_h.max(32.0))
    };
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.view = view;
        ed.view_custom = false;
    }
    push_canvas(ui, state);
}

fn canvas_pan(ui: &AppWindow, state: &Rc<RefCell<AppState>>, dx: f32, dy: f32) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.view = canvas::pan(ed.view, dx, dy);
        ed.view_custom = true;
    }
    push_canvas(ui, state);
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
        if ed.selected != idx {
            ed.selected = idx;
            ed.drag_origin = None;
            ed.drag_press_px = None;
        }
    }
    ui.set_selected_index(idx as i32);
    push_canvas(ui, state);
    push_inspector(ui, state);
}

fn end_drag(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.drag_origin = None;
        ed.drag_press_px = None;
        ed.active_guides.clear();
    }
    push_canvas(ui, state);
}

fn drag_monitor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, idx: usize, cx: f32, cy: f32) {
    let snap_edges = ui.get_helper_snap_edges();
    let snap_centers = ui.get_helper_snap_centers();
    {
        let mut st = state.borrow_mut();
        if st.editor.as_ref().is_none_or(|e| idx >= e.positions.len()) {
            return;
        }
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        ed.selected = idx;
        if ed.drag_origin.is_none() {
            ed.drag_origin = Some(ed.positions[idx]);
            ed.drag_press_px = Some((cx, cy));
        }
        let origin = ed.drag_origin.unwrap();
        let (px, py) = ed.drag_press_px.unwrap_or((cx, cy));
        let scale = ed.view.scale.max(0.01);
        let mut pos = (
            origin.0 + ((cx - px) / scale) as i32,
            origin.1 + ((cy - py) / scale) as i32,
        );

        ed.active_guides.clear();
        if snap_edges || snap_centers {
            let rects: Vec<align::Rect> = ed
                .profile
                .monitors
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let (x, y) = if i == idx {
                        pos
                    } else {
                        ed.positions.get(i).copied().unwrap_or((0, 0))
                    };
                    let ro = ResolvedOutput {
                        name: String::new(),
                        selector: String::new(),
                        mode: m.mode,
                        position: (x, y),
                        scale: m.scale,
                        transform: m.transform,
                        enabled: true,
                    };
                    let (w, h) = canvas::logical_size(&ro);
                    align::Rect { x, y, w, h }
                })
                .collect();
            let moving = rects[idx];
            let others: Vec<align::Rect> = rects
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != idx)
                .map(|(_, r)| *r)
                .collect();
            let snapped = align::snap_position(
                moving,
                &others,
                align::SnapOpts {
                    edges: snap_edges,
                    centers: snap_centers,
                    threshold: 32,
                },
            );
            pos = (snapped.x, snapped.y);
            ed.active_guides = snapped.guides;
        }

        ed.positions[idx] = pos;
    }
    ui.set_selected_index(idx as i32);
    ui.set_editor_dirty(true);
    push_canvas(ui, state);
    let st = state.borrow();
    if let Some(ed) = &st.editor {
        let (x, y) = ed.positions[ed.selected];
        ui.set_insp_pos_x(x.to_string().into());
        ui.set_insp_pos_y(y.to_string().into());
        ui.set_insp_is_origin(x == 0 && y == 0);
    }
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
    ui.set_insp_is_origin(x == 0 && y == 0);
    ui.set_insp_enabled(m.enabled);
    ui.set_insp_unsaved(ed.unsaved.get(i).copied().unwrap_or(false));

    let mut modes = modes_for(&st.live, &m.output);
    let current = m.mode.map(|md| md.to_string()).unwrap_or_default();
    if !current.is_empty() && !modes.iter().any(|t| mode_strings_match(t, &current)) {
        modes.insert(0, current.clone());
    }
    let idx = modes
        .iter()
        .position(|t| mode_strings_match(t, &current))
        .unwrap_or(0);
    if modes.is_empty() {
        let fallback = if current.is_empty() {
            "preferred".to_string()
        } else {
            current
        };
        ui.set_insp_modes(ModelRc::new(VecModel::from(vec![SharedString::from(
            fallback,
        )])));
        ui.set_insp_mode_index(0);
    } else {
        ui.set_insp_modes(ModelRc::new(VecModel::from(
            modes
                .iter()
                .map(|t| SharedString::from(t.as_str()))
                .collect::<Vec<_>>(),
        )));
        ui.set_insp_mode_index(idx as i32);
    }
}

fn modes_for(live: &[hyprctl::HyprMonitor], selector: &str) -> Vec<String> {
    let desc = selector.strip_prefix("desc:").unwrap_or(selector);
    live.iter()
        .find(|m| m.name == selector || m.description.starts_with(desc))
        .map(|m| {
            let mut v = m.available_modes.clone();
            v.sort_by(|a, b| mode_sort_key(b).cmp(&mode_sort_key(a)));
            v.dedup();
            v
        })
        .unwrap_or_default()
}

/// Prefer larger modes first (area, then refresh) — not lexicographic "1024…" first.
fn mode_sort_key(s: &str) -> (u64, u64) {
    let (w, h, hz) = parse_mode_parts(s);
    (w.saturating_mul(h), hz)
}

fn parse_mode_parts(s: &str) -> (u64, u64, u64) {
    let s = s.trim().trim_end_matches("Hz");
    let Some((wh, rest)) = s.split_once('x') else {
        return (0, 0, 0);
    };
    let w = wh.parse().unwrap_or(0);
    let (h_str, hz_str) = rest
        .split_once('@')
        .map(|(h, hz)| (h, hz))
        .unwrap_or((rest, "0"));
    let h = h_str.parse().unwrap_or(0);
    let hz = hz_str.parse::<f64>().map(|f| f.round() as u64).unwrap_or(0);
    (w, h, hz)
}

fn mode_strings_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (aw, ah, az) = parse_mode_parts(a);
    let (bw, bh, bz) = parse_mode_parts(b);
    aw == bw && ah == bh && aw > 0 && (az == bz || az.abs_diff(bz) <= 1)
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
        ed.drag_press_px = None;
    }
    mark_dirty(ui, state);
}

fn set_enabled(ui: &AppWindow, state: &Rc<RefCell<AppState>>, en: bool) {
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.profile.monitors[ed.selected].enabled = en;
    }
    mark_dirty(ui, state);
}

fn set_as_origin(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        let i = ed.selected;
        let (ox, oy) = ed.positions[i];
        if ox == 0 && oy == 0 {
            return;
        }
        for pos in &mut ed.positions {
            pos.0 -= ox;
            pos.1 -= oy;
        }
        ed.drag_origin = None;
        ed.drag_press_px = None;
    }
    mark_dirty(ui, state);
}

fn align_to_neighbor(ui: &AppWindow, state: &Rc<RefCell<AppState>>, op: i32) {
    let Some(op) = align::AlignOp::from_i32(op) else {
        return;
    };
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        if ed.profile.monitors.len() < 2 {
            return;
        }
        let i = ed.selected;
        let rects: Vec<align::Rect> = ed
            .profile
            .monitors
            .iter()
            .enumerate()
            .map(|(j, m)| {
                let (x, y) = ed.positions.get(j).copied().unwrap_or((0, 0));
                let ro = ResolvedOutput {
                    name: String::new(),
                    selector: String::new(),
                    mode: m.mode,
                    position: (x, y),
                    scale: m.scale,
                    transform: m.transform,
                    enabled: true,
                };
                let (w, h) = canvas::logical_size(&ro);
                align::Rect { x, y, w, h }
            })
            .collect();
        let Some(j) = align::nearest_neighbor(i, &rects) else {
            return;
        };
        ed.positions[i] = align::apply_align(op, rects[i], rects[j]);
        ed.active_guides = op.guides(rects[j]);
        ed.drag_origin = None;
        ed.drag_press_px = None;
    }
    mark_dirty(ui, state);
}

fn rename_profile(ui: &AppWindow, state: &Rc<RefCell<AppState>>, raw: SharedString) {
    let name = raw.trim().to_string();
    if name.is_empty() {
        ui.set_status_text("Name cannot be empty.".into());
        if let Some(ed) = state.borrow().editor.as_ref() {
            ui.set_editor_name(ed.profile.name.clone().into());
        }
        return;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        ui.set_status_text("Name cannot contain path separators.".into());
        if let Some(ed) = state.borrow().editor.as_ref() {
            ui.set_editor_name(ed.profile.name.clone().into());
        }
        return;
    }
    let stem = sanitize_profile_name(&name);
    if stem.is_empty() {
        ui.set_status_text("Name needs letters or numbers.".into());
        if let Some(ed) = state.borrow().editor.as_ref() {
            ui.set_editor_name(ed.profile.name.clone().into());
        }
        return;
    }
    {
        let st = state.borrow();
        let Some(ed) = st.editor.as_ref() else {
            return;
        };
        if ed.profile.name == name {
            return;
        }
        let conflict = st
            .listed
            .iter()
            .any(|l| sanitize_profile_name(&l.profile.name) == stem && l.path != ed.listed.path);
        if conflict {
            drop(st);
            ui.set_status_text(format!("Profile “{stem}” already exists.").into());
            if let Some(ed) = state.borrow().editor.as_ref() {
                ui.set_editor_name(ed.profile.name.clone().into());
            }
            return;
        }
    }
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.profile.name = name.clone();
    }
    ui.set_editor_name(name.into());
    mark_dirty(ui, state);
}

fn save_editor(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let old_path;
    let path;
    let profile;
    let wrote_user_override;
    {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        for (m, pos) in ed.profile.monitors.iter_mut().zip(ed.positions.iter()) {
            m.position = Some(*pos);
        }
        let stem = sanitize_profile_name(&ed.profile.name);
        if stem.is_empty() {
            drop(st);
            ui.set_status_text("Name needs letters or numbers.".into());
            return;
        }
        ed.profile.name = stem.clone();
        old_path = ed.listed.path.clone();
        // Shared: write in place when writable, else fall back to user (no perms fight).
        let (dest, user_override) = match ed.listed.source {
            profiles_io::Source::User => (profiles_io::user_profile_path(&stem), false),
            profiles_io::Source::Shared => {
                if profiles_io::shared_writable() {
                    (profiles_io::shared_profile_path(&stem), false)
                } else {
                    (profiles_io::user_profile_path(&stem), true)
                }
            }
        };
        path = dest;
        profile = ed.profile.clone();
        wrote_user_override = user_override;
    }
    match write_atomic(&path, &profile) {
        Ok(()) => {
            if old_path != path {
                let _ = profiles_io::remove_file(&old_path);
            }
            ui.set_editor_dirty(false);
            ui.set_editor_name(profile.name.clone().into());
            ui.set_status_text("Waiting for session…".into());
            {
                let mut st = state.borrow_mut();
                st.save_wait = Some(Instant::now());
                st.save_sequence = st.save_sequence.wrapping_add(1);
            }
            if let Some(ed) = state.borrow_mut().editor.as_mut() {
                ed.listed.profile = profile;
                ed.listed.path = path.clone();
                ed.unsaved.iter_mut().for_each(|u| *u = false);
                if wrote_user_override {
                    ed.listed.source = profiles_io::Source::User;
                    ui.set_editor_source("user".into());
                    ui.set_can_promote_shared(profiles_io::shared_writable());
                }
            }
            let name = state
                .borrow()
                .editor
                .as_ref()
                .map(|e| e.profile.name.clone());
            refresh_list(ui, state);
            if let Some(name) = name {
                if state.borrow().active_name.as_deref() == Some(name.as_str()) {
                    ui.set_editor_badge("Current desk · Active".into());
                } else {
                    ui.set_editor_badge("Current desk".into());
                }
            }
        }
        Err(e) => ui.set_status_text(format!("Save failed: {e}").into()),
    }
}

fn promote_to_shared(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if !profiles_io::shared_writable() {
        ui.set_status_text("No write access to shared profiles.".into());
        return;
    }
    let (user_path, shared_path, profile) = {
        let mut st = state.borrow_mut();
        let Some(ed) = st.editor.as_mut() else {
            return;
        };
        if ed.listed.source != profiles_io::Source::User {
            return;
        }
        for (m, pos) in ed.profile.monitors.iter_mut().zip(ed.positions.iter()) {
            m.position = Some(*pos);
        }
        (
            ed.listed.path.clone(),
            profiles_io::shared_profile_path(&ed.profile.name),
            ed.profile.clone(),
        )
    };
    if let Err(e) = write_atomic(&shared_path, &profile) {
        ui.set_status_text(format!("Promote failed: {e}").into());
        return;
    }
    if let Err(e) = profiles_io::remove_file(&user_path) {
        ui.set_status_text(format!("Wrote shared, but could not remove user copy: {e}").into());
    }
    if let Some(ed) = state.borrow_mut().editor.as_mut() {
        ed.listed.profile = profile;
        ed.listed.path = shared_path;
        ed.listed.source = profiles_io::Source::Shared;
    }
    ui.set_editor_source("shared".into());
    ui.set_can_promote_shared(false);
    ui.set_editor_dirty(false);
    ui.set_status_text("Promoted to shared.".into());
    refresh_list(ui, state);
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

enum SensorKind {
    Lid,
    Ac,
    Battery,
}

fn profile_at(index: i32) -> PowerProfile {
    match index {
        0 => PowerProfile::PowerSaver,
        2 => PowerProfile::Performance,
        _ => PowerProfile::Balanced,
    }
}

fn profile_index(profile: PowerProfile) -> i32 {
    match profile {
        PowerProfile::PowerSaver => 0,
        PowerProfile::Balanced => 1,
        PowerProfile::Performance => 2,
    }
}

fn parse_low_pct(text: &str) -> Option<u8> {
    let n: i32 = text.trim().parse().ok()?;
    if (1..=50).contains(&n) {
        u8::try_from(n).ok()
    } else {
        None
    }
}

fn option_labels(options: &[sensors::SensorOption]) -> Vec<SharedString> {
    options
        .iter()
        .map(|o| SharedString::from(o.label.as_str()))
        .collect()
}

fn to_help_nodes(nodes: &[help_graph::Node]) -> ModelRc<HelpNode> {
    ModelRc::new(VecModel::from(
        nodes
            .iter()
            .map(|n| HelpNode {
                id: n.id.clone().into(),
                caption: n.caption.clone().into(),
                label: n.label.clone().into(),
                kind: n.kind.clone().into(),
                shape: n.shape.clone().into(),
                x: n.x,
                y: n.y,
                w: n.w,
                h: n.h,
                active: n.active,
            })
            .collect::<Vec<_>>(),
    ))
}

fn to_help_edges(edges: &[help_graph::Edge], active: bool) -> ModelRc<HelpEdge> {
    ModelRc::new(VecModel::from(
        edges
            .iter()
            .filter(|e| e.active == active)
            .map(|e| HelpEdge {
                id: e.id.clone().into(),
                commands: e.commands.clone().into(),
                label: e.label.clone().into(),
                mid_x: e.mid_x,
                mid_y: e.mid_y,
                active: e.active,
                pruned: e.pruned,
                slash: e.slash.clone().into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn ext_mon_count(live: &[hyprctl::HyprMonitor]) -> u32 {
    let n = live.iter().filter(|m| !m.name.starts_with("eDP")).count();
    u32::try_from(n).unwrap_or(0)
}

fn display_match_rows(
    signature: &[String],
    profiles: &[Profile],
) -> (Option<String>, Vec<(String, String, String, bool)>) {
    let mut matching: Vec<&Profile> = profiles
        .iter()
        .filter(|p| profile_matches_sig(p, signature))
        .collect();
    matching.sort_by(|a, b| {
        (b.priority, b.matches.len(), &b.name).cmp(&(a.priority, a.matches.len(), &a.name))
    });
    let winner_name = matching.first().map(|p| p.name.clone());
    let rows = matching
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let selected = winner_name.as_deref() == Some(p.name.as_str());
            let id = format!("m{i}");
            let caption = if selected {
                "selected".to_string()
            } else {
                "also matches".to_string()
            };
            let label = format!(
                "{} · {} prefixes · priority {}",
                p.name,
                p.matches.len(),
                p.priority
            );
            (id, caption, label, selected)
        })
        .collect();
    (winner_name, rows)
}

fn load_policy_ui(ui: &AppWindow) {
    let (policy, low_pct, warnings) = power_io::load();
    ui.set_docked_ac_index(profile_index(policy.docked_ac));
    ui.set_ac_index(profile_index(policy.ac));
    ui.set_battery_index(profile_index(policy.battery));
    ui.set_battery_low_index(profile_index(policy.battery_low));
    ui.set_low_percent(low_pct.to_string().into());
    if !warnings.is_empty() {
        ui.set_status_text(warnings.join(" · ").into());
    }
}

fn policy_from_ui(ui: &AppWindow) -> PowerPolicy {
    PowerPolicy {
        docked_ac: profile_at(ui.get_docked_ac_index()),
        ac: profile_at(ui.get_ac_index()),
        battery: profile_at(ui.get_battery_index()),
        battery_low: profile_at(ui.get_battery_low_index()),
    }
}

fn save_power_policy(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let Some(low_pct) = parse_low_pct(ui.get_low_percent().as_str()) else {
        ui.set_status_text("battery-low-percent must be 1–50.".into());
        return;
    };
    let policy = policy_from_ui(ui);
    match power_io::save(&policy, low_pct) {
        Ok(()) => {
            ui.set_status_text("Wrote power.conf.".into());
            refresh_power(ui, state);
        }
        Err(e) => ui.set_status_text(format!("power.conf: {e}").into()),
    }
}

fn set_sensor_pick(ui: &AppWindow, state: &Rc<RefCell<AppState>>, kind: SensorKind, index: i32) {
    let picks = state.borrow().sensor_picks.clone();
    let reading = sensors::read(&picks);
    let mut next = picks;
    match kind {
        SensorKind::Lid => {
            next.lid = sensors::persist_from_index(&reading.lid_options, index);
        }
        SensorKind::Ac => {
            next.ac = sensors::persist_from_index(&reading.ac_options, index);
        }
        SensorKind::Battery => {
            next.battery = sensors::persist_from_index(&reading.battery_options, index);
        }
    }
    if let Err(e) = sensors::save_picks(&next) {
        ui.set_status_text(format!("sensors: {e}").into());
        return;
    }
    state.borrow_mut().sensor_picks = next;
    refresh_power(ui, state);
}

fn run_override(ui: &AppWindow, state: &Rc<RefCell<AppState>>, action: &str) {
    match power_io::apply_override(action) {
        Ok(()) => {
            ui.set_status_text(format!("power {action}").into());
            refresh_power(ui, state);
        }
        Err(e) => ui.set_status_text(e.into()),
    }
}

struct ResolutionSnap {
    reading: sensors::SensorReading,
    on_ac: bool,
    low_battery: bool,
    over: Option<String>,
    applied: String,
}

fn sample_resolution(state: &AppState) -> ResolutionSnap {
    let reading = sensors::read(&state.sensor_picks);
    let frame = &state.help_live;
    let on_ac = if frame.have_frame {
        frame.on_ac
    } else {
        reading.on_ac.unwrap_or(true)
    };
    let low_battery = if frame.have_frame {
        frame.low_battery
    } else {
        false
    };
    let over = power_io::override_profile();
    let applied = if frame.have_frame && !frame.applied_profile.is_empty() {
        frame.applied_profile.clone()
    } else if frame.have_frame {
        String::from("unavailable")
    } else {
        power_io::applied_profile().unwrap_or_else(|| "unavailable".into())
    };
    ResolutionSnap {
        reading,
        on_ac,
        low_battery,
        over,
        applied,
    }
}

fn apply_help_from_live(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let live = &st.help_live;
    let (policy, _, _) = power_io::load();
    let signature = if st.signature.is_empty() {
        hyprctl::signature(&st.live)
    } else {
        st.signature.clone()
    };
    let profiles: Vec<Profile> = st.listed.iter().map(|l| l.profile.clone()).collect();
    let (_winner, match_rows) = display_match_rows(&signature, &profiles);
    let match_rows = if live.have_frame {
        telemetry::display_rows(&match_rows, &live.active_profile)
    } else {
        match_rows
            .into_iter()
            .map(|(id, c, l, _)| (id, c, l, false))
            .collect()
    };

    let ext = if live.have_frame {
        live.ext_mon_count
    } else {
        ext_mon_count(&st.live)
    };
    let ext_body = if ext == 1 {
        "1 external".to_string()
    } else {
        format!("{ext} externals")
    };
    let lid_body = if live.have_frame {
        if live.lid_closed {
            String::from("closed · daemon")
        } else {
            String::from("open · daemon")
        }
    } else {
        String::from("—")
    };
    let inh_body = if live.have_frame {
        if live.inhibitor {
            String::from("held · daemon")
        } else {
            String::from("idle · none")
        }
    } else {
        String::from("—")
    };
    let ac_body = if live.have_frame {
        if live.on_ac {
            String::from("plugged · daemon")
        } else {
            String::from("on battery · daemon")
        }
    } else {
        String::from("—")
    };
    let bat_body = if live.have_frame {
        if live.low_battery {
            String::from("low · daemon")
        } else {
            String::from("ok · daemon")
        }
    } else {
        String::from("—")
    };
    let sig_body = if signature.is_empty() {
        "(none)".into()
    } else {
        signature.join("\n")
    };
    let win = if live.have_frame {
        live.to.as_str()
    } else {
        ""
    };
    let power_base = if live.have_frame {
        live.power_base.as_str()
    } else {
        ""
    };
    let desired = if live.have_frame {
        live.desired_profile.as_str()
    } else {
        ""
    };
    let override_active = live.have_frame
        && !live.desired_profile.is_empty()
        && power_io::override_profile().as_deref() == Some(live.desired_profile.as_str());

    let claim = help_graph::Claim {
        inhibitor: live.have_frame && live.inhibitor,
        locked: live.have_frame && live.locked,
        battery_low: live.have_frame && live.low_battery,
    };
    let lock_body = if live.have_frame {
        if live.locked {
            String::from("locked · daemon")
        } else {
            String::from("unlocked · daemon")
        }
    } else {
        String::from("—")
    };
    // The daemon reports the world FSM state, not the ladder node, so the
    // input-idle box cannot show a real clock yet.
    // TODO(Q7): body from "current ladder node + claim holders" telemetry.
    let idle_body = if live.have_frame {
        String::from("not reported yet")
    } else {
        String::from("—")
    };
    let lid_g = help_graph::lid_graph(&lid_body, ext, &ext_body, &inh_body, claim, win);
    let idle_g = help_graph::idle_graph(&idle_body, &inh_body, &lock_body, &bat_body, claim, win);
    let power_g = help_graph::power_graph(
        &ac_body,
        &ext_body,
        &bat_body,
        &policy,
        desired,
        override_active,
        power_base,
    );
    let display_g = help_graph::display_graph(&sig_body, &match_rows);

    ui.set_help_status_text(if live.have_frame {
        format!("Daemon frame: {} ({})", live.kind, live.event).into()
    } else {
        "Waiting for hyprstate daemon telemetry…".into()
    });
    ui.set_help_idle_now(help_graph::idle_now(win, claim).into());
    ui.set_help_lid_now(help_graph::lid_now(win, claim).into());
    ui.set_help_power_now(help_graph::power_now(power_base, desired).into());
    ui.set_help_display_now(
        help_graph::display_now(if live.have_frame {
            live.active_profile.as_str()
        } else {
            ""
        })
        .into(),
    );

    ui.set_help_idle_nodes(to_help_nodes(&idle_g.nodes));
    ui.set_help_idle_muted_edges(to_help_edges(&idle_g.edges, false));
    ui.set_help_idle_lit_edges(to_help_edges(&idle_g.edges, true));
    ui.set_help_idle_width(idle_g.width);
    ui.set_help_idle_height(idle_g.height);
    ui.set_help_lid_nodes(to_help_nodes(&lid_g.nodes));
    ui.set_help_lid_muted_edges(to_help_edges(&lid_g.edges, false));
    ui.set_help_lid_lit_edges(to_help_edges(&lid_g.edges, true));
    ui.set_help_lid_width(lid_g.width);
    ui.set_help_lid_height(lid_g.height);
    ui.set_help_power_nodes(to_help_nodes(&power_g.nodes));
    ui.set_help_power_muted_edges(to_help_edges(&power_g.edges, false));
    ui.set_help_power_lit_edges(to_help_edges(&power_g.edges, true));
    ui.set_help_power_width(power_g.width);
    ui.set_help_power_height(power_g.height);
    ui.set_help_display_nodes(to_help_nodes(&display_g.nodes));
    ui.set_help_display_muted_edges(to_help_edges(&display_g.edges, false));
    ui.set_help_display_lit_edges(to_help_edges(&display_g.edges, true));
    ui.set_help_display_width(display_g.width);
    ui.set_help_display_height(display_g.height);
}

fn refresh_power(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let picks = state.borrow().sensor_picks.clone();
    let live = state.borrow().help_live.clone();
    let snap = sample_resolution(&state.borrow());

    if live.have_frame {
        ui.set_now_lid(
            format!(
                "lid {} (daemon)",
                if live.lid_closed { "closed" } else { "open" }
            )
            .into(),
        );
        ui.set_now_ac(if snap.on_ac {
            "AC plugged (daemon)".into()
        } else {
            "on battery (daemon)".into()
        });
        ui.set_now_battery(if snap.low_battery {
            "battery low (daemon)".into()
        } else {
            "battery ok (daemon)".into()
        });
        ui.set_now_base(live.power_base.clone().into());
        ui.set_now_desired(live.desired_profile.clone().into());
    } else {
        ui.set_now_lid(
            format!(
                "lid {} ({})",
                if snap.reading.lid_closed {
                    "closed"
                } else {
                    "open"
                },
                snap.reading.lid_source
            )
            .into(),
        );
        ui.set_now_ac(match snap.reading.on_ac {
            Some(true) => format!("AC {} online", snap.reading.ac_source).into(),
            Some(false) => format!("on battery ({} offline)", snap.reading.ac_source).into(),
            None => "AC unknown — treated as plugged (desktop default)".into(),
        });
        ui.set_now_battery(match snap.reading.battery_pct {
            Some(pct) => format!("battery {} {pct:.0}%", snap.reading.battery_source).into(),
            None => format!("no battery ({})", snap.reading.battery_source).into(),
        });
        ui.set_now_base("waiting for hyprstate daemon…".into());
        ui.set_now_desired("—".into());
    }
    ui.set_now_applied(snap.applied.clone().into());
    ui.set_now_override(snap.over.clone().unwrap_or_else(|| "none".into()).into());

    ui.set_lid_sensors(ModelRc::new(VecModel::from(option_labels(
        &snap.reading.lid_options,
    ))));
    ui.set_ac_sensors(ModelRc::new(VecModel::from(option_labels(
        &snap.reading.ac_options,
    ))));
    ui.set_battery_sensors(ModelRc::new(VecModel::from(option_labels(
        &snap.reading.battery_options,
    ))));
    ui.set_lid_sensor_index(sensors::index_of(&snap.reading.lid_options, &picks.lid));
    ui.set_ac_sensor_index(sensors::index_of(&snap.reading.ac_options, &picks.ac));
    ui.set_battery_sensor_index(sensors::index_of(
        &snap.reading.battery_options,
        &picks.battery,
    ));
}

/// Same scan the More page runs, from the environment.
fn scan_entries() -> Result<Vec<entries::Entry>, xdg_paths::Error> {
    let dirs = xdg_paths::ConfigDirs::from_env()?;
    let xdg = std::env::var("XDG_DATA_DIRS").ok();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    Ok(entries::scan(
        &entries::data_dirs(dirs.data_home(), xdg.as_deref()),
        &entries::current_desktop(desktop.as_deref()),
    ))
}

/// `dials --entries`: what More would list, one per line.
fn print_entries() -> Result<(), Box<dyn std::error::Error>> {
    for e in scan_entries()? {
        println!("{}\t{}\t{}\t{}", e.section, e.id, e.name, e.argv.join(" "));
    }
    Ok(())
}

/// Rescan XDG desktop entries for the More page. Missing XDG dirs are an
/// empty list, not an error: the page just says so.
fn refresh_entries(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let found = match scan_entries() {
        Ok(found) => found,
        Err(e) => {
            ui.set_status_text(format!("XDG paths: {e}").into());
            Vec::new()
        }
    };
    let rows: Vec<SettingsEntry> = found
        .iter()
        .map(|e| SettingsEntry {
            name: e.name.clone().into(),
            comment: e.comment.clone().into(),
            section: e.section.clone().into(),
        })
        .collect();
    ui.set_entries(ModelRc::from(Rc::new(VecModel::from(rows))));
    state.borrow_mut().entries = found;
}

#[cfg(test)]
mod idle_tests {
    #[test]
    fn native_event_loop_has_no_periodic_pollers() {
        let repeated_timer = ["TimerMode", "::Repeated"].concat();
        assert!(
            !include_str!("main.rs").contains(&repeated_timer),
            "periodic Slint timers prevent indefinite idle sleep"
        );
    }
}

#[cfg(test)]
mod unsaved_monitor_tests {
    use super::*;

    fn live(name: &str, desc: &str, x: i32) -> hyprctl::HyprMonitor {
        hyprctl::HyprMonitor {
            name: name.into(),
            description: desc.into(),
            x,
            y: 0,
            width: 1920,
            height: 1080,
            transform: 0,
            scale: 1.0,
            available_modes: vec![],
            refresh_rate: 60.0,
        }
    }

    #[test]
    fn newly_plugged_outputs_are_reported_and_seeded_from_live() {
        let live = vec![
            live("DP-1", "Dell U2720Q ABC12345", 0),
            live("HDMI-A-1", "LG HDR 4K 0x0001", 1920),
        ];
        let connected = hyprctl::connected(&live);
        let profile = Profile {
            monitors: vec![Monitor {
                output: "desc:Dell U2720Q".into(),
                mode: None,
                scale: 1.0,
                position: None,
                transform: 0,
                enabled: true,
            }],
            ..seed_profile_from_live(&[], "x").0
        };
        let missing = live_missing_from_profile(&profile, &live, &connected);
        assert_eq!(missing.len(), 1);
        let m = monitor_from_live(missing[0]);
        assert_eq!(m.output, "desc:LG HDR 4K");
        assert_eq!(m.position, Some((1920, 0)));
        assert_eq!(m.mode.map(|md| md.width), Some(1920));

        // Once the entry exists, nothing is missing.
        let mut full = profile.clone();
        full.monitors.push(m);
        assert!(live_missing_from_profile(&full, &live, &connected).is_empty());
    }
}
