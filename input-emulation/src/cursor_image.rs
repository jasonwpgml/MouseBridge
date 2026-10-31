//! Pictures of a second cursor: an outlined arrow with a soft shadow, and its
//! name as bare text in a soft dark glow. Nothing is filled behind the name,
//! so what lies under it stays visible. Antialiased by supersampling.

use std::time::Duration;

/// position of the arrow's tip in the image at scale 1
const TIP: (f32, f32) = (3.0, 3.0);

/// arrow outline, tip at the origin
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 17.5),
    (4.3, 13.6),
    (7.0, 19.8),
    (9.5, 18.7),
    (6.9, 12.6),
    (12.3, 12.6),
];
const OUTLINE: f32 = 1.25;
const SHADOW_OFFSET: (f32, f32) = (0.6, 1.2);
const SHADOW_SPREAD: f32 = 2.0;
const SHADOW_ALPHA: f32 = 0.22;
/// where the name starts relative to the tip
const LABEL_OFFSET: (f32, f32) = (15.0, 14.0);
/// reach of the glow around the name
const GLOW_RADIUS: f32 = 2.0;
const GLOW_ALPHA: f32 = 0.72;
const SAMPLES: usize = 4;
/// how long the name stays after entering, then how long it takes to fade
const LABEL_HOLD: Duration = Duration::from_millis(1200);
const LABEL_FADE: Duration = Duration::from_millis(350);

/// how visible the name is this long after entering; 0 means it is gone
pub(crate) fn label_opacity(elapsed: Duration) -> f32 {
    let fading = elapsed.saturating_sub(LABEL_HOLD);
    (1.0 - fading.as_secs_f32() / LABEL_FADE.as_secs_f32()).clamp(0.0, 1.0)
}

const PALETTE: [(u8, u8, u8); 6] = [
    (234, 88, 12),
    (13, 148, 136),
    (124, 58, 237),
    (37, 99, 235),
    (219, 39, 119),
    (22, 163, 74),
];

/// a color that stays the same for the same name
pub(crate) fn color_for(name: &str) -> (u8, u8, u8) {
    // FNV-1a, stable across runs unlike std's hasher
    let hash = name.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    PALETTE[(hash % PALETTE.len() as u64) as usize]
}

/// coverage of rendered text, one byte per pixel
pub(crate) struct TextMask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

/// premultiplied pixels, row by row from the top
pub(crate) struct Image {
    pub width: usize,
    pub height: usize,
    /// red, green, blue, alpha
    pub pixels: Vec<[u8; 4]>,
}

/// the arrow and the name as separate layers, so the name can fade alone
pub(crate) struct Picture {
    /// position of the arrow's tip in the image
    pub tip: (i32, i32),
    width: usize,
    height: usize,
    arrow: Vec<Rgba>,
    label: Vec<Rgba>,
}

impl Picture {
    /// the arrow over the name, which shows at `label_opacity`
    pub(crate) fn frame(&self, label_opacity: f32) -> Image {
        let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        let pixels = self
            .label
            .iter()
            .zip(&self.arrow)
            .map(|(label, arrow)| {
                let k = label_opacity;
                let c = Rgba(label.0 * k, label.1 * k, label.2 * k, label.3 * k).under(*arrow);
                [byte(c.0), byte(c.1), byte(c.2), byte(c.3)]
            })
            .collect();
        Image {
            width: self.width,
            height: self.height,
            pixels,
        }
    }
}

#[derive(Clone, Copy)]
struct Rgba(f32, f32, f32, f32);

impl Rgba {
    const CLEAR: Rgba = Rgba(0.0, 0.0, 0.0, 0.0);

    fn solid((r, g, b): (u8, u8, u8), alpha: f32) -> Self {
        let c = |v: u8| v as f32 / 255.0 * alpha;
        Rgba(c(r), c(g), c(b), alpha)
    }

