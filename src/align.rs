//! Edge/center snap while dragging and one-shot align/attach to a neighbor.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub fn cx(self) -> i32 {
        self.x + self.w / 2
    }

    pub fn cy(self) -> i32 {
        self.y + self.h / 2
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SnapOpts {
    pub edges: bool,
    pub centers: bool,
    /// Max distance in logical pixels to pull toward a guide.
    pub threshold: i32,
}

/// Logical guide line engaged by a snap (shown only while active).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guide {
    /// Vertical line at logical x
    Vertical(i32),
    /// Horizontal line at logical y
    Horizontal(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapResult {
    pub x: i32,
    pub y: i32,
    pub guides: Vec<Guide>,
}

/// Place beside neighbor: left/right also center vertically; above/below also center horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignOp {
    Left,
    Right,
    Above,
    Below,
}

impl AlignOp {
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::Left,
            1 => Self::Right,
            2 => Self::Above,
            3 => Self::Below,
            _ => return None,
        })
    }

    pub fn guides(self, neighbor: Rect) -> Vec<Guide> {
        match self {
            Self::Left => vec![
                Guide::Vertical(neighbor.x),
                Guide::Horizontal(neighbor.cy()),
            ],
            Self::Right => {
                vec![
                    Guide::Vertical(neighbor.right()),
                    Guide::Horizontal(neighbor.cy()),
                ]
            }
            Self::Above => {
                vec![
                    Guide::Horizontal(neighbor.y),
                    Guide::Vertical(neighbor.cx()),
                ]
            }
            Self::Below => {
                vec![
                    Guide::Horizontal(neighbor.bottom()),
                    Guide::Vertical(neighbor.cx()),
                ]
            }
        }
    }
}

/// Index of the nearest other rect by center distance.
pub fn nearest_neighbor(index: usize, rects: &[Rect]) -> Option<usize> {
    let me = *rects.get(index)?;
    let mut best: Option<(usize, i64)> = None;
    for (j, r) in rects.iter().enumerate() {
        if j == index {
            continue;
        }
        let dx = i64::from(me.cx() - r.cx());
        let dy = i64::from(me.cy() - r.cy());
        let d2 = dx * dx + dy * dy;
        if best.is_none_or(|(_, bd)| d2 < bd) {
            best = Some((j, d2));
        }
    }
    best.map(|(j, _)| j)
}

pub fn apply_align(op: AlignOp, moving: Rect, neighbor: Rect) -> (i32, i32) {
    match op {
        AlignOp::Left => (neighbor.x - moving.w, neighbor.cy() - moving.h / 2),
        AlignOp::Right => (neighbor.right(), neighbor.cy() - moving.h / 2),
        AlignOp::Above => (neighbor.cx() - moving.w / 2, neighbor.y - moving.h),
        AlignOp::Below => (neighbor.cx() - moving.w / 2, neighbor.bottom()),
    }
}

/// Snap proposed top-left of `moving` toward edges/centers of `others`.
pub fn snap_position(moving: Rect, others: &[Rect], opts: SnapOpts) -> SnapResult {
    if (!opts.edges && !opts.centers) || opts.threshold <= 0 || others.is_empty() {
        return SnapResult {
            x: moving.x,
            y: moving.y,
            guides: Vec::new(),
        };
    }
    let thr = opts.threshold;
    let mut x = moving.x;
    let mut y = moving.y;
    let mut best_dx = thr + 1;
    let mut best_dy = thr + 1;
    let mut guide_x: Option<i32> = None;
    let mut guide_y: Option<i32> = None;

    for o in others {
        if opts.edges {
            snap_axis(
                &[moving.x, moving.right()],
                &[o.x, o.right()],
                moving.w,
                thr,
                &mut x,
                &mut best_dx,
                &mut guide_x,
            );
            snap_axis(
                &[moving.y, moving.bottom()],
                &[o.y, o.bottom()],
                moving.h,
                thr,
                &mut y,
                &mut best_dy,
                &mut guide_y,
            );
        }
        if opts.centers {
            let dx = (o.cx() - moving.cx()).abs();
            if dx <= thr && dx < best_dx {
                best_dx = dx;
                x = o.cx() - moving.w / 2;
                guide_x = Some(o.cx());
            }
            let dy = (o.cy() - moving.cy()).abs();
            if dy <= thr && dy < best_dy {
                best_dy = dy;
                y = o.cy() - moving.h / 2;
                guide_y = Some(o.cy());
            }
        }
    }

    let mut guides = Vec::new();
    if let Some(gx) = guide_x {
        guides.push(Guide::Vertical(gx));
    }
    if let Some(gy) = guide_y {
        guides.push(Guide::Horizontal(gy));
    }

    SnapResult { x, y, guides }
}

/// Align moving edges (`edges_m`: start + end) to guide edges; write top-left into `pos`.
fn snap_axis(
    edges_m: &[i32],
    edges_g: &[i32],
    size: i32,
    thr: i32,
    pos: &mut i32,
    best: &mut i32,
    guide: &mut Option<i32>,
) {
    for (ei, &me) in edges_m.iter().enumerate() {
        for &g in edges_g {
            let d = (me - g).abs();
            if d <= thr && d < *best {
                *best = d;
                *pos = if ei == 0 { g } else { g - size };
                *guide = Some(g);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_left_edges() {
        let moving = Rect {
            x: 12,
            y: 0,
            w: 100,
            h: 50,
        };
        let other = Rect {
            x: 0,
            y: 100,
            w: 200,
            h: 50,
        };
        let r = snap_position(
            moving,
            &[other],
            SnapOpts {
                edges: true,
                centers: false,
                threshold: 20,
            },
        );
        assert_eq!((r.x, r.y), (0, 0));
        assert_eq!(r.guides, vec![Guide::Vertical(0)]);
    }

    #[test]
    fn snap_attach_right_edge() {
        let moving = Rect {
            x: 205,
            y: 10,
            w: 100,
            h: 50,
        };
        let other = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 80,
        };
        let r = snap_position(
            moving,
            &[other],
            SnapOpts {
                edges: true,
                centers: false,
                threshold: 16,
            },
        );
        assert_eq!(r.x, 200);
        assert!(r.guides.contains(&Guide::Vertical(200)));
    }

    #[test]
    fn snap_centers() {
        let moving = Rect {
            x: 48,
            y: 5,
            w: 100,
            h: 40,
        };
        let other = Rect {
            x: 0,
            y: 100,
            w: 200,
            h: 40,
        };
        let r = snap_position(
            moving,
            &[other],
            SnapOpts {
                edges: false,
                centers: true,
                threshold: 10,
            },
        );
        assert_eq!(r.x, 50);
        assert_eq!(r.guides, vec![Guide::Vertical(100)]);
    }

    #[test]
    fn place_right_and_above_center() {
        let moving = Rect {
            x: 10,
            y: 0,
            w: 100,
            h: 50,
        };
        let n = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 100,
        };
        assert_eq!(apply_align(AlignOp::Right, moving, n), (200, 25));
        assert_eq!(apply_align(AlignOp::Above, moving, n), (50, -50));
        assert_eq!(
            AlignOp::Right.guides(n),
            vec![Guide::Vertical(200), Guide::Horizontal(50)]
        );
    }

    #[test]
    fn nearest_picks_closer() {
        let rects = [
            Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            Rect {
                x: 100,
                y: 0,
                w: 10,
                h: 10,
            },
            Rect {
                x: 20,
                y: 0,
                w: 10,
                h: 10,
            },
        ];
        assert_eq!(nearest_neighbor(0, &rects), Some(2));
    }
}
