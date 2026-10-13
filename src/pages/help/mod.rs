//! Help: live graphs of how hyprstate derives lid, idle, power and display
//! state, from the daemon's telemetry plus the Displays and Power pages' data.
use std::cell::RefCell;
use std::rc::Rc;

use monitor_profiles::Profile;
use slint::{ModelRc, VecModel};

use crate::hyprstate::telemetry;
use crate::pages::displays::{hyprctl, profile_matches_sig};
use crate::pages::power::power_io;
use crate::{AppState, AppWindow, HelpEdge, HelpNode};

pub mod help_graph;

/// Draw the graphs from whatever is known at startup.
pub fn attach(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    apply(ui, state);
}

/// The Help section was opened.
pub fn enter(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    apply(ui, state);
}

/// A new hyprstate telemetry frame is in `state.hyprstate`.
pub fn on_telemetry_frame(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    apply(ui, state);
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
                active: e.active,
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

pub fn apply(ui: &AppWindow, state: &Rc<RefCell<AppState>>) {
    let st = state.borrow();
    let live = &st.hyprstate;
    let (policy, _, _) = power_io::load();
    let signature = if st.displays.signature.is_empty() {
        hyprctl::signature(&st.displays.live)
    } else {
        st.displays.signature.clone()
    };
    let profiles: Vec<Profile> = st
        .displays
        .listed
        .iter()
        .map(|l| l.profile.clone())
        .collect();
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
        ext_mon_count(&st.displays.live)
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
