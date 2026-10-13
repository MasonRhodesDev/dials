//! More: every other settings tool installed on the system, from its XDG
//! desktop entry, launched in its own window. Third-party tools stay
//! external: dials never embeds or imports one.
use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppState, AppWindow, SettingsEntry, entries};

pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
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
    refresh_entries(ui, state);
}

/// The More section was opened.
pub fn enter(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    refresh_entries(ui, state);
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
pub fn print_entries() -> Result<(), Box<dyn std::error::Error>> {
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
