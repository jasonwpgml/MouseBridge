//! Where devices sit next to each other, and the stretches of screen edge
//! the pointer may cross between them.
//!
//! Every device's displays are placed on a shared canvas: a device's own
//! display coordinates, in device-independent units, scaled by its size and
//! moved to its place. Two devices are connected wherever a display of one
//! touches a display of the other.

use lan_mouse_ipc::{DisplayInfo, Placement, Position};

/// Displays further apart than this on the canvas do not touch.
const TOUCH_TOLERANCE: i32 = 2;

/// a device on the canvas
pub(crate) struct Device<'a> {
    pub name: &'a str,
    pub displays: &'a [DisplayInfo],
    pub placement: Placement,
}

/// A stretch of a local display edge that leads to a display of a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Portal {
    pub client: String,
    /// index of the local display whose edge this is
    pub display: usize,
    /// edge of the local display the pointer leaves through
    pub side: Position,
    /// coordinate of that edge, x for left and right, y for top and bottom
    pub at: i32,
    /// stretch of the edge, end exclusive: y for left and right, x for top
    /// and bottom
    pub from: i32,
    pub to: i32,
    local: Placement,
    remote: Placement,
}

impl Portal {
    /// the point the pointer enters the client at, just inside its display
    pub(crate) fn entry_point(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let horizontal = matches!(self.side, Position::Left | Position::Right);
        let along = if horizontal { y } else { x }.clamp(self.from, self.to - 1);
        // the edge is shared, so it maps onto the client's edge
        let (at, along) = if horizontal {
            (
                remote_x(self.remote, local_x(self.local, self.at)),
                remote_y(self.remote, local_y(self.local, along)),
            )
        } else {
            (
                remote_y(self.remote, local_y(self.local, self.at)),
                remote_x(self.remote, local_x(self.local, along)),
            )
        };
        // the client's edge is exclusive on its far side
        let at = match self.side {
            Position::Left | Position::Top => at - 1,
            Position::Right | Position::Bottom => at,
        };
        if horizontal { (at, along) } else { (along, at) }
    }
}

/// All portals from the local device to the others.
pub(crate) fn portals(local: &Device, others: &[Device]) -> Vec<Portal> {
    let mut portals = Vec::new();
    for (index, l) in local.displays.iter().enumerate() {
        let l_rect = canvas_rect(l, local.placement);
        for other in others {
            for o in other.displays {
                let o_rect = canvas_rect(o, other.placement);
                for (side, at, from, to) in touching(l_rect, o_rect) {
                    // back from the canvas to local coordinates
                    let p = local.placement;
                    let (at, from, to) = match side {
                        Position::Left | Position::Right => (
                            from_canvas(at, p.x, p),
                            from_canvas(from, p.y, p),
                            from_canvas(to, p.y, p),
                        ),
                        Position::Top | Position::Bottom => (
                            from_canvas(at, p.y, p),
                            from_canvas(from, p.x, p),
                            from_canvas(to, p.x, p),
                        ),
                    };
                    if from < to {
                        portals.push(Portal {
                            client: other.name.to_owned(),
                            display: index,
                            side,
                            at,
                            from,
                            to,
                            local: local.placement,
                            remote: other.placement,
                        });
                    }
                }
            }
        }
    }
    portals
}

/// The side most of the way to `client` leads through, if it touches at all.
pub(crate) fn main_side(portals: &[Portal], client: &str) -> Option<Position> {
    portals
        .iter()
        .filter(|p| p.client == client)
        .max_by_key(|p| p.to - p.from)
        .map(|p| p.side)
}

/// left, top, right, bottom on the canvas, right and bottom exclusive
type Rect = (i32, i32, i32, i32);

fn canvas_rect(d: &DisplayInfo, p: Placement) -> Rect {
    (
        to_canvas(d.x, p.x, p),
        to_canvas(d.y, p.y, p),
        to_canvas(d.x + d.width, p.x, p),
        to_canvas(d.y + d.height, p.y, p),
    )
}

fn to_canvas(v: i32, origin: i32, p: Placement) -> i32 {
    origin + (v as f64 * p.size.max(1) as f64 / 100.0).round() as i32
}

fn from_canvas(v: i32, origin: i32, p: Placement) -> i32 {
    ((v - origin) as f64 * 100.0 / p.size.max(1) as f64).round() as i32
}

fn local_x(p: Placement, v: i32) -> i32 {
    to_canvas(v, p.x, p)
}

fn local_y(p: Placement, v: i32) -> i32 {
    to_canvas(v, p.y, p)
}

fn remote_x(p: Placement, v: i32) -> i32 {
    from_canvas(v, p.x, p)
}

fn remote_y(p: Placement, v: i32) -> i32 {
    from_canvas(v, p.y, p)
}

