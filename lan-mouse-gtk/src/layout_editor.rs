//! Arranges the devices by dragging their displays: drag a device to move
//! it, drag its corner to resize it, double-click it to give it its real
//! size again.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use gtk::{cairo, glib, prelude::*};

use lan_mouse_ipc::{ClientHandle, DisplayInfo, Placement};

/// a device on the canvas
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Key {
    Local,
    Client(ClientHandle),
}

#[derive(Clone, Debug, Default)]
struct Item {
    name: String,
    displays: Vec<DisplayInfo>,
    /// `None` until the device was placed; it is shown next to the others
    placement: Option<Placement>,
}

/// canvas units to widget pixels
#[derive(Clone, Copy, Debug)]
struct View {
    scale: f64,
    x: f64,
    y: f64,
}

impl View {
    fn to_widget(self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.x + x * self.scale, self.y + y * self.scale)
    }
}

#[derive(Clone, Copy, Debug)]
enum DragKind {
    Move,
    Resize,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    key: Key,
    kind: DragKind,
    start: Placement,
    /// the device's bounds on the canvas when the drag started
    bounds: Rect,
}

/// left, top, right, bottom on the canvas
type Rect = (f64, f64, f64, f64);

/// widget pixels a dragged edge snaps over
const SNAP_PX: f64 = 14.0;
/// side of the resize handle in widget pixels
const HANDLE_PX: f64 = 12.0;
const PADDING_PX: f64 = 24.0;
const MIN_SIZE: u16 = 20;
const MAX_SIZE: u16 = 400;

#[derive(Default)]
struct State {
    items: BTreeMap<Key, Item>,
    view: Option<View>,
    drag: Option<Drag>,
}

impl State {
    /// where an item sits: its own placement, or next to all placed items
    fn placement(&self, key: Key) -> Option<Placement> {
        let item = self.items.get(&key)?;
        if let Some(p) = item.placement {
            return Some(p);
        }
        // unplaced devices queue up to the right of everything placed
        let right = self
            .items
            .iter()
            .filter(|(k, i)| **k < key || i.placement.is_some())
            .filter(|(k, _)| **k != key)
            .filter_map(|(&k, _)| self.bounds(k))
            .map(|b| b.2)
            .fold(None, |m: Option<f64>, r| Some(m.map_or(r, |m| m.max(r))));
        let (min_x, min_y) = extent(&item.displays).map_or((0, 0), |e| (e.0, e.1));
        let x = right.map_or(0.0, |r| r + 200.0) as i32 - min_x;
        Some(Placement {
            x,
            y: -min_y,
            size: 100,
        })
    }

    fn display_rects(&self, key: Key) -> Vec<Rect> {
        let (Some(item), Some(p)) = (self.items.get(&key), self.placement(key)) else {
            return Vec::new();
        };
        item.displays.iter().map(|d| canvas_rect(d, p)).collect()
    }

    fn bounds(&self, key: Key) -> Option<Rect> {
        self.display_rects(key)
            .into_iter()
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
    }

    fn visible(&self) -> Vec<Key> {
        self.items
            .iter()
            .filter(|(_, i)| !i.displays.is_empty())
            .map(|(&k, _)| k)
            .collect()
    }

    /// fits all devices into the widget
    fn fit(&mut self, width: f64, height: f64) {
        let all = self
            .visible()
            .into_iter()
            .filter_map(|k| self.bounds(k))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
        let Some((l, t, r, b)) = all else {
            self.view = None;
            return;
        };
        let (w, h) = ((r - l).max(1.0), (b - t).max(1.0));
        let scale = ((width - 2.0 * PADDING_PX) / w)
            .min((height - 2.0 * PADDING_PX) / h)
            .max(0.01);
        self.view = Some(View {
            scale,
            x: (width - w * scale) / 2.0 - l * scale,
            y: (height - h * scale) / 2.0 - t * scale,
        });
    }

    /// what is under a widget point: a resize handle or a device
    fn hit(&self, (x, y): (f64, f64)) -> Option<(Key, DragKind)> {
        let view = self.view?;
        let keys = self.visible();
        // later devices are drawn on top
        for &key in keys.iter().rev() {
            let Some(b) = self.bounds(key) else { continue };
            let (hx, hy) = view.to_widget((b.2, b.3));
            if (x - hx).abs() <= HANDLE_PX && (y - hy).abs() <= HANDLE_PX {
                return Some((key, DragKind::Resize));
            }
        }
        for &key in keys.iter().rev() {
            let inside = self.display_rects(key).into_iter().any(|r| {
                let (l, t) = view.to_widget((r.0, r.1));
                let (rr, bb) = view.to_widget((r.2, r.3));
                l <= x && x < rr && t <= y && y < bb
            });
            if inside {
                return Some((key, DragKind::Move));
            }
        }
        None
    }

