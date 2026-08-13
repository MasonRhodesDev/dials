//! Fit resolved monitors into the canvas widget.
use monitor_profiles::ResolvedOutput;

#[derive(Debug, Clone)]
pub struct Drawn {
    pub label: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub ghost: bool,
    pub index: usize,
}

pub fn logical_size(out: &ResolvedOutput) -> (i32, i32) {
    let Some(mode) = out.mode else {
        return (1920, 1080);
    };
    let (pw, ph) = if out.transform % 2 == 1 {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    };
    let w = (f64::from(pw) / out.scale + 0.5).floor() as i32;
    let h = (f64::from(ph) / out.scale + 0.5).floor() as i32;
    (w.max(1), h.max(1))
}

/// Map logical layout into a widget of `cw`×`ch` with padding.
pub fn layout_drawn(
    outputs: &[(ResolvedOutput, String, bool)],
    cw: f32,
    ch: f32,
) -> Vec<Drawn> {
    if outputs.is_empty() || cw < 32.0 || ch < 32.0 {
        return Vec::new();
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let sizes: Vec<_> = outputs
        .iter()
        .map(|(o, _, _)| {
            let (w, h) = logical_size(o);
            let (x, y) = o.position;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + w);
            max_y = max_y.max(y + h);
            (w, h)
        })
        .collect();
    let world_w = (max_x - min_x).max(1) as f32;
    let world_h = (max_y - min_y).max(1) as f32;
    let pad = 24.0;
    let scale = ((cw - 2.0 * pad) / world_w)
        .min((ch - 2.0 * pad) / world_h)
        .max(0.01);
    // Keep scale so we can invert drags — stash via uniform.
    outputs
        .iter()
        .zip(sizes)
        .enumerate()
        .map(|(i, ((o, label, ghost), (w, h)))| {
            let (x, y) = o.position;
            Drawn {
                label: label.clone(),
                x: pad + (x - min_x) as f32 * scale,
                y: pad + (y - min_y) as f32 * scale,
                w: w as f32 * scale,
                h: h as f32 * scale,
                ghost: *ghost,
                index: i,
            }
        })
        .collect()
}

pub fn canvas_scale(outputs: &[(ResolvedOutput, String, bool)], cw: f32, ch: f32) -> f32 {
    if outputs.is_empty() {
        return 1.0;
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for (o, _, _) in outputs {
        let (w, h) = logical_size(o);
        let (x, y) = o.position;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    let world_w = (max_x - min_x).max(1) as f32;
    let world_h = (max_y - min_y).max(1) as f32;
    let pad = 24.0;
    ((cw - 2.0 * pad) / world_w)
        .min((ch - 2.0 * pad) / world_h)
        .max(0.01)
}
