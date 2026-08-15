//! Help graphs: rule cascade + plain-language on-enter effects.
//! Lit path comes from daemon telemetry (`win` / power_base / active profile),
//! not from a parallel GUI recompute.
//!
//! Layout is a left→right flowchart: inputs → questions → state → effects,
//! with orthogonal edges that dock to node borders (not floating beziers).
use hyprstate_fsm::power::{BaseState, PowerPolicy};

const PAD: f32 = 8.0;
const GUTTER: f32 = 64.0;
const IN_W: f32 = 168.0;
const IN_H: f32 = 48.0;
const LOGIC_W: f32 = 200.0;
const STATE_W: f32 = 148.0;
const FX_W: f32 = 260.0;
const ROW_H: f32 = 84.0;
const GAP_Y: f32 = 16.0;

const COL0: f32 = PAD;
const COL1: f32 = COL0 + IN_W + GUTTER;
const COL2: f32 = COL1 + LOGIC_W + GUTTER;
const COL3: f32 = COL2 + STATE_W + GUTTER;

struct Step {
    q_id: &'static str,
    q_cap: &'static str,
    q_label: &'static str,
    state_id: &'static str,
    state_label: &'static str,
    fx_id: &'static str,
    fx_label: String,
    taken: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub caption: String,
    pub label: String,
    pub kind: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub id: String,
    pub commands: String,
    pub active: bool,
}

pub fn lid_now(win: &str, inhibitor: bool) -> String {
    match win {
        "LID_OPEN" => {
            if inhibitor {
                "Now: defer/awake.".into()
            } else {
                "Now: idle/sleep.".into()
            }
        }
        "DOCKED" => {
            if inhibitor {
                "Now: docked — ignore lid; defer/awake.".into()
            } else {
                "Now: docked — ignore lid; idle/sleep.".into()
            }
        }
        "DEFERRED" => "Now: defer/awake — lid closed undocked; pause media.".into(),
        "COUNTDOWN" => "Now: idle/sleep — 30s grace, then lock and suspend.".into(),
        "SUSPENDING" => "Now: idle/sleep — locking and suspending.".into(),
        "" => "Now: waiting for hyprstate daemon…".into(),
        other => format!("Now: {other}"),
    }
}

pub fn power_now(base: &str, desired: &str) -> String {
    if base.is_empty() {
        return "Now: waiting for hyprstate daemon…".into();
    }
    if desired.is_empty() {
        format!("Now: power base {base}.")
    } else {
        format!("Now: {base} → apply {desired}.")
    }
}

pub fn display_now(active: &str) -> String {
    if active.is_empty() {
        "Now: waiting for hyprstate daemon…".into()
    } else {
        format!("Now: apply layout “{active}”.")
    }
}