    /// `top` composited over `self`, both premultiplied
    fn under(self, top: Rgba) -> Self {
        let k = 1.0 - top.3;
        Rgba(
            top.0 + self.0 * k,
            top.1 + self.1 * k,
            top.2 + self.2 * k,
            top.3 + self.3 * k,
        )
    }
}

/// `text` is in device pixels, `scale` is the display's (1 at 96 dpi)
pub(crate) fn render(color: (u8, u8, u8), text: &TextMask, scale: f32) -> Picture {
    let tip = (TIP.0 * scale, TIP.1 * scale);
    let glow = (GLOW_RADIUS * scale).ceil() as usize;
    let text_x = (tip.0 + LABEL_OFFSET.0 * scale).round() as usize;
    let text_y = (tip.1 + LABEL_OFFSET.1 * scale).round() as usize;
    let arrow_bottom = tip.1 + (20.0 + OUTLINE + SHADOW_SPREAD + SHADOW_OFFSET.1) * scale;
    let width = text_x + text.width + glow + 1;
    let height = (text_y + text.height + glow + 1).max(arrow_bottom.ceil() as usize);

    let mut arrow = Vec::with_capacity(width * height);
    let mut label = Vec::with_capacity(width * height);
    for py in 0..height {
        for px in 0..width {
            arrow.push(arrow_pixel((px, py), color, tip, scale));
            label.push(label_pixel(
                (px as i64 - text_x as i64, py as i64 - text_y as i64),
                text,
                glow as i64,
            ));
        }
    }
    Picture {
        tip: (tip.0.round() as i32, tip.1.round() as i32),
        width,
        height,
        arrow,
        label,
    }
}

fn arrow_pixel((px, py): (usize, usize), color: (u8, u8, u8), tip: (f32, f32), scale: f32) -> Rgba {
    let mut sum = Rgba::CLEAR;
    for sy in 0..SAMPLES {
        for sx in 0..SAMPLES {
            let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
            let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
            let s = arrow_sample(((x - tip.0) / scale, (y - tip.1) / scale), color);
            sum = Rgba(sum.0 + s.0, sum.1 + s.1, sum.2 + s.2, sum.3 + s.3);
        }
    }
    let n = (SAMPLES * SAMPLES) as f32;
    Rgba(sum.0 / n, sum.1 / n, sum.2 / n, sum.3 / n)
}

/// color at one sample point of the arrow, in unscaled units from the tip
fn arrow_sample(local: (f32, f32), color: (u8, u8, u8)) -> Rgba {
    let shadow = arrow_distance((local.0 - SHADOW_OFFSET.0, local.1 - SHADOW_OFFSET.1));
    let falloff = (1.0 - (shadow - OUTLINE) / SHADOW_SPREAD).clamp(0.0, 1.0);
    let c = Rgba::CLEAR.under(Rgba::solid((0, 0, 0), SHADOW_ALPHA * falloff));

    let d = arrow_distance(local);
    if d <= 0.0 {
        c.under(Rgba::solid(color, 1.0))
    } else if d <= OUTLINE {
        c.under(Rgba::solid((255, 255, 255), 1.0))
    } else {
        c
    }
}

/// white text over the glow, `(x, y)` relative to the text's top left
fn label_pixel((x, y): (i64, i64), text: &TextMask, glow: i64) -> Rgba {
    let coverage = |x: i64, y: i64| -> f32 {
        if (0..text.width as i64).contains(&x) && (0..text.height as i64).contains(&y) {
            text.coverage[y as usize * text.width + x as usize] as f32 / 255.0
        } else {
            0.0
        }
    };
    let mut sum = 0.0;
    for gy in -glow..=glow {
        for gx in -glow..=glow {
            sum += coverage(x + gx, y + gy);
        }
    }
    // thin strokes cover little of the area, so the average is lifted
    let area = ((2 * glow + 1) * (2 * glow + 1)) as f32;
    let halo = (sum / area * 2.5).min(1.0) * GLOW_ALPHA;
    Rgba::solid((0, 0, 0), halo).under(Rgba::solid((255, 255, 255), coverage(x, y)))
}

