use crate::Edge;

/// a display's bounds in virtual screen coordinates, right and bottom exclusive
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        self.left <= x && x < self.right && self.top <= y && y < self.bottom
    }
}

/// Position of a pointer that moves across the displays on its own,
/// independent of the system cursor.
pub(super) struct Pointer {
    x: f64,
    y: f64,
    /// edge the pointer is currently pushed against, reported once
    pushing: Option<Edge>,
}

impl Pointer {
    pub(super) fn new((x, y): (i32, i32)) -> Self {
        Self {
            x: x as f64,
            y: y as f64,
            pushing: None,
        }
    }

    pub(super) fn position(&self) -> (i32, i32) {
        (self.x.floor() as i32, self.y.floor() as i32)
    }

    /// Moves the pointer by the given delta, keeping it on the displays.
    ///
    /// Returns the edge it was pushed against when the move would leave the
    /// displays, but only on the first push: holding it against the edge
    /// reports nothing more until it moved away again.
    pub(super) fn move_by(&mut self, dx: f64, dy: f64, displays: &[Rect]) -> Option<Edge> {
        let (x, y) = (self.x + dx, self.y + dy);
        let target = (x.floor() as i32, y.floor() as i32);
        if displays.iter().any(|d| d.contains(target)) {
            (self.x, self.y) = (x, y);
            self.pushing = None;
            return None;
        }
        let current = self.position();
        let display = displays
            .iter()
            .find(|d| d.contains(current))
            .or(displays.first())?;
        let edge = if target.0 < display.left {
            Edge::Left
        } else if target.0 >= display.right {
            Edge::Right
        } else if target.1 < display.top {
            Edge::Top
        } else {
            Edge::Bottom
        };
        self.x = x.clamp(display.left as f64, (display.right - 1) as f64);
        self.y = y.clamp(display.top as f64, (display.bottom - 1) as f64);
        if self.pushing == Some(edge) {
            return None;
        }
        self.pushing = Some(edge);
        Some(edge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a landscape display and a taller portrait display to its right that
    /// reaches further down
    fn displays() -> Vec<Rect> {
        vec![
            Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            Rect {
                left: 1920,
                top: -155,
                right: 3000,
                bottom: 1765,
            },
        ]
    }

    #[test]
    fn moves_freely_and_onto_neighbouring_display() {
        let mut pointer = Pointer::new((1900, 500));
        assert_eq!(pointer.move_by(50.0, -600.0, &displays()), None);
        assert_eq!(pointer.position(), (1950, -100));
    }

    #[test]
    fn keeps_fractional_motion() {
        let mut pointer = Pointer::new((100, 100));
        for _ in 0..4 {
            pointer.move_by(0.5, 0.25, &displays());
        }
        assert_eq!(pointer.position(), (102, 101));
    }

    #[test]
    fn reports_free_edge_once_and_stays_on_the_display() {
        let mut pointer = Pointer::new((960, 1070));
        assert_eq!(pointer.move_by(0.0, 30.0, &displays()), Some(Edge::Bottom));
        assert_eq!(pointer.position(), (960, 1079));
        assert_eq!(pointer.move_by(0.0, 30.0, &displays()), None);
        assert_eq!(pointer.move_by(0.0, -10.0, &displays()), None);
        assert_eq!(pointer.move_by(0.0, 30.0, &displays()), Some(Edge::Bottom));
    }

    #[test]
    fn edge_next_to_empty_space_counts() {
        let mut pointer = Pointer::new((1925, 1500));
        assert_eq!(pointer.move_by(-10.0, 0.0, &displays()), Some(Edge::Left));
        assert_eq!(pointer.position(), (1920, 1500));
    }
}