/// `win` is the daemon's current State label (e.g. LID_OPEN).
/// `inhibitor` is the live idle-inhibit bit (logind or wayland).
///
/// Lid and dock are path, not terminals. The only terminals are idle/sleep
/// and defer/awake. 30s grace precedes idle/sleep when the lid is closed
/// and undocked.
pub fn lid_graph(
    lid_body: &str,
    ext_mon_count: u32,
    ext_body: &str,
    inh_body: &str,
    inhibitor: bool,
    win: &str,
) -> Graph {
    let open = win == "LID_OPEN";
    let docked = win == "DOCKED";
    let deferred = win == "DEFERRED";
    let countdown = win == "COUNTDOWN";
    let suspending = win == "SUSPENDING";
    let closed = docked || deferred || countdown || suspending;
    let known = open || closed;
    let grace = countdown || suspending;
    let defer = (open && inhibitor) || (docked && inhibitor) || deferred;
    let sleep = (open && !inhibitor) || (docked && !inhibitor) || grace;

    let inter_w = 168.0;
    let term_w = 168.0;
    let col_inter = COL2;
    let col_policy = col_inter + inter_w + GUTTER;
    let col_grace = col_policy + LOGIC_W + GUTTER;
    let col_term = col_grace + inter_w + GUTTER;

    let mut nodes = Vec::new();
    stack_inputs(
        &mut nodes,
        &[
            ("lid", "lid", lid_body, true),
            ("ext", "ext", ext_body, ext_mon_count >= 1),
            ("inh", "idle-inhibit", inh_body, inhibitor),
        ],
    );
    nodes.push(node(
        "q-open",
        "if",
        "lid open?",
        "logic",
        COL1,
        span_center(0, 1, ROW_H),
        LOGIC_W,
        ROW_H,
        known,
    ));
    nodes.push(node(
        "q-ext",
        "else",
        "externals ≥ 1?",
        "logic",
        COL1,
        col_y(2, ROW_H),
        LOGIC_W,
        ROW_H,
        closed,
    ));
    nodes.push(node(
        "DOCKED",
        "intermediate",
        "ignore lid",
        "out",
        col_inter,
        col_y(2, ROW_H),
        inter_w,
        ROW_H,
        docked,
    ));
    nodes.push(node(
        "policy",
        "resolve",
        "idle policy\nheld → defer\nreleased → idle",
        "logic",
        col_policy,
        col_y(1, ROW_H),
        LOGIC_W,
        ROW_H,
        known,
    ));
    nodes.push(node(
        "COUNTDOWN",
        "intermediate",
        if suspending {
            "lid action · 30s elapsed\nlocking and suspending"
        } else {
            "30s grace\nlid actions only"
        },
        "out",
        col_grace,
        col_y(2, ROW_H),
        inter_w,
        ROW_H,
        grace,
    ));
    nodes.push(node(
        "t-defer",
        "terminal",
        "defer/awake",
        "out",
        col_term,
        col_y(0, ROW_H),
        term_w,
        ROW_H,
        defer,
    ));
    nodes.push(node(
        "t-sleep",
        "terminal",
        "idle/sleep",
        "out",
        col_term,
        col_y(1, ROW_H),
        term_w,
        ROW_H,
        sleep,
    ));

    let mut edges = Vec::new();
    edges.push(ortho_h(&nodes, "lid", "q-open", known));
    edges.push(ortho_h(&nodes, "ext", "q-ext", closed));
    edges.push(ortho_h(&nodes, "q-open", "policy", open));
    edges.push(ortho_v_spine(&nodes, "q-open", "q-ext", closed));
    edges.push(ortho_h(&nodes, "q-ext", "DOCKED", docked));
    edges.push(ortho_h(&nodes, "DOCKED", "policy", docked));

    let bottom = last_bottom(&nodes) + GAP_Y;
    edges.push(ortho_bottom_lane(
        &nodes,
        "inh",
        "policy",
        known,
        COL1 - 32.0,
        col_policy - GUTTER * 0.5,
        bottom,
    ));
    edges.push(ortho_bottom_lane(
        &nodes,
        "q-ext",
        "policy",
        deferred || grace,
        COL2 - 32.0,
        col_policy - GUTTER * 0.5 + 12.0,
        bottom + 12.0,
    ));

    edges.push(ortho_h(&nodes, "policy", "t-defer", defer));
    edges.push(ortho_h(&nodes, "policy", "t-sleep", sleep && !grace));
    edges.push(ortho_h(&nodes, "policy", "COUNTDOWN", grace));
    edges.push(ortho_h(&nodes, "COUNTDOWN", "t-sleep", grace));
    let mut graph = finish(nodes, edges);
    graph.height = bottom + 12.0 + PAD;
    graph
}

