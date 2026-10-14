//! System: hostname, time zone, automatic time, language and keyboard layout
//! through systemd's hostnamed, timedated and localed. Writes go through the
//! admin worker, since each one may wait on a polkit prompt.
use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::admin::{Cmd, SystemCmd, SystemEvent};
use crate::{AppState, AppWindow};

pub mod systemd;

/// Picker rows shown under a search box.
const MATCH_LIMIT: usize = 8;

#[derive(Default)]
pub struct State {
    snapshot: systemd::Snapshot,
    lists: systemd::Lists,
    lists_requested: bool,
}

/// Bind the page's callbacks. Nothing is read until the page is opened.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sys_apply_hostname(move |name| {
            let ui = ui_weak.unwrap();
            let name = name.trim().to_string();
            match systemd::validate_static_hostname(&name) {
                Ok(()) => write(&ui, &state, SystemCmd::SetStaticHostname(name)),
                Err(e) => ui.set_sys_status_text(e.into()),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sys_apply_pretty(move |name| {
            let ui = ui_weak.unwrap();
            let name = name.trim().to_string();
            match systemd::validate_pretty_hostname(&name) {
                Ok(()) => write(&ui, &state, SystemCmd::SetPrettyHostname(name)),
                Err(e) => ui.set_sys_status_text(e.into()),
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sys_set_ntp(move |on| write(&ui_weak.unwrap(), &state, SystemCmd::SetNtp(on)));
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sys_query_changed(move |kind, query| {
            update_matches(&ui_weak.unwrap(), &state, kind, &query);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_sys_picked(move |kind, idx| pick(&ui_weak.unwrap(), &state, kind, idx));
    }
}

/// The page was opened: re-read, and load the picker lists once.
pub fn enter(_ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    send(state, SystemCmd::Refresh);
    let first = !std::mem::replace(&mut state.borrow_mut().system.lists_requested, true);
    if first {
        send(state, SystemCmd::LoadLists);
    }
}

pub fn on_event(ui: &AppWindow, state: &Rc<RefCell<AppState>>, event: SystemEvent) {
    match event {
        SystemEvent::Snapshot(snap) => apply_snapshot(ui, state, snap),
        SystemEvent::Lists(lists) => {
            state.borrow_mut().system.lists = lists;
            // Re-run any search typed before the lists arrived.
            update_matches(ui, state, 0, &ui.get_sys_tz_query());
            update_matches(ui, state, 1, &ui.get_sys_lang_query());
            update_matches(ui, state, 2, &ui.get_sys_layout_query());
        }
        SystemEvent::Status(text) => ui.set_sys_status_text(text.into()),
    }
}

fn send(state: &Rc<RefCell<AppState>>, cmd: SystemCmd) {
    state.borrow_mut().admin.send(Cmd::System(cmd));
}

/// A write: say so first, since the polkit prompt may take a while.
fn write(ui: &AppWindow, state: &Rc<RefCell<AppState>>, cmd: SystemCmd) {
    ui.set_sys_status_text("Waiting for authorization…".into());
    send(state, cmd);
}

fn apply_snapshot(ui: &AppWindow, state: &Rc<RefCell<AppState>>, snap: systemd::Snapshot) {
    let mut st = state.borrow_mut();
    let st = &mut st.system;
    // Follow the live value unless the user has an edit in progress.
    if ui.get_sys_hostname_draft() == st.snapshot.static_hostname.as_str() {
        ui.set_sys_hostname_draft(snap.static_hostname.as_str().into());
    }
    if ui.get_sys_pretty_draft() == st.snapshot.pretty_hostname.as_str() {
        ui.set_sys_pretty_draft(snap.pretty_hostname.as_str().into());
    }
    ui.set_sys_static_hostname(snap.static_hostname.as_str().into());
    ui.set_sys_pretty_hostname(snap.pretty_hostname.as_str().into());
    ui.set_sys_timezone(snap.timezone.as_str().into());
    ui.set_sys_can_ntp(snap.can_ntp);
    ui.set_sys_ntp(snap.ntp);
    ui.set_sys_ntp_synced(snap.ntp_synced);
    ui.set_sys_lang(systemd::lang_of(&snap.locale).into());
    let layout = match snap.x11_variant.as_str() {
        "" => snap.x11_layout.clone(),
        variant => format!("{} ({variant})", snap.x11_layout),
    };
    ui.set_sys_layout(layout.into());
    st.snapshot = snap;
}

fn update_matches(ui: &AppWindow, state: &Rc<RefCell<AppState>>, kind: i32, query: &str) {
    let st = state.borrow();
    let lists = &st.system.lists;
    let source = match kind {
        0 => &lists.timezones,
        1 => &lists.locales,
        _ => &lists.layouts,
    };
    let rows: Vec<SharedString> = systemd::matches(source, query, MATCH_LIMIT)
        .into_iter()
        .map(SharedString::from)
        .collect();
    let model = ModelRc::new(VecModel::from(rows));
    match kind {
        0 => ui.set_sys_tz_matches(model),
        1 => ui.set_sys_lang_matches(model),
        _ => ui.set_sys_layout_matches(model),
    }
}

fn pick(ui: &AppWindow, state: &Rc<RefCell<AppState>>, kind: i32, idx: i32) {
    let model = match kind {
        0 => ui.get_sys_tz_matches(),
        1 => ui.get_sys_lang_matches(),
        _ => ui.get_sys_layout_matches(),
    };
    let Some(value) = model.row_data(idx as usize).map(|v| v.to_string()) else {
        return;
    };
    let empty = ModelRc::new(VecModel::<SharedString>::default());
    let cmd = match kind {
        0 => {
            ui.set_sys_tz_query("".into());
            ui.set_sys_tz_matches(empty);
            SystemCmd::SetTimezone(value)
        }
        1 => {
            ui.set_sys_lang_query("".into());
            ui.set_sys_lang_matches(empty);
            SystemCmd::SetLang(value)
        }
        _ => {
            ui.set_sys_layout_query("".into());
            ui.set_sys_layout_matches(empty);
            SystemCmd::SetLayout(value)
        }
    };
    write(ui, state, cmd);
}

/// `dials --system`: the page's reads, without the UI.
pub fn print_system() -> Result<(), Box<dyn std::error::Error>> {
    let conn = zbus::blocking::Connection::system()?;
    let (snap, errors) = systemd::read(&conn);
    println!("{snap:#?}");
    for e in errors {
        eprintln!("{e}");
    }
    let lists = systemd::read_lists(&conn);
    println!(
        "lists: {} time zones, {} locales, {} layouts",
        lists.timezones.len(),
        lists.locales.len(),
        lists.layouts.len()
    );
    Ok(())
}
