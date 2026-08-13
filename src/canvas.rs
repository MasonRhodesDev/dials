//! Fit resolved monitors into the canvas widget.
use monitor_profiles::ResolvedOutput;

/// Schematic monitor tile: fixed height, width from aspect ratio.
pub const TILE_HEIGHT: f32 = 72.0;

#[derive(Debug, Clone, Copy)]
pub struct CanvasView {
    pub scale: f32,
    pub min_x: i32,
    pub min_y: i32,
    pub pad: f32,
}

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

/// Fixed-height tile; width follows aspect (logical w/h).
pub fn tile_size(out: &ResolvedOutput) -> (f32, f32) {
    let (lw, lh) = logical_size(out);
    let aspect = lw as f32 / lh as f32;
    let h = TILE_HEIGHT;
    let w = (h * aspect).max(28.0);
    (w, h)
}

pub fn compute_view(
    outputs: &[(ResolvedOutput, String, bool)],
    cw: f32,
    ch: f32,
) -> CanvasView {
    let pad = 24.0;
    if outputs.is_empty() {
        return CanvasView {
            scale: 1.0,
            min_x: 0,
            min_y: 0,
            pad,
        };
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
    let scale = ((cw - 2.0 * pad) / world_w)
        .min((ch - 2.0 * pad) / world_h)
        .max(0.01);
    CanvasView {
        scale,
        min_x,
        min_y,
        pad,
    }
}

/// Map logical layout into a widget of `cw`×`ch` with padding.
pub fn layout_drawn(
    outputs: &[(ResolvedOutput, String, bool)],
    cw: f32,
    ch: f32,
    locked: Option<CanvasView>,
) -> (Vec<Drawn>, CanvasView) {
    if outputs.is_empty() || cw < 32.0 || ch < 32.0 {
        let view = locked.unwrap_or(CanvasView {
            scale: 1.0,
            min_x: 0,
            min_y: 0,
            pad: 24.0,
        });
        return (Vec::new(), view);
    }
    let view = locked.unwrap_or_else(|| compute_view(outputs, cw, ch));
    let drawn = outputs
        .iter()
        .enumerate()
        .map(|(i, (o, label, ghost))| {
            let (tw, th) = tile_size(o);
            let (x, y) = o.position;
            Drawn {
                label: label.clone(),
                x: view.pad + (x - view.min_x) as f32 * view.scale,
                y: view.pad + (y - view.min_y) as f32 * view.scale,
                w: tw,
                h: th,
                ghost: *ghost,
                index: i,
            }
        })
        .collect();
    (drawn, view)
}