/// `win` is daemon `power_base` (docked-ac|ac|battery|battery-low).
pub fn power_graph(
    ac_body: &str,
    ext_body: &str,
    bat_body: &str,
    policy: &PowerPolicy,
    desired: &str,
    override_active: bool,
    win: &str,
) -> Graph {
    let apply = if override_active {
        format!("Apply power profile ({desired})\nManual override (skips map)")
    } else if desired.is_empty() {
        "Apply power profile".into()
    } else {
        format!("Apply power profile ({desired})\nFrom power.conf map")
    };
    let would = |b: BaseState| format!("Would map to {}", policy.for_base(b).as_str());
    cascade(
        &[
            ("ac", "ac", ac_body, true),
            ("ext", "ext", ext_body, true),
            ("bat", "battery", bat_body, true),
        ],
        &[
            ("ac", "q-dock"),
            ("ext", "q-dock"),
            ("ac", "q-ac"),
            ("bat", "q-low"),
        ],
        &[
            Step {
                q_id: "q-dock",
                q_cap: "if",
                q_label: "AC and externals ≥ 1",
                state_id: "docked-ac",
                state_label: "docked-ac",
                fx_id: "fx-docked-ac",
                fx_label: if win == "docked-ac" {
                    apply.clone()
                } else {
                    would(BaseState::DockedAc)
                },
                taken: win == "docked-ac",
            },
            Step {
                q_id: "q-ac",
                q_cap: "else if",
                q_label: "AC, no externals",
                state_id: "ac-out",
                state_label: "ac",
                fx_id: "fx-ac",
                fx_label: if win == "ac" {
                    apply.clone()
                } else {
                    would(BaseState::Ac)
                },
                taken: win == "ac",
            },
            Step {
                q_id: "q-low",
                q_cap: "else if",
                q_label: "on battery, low",
                state_id: "battery-low",
                state_label: "battery-low",
                fx_id: "fx-battery-low",
                fx_label: if win == "battery-low" {
                    apply.clone()
                } else {
                    would(BaseState::BatteryLow)
                },
                taken: win == "battery-low",
            },
            Step {
                q_id: "q-bat",
                q_cap: "else",
                q_label: "on battery",
                state_id: "battery",
                state_label: "battery",
                fx_id: "fx-battery",
                fx_label: if win == "battery" {
                    apply
                } else {
                    would(BaseState::Battery)
                },
                taken: win == "battery",
            },
        ],
    )
}

/// `matches`: (id, caption, label, selected) already lit from daemon active_profile.
pub fn display_graph(signature_body: &str, matches: &[(String, String, String, bool)]) -> Graph {
    let mut nodes = Vec::new();
    let sig_w = 280.0;
    let sig_lines = signature_body.lines().count().max(1);
    let sig_lines = f32::from(u16::try_from(sig_lines).unwrap_or(u16::MAX));
    let sig_h = (36.0 + sig_lines * 18.0).max(IN_H);
    let logic_x = PAD + sig_w + GUTTER;
    let match_x = logic_x + LOGIC_W + GUTTER;
    let action_x = match_x + STATE_W + 24.0 + GUTTER;
    nodes.push(node(
        "sig",
        "signature",
        signature_body,
        "input",
        PAD,
        PAD,
        sig_w,
        sig_h,
        true,
    ));
    nodes.push(node(
        "select",
        "rank",
        "Priority → prefix count → name\nall descending",
        "logic",
        logic_x,
        PAD,
        LOGIC_W,
        ROW_H,
        true,
    ));
    if matches.is_empty() {
        nodes.push(node(
            "none",
            "no match",
            "no profile matches",
            "out",
            match_x,
            PAD,
            STATE_W,
            ROW_H,
            true,
        ));
        nodes.push(node(
            "fx-none",
            "result",
            "Do not apply a layout",
            "effect",
            action_x,
            PAD,
            FX_W,
            ROW_H,
            true,
        ));
    } else {
        for (i, (id, caption, label, selected)) in matches.iter().enumerate() {
            let y = col_y(i, ROW_H);
            nodes.push(node(
                id,
                caption,
                label,
                "out",
                match_x,
                y,
                STATE_W + 24.0,
                ROW_H,
                *selected,
            ));
            nodes.push(node(
                format!("fx-{id}"),
                "action",
                if *selected {
                    "Apply monitor layout\nRemember GPU for next login\nCheck GPU drift"
                } else {
                    "Not applied"
                },
                "effect",
                action_x,
                y,
                FX_W,
                ROW_H,
                *selected,
            ));
        }
    }

    let mut edges = Vec::new();
    edges.push(ortho_h(&nodes, "sig", "select", true));
    if matches.is_empty() {
        edges.push(ortho_h(&nodes, "select", "none", true));
        edges.push(ortho_h(&nodes, "none", "fx-none", true));
    } else {
        for (id, _, _, selected) in matches {
            edges.push(ortho_h(&nodes, "select", id, *selected));
            edges.push(ortho_h(&nodes, id, &format!("fx-{id}"), *selected));
        }
    }
    finish(nodes, edges)
}

