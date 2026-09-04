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
    /// Layout/telemetry kind: "input" | "logic" | "out" | "effect".
    pub kind: String,
    /// Decision-tree shape, the single source of truth for how the node is
    /// drawn: "decision" (square), "outcome" (terminal triangle), "input"
    /// (data card) or "effect" (borderless annotation beside its outcome).
    /// Derived from `kind` in exactly one place — [`shape_for`].
    pub shape: String,
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
    /// Branch condition that selects this edge (the answer, e.g. "open",
    /// "≥ 1 ext", "yes"/"no") — empty for structural feeds that carry no choice.
    pub label: String,
    /// A representative point on the edge, used to anchor the label and the
    /// pruned double-slash mark.
    pub mid_x: f32,
    pub mid_y: f32,
    pub active: bool,
    /// A not-taken out-branch of a decision that WAS evaluated. Deterministic
    /// FSMs have no chance nodes, so this is the only "road not taken" mark.
    pub pruned: bool,
    /// Path commands for the two prune slashes, in graph viewbox coords.
    /// Empty unless `pruned`.
    pub slash: String,
}

/// The one place `kind` becomes a drawn shape. Square decisions, triangle
/// outcomes, data-card inputs, borderless effect annotations.
fn shape_for(kind: &str) -> &'static str {
    match kind {
        "logic" => "decision",
        "out" => "outcome",
        "input" => "input",
        _ => "effect",
    }
}

/// Live keep-awake claim, plus the two conditions that end its authority.
///
/// An app idle-inhibitor and the user's keep-awake toggle are one thing by
/// design — the daemon cannot tell them apart, so neither does this. Under
/// the decided ladder model (POWER_SPEC "The idle/power ladder", 2026-09-04)
/// a claim governs **only the unlocked machine**: the lock ends every claim's
/// authority (decision 3), and battery-low overrides it outright (decision 6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Claim {
    /// An app inhibitor or the user toggle is held.
    pub inhibitor: bool,
    /// The compositor holds the session lock.
    pub locked: bool,
    /// On battery below the low threshold.
    pub battery_low: bool,
}

impl Claim {
    /// Whether the claim actually defers anything right now.
    pub fn defers(self) -> bool {
        self.inhibitor && !self.locked && !self.battery_low
    }

    /// Why a held claim is not deferring — empty when there is nothing to say.
    fn revoked_note(self) -> &'static str {
        if !self.inhibitor || self.defers() {
            ""
        } else if self.battery_low {
            " — keep-awake overridden by battery-low"
        } else {
            " — keep-awake ended at the lock"
        }
    }
}

pub fn lid_now(win: &str, claim: Claim) -> String {
    let note = claim.revoked_note();
    match win {
        "LID_OPEN" => {
            if claim.defers() {
                "Now: defer/awake — keep-awake held, unlocked.".into()
            } else {
                format!("Now: idle/sleep{note}.")
            }
        }
        "DOCKED" => {
            if claim.defers() {
                "Now: docked — lid ignored; defer/awake (keep-awake held).".into()
            } else {
                format!("Now: docked — lid ignored; idle/sleep{note}.")
            }
        }
        "DEFERRED" => "Now: defer/awake — keep-awake held on an unlocked machine.".into(),
        "COUNTDOWN" => "Now: idle/sleep — 30s grace, then lock and suspend.".into(),
        "SUSPENDING" => "Now: idle/sleep — locking and suspending.".into(),
        "" => "Now: waiting for hyprstate daemon…".into(),
        other => format!("Now: {other}"),
    }
}

/// Plain-language "Now" line for the idle ladder.
pub fn idle_now(win: &str, claim: Claim) -> String {
    match lit_ladder_node(win, claim) {
        "" => "Now: waiting for hyprstate daemon…".into(),
        "AWAKE" => "Now: awake — no keep-awake needed (WARN not yet reported separately).".into(),
        "HELD_AWAKE" => "Now: held awake — a keep-awake claim is deferring the 180s lock.".into(),
        "LOCK" => {
            "Now: locked — claims no longer count; blank follows at 30s (BLANK not yet reported \
             separately)."
                .into()
        }
        "GRACE" => "Now: 30s grace — locker proven live, then suspend.".into(),
        "SUSPEND" => "Now: suspending.".into(),
        other => format!("Now: {other}"),
    }
}

