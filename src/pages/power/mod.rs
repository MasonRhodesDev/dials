//! Power: hyprstate's power policy map (`~/.config/hypr/power.conf`),
//! sensor picks, and overrides through `hyprstate power`.
use std::cell::RefCell;
use std::rc::Rc;

use hyprstate_fsm::power::{PowerPolicy, PowerProfile};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppState, AppWindow};

pub mod power_io;
pub mod sensors;

pub struct State {
    pub sensor_picks: sensors::SensorPicks,
}

impl State {
    pub fn new() -> Self {
        Self {
            sensor_picks: sensors::load_picks(),
        }
    }
}

/// Bind the page's callbacks and load the policy.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
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
    load_policy_ui(ui);
    refresh(ui, state);
}

/// The Power section was opened.
pub fn enter(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    refresh(ui, state);
}

/// A new hyprstate telemetry frame is in `state.hyprstate`.
pub fn on_telemetry_frame(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    if ui.get_section() == crate::pages::POWER {
        refresh(ui, state);
    }
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
            refresh(ui, state);
        }
        Err(e) => ui.set_status_text(format!("power.conf: {e}").into()),
    }
}

fn set_sensor_pick(ui: &AppWindow, state: &Rc<RefCell<AppState>>, kind: SensorKind, index: i32) {
    let picks = state.borrow().power.sensor_picks.clone();
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
    state.borrow_mut().power.sensor_picks = next;
    refresh(ui, state);
}

fn run_override(ui: &AppWindow, state: &Rc<RefCell<AppState>>, action: &str) {
    match power_io::apply_override(action) {
        Ok(()) => {
            ui.set_status_text(format!("power {action}").into());
            refresh(ui, state);
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
    let reading = sensors::read(&state.power.sensor_picks);
    let frame = &state.hyprstate;
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

fn refresh(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let picks = state.borrow().power.sensor_picks.clone();
    let live = state.borrow().hyprstate.clone();
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