/// signed distance to the arrow, negative inside
fn arrow_distance(p: (f32, f32)) -> f32 {
    let mut inside = false;
    let mut distance = f32::MAX;
    for i in 0..ARROW.len() {
        let a = ARROW[i];
        let b = ARROW[(i + 1) % ARROW.len()];
        distance = distance.min(segment_distance(p, a, b));
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    if inside { -distance } else { distance }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (dx, dy) = (p.0 - (a.0 + t * abx), p.1 - (a.1 + t * aby));
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(width: usize, height: usize) -> TextMask {
        TextMask {
            width,
            height,
            coverage: vec![255; width * height],
        }
    }

    fn pixel(image: &Image, (x, y): (i32, i32)) -> [u8; 4] {
        image.pixels[y as usize * image.width + x as usize]
    }

    /// where the text starts in the image at scale 1
    const TEXT: (i32, i32) = (18, 17);

    #[test]
    fn color_is_stable_per_name() {
        assert_eq!(color_for("Jason-MacAir"), color_for("Jason-MacAir"));
        assert!(PALETTE.contains(&color_for("DESKTOP-SN73G0N")));
    }

    #[test]
    fn arrow_is_filled_and_outlined() {
        let picture = render((234, 88, 12), &text(40, 14), 1.0);
        let image = picture.frame(1.0);
        // inside the arrow, below the tip
        let tip = picture.tip;
        assert_eq!(pixel(&image, (tip.0 + 3, tip.1 + 10)), [234, 88, 12, 255]);
        // just left of the arrow's left edge: white outline
        let outline = pixel(&image, (tip.0 - 1, tip.1 + 10));
        assert!(outline[3] > 200 && outline[0] > 200 && outline[2] > 200);
    }

    #[test]
    fn nothing_is_painted_away_from_the_text() {
        // a single dot of text: the glow fades out, the rest stays clear
        let mut mask = text(40, 14);
        mask.coverage.fill(0);
        mask.coverage[7 * 40 + 20] = 255;
        let image = render((234, 88, 12), &mask, 1.0).frame(1.0);
        assert_eq!(pixel(&image, (TEXT.0 + 20, TEXT.1 + 7))[0], 255);
        assert_eq!(pixel(&image, (TEXT.0 + 30, TEXT.1 + 7)), [0, 0, 0, 0]);
        assert_eq!(pixel(&image, (image.width as i32 - 1, 0)), [0, 0, 0, 0]);
    }

    #[test]
    fn faded_label_leaves_only_the_arrow() {
        let picture = render((234, 88, 12), &text(40, 14), 1.0);
        let image = picture.frame(0.0);
        assert_eq!(pixel(&image, (TEXT.0 + 20, TEXT.1 + 7)), [0, 0, 0, 0]);
        assert_eq!(
            pixel(&picture.frame(1.0), (TEXT.0 + 20, TEXT.1 + 7))[3],
            255
        );
    }

    #[test]
    fn label_holds_then_fades_out() {
        assert_eq!(label_opacity(Duration::ZERO), 1.0);
        assert_eq!(label_opacity(LABEL_HOLD), 1.0);
        let halfway = label_opacity(LABEL_HOLD + LABEL_FADE / 2);
        assert!(halfway > 0.4 && halfway < 0.6);
        assert_eq!(label_opacity(LABEL_HOLD + LABEL_FADE), 0.0);
    }

    #[test]
    fn scale_grows_the_arrow() {
        let picture = render((234, 88, 12), &text(40, 14), 2.0);
        assert_eq!(picture.tip, (6, 6));
        // would be outside the arrow at scale 1
        let image = picture.frame(0.0);
        assert_eq!(pixel(&image, (6 + 6, 6 + 20)), [234, 88, 12, 255]);
    }

    #[test]
    fn pixels_are_premultiplied() {
        let image = render((234, 88, 12), &text(40, 14), 1.0).frame(0.5);
        assert!(
            image
                .pixels
                .iter()
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
        );
    }
}