    fn drag_to(&mut self, (dx, dy): (f64, f64)) {
        let (Some(drag), Some(view)) = (self.drag, self.view) else {
            return;
        };
        let placement = match drag.kind {
            DragKind::Move => Placement {
                x: drag.start.x + (dx / view.scale).round() as i32,
                y: drag.start.y + (dy / view.scale).round() as i32,
                ..drag.start
            },
            DragKind::Resize => {
                // the top left corner stays put
                let (l, t, r, b) = drag.bounds;
                // follow whichever way the corner moved most
                let wide = (r - l + dx / view.scale) / (r - l);
                let tall = (b - t + dy / view.scale) / (b - t);
                let grow = if (wide - 1.0).abs() > (tall - 1.0).abs() {
                    wide
                } else {
                    tall
                };
                let mut size = (drag.start.size as f64 * grow).round() as i32;
                if (size - 100).abs() <= 4 {
                    size = 100;
                }
                let size = size.clamp(MIN_SIZE as i32, MAX_SIZE as i32) as u16;
                let ratio = size as f64 / drag.start.size as f64;
                Placement {
                    x: (l - (l - drag.start.x as f64) * ratio).round() as i32,
                    y: (t - (t - drag.start.y as f64) * ratio).round() as i32,
                    size,
                }
            }
        };
        if let Some(item) = self.items.get_mut(&drag.key) {
            item.placement = Some(placement);
        }
    }

    /// Moves a dragged device so its edges meet or line up with nearby
    /// edges of other devices.
    fn snap(&mut self, key: Key) {
        let (Some(view), Some(mut p)) = (self.view, self.placement(key)) else {
            return;
        };
        let reach = SNAP_PX / view.scale;
        let mine = self.display_rects(key);
        let others = self
            .visible()
            .into_iter()
            .filter(|&k| k != key)
            .flat_map(|k| self.display_rects(k))
            .collect::<Vec<_>>();
        let closest = |pairs: Vec<(f64, f64)>| {
            pairs
                .into_iter()
                .map(|(a, b)| b - a)
                .filter(|d| d.abs() <= reach)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        };
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for m in &mine {
            for o in &others {
                for a in [m.0, m.2] {
                    for b in [o.0, o.2] {
                        xs.push((a, b));
                    }
                }
                for a in [m.1, m.3] {
                    for b in [o.1, o.3] {
                        ys.push((a, b));
                    }
                }
            }
        }
        p.x += closest(xs).unwrap_or(0.0).round() as i32;
        p.y += closest(ys).unwrap_or(0.0).round() as i32;
        if let Some(item) = self.items.get_mut(&key) {
            item.placement = Some(p);
        }
    }
}

fn canvas_rect(d: &DisplayInfo, p: Placement) -> Rect {
    let s = p.size.max(1) as f64 / 100.0;
    (
        p.x as f64 + d.x as f64 * s,
        p.y as f64 + d.y as f64 * s,
        p.x as f64 + (d.x + d.width) as f64 * s,
        p.y as f64 + (d.y + d.height) as f64 * s,
    )
}

/// top left of all displays in the device's own coordinates
fn extent(displays: &[DisplayInfo]) -> Option<(i32, i32)> {
    let x = displays.iter().map(|d| d.x).min()?;
    let y = displays.iter().map(|d| d.y).min()?;
    Some((x, y))
}

type OnChange = Box<dyn Fn(Key, Placement)>;