fn cascade(
    inputs: &[(&str, &str, &str, bool)],
    input_wires: &[(&str, &str)],
    steps: &[Step],
) -> Graph {
    let mut nodes = Vec::new();
    stack_inputs(&mut nodes, inputs);
    let win = steps.iter().position(|s| s.taken);
    for (i, step) in steps.iter().enumerate() {
        let y = col_y(i, ROW_H);
        nodes.push(node(
            step.q_id,
            step.q_cap,
            step.q_label,
            "logic",
            COL1,
            y,
            LOGIC_W,
            ROW_H,
            step.taken,
        ));
        nodes.push(node(
            step.state_id,
            "base",
            step.state_label,
            "out",
            COL2,
            y,
            STATE_W,
            ROW_H,
            step.taken,
        ));
        nodes.push(node(
            step.fx_id,
            "mapping",
            &step.fx_label,
            "effect",
            COL3,
            y,
            FX_W,
            ROW_H,
            step.taken,
        ));
    }

    let mut edges = Vec::new();
    // Input → question: structural muted unless that question was evaluated
    // (index <= winner) so dead arcs don't light up like a lit path.
    for (from, to) in input_wires {
        let q_idx = steps.iter().position(|s| s.q_id == *to);
        let evaluated = match (win, q_idx) {
            (Some(w), Some(q)) => q <= w,
            _ => false,
        };
        edges.push(ortho_h(&nodes, from, to, evaluated));
    }
    for (i, step) in steps.iter().enumerate() {
        edges.push(ortho_h(&nodes, step.q_id, step.state_id, step.taken));
        edges.push(ortho_h(&nodes, step.state_id, step.fx_id, step.taken));
        if let Some(next) = steps.get(i + 1) {
            // Fallthrough spine: lit when the winner is below this step.
            let fell = win.is_some_and(|w| w > i);
            edges.push(ortho_v_spine(&nodes, step.q_id, next.q_id, fell));
        }
    }
    finish(nodes, edges)
}

fn stack_inputs(nodes: &mut Vec<Node>, items: &[(&str, &str, &str, bool)]) {
    for (i, (id, caption, body, active)) in items.iter().enumerate() {
        let y = col_y(i, IN_H);
        nodes.push(node(
            *id, *caption, *body, "input", COL0, y, IN_W, IN_H, *active,
        ));
    }
}

fn col_y(index: usize, h: f32) -> f32 {
    PAD + f32::from(u16::try_from(index).unwrap_or(0)) * (h + GAP_Y)
}

fn span_center(row0: usize, row1: usize, h: f32) -> f32 {
    let top = col_y(row0, ROW_H);
    let bot = col_y(row1, ROW_H) + ROW_H;
    top + (bot - top - h) * 0.5
}

fn last_bottom(nodes: &[Node]) -> f32 {
    nodes.iter().map(|n| n.y + n.h).fold(0.0, f32::max)
}

fn node(
    id: impl Into<String>,
    caption: impl Into<String>,
    label: impl Into<String>,
    kind: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    active: bool,
) -> Node {
    Node {
        id: id.into(),
        caption: caption.into(),
        label: label.into(),
        kind: kind.into(),
        x,
        y,
        w,
        h,
        active,
    }
}

fn find<'a>(nodes: &'a [Node], id: &str) -> &'a Node {
    nodes.iter().find(|n| n.id == id).expect("node")
}

/// Orthogonal left→right edge: out mid-right → gutter → in mid-left.
/// Muted elbows use a parallel gutter lane so they cannot bury lit verticals
/// that share the same column (Slint Path paint order alone is not enough for
/// anti-aliased overlapping strokes).
fn ortho_h(nodes: &[Node], from: &str, to: &str, active: bool) -> Edge {
    let a = find(nodes, from);
    let b = find(nodes, to);
    let x0 = a.x + a.w;
    let x1 = b.x;
    ortho_h_gutter(nodes, from, to, active, x0 + (x1 - x0) * 0.5)
}

/// Orthogonal left→right edge with an explicit elbow x (used when a wire
/// skips a column, e.g. idle-inhibit → terminals).
fn ortho_h_gutter(nodes: &[Node], from: &str, to: &str, active: bool, gutter_x: f32) -> Edge {
    let a = find(nodes, from);
    let b = find(nodes, to);
    let x0 = a.x + a.w;
    let y0 = a.y + a.h * 0.5;
    let x1 = b.x;
    let y1 = b.y + b.h * 0.5;
    let gutter = gutter_x + if active { 0.0 } else { 12.0 };
    let commands = if (y0 - y1).abs() < 0.5 {
        format!("M {x0:.1} {y0:.1} L {x1:.1} {y1:.1}")
    } else {
        format!("M {x0:.1} {y0:.1} L {gutter:.1} {y0:.1} L {gutter:.1} {y1:.1} L {x1:.1} {y1:.1}")
    };
    Edge {
        id: format!("{from}->{to}"),
        commands,
        active,
    }
}

