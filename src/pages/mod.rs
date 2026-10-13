//! Page modules. Each core-family integration (hyprstate, monitor-profiles,
//! lmtt and the appearance registry) is an in-process module behind a cargo
//! feature: it owns its state, its Slint callbacks and its IO, and reaches
//! other pages only through the hooks below. A build without a feature drops
//! that page and its crates. Third-party tools are never pages: the More page
//! launches them in their own window.
use std::cell::RefCell;
use std::rc::Rc;

use crate::{AppState, AppWindow};

#[cfg(feature = "appearance")]
pub mod appearance;
#[cfg(feature = "displays")]
pub mod displays;
#[cfg(feature = "help")]
pub mod help;
pub mod more;
#[cfg(feature = "power")]
pub mod power;
#[cfg(feature = "wallpaper")]
pub mod wallpaper;

/// Toolbar section indices. A page that is not compiled in, or whose backing
/// tool is absent, is hidden; the others keep their index.
pub const DISPLAYS: i32 = 0;
pub const POWER: i32 = 1;
pub const HELP: i32 = 2;
pub const MORE: i32 = 3;
#[cfg_attr(not(feature = "appearance"), allow(dead_code))]
pub const APPEARANCE: i32 = 4;

/// Show the toolbar buttons for the pages this build has and the machine
/// can use; returns the section to open first.
pub fn configure_toolbar(ui: &AppWindow) -> i32 {
    #[cfg(feature = "displays")]
    let displays = displays::available();
    #[cfg(not(feature = "displays"))]
    let displays = false;
    ui.set_page_displays(displays);
    ui.set_page_power(cfg!(feature = "power"));
    ui.set_page_help(cfg!(feature = "help"));
    ui.set_page_appearance(cfg!(feature = "appearance"));
    if displays {
        DISPLAYS
    } else if cfg!(feature = "power") {
        POWER
    } else if cfg!(feature = "help") {
        HELP
    } else {
        MORE
    }
}

/// Bind every compiled page's callbacks and load its initial state.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    #[cfg(feature = "displays")]
    displays::attach(ui, state);
    #[cfg(feature = "power")]
    power::attach(ui, state);
    #[cfg(feature = "help")]
    help::attach(ui, state);
    #[cfg(feature = "wallpaper")]
    wallpaper::attach(ui, state);
    #[cfg(feature = "appearance")]
    appearance::attach(ui, state);
    more::attach(ui, state);
}

/// The toolbar switched to `section`.
pub fn enter_section(ui: &AppWindow, state: &Rc<RefCell<AppState>>, section: i32) {
    #[cfg(feature = "power")]
    if section == POWER {
        power::enter(ui, state);
    }
    #[cfg(feature = "help")]
    if section == HELP {
        help::enter(ui, state);
    }
    if section == MORE {
        more::enter(ui, state);
    }
    #[cfg(feature = "appearance")]
    if section == APPEARANCE {
        appearance::enter(ui, state);
    }
}

/// hyprstate delivered a telemetry frame; it is already in `state.hyprstate`.
#[cfg(feature = "hyprstate")]
pub fn telemetry_frame(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    #[cfg(feature = "help")]
    help::on_telemetry_frame(ui, state);
    #[cfg(feature = "power")]
    power::on_telemetry_frame(ui, state);
    #[cfg(not(any(feature = "help", feature = "power")))]
    let _ = (ui, state);
}

/// Displays rebuilt its canvas rows: decorate them.
#[cfg(feature = "displays")]
pub fn canvas_updated(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    #[cfg(feature = "wallpaper")]
    wallpaper::on_canvas_updated(ui, state);
    #[cfg(not(feature = "wallpaper"))]
    let _ = (ui, state);
}

/// Displays changed the selected monitor or redrew its inspector.
#[cfg(feature = "displays")]
pub fn selection_changed(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    #[cfg(feature = "wallpaper")]
    wallpaper::on_selection_changed(ui, state);
    #[cfg(not(feature = "wallpaper"))]
    let _ = (ui, state);
}
