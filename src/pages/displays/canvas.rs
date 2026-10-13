//! Fit resolved monitors into the canvas widget.
//!
//! Geometry is **Hyprland logical pixels** (same as `resolve` / session):
//! - size = mode axes after transform, divided by scale
//! - position = profile `position`
//!
//! Neighboring borders stay flush: each edge is projected independently so
//! shared logical edges map to the same canvas coordinate.
//!
//! View transform: `canvas = pan + logical * scale` (user zoom/pan supported).

use monitor_profiles::ResolvedOutput;

pub const ZOOM_MIN: f32 = 0.02;
pub const ZOOM_MAX: f32 = 4.0;

#[derive(Debug, Clone, Copy)]
pub struct CanvasView {
    /// canvas_px per logical pixel
    pub scale: f32,
    /// canvas offset: `x = pan_x + logical_x * scale`
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Drawn {
    pub label: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub ghost: bool,
    pub origin: bool,
    pub index: usize,
}

/// Logical width×height in session pixels (transform + scale applied).
pub fn logical_size(out: &ResolvedOutput) -> (i32, i32) {
    let Some(mode) = out.mode else {
        return (1920, 1080);
    };
    let (pw, ph) = if out.transform % 2 == 1 {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    };
    let scale = out.scale.max(0.1);
    let w = (f64::from(pw) / scale + 0.5).floor() as i32;
    let h = (f64::from(ph) / scale + 0.5).floor() as i32;
    (w.max(1), h.max(1))
}

fn world_bounds(outputs: &[(ResolvedOutput, String, bool)]) -> (i32, i32, i32, i32) {
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
    if min_x == i32::MAX {
        (0, 0, 1, 1)
    } else {
        (min_x, min_y, max_x, max_y)
    }
}

/// Fit all monitors into `cw`×`ch`, centered, with padding.
pub fn fit_view(outputs: &[(ResolvedOutput, String, bool)], cw: f32, ch: f32) -> CanvasView {
    let pad = 16.0;
    if outputs.is_empty() {
        return CanvasView::default();
    }
    let (min_x, min_y, max_x, max_y) = world_bounds(outputs);
    let world_w = (max_x - min_x).max(1) as f32;
    let world_h = (max_y - min_y).max(1) as f32;
    let inner_w = (cw - 2.0 * pad).max(1.0);
    let inner_h = (ch - 2.0 * pad).max(1.0);
    let scale = (inner_w / world_w)
        .min(inner_h / world_h)
        .clamp(ZOOM_MIN, ZOOM_MAX);
    let drawn_w = world_w * scale;
    let drawn_h = world_h * scale;
    CanvasView {
        scale,
        pan_x: (cw - drawn_w) * 0.5 - min_x as f32 * scale,
        pan_y: (ch - drawn_h) * 0.5 - min_y as f32 * scale,
    }
}

/// Zoom by `factor` (>1 in, <1 out) keeping canvas point `(cx, cy)` fixed.
pub fn zoom_at(view: CanvasView, factor: f32, cx: f32, cy: f32) -> CanvasView {
    let factor = factor.clamp(0.25, 4.0);
    let new_scale = (view.scale * factor).clamp(ZOOM_MIN, ZOOM_MAX);
    if (new_scale - view.scale).abs() < f32::EPSILON {
        return view;
    }
    let lx = (cx - view.pan_x) / view.scale.max(0.0001);
    let ly = (cy - view.pan_y) / view.scale.max(0.0001);
    CanvasView {
        scale: new_scale,
        pan_x: cx - lx * new_scale,
        pan_y: cy - ly * new_scale,
    }
}

/// Wheel delta → zoom factor (positive delta_y = zoom in on many devices).
pub fn zoom_factor_from_wheel(delta_y: f32) -> f32 {
    // Typical wheel step ~±15–120; normalize gently.
    let steps = (delta_y / 60.0).clamp(-4.0, 4.0);
    (1.1_f32).powf(steps)
}

pub fn pan(view: CanvasView, dx: f32, dy: f32) -> CanvasView {
    CanvasView {
        scale: view.scale,
        pan_x: view.pan_x + dx,
        pan_y: view.pan_y + dy,
    }
}

#[inline]
pub fn map_x(view: &CanvasView, logical: i32) -> f32 {
    view.pan_x + logical as f32 * view.scale
}

#[inline]
pub fn map_y(view: &CanvasView, logical: i32) -> f32 {
    view.pan_y + logical as f32 * view.scale
}

/// Map logical layout with an explicit view (fit or user zoom/pan).
pub fn layout_drawn(outputs: &[(ResolvedOutput, String, bool)], view: CanvasView) -> Vec<Drawn> {
    outputs
        .iter()
        .enumerate()
        .map(|(i, (o, label, ghost))| {
            let (lw, lh) = logical_size(o);
            let (x, y) = o.position;
            let left = map_x(&view, x);
            let top = map_y(&view, y);
            let right = map_x(&view, x + lw);
            let bottom = map_y(&view, y + lh);
            Drawn {
                label: label.clone(),
                x: left,
                y: top,
                w: (right - left).max(1.0),
                h: (bottom - top).max(1.0),
                ghost: *ghost,
                origin: x == 0 && y == 0,
                index: i,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitor_profiles::{Mode, ResolvedOutput};

    fn mon(w: u32, h: u32, scale: f64, transform: u8, pos: (i32, i32)) -> ResolvedOutput {
        ResolvedOutput {
            name: String::new(),
            selector: String::new(),
            mode: Some(Mode {
                width: w,
                height: h,
                refresh: 60.0,
            }),
            position: pos,
            scale,
            transform,
            enabled: true,
        }
    }

    #[test]
    fn portrait_scale_and_center_shares_uw_right_edge() {
        let uw = mon(3440, 1440, 1.0, 0, (0, 0));
        let port = mon(3840, 2160, 1.5, 3, (3440, -560));
        assert_eq!(logical_size(&uw), (3440, 1440));
        assert_eq!(logical_size(&port), (1440, 2560));

        let outs = vec![(uw, "UW".into(), false), (port, "4K".into(), false)];
        let view = fit_view(&outs, 800.0, 600.0);
        let drawn = layout_drawn(&outs, view);
        assert_eq!(drawn.len(), 2);
        let a = &drawn[0];
        let b = &drawn[1];
        assert!(
            (a.x + a.w - b.x).abs() < 0.01,
            "gap/overlap {} vs {}",
            a.x + a.w,
            b.x
        );
        let aspect_a = a.w / a.h;
        assert!((aspect_a - 3440.0 / 1440.0).abs() < 0.02);
        let aspect_b = b.w / b.h;
        assert!((aspect_b - 1440.0 / 2560.0).abs() < 0.02);
    }

    #[test]
    fn fit_view_centers_and_fills_canvas() {
        let uw = mon(3440, 1440, 1.0, 0, (0, 0));
        let outs = vec![(uw, "UW".into(), false)];
        let cw = 800.0;
        let ch = 600.0;
        let view = fit_view(&outs, cw, ch);
        let drawn = layout_drawn(&outs, view);
        let a = &drawn[0];
        // Fills the wider axis within padding (16px each side).
        assert!(
            (a.w - (cw - 32.0)).abs() < 1.0,
            "w={} expected ~{}",
            a.w,
            cw - 32.0
        );
        let cx = a.x + a.w * 0.5;
        let cy = a.y + a.h * 0.5;
        assert!((cx - cw * 0.5).abs() < 1.0, "cx={cx}");
        assert!((cy - ch * 0.5).abs() < 1.0, "cy={cy}");
    }

    #[test]
    fn zoom_keeps_cursor_logical_point_fixed() {
        let view = CanvasView {
            scale: 0.2,
            pan_x: 10.0,
            pan_y: 20.0,
        };
        let cx = 100.0;
        let cy = 80.0;
        let lx = (cx - view.pan_x) / view.scale;
        let ly = (cy - view.pan_y) / view.scale;
        let z = zoom_at(view, 2.0, cx, cy);
        let lx2 = (cx - z.pan_x) / z.scale;
        let ly2 = (cy - z.pan_y) / z.scale;
        assert!((lx - lx2).abs() < 0.01);
        assert!((ly - ly2).abs() < 0.01);
        assert!((z.scale - 0.4).abs() < 0.001);
    }
}