/// Long edge routed below every node. Separate departure, bottom, and
/// arrival lanes keep parallel wires distinguishable and prevent them from
/// crossing intermediate boxes.
fn ortho_bottom_lane(
    nodes: &[Node],
    from: &str,
    to: &str,
    active: bool,
    departure_x: f32,
    arrival_x: f32,
    bottom_y: f32,
) -> Edge {
    let a = find(nodes, from);
    let b = find(nodes, to);
    let x0 = a.x + a.w;
    let y0 = a.y + a.h * 0.5;
    let x1 = b.x;
    let y1 = b.y + b.h * 0.5;
    Edge {
        id: format!("{from}->{to}"),
        commands: format!(
            "M {x0:.1} {y0:.1} L {departure_x:.1} {y0:.1} L {departure_x:.1} {bottom_y:.1} L {arrival_x:.1} {bottom_y:.1} L {arrival_x:.1} {y1:.1} L {x1:.1} {y1:.1}"
        ),
        active,
    }
}

/// Cascade fallthrough: docks to the left edge of logic boxes so the spine
/// reads as one vertical chain beside the questions (not floating mid-box).
fn ortho_v_spine(nodes: &[Node], from: &str, to: &str, active: bool) -> Edge {
    let a = find(nodes, from);
    let b = find(nodes, to);
    let x = a.x.min(b.x) - 10.0;
    let y0 = a.y + a.h * 0.5;
    let y1 = b.y + b.h * 0.5;
    let x_a = a.x;
    let x_b = b.x;
    let commands =
        format!("M {x_a:.1} {y0:.1} L {x:.1} {y0:.1} L {x:.1} {y1:.1} L {x_b:.1} {y1:.1}");
    Edge {
        id: format!("{from}->{to}"),
        commands,
        active,
    }
}