/// Edges where `a` touches `b`, seen from `a`: side, edge coordinate and the
/// shared stretch.
fn touching(a: Rect, b: Rect) -> Vec<(Position, i32, i32, i32)> {
    let (al, at, ar, ab) = a;
    let (bl, bt, br, bb) = b;
    let near = |p: i32, q: i32| (p - q).abs() <= TOUCH_TOLERANCE;
    let overlap = |a0: i32, a1: i32, b0: i32, b1: i32| {
        let (from, to) = (a0.max(b0), a1.min(b1));
        (from < to).then_some((from, to))
    };
    let mut edges = Vec::new();
    if near(ar, bl) {
        if let Some((from, to)) = overlap(at, ab, bt, bb) {
            edges.push((Position::Right, ar, from, to));
        }
    }
    if near(al, br) {
        if let Some((from, to)) = overlap(at, ab, bt, bb) {
            edges.push((Position::Left, al, from, to));
        }
    }
    if near(ab, bt) {
        if let Some((from, to)) = overlap(al, ar, bl, br) {
            edges.push((Position::Bottom, ab, from, to));
        }
    }
    if near(at, bb) {
        if let Some((from, to)) = overlap(al, ar, bl, br) {
            edges.push((Position::Top, at, from, to));
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: i32, y: i32, width: i32, height: i32) -> DisplayInfo {
        DisplayInfo {
            x,
            y,
            width,
            height,
            scale: 100,
        }
    }

    fn at(x: i32, y: i32) -> Placement {
        Placement { x, y, size: 100 }
    }

    /// The desk: a MacBook below the desktop's landscape display, which has
    /// a taller portrait display to its right, and a laptop up and left of
    /// the MacBook.
    fn desk() -> (Vec<DisplayInfo>, Vec<DisplayInfo>, Vec<DisplayInfo>) {
        let mac = vec![display(0, 0, 1470, 956)];
        let desktop = vec![display(0, 0, 1920, 1080), display(1920, -155, 1080, 1920)];
        let laptop = vec![display(0, 0, 1536, 864)];
        (mac, desktop, laptop)
    }

    #[test]
    fn mac_reaches_the_desktop_only_where_they_touch() {
        let (mac, desktop, laptop) = desk();
        let local = Device {
            name: "mac",
            displays: &mac,
            placement: at(200, 1080),
        };
        let others = [
            Device {
                name: "desktop",
                displays: &desktop,
                placement: at(0, 0),
            },
            Device {
                name: "laptop",
                displays: &laptop,
                placement: at(-1536, 300),
            },
        ];
        let portals = portals(&local, &others);
        // the Mac's top edge meets the landscape display from x 200 to 1670
        // on the canvas, all of the Mac's width
        let desktop = portals.iter().find(|p| p.client == "desktop").unwrap();
        assert_eq!(
            (desktop.side, desktop.at, desktop.from, desktop.to),
            (Position::Top, 0, 0, 1470)
        );
        // the laptop is not touching the Mac in this arrangement
        assert!(portals.iter().all(|p| p.client != "laptop"));
        assert_eq!(main_side(&portals, "desktop"), Some(Position::Top));
        assert_eq!(main_side(&portals, "laptop"), None);
    }

    #[test]
    fn edge_is_split_between_two_devices() {
        let (mac, desktop, laptop) = desk();
        // desktop above the right half of the Mac, laptop above the left half
        let local = Device {
            name: "mac",
            displays: &mac,
            placement: at(0, 1080),
        };
        let others = [
            Device {
                name: "desktop",
                displays: &desktop,
                placement: at(735, 0),
            },
            Device {
                name: "laptop",
                displays: &laptop,
                placement: at(-801, 216),
            },
        ];
        let portals = portals(&local, &others);
        let laptop = portals.iter().find(|p| p.client == "laptop").unwrap();
        let desktop = portals.iter().find(|p| p.client == "desktop").unwrap();
        assert_eq!(
            (laptop.side, laptop.from, laptop.to),
            (Position::Top, 0, 735)
        );
        assert_eq!(
            (desktop.side, desktop.from, desktop.to),
            (Position::Top, 735, 1470)
        );
    }

    #[test]
    fn entry_point_lands_just_inside_the_client() {
        let (mac, desktop, _) = desk();
        let local = Device {
            name: "mac",
            displays: &mac,
            placement: at(200, 1080),
        };
        let others = [Device {
            name: "desktop",
            displays: &desktop,
            placement: at(0, 0),
        }];
        let portal = &portals(&local, &others)[0];
        // leaving the Mac's top edge at x 100 enters the desktop's bottom
        // row at x 300
        assert_eq!(portal.entry_point((100, 0)), (300, 1079));
    }

    #[test]
    fn a_smaller_device_is_entered_in_proportion() {
        let (mac, _, laptop) = desk();
        // the laptop drawn at half its size, its right edge against the
        // Mac's left edge, its top 100 above the Mac's
        let local = Device {
            name: "mac",
            displays: &mac,
            placement: at(0, 0),
        };
        let others = [Device {
            name: "laptop",
            displays: &laptop,
            placement: Placement {
                x: -768,
                y: -100,
                size: 50,
            },
        }];
        let portals = portals(&local, &others);
        let portal = &portals[0];
        // the laptop is 432 high on the canvas, reaching down to 332
        assert_eq!(
            (portal.side, portal.at, portal.from, portal.to),
            (Position::Left, 0, 0, 332)
        );
        // 200 down the Mac is 300 down the laptop on the canvas, 600 in its
        // own units; its right edge is x 1536
        assert_eq!(portal.entry_point((0, 200)), (1535, 600));
        // below the stretch the pointer is held at its end
        assert_eq!(portal.entry_point((0, 900)), (1535, 862));
    }

    #[test]
    fn displays_of_one_device_next_to_each_other_are_no_portal() {
        let (_, desktop, _) = desk();
        let local = Device {
            name: "desktop",
            displays: &desktop,
            placement: at(0, 0),
        };
        assert!(portals(&local, &[]).is_empty());
    }
}