#[derive(Clone)]
pub(crate) struct LayoutEditor {
    area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl LayoutEditor {
    /// `on_change` is called with a device's new placement once it was
    /// dropped
    pub(crate) fn new(on_change: impl Fn(Key, Placement) + 'static) -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(280)
            .hexpand(true)
            .build();
        let state = Rc::new(RefCell::new(State::default()));
        let on_change: Rc<OnChange> = Rc::new(Box::new(on_change));

        area.set_draw_func(glib::clone!(
            #[strong]
            state,
            move |area, cr, width, height| {
                let mut state = state.borrow_mut();
                if state.drag.is_none() {
                    state.fit(width as f64, height as f64);
                }
                draw(&state, area, cr);
            }
        ));

        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            state,
            move |gesture, x, y| {
                let mut state = state.borrow_mut();
                let Some((key, kind)) = state.hit((x, y)) else {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                };
                let (Some(start), Some(bounds)) = (state.placement(key), state.bounds(key)) else {
                    return;
                };
                state.drag = Some(Drag {
                    key,
                    kind,
                    start,
                    bounds,
                });
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[strong]
            state,
            #[weak]
            area,
            move |_, dx, dy| {
                state.borrow_mut().drag_to((dx, dy));
                area.queue_draw();
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[strong]
            state,
            #[strong]
            on_change,
            #[weak]
            area,
            move |_, dx, dy| {
                let changed = {
                    let mut state = state.borrow_mut();
                    state.drag_to((dx, dy));
                    let Some(drag) = state.drag.take() else {
                        return;
                    };
                    if let DragKind::Move = drag.kind {
                        state.snap(drag.key);
                    }
                    state
                        .placement(drag.key)
                        .filter(|&p| p != drag.start || dx != 0.0 || dy != 0.0)
                        .map(|p| (drag.key, p))
                };
                if let Some((key, placement)) = changed {
                    on_change(key, placement);
                }
                area.queue_draw();
            }
        ));
        area.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[strong]
            state,
            #[strong]
            on_change,
            #[weak]
            area,
            move |_, presses, x, y| {
                if presses != 2 {
                    return;
                }
                let changed = {
                    let mut state = state.borrow_mut();
                    let Some((key, _)) = state.hit((x, y)) else {
                        return;
                    };
                    let Some(p) = state.placement(key) else {
                        return;
                    };
                    let p = Placement { size: 100, ..p };
                    if let Some(item) = state.items.get_mut(&key) {
                        item.placement = Some(p);
                    }
                    (key, p)
                };
                on_change(changed.0, changed.1);
                area.queue_draw();
            }
        ));
        area.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            state,
            #[weak]
            area,
            move |_, x, y| {
                let cursor = match state.borrow().hit((x, y)) {
                    Some((_, DragKind::Resize)) => "nwse-resize",
                    Some((_, DragKind::Move)) => "grab",
                    None => "default",
                };
                area.set_cursor_from_name(Some(cursor));
            }
        ));
        area.add_controller(motion);

        Self { area, state }
    }

    pub(crate) fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    fn update(&self, key: Key, f: impl FnOnce(&mut Item)) {
        {
            let mut state = self.state.borrow_mut();
            // never pull a device away from under the pointer
            if state.drag.is_some_and(|d| d.key == key) {
                return;
            }
            f(state.items.entry(key).or_default());
        }
        self.area.queue_draw();
    }

    pub(crate) fn set_local(&self, name: String, displays: Vec<DisplayInfo>, placement: Placement) {
        self.update(Key::Local, |item| {
            item.name = name;
            item.displays = displays;
            item.placement = Some(placement);
        });
    }

    pub(crate) fn set_client(
        &self,
        handle: ClientHandle,
        name: String,
        placement: Option<Placement>,
    ) {
        self.update(Key::Client(handle), |item| {
            item.name = name;
            item.placement = placement;
        });
    }

    pub(crate) fn set_client_displays(&self, handle: ClientHandle, displays: Vec<DisplayInfo>) {
        self.update(Key::Client(handle), |item| item.displays = displays);
    }

    pub(crate) fn remove_client(&self, handle: ClientHandle) {
        self.state.borrow_mut().items.remove(&Key::Client(handle));
        self.area.queue_draw();
    }
}

/// fill colors, the local device first
const COLORS: [(f64, f64, f64); 5] = [
    (0.21, 0.52, 0.89),
    (0.90, 0.38, 0.62),
    (0.20, 0.68, 0.47),
    (0.96, 0.62, 0.17),
    (0.57, 0.39, 0.85),
];

