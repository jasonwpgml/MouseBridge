use windows::Win32::Foundation::RECT;

use crate::Position;

fn is_within_dp_region(point: (i32, i32), display: &RECT) -> bool {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .iter()
    .all(|&pos| is_within_dp_boundary(point, display, pos))
}

fn is_within_dp_boundary(point: (i32, i32), display: &RECT, pos: Position) -> bool {
    let (x, y) = point;
    match pos {
        Position::Left => display.left <= x,
        Position::Right => display.right > x,
        Position::Top => display.top <= y,
        Position::Bottom => display.bottom > y,
    }
}

fn in_display_region(point: (i32, i32), displays: &[RECT]) -> bool {
    displays.iter().any(|d| is_within_dp_region(point, d))
}

/// Returns the edge the cursor crossed when it left the display it was on
/// and landed on no other display.
///
/// Edges are per display: with displays of different sizes the bottom edge
/// of a shorter one counts as well, as long as no display continues below it
/// at that point.
pub(crate) fn entered_barrier(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
) -> Option<Position> {
    if in_display_region(curr_pos, displays) {
        return None;
    }
    let display = displays.iter().find(|d| is_within_dp_region(prev_pos, d))?;
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .into_iter()
    .find(|&pos| !is_within_dp_boundary(curr_pos, display, pos))
}

///
/// clamp point to display bounds
///
/// # Arguments
///
/// * `prev_point`: coordinates, the cursor was before entering, within bounds of a display
/// * `entry_point`: point to clamp
///
/// returns: (i32, i32), the corrected entry point
///
pub(crate) fn clamp_to_display_bounds(
    display_regions: &[RECT],
    prev_point: (i32, i32),
    point: (i32, i32),
) -> (i32, i32) {
    /* find display where movement came from */
    let display = display_regions
        .iter()
        .find(|&d| is_within_dp_region(prev_point, d))
        .unwrap();

    /* clamp to bounds (inclusive) */
    let (x, y) = point;
    let (min_x, max_x) = (display.left, display.right - 1);
    let (min_y, max_y) = (display.top, display.bottom - 1);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    /// a landscape display and a taller portrait display to its right that
    /// reaches further down
    fn displays() -> Vec<RECT> {
        vec![rect(0, 0, 2560, 1440), rect(2560, -600, 4000, 1960)]
    }

    #[test]
    fn bottom_of_shorter_display_is_a_barrier() {
        let barrier = entered_barrier((1000, 1439), (1000, 1445), &displays());
        assert_eq!(barrier, Some(Position::Bottom));
    }

    #[test]
    fn moving_onto_neighbouring_display_is_no_barrier() {
        assert_eq!(entered_barrier((2559, 500), (2565, 500), &displays()), None);
    }

    #[test]
    fn edge_next_to_empty_space_is_a_barrier() {
        let barrier = entered_barrier((2560, 1700), (2555, 1700), &displays());
        assert_eq!(barrier, Some(Position::Left));
    }

    #[test]
    fn outer_edges_are_barriers() {
        let displays = displays();
        assert_eq!(
            entered_barrier((3000, 1959), (3000, 1965), &displays),
            Some(Position::Bottom)
        );
        assert_eq!(
            entered_barrier((3999, 0), (4005, 0), &displays),
            Some(Position::Right)
        );
    }

    #[test]
    fn starting_outside_all_displays_is_no_barrier() {
        assert_eq!(
            entered_barrier((1000, 1500), (1000, 1510), &displays()),
            None
        );
    }
}