/// Which ladder node the live telemetry can actually prove.
///
/// The daemon reports the world FSM state plus the claim bits, not the ladder
/// node itself, so AWAKE/WARN and LOCK/BLANK collapse into one lit box each.
// TODO(Q7): light from "current ladder node + claim holders" telemetry
// (POWER_SPEC decision 7) instead of deriving from the world state.
fn lit_ladder_node(win: &str, claim: Claim) -> &'static str {
    match win {
        "" => "",
        "SUSPENDING" => "SUSPEND",
        "COUNTDOWN" => "GRACE",
        _ if claim.locked => "LOCK",
        "DEFERRED" => "HELD_AWAKE",
        _ if claim.defers() => "HELD_AWAKE",
        _ => "AWAKE",
    }
}

/// The decided idle ladder (POWER_SPEC "The idle/power ladder", 2026-09-04).
///
/// Keep-awake has exactly one power: it prevents entry into the warn→lock
/// ladder while the machine is still unlocked. Once locked, no claim is
/// consulted again — blank and suspend march unconditionally.
///
/// Lit path is derived from what the daemon reports today (world state +
/// inhibitor/locked/battery-low); see [`lit_ladder_node`] for the gap.
pub fn idle_graph(
    idle_body: &str,
    claim_body: &str,
    lock_body: &str,
    bat_body: &str,
    claim: Claim,
    win: &str,
) -> Graph {
    let lit = lit_ladder_node(win, claim);
    let step = |q_id, q_cap, q_label, state_id: &'static str, fx_id, fx: &str| Step {
        q_id,
        q_cap,
        q_label,
        state_id,
        state_label: state_id,
        fx_id,
        fx_label: fx.to_string(),
        taken: lit == state_id,
    };
    cascade(
        &[
            ("idle", "input idle", idle_body, !win.is_empty()),
            ("claim", "keep-awake", claim_body, claim.inhibitor),
            ("lock", "session", lock_body, claim.locked),
            ("bat", "battery", bat_body, claim.battery_low),
        ],
        &[
            ("idle", "q-awake"),
            ("claim", "q-held"),
            ("bat", "q-held"),
            ("lock", "q-blank"),
        ],
        &[
            step(
                "q-awake",
                "if",
                "local input < 180s",
                "AWAKE",
                "fx-awake",
                "Stay lit.\nAny local input cancels a warn or grace.",
            ),
            step(
                "q-held",
                "else if",
                "keep-awake held\nand unlocked",
                "HELD_AWAKE",
                "fx-held-awake",
                "Stay lit, unlocked.\nRelease acts on true idle:\nbrief warn, then the lock.",
            ),
            step(
                "q-warn",
                "else if",
                "no claim,\nidle 180s",
                "WARN",
                "fx-warn",
                "Blur ramp.\nAny input cancels it.",
            ),
            step(
                "q-lock",
                "else if",
                "warn elapsed",
                "LOCK",
                "fx-lock",
                "Lock the session.\nEvery claim's authority ends here.",
            ),
            step(
                "q-blank",
                "else if",
                "locked 30s",
                "BLANK",
                "fx-blank",
                "Blank the screen — always,\nkeep-awake toggle included.",
            ),
            step(
                "q-grace",
                "else if",
                "idle 900s total",
                "GRACE",
                "fx-grace",
                "30s grace, locker proven live.\nBattery-low self-requests it\npast the claim.",
            ),
            step(
                "q-suspend",
                "else",
                "grace elapsed",
                "SUSPEND",
                "fx-suspend",
                "Suspend. A standing request outranks docked,\nso an idle docked laptop suspends too.",
            ),
        ],
    )
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
/// `claim` is the live keep-awake claim plus the bits that revoke it.
///
/// Lid and dock are path, not terminals. The only terminals are idle/sleep
/// and defer/awake. 30s grace precedes idle/sleep when the lid is closed
/// and undocked. A claim defers only while the machine is unlocked and off
/// the battery-low floor (POWER_SPEC decisions 3 and 6).
pub fn lid_graph(
    lid_body: &str,
    ext_mon_count: u32,
    ext_body: &str,
    inh_body: &str,
    claim: Claim,
    win: &str,
) -> Graph {
    let defers = claim.defers();
    let open = win == "LID_OPEN";
    let docked = win == "DOCKED";
    let deferred = win == "DEFERRED";
    let countdown = win == "COUNTDOWN";
    let suspending = win == "SUSPENDING";
    let closed = docked || deferred || countdown || suspending;
    let known = open || closed;
    let grace = countdown || suspending;
    let defer = (open && defers) || (docked && defers) || deferred;
    let sleep = (open && !defers) || (docked && !defers) || grace;

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
            ("inh", "keep-awake", inh_body, claim.inhibitor),
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
        "ignore lid\nidle suspend still applies",
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
        "idle policy\nheld & unlocked → defer\nlocked · battery-low · released → idle",
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

    // Branch conditions on every decision arm; effects stay on the outcomes.
    label(&mut edges, "q-open->policy", "open");
    label(&mut edges, "q-open->q-ext", "closed");
    label(&mut edges, "q-ext->DOCKED", "≥ 1 ext");
    label(&mut edges, "q-ext->policy", "0 ext");
    label(&mut edges, "policy->t-defer", "held & unlocked");
    label(&mut edges, "policy->t-sleep", "locked · released");
    label(&mut edges, "policy->COUNTDOWN", "closed · undocked");
    label(&mut edges, "COUNTDOWN->t-sleep", "30s");
    // Prune the not-taken arms of the decisions the live path reached. q-ext
    // is only reached when the lid is closed; leave it neutral when open.
    prune_arms(&mut edges, known, &["q-open->policy", "q-open->q-ext"]);
    prune_arms(&mut edges, closed, &["q-ext->DOCKED", "q-ext->policy"]);
    prune_arms(
        &mut edges,
        known,
        &["policy->t-defer", "policy->t-sleep", "policy->COUNTDOWN"],
    );

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
        label(&mut edges, "select->none", "no fit");
    } else {
        let mut arms = Vec::new();
        for (id, _, _, selected) in matches {
            edges.push(ortho_h(&nodes, "select", id, *selected));
            edges.push(ortho_h(&nodes, id, &format!("fx-{id}"), *selected));
            let arm = format!("select->{id}");
            label(
                &mut edges,
                &arm,
                if *selected { "top rank" } else { "outranked" },
            );
            arms.push(arm);
        }
        // The ranking is always evaluated; every arm but the winner is a road
        // not taken.
        let arm_refs: Vec<&str> = arms.iter().map(String::as_str).collect();
        prune_arms(&mut edges, true, &arm_refs);
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
    // Every decision's two arms carry the answer that selects them: the match
    // arm ("yes") to its outcome, the fall-through spine ("no") to the next
    // question. Prune the not-taken arm of each decision the path reached
    // (index ≤ winner); decisions past the winner were never evaluated.
    for (i, step) in steps.iter().enumerate() {
        let match_arm = format!("{}->{}", step.q_id, step.state_id);
        label(&mut edges, &match_arm, "yes");
        let evaluated = win.is_some_and(|w| i <= w);
        if let Some(next) = steps.get(i + 1) {
            let spine = format!("{}->{}", step.q_id, next.q_id);
            label(&mut edges, &spine, "no");
            prune_arms(&mut edges, evaluated, &[&match_arm, &spine]);
        } else {
            prune_arms(&mut edges, evaluated, &[&match_arm]);
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
        shape: shape_for(kind).into(),
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
    // Anchor the label/prune mark at the elbow (the actual gutter x, jitter
    // included so a slash lands on the drawn line) at the mean docking height.
    // Arms fanning from one decision to different rows separate vertically
    // instead of stacking on a single point.
    edge(from, to, commands, gutter, (y0 + y1) * 0.5, active)
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
    edge(
        from,
        to,
        format!(
            "M {x0:.1} {y0:.1} L {departure_x:.1} {y0:.1} L {departure_x:.1} {bottom_y:.1} L {arrival_x:.1} {bottom_y:.1} L {arrival_x:.1} {y1:.1} L {x1:.1} {y1:.1}"
        ),
        departure_x,
        (y0 + bottom_y) * 0.5,
        active,
    )
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
    // The "else" / fall-through arm: anchor on the vertical spine, in the
    // clear gutter just left of the logic column.
    edge(from, to, commands, x - 16.0, (y0 + y1) * 0.5, active)
}

/// Build an [`Edge`] with a label/prune anchor and no branch label yet.
fn edge(from: &str, to: &str, commands: String, mid_x: f32, mid_y: f32, active: bool) -> Edge {
    Edge {
        id: format!("{from}->{to}"),
        commands,
        label: String::new(),
        mid_x,
        mid_y,
        active,
        pruned: false,
        slash: String::new(),
    }
}

/// Two forward slashes across the edge at its anchor — Lucid's "branch not
/// taken" mark, in graph viewbox coords.
fn slash_marks(x: f32, y: f32) -> String {
    format!(
        "M {:.1} {:.1} L {:.1} {:.1} M {:.1} {:.1} L {:.1} {:.1}",
        x - 4.0,
        y + 6.0,
        x + 1.0,
        y - 6.0,
        x + 3.0,
        y + 6.0,
        x + 8.0,
        y - 6.0,
    )
}

/// Put the branch condition on an edge (the answer that selects it).
fn label(edges: &mut [Edge], id: &str, text: &str) {
    if let Some(e) = edges.iter_mut().find(|e| e.id == id) {
        e.label = text.into();
    }
}

/// Mark the not-taken arms of a decision that was evaluated. A decision that
/// was never reached is left neutral (`evaluated == false`): only the roads
/// off a road actually travelled get the prune slashes.
fn prune_arms(edges: &mut [Edge], evaluated: bool, ids: &[&str]) {
    if !evaluated {
        return;
    }
    for id in ids {
        if let Some(e) = edges.iter_mut().find(|e| e.id == *id && !e.active) {
            e.pruned = true;
            e.slash = slash_marks(e.mid_x, e.mid_y);
        }
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

    /// A keep-awake claim on an unlocked machine off the battery-low floor:
    /// the only shape that actually defers anything.
    fn held() -> Claim {
        Claim {
            inhibitor: true,
            ..Claim::default()
        }
    }

    fn node_active(g: &Graph, id: &str) -> bool {
        g.nodes.iter().find(|n| n.id == id).unwrap().active
    }

    fn node_shape<'a>(g: &'a Graph, id: &str) -> &'a str {
        g.nodes.iter().find(|n| n.id == id).unwrap().shape.as_str()
    }

    fn edge_label<'a>(g: &'a Graph, id: &str) -> &'a str {
        g.edges.iter().find(|e| e.id == id).unwrap().label.as_str()
    }

    fn edge_pruned(g: &Graph, id: &str) -> bool {
        g.edges.iter().find(|e| e.id == id).unwrap().pruned
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
        let g = lid_graph(
            "open",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "LID_OPEN",
        );
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
            Claim::default(),
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
        let g = lid_graph(
            "closed",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "COUNTDOWN",
        );
        let h = edge_cmds(&g, "COUNTDOWN->t-sleep");
        assert!(h.contains('L'), "expected line segments: {h}");
        assert!(!h.contains('C'), "no bezier curves: {h}");
        let v = edge_cmds(&g, "q-open->q-ext");
        assert!(v.contains('L'));
        assert!(!v.contains('C'));
    }

    #[test]
    fn lid_deferred_from_daemon() {
        let g = lid_graph("closed", 0, "0 externals", "held", held(), "DEFERRED");
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
            held(),
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
        let g = lid_graph(
            "open",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "LID_OPEN",
        );
        assert!(node_active(&g, "t-sleep"));
        assert!(edge_active(&g, "inh->policy"));
        assert!(edge_active(&g, "q-open->policy"));
        assert!(edge_active(&g, "policy->t-sleep"));
        assert!(!node_active(&g, "t-defer"));
        assert!(!node_active(&g, "COUNTDOWN"));
    }

    #[test]
    fn lid_docked_is_intermediate_then_idle_terminal() {
        let g = lid_graph("closed", 2, "2 externals", "held", held(), "DOCKED");
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
        let g = lid_graph(
            "closed",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "SUSPENDING",
        );
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
        let g = lid_graph(
            "open",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "LID_OPEN",
        );
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
        assert!(lid_now("LID_OPEN", held()).contains("defer/awake"));
        assert!(lid_now("LID_OPEN", Claim::default()).contains("idle/sleep"));
        assert!(lid_now("DOCKED", Claim::default()).contains("idle/sleep"));
        assert!(lid_now("DEFERRED", held()).contains("defer/awake"));
    }

    #[test]
    fn a_claim_defers_only_on_an_unlocked_healthy_machine() {
        assert!(held().defers());
        assert!(
            !Claim {
                locked: true,
                ..held()
            }
            .defers(),
            "the lock ends every claim's authority (decision 3)"
        );
        assert!(
            !Claim {
                battery_low: true,
                ..held()
            }
            .defers(),
            "battery-low overrides keep-awake (decision 6)"
        );
        assert!(!Claim::default().defers());
    }

    #[test]
    fn lid_resolve_text_states_the_decided_model() {
        let g = lid_graph("open", 0, "0 externals", "held", held(), "LID_OPEN");
        let policy = g.nodes.iter().find(|n| n.id == "policy").unwrap();
        assert!(
            policy.label.contains("held & unlocked → defer"),
            "a claim defers only while unlocked: {}",
            policy.label
        );
        assert!(
            policy
                .label
                .contains("locked · battery-low · released → idle"),
            "locked and battery-low bypass the claim: {}",
            policy.label
        );
        let docked = g.nodes.iter().find(|n| n.id == "DOCKED").unwrap();
        assert!(
            docked.label.contains("idle suspend still applies"),
            "docking neutralizes the lid trigger only; a request outranks it: {}",
            docked.label
        );
        let inh = g.nodes.iter().find(|n| n.id == "inh").unwrap();
        assert_eq!(inh.caption, "keep-awake");
    }

    #[test]
    fn a_locked_claim_defers_nothing_on_the_lid_graph() {
        let claim = Claim {
            locked: true,
            ..held()
        };
        let g = lid_graph("open", 0, "0 externals", "held", claim, "LID_OPEN");
        assert!(node_active(&g, "t-sleep"));
        assert!(!node_active(&g, "t-defer"));
        assert!(lid_now("LID_OPEN", claim).contains("ended at the lock"));
    }

    #[test]
    fn battery_low_beats_a_claim_on_the_lid_graph() {
        let claim = Claim {
            battery_low: true,
            ..held()
        };
        let g = lid_graph("closed", 2, "2 externals", "held", claim, "DOCKED");
        assert!(node_active(&g, "t-sleep"));
        assert!(!node_active(&g, "t-defer"));
        assert!(lid_now("DOCKED", claim).contains("overridden by battery-low"));
    }

    #[test]
    fn idle_ladder_has_the_decided_rungs_in_order() {
        let g = idle_graph("—", "idle", "unlocked", "ok", Claim::default(), "");
        let rungs: Vec<_> = g
            .nodes
            .iter()
            .filter(|n| n.caption == "base")
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            rungs,
            [
                "AWAKE",
                "HELD_AWAKE",
                "WARN",
                "LOCK",
                "BLANK",
                "GRACE",
                "SUSPEND"
            ]
        );
        // Cascade shape: inputs → questions → rung → effect, left to right.
        let q = g.nodes.iter().find(|n| n.id == "q-held").unwrap();
        let rung = g.nodes.iter().find(|n| n.id == "HELD_AWAKE").unwrap();
        let fx = g.nodes.iter().find(|n| n.id == "fx-held-awake").unwrap();
        assert!(q.x + q.w < rung.x);
        assert!(rung.x + rung.w < fx.x);
        assert!(has_edge(&g, "claim->q-held"));
        assert!(has_edge(&g, "bat->q-held"));
        assert!(has_edge(&g, "lock->q-blank"));
    }

    #[test]
    fn idle_ladder_rule_text_pins_the_decided_model() {
        let g = idle_graph("—", "idle", "unlocked", "ok", Claim::default(), "");
        let text = |id: &str| {
            g.nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .label
                .to_string()
        };
        assert!(text("q-held").contains("unlocked"), "{}", text("q-held"));
        assert!(
            text("fx-lock").contains("authority ends here"),
            "the lock ends every claim: {}",
            text("fx-lock")
        );
        assert!(
            text("fx-blank").contains("always"),
            "a locked screen always blanks: {}",
            text("fx-blank")
        );
        assert!(
            text("fx-grace").contains("Battery-low"),
            "battery-low self-requests past the claim: {}",
            text("fx-grace")
        );
        assert!(
            text("fx-suspend").contains("outranks docked"),
            "a standing request outranks Docked: {}",
            text("fx-suspend")
        );
    }

    #[test]
    fn idle_ladder_lights_from_daemon_state() {
        let held_g = idle_graph("—", "held", "unlocked", "ok", held(), "DEFERRED");
        assert!(node_active(&held_g, "HELD_AWAKE"));
        assert!(!node_active(&held_g, "AWAKE"));

        let locked = Claim {
            locked: true,
            ..held()
        };
        let locked_g = idle_graph("—", "held", "locked", "ok", locked, "LID_OPEN");
        assert!(
            node_active(&locked_g, "LOCK"),
            "a claim on a locked machine holds nothing"
        );
        assert!(!node_active(&locked_g, "HELD_AWAKE"));

        let grace = idle_graph("—", "idle", "locked", "ok", Claim::default(), "COUNTDOWN");
        assert!(node_active(&grace, "GRACE"));
        let susp = idle_graph("—", "idle", "locked", "ok", Claim::default(), "SUSPENDING");
        assert!(node_active(&susp, "SUSPEND"));

        // No daemon frame: nothing is lit and nothing is claimed.
        let dark = idle_graph("—", "—", "—", "—", Claim::default(), "");
        assert!(
            dark.nodes
                .iter()
                .filter(|n| n.caption == "base")
                .all(|n| !n.active)
        );
        assert!(idle_now("", Claim::default()).contains("waiting"));
    }

    #[test]
    fn idle_ladder_admits_what_it_cannot_yet_see() {
        // Q7 gap: the daemon reports the world state, not the ladder node, so
        // AWAKE/WARN and LOCK/BLANK are one lit box each until it does.
        assert!(idle_now("LID_OPEN", Claim::default()).contains("WARN not yet reported"));
        assert!(
            idle_now(
                "LID_OPEN",
                Claim {
                    locked: true,
                    ..Claim::default()
                }
            )
            .contains("BLANK not yet reported")
        );
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
    fn shapes_follow_decision_tree_convention() {
        // Lucidchart mapping: square = decision, terminal triangle = outcome,
        // data card = input/given, borderless = effect annotation.
        let g = lid_graph("open", 2, "2 externals", "held", held(), "DOCKED");
        for id in ["q-open", "q-ext", "policy"] {
            assert_eq!(node_shape(&g, id), "decision", "{id} is a decision");
        }
        for id in ["DOCKED", "COUNTDOWN", "t-defer", "t-sleep"] {
            assert_eq!(node_shape(&g, id), "outcome", "{id} is an outcome");
        }
        for id in ["lid", "ext", "inh"] {
            assert_eq!(
                node_shape(&g, id),
                "input",
                "{id} is a given, not a decision"
            );
        }

        let p = power_graph(
            "plugged",
            "2 externals",
            "ok",
            &PowerPolicy::default(),
            "balanced",
            false,
            "docked-ac",
        );
        assert_eq!(node_shape(&p, "q-dock"), "decision");
        assert_eq!(node_shape(&p, "docked-ac"), "outcome");
        assert_eq!(node_shape(&p, "fx-docked-ac"), "effect");
        assert_eq!(node_shape(&p, "ac"), "input");

        let idle = idle_graph("—", "held", "unlocked", "ok", held(), "DEFERRED");
        assert_eq!(node_shape(&idle, "q-held"), "decision");
        assert_eq!(node_shape(&idle, "HELD_AWAKE"), "outcome");
        assert_eq!(node_shape(&idle, "fx-held-awake"), "effect");
        assert_eq!(node_shape(&idle, "idle"), "input");

        let d = display_graph("Dell", &[("m0".into(), "sel".into(), "x".into(), true)]);
        assert_eq!(node_shape(&d, "sig"), "input");
        assert_eq!(node_shape(&d, "select"), "decision");
        assert_eq!(node_shape(&d, "m0"), "outcome");
        assert_eq!(node_shape(&d, "fx-m0"), "effect");
    }

    #[test]
    fn branch_labels_carry_the_condition_not_the_effect() {
        let g = lid_graph("open", 2, "2 externals", "held", held(), "DOCKED");
        // Each decision's arms carry the answer that selects them.
        assert_eq!(edge_label(&g, "q-open->policy"), "open");
        assert_eq!(edge_label(&g, "q-open->q-ext"), "closed");
        assert_eq!(edge_label(&g, "q-ext->DOCKED"), "≥ 1 ext");
        assert_eq!(edge_label(&g, "q-ext->policy"), "0 ext");
        assert_eq!(edge_label(&g, "policy->t-defer"), "held & unlocked");
        assert_eq!(edge_label(&g, "policy->t-sleep"), "locked · released");
        assert_eq!(edge_label(&g, "policy->COUNTDOWN"), "closed · undocked");

        // Cascade decisions answer yes / no.
        let p = power_graph(
            "plugged",
            "0 externals",
            "ok",
            &PowerPolicy::default(),
            "balanced",
            false,
            "ac",
        );
        assert_eq!(edge_label(&p, "q-dock->docked-ac"), "yes");
        assert_eq!(edge_label(&p, "q-dock->q-ac"), "no");
        // The effect text stays on its node; it never rides on an edge.
        let fx = p.nodes.iter().find(|n| n.id == "fx-ac").unwrap();
        assert!(fx.label.contains("Apply"));
        assert!(
            p.edges.iter().all(|e| !e.label.contains("Apply")),
            "effects must not become edge labels"
        );
    }

    #[test]
    fn evaluated_decisions_prune_their_not_taken_arms() {
        // Winner is the last rung: every earlier decision was reached, and its
        // match arm is a road not taken — pruned, with the slash marks.
        let g = power_graph(
            "battery",
            "0 externals",
            "low",
            &PowerPolicy::default(),
            "powersave",
            false,
            "battery",
        );
        for arm in ["q-dock->docked-ac", "q-ac->ac-out", "q-low->battery-low"] {
            assert!(edge_pruned(&g, arm), "{arm} is a not-taken arm");
            assert!(!edge_active(&g, arm));
            let e = g.edges.iter().find(|e| e.id == arm).unwrap();
            assert!(!e.slash.is_empty(), "{arm} carries the prune slashes");
        }
        assert!(
            !edge_pruned(&g, "q-bat->battery"),
            "the taken arm is not pruned"
        );

        // Winner is the first rung: the else-spine it did not fall through is
        // pruned; decisions below were never reached and stay neutral.
        let d = power_graph(
            "plugged",
            "2 externals",
            "ok",
            &PowerPolicy::default(),
            "balanced",
            false,
            "docked-ac",
        );
        assert!(
            edge_pruned(&d, "q-dock->q-ac"),
            "the winner did not fall through"
        );
        assert!(
            !edge_pruned(&d, "q-ac->ac-out"),
            "a decision never reached is left neutral"
        );
        assert!(!edge_pruned(&d, "q-low->battery-low"));

        // Lid: q-ext is only reached when the lid is closed.
        let open = lid_graph(
            "open",
            0,
            "0 externals",
            "idle",
            Claim::default(),
            "LID_OPEN",
        );
        assert!(
            !edge_pruned(&open, "q-ext->DOCKED"),
            "q-ext is never reached with the lid open"
        );
        assert!(
            edge_pruned(&open, "q-open->q-ext"),
            "the closed spine is the road not taken"
        );
        assert!(edge_pruned(&open, "policy->t-defer"));
        assert!(edge_pruned(&open, "policy->COUNTDOWN"));
        assert!(
            !edge_pruned(&open, "policy->t-sleep"),
            "the taken policy arm is not pruned"
        );

        // No daemon frame: nothing evaluated, nothing pruned.
        let dark = idle_graph("—", "—", "—", "—", Claim::default(), "");
        assert!(dark.edges.iter().all(|e| !e.pruned));
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