fn draw(state: &State, area: &gtk::DrawingArea, cr: &cairo::Context) {
    let dark = adw::StyleManager::default().is_dark();
    let ink = if dark { 0.92 } else { 0.15 };
    let Some(view) = state.view else {
        cr.set_source_rgb(ink, ink, ink);
        cr.set_font_size(13.0);
        let text = "Displays show up here once devices are connected";
        if let Ok(ext) = cr.text_extents(text) {
            cr.move_to(
                (area.width() as f64 - ext.width()) / 2.0,
                area.height() as f64 / 2.0,
            );
            let _ = cr.show_text(text);
        }
        return;
    };

    for (n, key) in state.visible().into_iter().enumerate() {
        let item = &state.items[&key];
        let (r, g, b) = COLORS[n % COLORS.len()];
        let dragging = state.drag.is_some_and(|d| d.key == key);
        let rects = state.display_rects(key);
        for rect in &rects {
            let (l, t) = view.to_widget((rect.0, rect.1));
            let (rr, bb) = view.to_widget((rect.2, rect.3));
            cr.rectangle(l + 1.0, t + 1.0, rr - l - 2.0, bb - t - 2.0);
            cr.set_source_rgba(r, g, b, if dragging { 0.55 } else { 0.35 });
            let _ = cr.fill_preserve();
            cr.set_source_rgb(r, g, b);
            cr.set_line_width(1.5);
            if item.placement.is_none() {
                cr.set_dash(&[5.0, 4.0], 0.0);
            }
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }

        // name and size on the widest display
        let Some(largest) = rects
            .iter()
            .max_by(|a, b| (a.2 - a.0).total_cmp(&(b.2 - b.0)))
        else {
            continue;
        };
        let (cx, cy) =
            view.to_widget(((largest.0 + largest.2) / 2.0, (largest.1 + largest.3) / 2.0));
        let size = state.placement(key).map_or(100, |p| p.size);
        let mut lines = vec![item.name.clone()];
        if item.placement.is_none() {
            lines.push("not placed".to_owned());
        } else if size != 100 {
            lines.push(format!("{size}%"));
        }
        let (l, _) = view.to_widget((largest.0, largest.1));
        let (r_, _) = view.to_widget((largest.2, largest.3));
        cr.set_source_rgb(ink, ink, ink);
        for (i, line) in lines.iter().enumerate() {
            let largest_size = if i == 0 { 12.0 } else { 10.0 };
            let Some((line, ext)) = fit_text(cr, line, r_ - l - 8.0, largest_size) else {
                continue;
            };
            let y = cy + (i as f64 - (lines.len() - 1) as f64 / 2.0) * 15.0 + ext.height() / 2.0;
            cr.move_to(cx - ext.width() / 2.0 - ext.x_bearing(), y);
            let _ = cr.show_text(&line);
        }

        // resize handle at the bottom right
        if let Some(bounds) = state.bounds(key) {
            let (hx, hy) = view.to_widget((bounds.2, bounds.3));
            cr.rectangle(
                hx - HANDLE_PX / 2.0,
                hy - HANDLE_PX / 2.0,
                HANDLE_PX,
                HANDLE_PX,
            );
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill();
        }
    }
}

/// `text` at a font size that fits `width`, shortened if even the smallest
/// does not
fn fit_text(
    cr: &cairo::Context,
    text: &str,
    width: f64,
    largest: f64,
) -> Option<(String, cairo::TextExtents)> {
    for size in [12.0, 11.0, 10.0, 9.0]
        .into_iter()
        .filter(|&s| s <= largest)
    {
        cr.set_font_size(size);
        let ext = cr.text_extents(text).ok()?;
        if ext.width() <= width {
            return Some((text.to_owned(), ext));
        }
    }
    let mut chars = text.chars().collect::<Vec<_>>();
    while chars.len() > 1 {
        chars.pop();
        let short = format!("{}…", chars.iter().collect::<String>());
        let ext = cr.text_extents(&short).ok()?;
        if ext.width() <= width {
            return Some((short, ext));
        }
    }
    None
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

    fn state() -> State {
        let mut state = State::default();
        state.items.insert(
            Key::Local,
            Item {
                name: "mac".into(),
                displays: vec![display(0, 0, 1470, 956)],
                placement: Some(Placement {
                    x: 0,
                    y: 1080,
                    size: 100,
                }),
            },
        );
        state.items.insert(
            Key::Client(0),
            Item {
                name: "desktop".into(),
                displays: vec![display(0, 0, 1920, 1080)],
                placement: None,
            },
        );
        state.fit(800.0, 400.0);
        state
    }

    #[test]
    fn unplaced_device_waits_to_the_right() {
        let state = state();
        let p = state.placement(Key::Client(0)).unwrap();
        assert_eq!((p.x, p.y, p.size), (1670, 0, 100));
    }

    #[test]
    fn dropped_device_snaps_onto_the_neighbouring_edge() {
        let mut state = state();
        // desktop dropped a little off the Mac's top edge and left edges
        state.items.get_mut(&Key::Client(0)).unwrap().placement = Some(Placement {
            x: 20,
            y: -15,
            size: 100,
        });
        state.snap(Key::Client(0));
        let p = state.items[&Key::Client(0)].placement.unwrap();
        assert_eq!((p.x, p.y), (0, 0));
    }

    #[test]
    fn resizing_keeps_the_top_left_corner() {
        let mut state = state();
        let start = state.placement(Key::Local).unwrap();
        let bounds = state.bounds(Key::Local).unwrap();
        state.drag = Some(Drag {
            key: Key::Local,
            kind: DragKind::Resize,
            start,
            bounds,
        });
        let scale = state.view.unwrap().scale;
        // drag the corner to make it half as wide
        state.drag_to((-735.0 * scale, 0.0));
        let p = state.items[&Key::Local].placement.unwrap();
        assert_eq!((p.x, p.y, p.size), (0, 1080, 50));
    }
}