fn finish(nodes: Vec<Node>, mut edges: Vec<Edge>) -> Graph {
    // Paint order follows model order in Slint; keep lit edges last so shared
    // gutter segments stay accent-colored over muted trunks.
    edges.sort_by_key(|e| e.active);
    // Spine docks 10px left of logic column; keep that inside the viewBox.
    let width = nodes.iter().map(|n| n.x + n.w).fold(COL3 + FX_W, f32::max) + PAD;
    let height = last_bottom(&nodes) + PAD;
    Graph {
        nodes,
        edges,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_active(g: &Graph, id: &str) -> bool {
        g.nodes.iter().find(|n| n.id == id).unwrap().active
    }

    fn edge_active(g: &Graph, id: &str) -> bool {
        g.edges.iter().find(|e| e.id == id).unwrap().active
    }

    fn edge_cmds<'a>(g: &'a Graph, id: &str) -> &'a str {
        g.edges
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .commands
            .as_str()
    }

    #[test]
    fn lit_edges_paint_after_muted() {
        let g = power_graph(
            "plugged",
            "2 externals",
            "ok",
            &PowerPolicy::default(),
            "balanced",
            false,
            "docked-ac",
        );
        let first_lit = g.edges.iter().position(|e| e.active).expect("lit edge");
        assert!(
            g.edges[..first_lit].iter().all(|e| !e.active),
            "muted edges must precede lit edges for Path draw order"
        );
        assert!(g.edges[first_lit..].iter().all(|e| e.active));
    }

    fn has_node(g: &Graph, id: &str) -> bool {
        g.nodes.iter().any(|n| n.id == id)
    }

    fn has_edge(g: &Graph, id: &str) -> bool {
        g.edges.iter().any(|e| e.id == id)
    }

    #[test]
    fn lid_tree_has_exactly_two_terminals() {
        let g = lid_graph("open", 0, "0 externals", "idle", false, "LID_OPEN");
        let terms: Vec<_> = g
            .nodes
            .iter()
            .filter(|n| n.caption == "terminal")
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(terms, ["t-defer", "t-sleep"]);
        assert!(!has_node(&g, "LID_OPEN"));
        assert!(!has_node(&g, "DEFERRED"));
        assert!(!has_node(&g, "SUSPENDING"));
        assert!(!has_edge(&g, "inh->DOCKED"));
        assert!(!has_edge(&g, "inh->COUNTDOWN"));
        let sleep = g.nodes.iter().find(|n| n.id == "t-sleep").unwrap();
        let grace = g.nodes.iter().find(|n| n.id == "COUNTDOWN").unwrap();
        assert!(
            grace.x + grace.w < sleep.x,
            "30s grace must precede idle/sleep"
        );
        assert_eq!(grace.caption, "intermediate");
        assert!(grace.label.contains("lid actions only"));
        assert!(!g.edges.iter().any(|e| e.id.starts_with("t-defer->")));
        assert!(!g.edges.iter().any(|e| e.id.starts_with("t-sleep->")));
    }

    #[test]
    fn lid_lights_from_daemon_win() {
        let g = lid_graph(
            "closed · logind",
            0,
            "0 externals",
            "idle · none",
            false,
            "COUNTDOWN",
        );
        assert!(node_active(&g, "q-ext"));
        assert!(node_active(&g, "t-sleep"));
        assert!(node_active(&g, "COUNTDOWN"));
        assert!(!node_active(&g, "t-defer"));
        assert!(!node_active(&g, "DOCKED"));
        assert!(edge_active(&g, "q-ext->policy"));
        assert!(edge_active(&g, "policy->COUNTDOWN"));
        assert!(edge_active(&g, "COUNTDOWN->t-sleep"));
        assert!(edge_active(&g, "q-open->q-ext"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(!edge_active(&g, "policy->t-defer"));
        assert!(!edge_active(&g, "q-ext->DOCKED"));
    }

    #[test]
    fn lid_edges_are_orthogonal() {
        let g = lid_graph("closed", 0, "0 externals", "idle", false, "COUNTDOWN");
        let h = edge_cmds(&g, "COUNTDOWN->t-sleep");
        assert!(h.contains('L'), "expected line segments: {h}");
        assert!(!h.contains('C'), "no bezier curves: {h}");
        let v = edge_cmds(&g, "q-open->q-ext");
        assert!(v.contains('L'));
        assert!(!v.contains('C'));
    }

    #[test]
    fn lid_deferred_from_daemon() {
        let g = lid_graph("closed", 0, "0 externals", "held", true, "DEFERRED");
        assert!(node_active(&g, "t-defer"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(edge_active(&g, "q-ext->policy"));
        assert!(edge_active(&g, "policy->t-defer"));
        assert!(!node_active(&g, "t-sleep"));
        assert!(!node_active(&g, "COUNTDOWN"));
    }

    #[test]
    fn lid_open_inhibit_lights_defer_awake() {
        let g = lid_graph(
            "open · daemon",
            2,
            "2 externals",
            "held · daemon",
            true,
            "LID_OPEN",
        );
        assert!(node_active(&g, "t-defer"));
        assert!(node_active(&g, "inh"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(edge_active(&g, "q-open->policy"));
        assert!(edge_active(&g, "policy->t-defer"));
        assert!(!node_active(&g, "t-sleep"));
        assert!(!node_active(&g, "DOCKED"));
        assert!(!node_active(&g, "q-ext"));
        assert!(!edge_active(&g, "q-open->q-ext"));
    }

    #[test]
    fn lid_open_no_inhibit_lights_idle_sleep() {
        let g = lid_graph("open", 0, "0 externals", "idle", false, "LID_OPEN");
        assert!(node_active(&g, "t-sleep"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(edge_active(&g, "q-open->policy"));
        assert!(edge_active(&g, "policy->t-sleep"));
        assert!(!node_active(&g, "t-defer"));
        assert!(!node_active(&g, "COUNTDOWN"));
    }

    #[test]
    fn lid_docked_is_intermediate_then_idle_terminal() {
        let g = lid_graph("closed", 2, "2 externals", "held", true, "DOCKED");
        assert!(node_active(&g, "DOCKED"));
        assert!(node_active(&g, "t-defer"));
        assert!(edge_active(&g, "q-ext->DOCKED"));
        assert!(edge_active(&g, "DOCKED->policy"));
        assert!(edge_active(&g, "policy->t-defer"));
        assert!(!has_edge(&g, "inh->DOCKED"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(!node_active(&g, "t-sleep"));
        assert!(!node_active(&g, "COUNTDOWN"));
    }

    #[test]
    fn lid_suspending_keeps_grace_then_idle_sleep() {
        let g = lid_graph("closed", 0, "0 externals", "idle", false, "SUSPENDING");
        assert!(node_active(&g, "t-sleep"));
        assert!(node_active(&g, "COUNTDOWN"));
        assert!(edge_active(&g, "q-ext->policy"));
        assert!(edge_active(&g, "policy->COUNTDOWN"));
        assert!(edge_active(&g, "COUNTDOWN->t-sleep"));
        assert!(edge_active(&g, "inh->policy"));
        let grace = g.nodes.iter().find(|n| n.id == "COUNTDOWN").unwrap();
        assert!(grace.label.contains("locking and suspending"));
    }

    #[test]
    fn edges_dock_to_node_borders() {
        let g = lid_graph("open", 0, "0 externals", "idle", false, "LID_OPEN");
        let open = g.nodes.iter().find(|n| n.id == "policy").unwrap();
        let state = g.nodes.iter().find(|n| n.id == "t-sleep").unwrap();
        let cmds = edge_cmds(&g, "policy->t-sleep");
        let start_x = open.x + open.w;
        let end_x = state.x;
        assert!(
            cmds.starts_with(&format!("M {start_x:.1}")),
            "start at source right edge: {cmds}"
        );
        assert!(
            cmds.contains(&format!("L {end_x:.1}")),
            "end at target left edge: {cmds}"
        );
    }

    #[test]
    fn lid_now_matches_lit_terminal() {
        assert!(lid_now("LID_OPEN", true).contains("defer/awake"));
        assert!(lid_now("LID_OPEN", false).contains("idle/sleep"));
        assert!(lid_now("DOCKED", false).contains("idle/sleep"));
        assert!(lid_now("DEFERRED", true).contains("defer/awake"));
    }

    #[test]
    fn power_lights_from_daemon_base() {
        let g = power_graph(
            "plugged",
            "2 externals",
            "ok",
            &PowerPolicy::default(),
            "balanced",
            false,
            "docked-ac",
        );
        assert!(node_active(&g, "docked-ac"));
        assert!(!node_active(&g, "ac-out"));
        let fx = g.nodes.iter().find(|n| n.id == "fx-docked-ac").unwrap();
        let base = g.nodes.iter().find(|n| n.id == "docked-ac").unwrap();
        assert_eq!(base.caption, "base");
        assert_eq!(fx.caption, "mapping");
        assert!(fx.label.contains("Apply power profile"));
        assert!(edge_active(&g, "q-dock->docked-ac"));
        assert!(!edge_active(&g, "q-dock->q-ac"));
    }

    #[test]
    fn power_override_plain_language() {
        let g = power_graph(
            "plugged",
            "0 externals",
            "ok",
            &PowerPolicy::default(),
            "performance",
            true,
            "ac",
        );
        let fx = g.nodes.iter().find(|n| n.id == "fx-ac").unwrap();
        assert!(fx.label.contains("override"));
    }

    #[test]
    fn display_lights_selected() {
        let matches = [
            (
                "m0".into(),
                "selected".into(),
                "two · 2 prefixes".into(),
                true,
            ),
            (
                "m1".into(),
                "also matches".into(),
                "one · 1 prefixes".into(),
                false,
            ),
        ];
        let g = display_graph("Dell · BOE", &matches);
        assert!(node_active(&g, "m0"));
        assert!(!node_active(&g, "m1"));
        assert!(node_active(&g, "fx-m0"));
        let rank = g.nodes.iter().find(|n| n.id == "select").unwrap();
        let action = g.nodes.iter().find(|n| n.id == "fx-m0").unwrap();
        assert!(rank.label.contains("Priority"));
        assert!(rank.label.contains("descending"));
        assert_eq!(action.caption, "action");
        assert!(edge_active(&g, "select->m0"));
        assert!(!edge_active(&g, "select->m1"));
    }

    #[test]
    fn display_signature_expands_for_monitor_lines() {
        let g = display_graph(
            "Dell Inc. DELL S3422DWG HSRRT563\nDell Inc. DELL S2721QS 6VSGM43",
            &[],
        );
        let sig = g.nodes.iter().find(|n| n.id == "sig").unwrap();
        let rank = g.nodes.iter().find(|n| n.id == "select").unwrap();
        assert!(sig.w > IN_W);
        assert!(sig.h > IN_H);
        assert!(rank.x > sig.x + sig.w);
    }
}
