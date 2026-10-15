//! dials — desktop settings. One window; each core-family integration is a
//! page module behind a cargo feature (see `pages`), and the More page
//! launches every other settings tool from its XDG desktop entry.
#[cfg(feature = "admin")]
mod admin;
mod entries;
#[cfg(feature = "hyprstate")]
mod hyprstate;
mod pages;

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;
use slint_kit::{ThemeBridge, apply_theme};

slint::include_modules!();

/// Shared application state: one slot per compiled page. Pages touch only
/// their own slot, except through the hooks in `pages`.
pub struct AppState {
    #[cfg(feature = "displays")]
    pub displays: pages::displays::State,
    #[cfg(feature = "power")]
    pub power: pages::power::State,
    /// Latest hyprstate telemetry frame.
    #[cfg(feature = "hyprstate")]
    pub hyprstate: hyprstate::telemetry::HelpLive,
    #[cfg(feature = "wallpaper")]
    pub wallpaper: pages::wallpaper::State,
    #[cfg(feature = "appearance")]
    pub appearance: pages::appearance::State,
    /// Worker for Users and System writes (started on first use).
    #[cfg(feature = "admin")]
    pub admin: admin::Client,
    #[cfg(feature = "users")]
    pub users: pages::users::State,
    #[cfg(feature = "system")]
    pub system: pages::system::State,
    /// External settings tools (More page), in display order.
    pub entries: Vec<entries::Entry>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--entries") => return pages::more::print_entries(),
        #[cfg(feature = "system")]
        Some("--system") => return pages::system::print_system(),
        Some("--help" | "-h") => {
            println!(
                "usage: dials [--entries | --system]\n  --entries  list the settings tools the More page would show, then exit\n  --system   print what the System page reads from systemd, then exit"
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

    let state = Rc::new(RefCell::new(AppState {
        #[cfg(feature = "displays")]
        displays: pages::displays::State::new(&ui),
        #[cfg(feature = "power")]
        power: pages::power::State::new(),
        #[cfg(feature = "hyprstate")]
        hyprstate: Default::default(),
        #[cfg(feature = "wallpaper")]
        wallpaper: Default::default(),
        #[cfg(feature = "appearance")]
        appearance: Default::default(),
        #[cfg(feature = "admin")]
        admin: admin::Client::new(&ui),
        #[cfg(feature = "users")]
        users: Default::default(),
        #[cfg(feature = "system")]
        system: Default::default(),
        entries: Vec::new(),
    }));

    let first = pages::configure_toolbar(&ui);
    ui.set_section(first);
    pages::attach(&ui, &state);
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_section_changed(move |section| {
            let ui = ui_weak.unwrap();
            pages::enter_section(&ui, &state, section);
        });
    }
    #[cfg(feature = "hyprstate")]
    hyprstate::attach(&ui, &state);

    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod idle_tests {
    /// Every page module, whether or not its feature is on.
    const SOURCES: &[&str] = &[
        include_str!("main.rs"),
        include_str!("pages/mod.rs"),
        include_str!("pages/more.rs"),
        include_str!("pages/displays/mod.rs"),
        include_str!("pages/power/mod.rs"),
        include_str!("pages/help/mod.rs"),
        include_str!("pages/wallpaper/mod.rs"),
        include_str!("pages/appearance/mod.rs"),
        include_str!("pages/users/mod.rs"),
        include_str!("pages/system/mod.rs"),
        include_str!("admin/mod.rs"),
        include_str!("hyprstate/mod.rs"),
    ];

    #[test]
    fn native_event_loop_has_no_periodic_pollers() {
        let repeated_timer = ["TimerMode", "::Repeated"].concat();
        for src in SOURCES {
            assert!(
                !src.contains(&repeated_timer),
                "periodic Slint timers prevent indefinite idle sleep"
            );
        }
    }
}
