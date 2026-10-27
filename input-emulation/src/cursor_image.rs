//! Picture of a second cursor: an outlined arrow with a soft shadow and a
//! translucent name tag, antialiased by supersampling.

/// position of the arrow's tip in the image
pub(crate) const TIP: (i32, i32) = (3, 3);

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
const OUTLINE: f32 = 1.5;
const SHADOW_OFFSET: (f32, f32) = (0.8, 1.6);
const SHADOW_SPREAD: f32 = 2.5;
const SHADOW_ALPHA: f32 = 0.3;
/// name tag position relative to the tip
const TAG_OFFSET: (f32, f32) = (13.0, 19.0);
const TAG_PADDING: (f32, f32) = (6.0, 2.0);
const TAG_ALPHA: f32 = 0.78;
const SAMPLES: usize = 4;

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

pub(crate) fn render(color: (u8, u8, u8), text: &TextMask) -> Image {
    let tag = (
        TIP.0 as f32 + TAG_OFFSET.0,
        TIP.1 as f32 + TAG_OFFSET.1,
        text.width as f32 + 2.0 * TAG_PADDING.0,
        text.height as f32 + 2.0 * TAG_PADDING.1,
    );
    let width = (tag.0 + tag.2).ceil() as usize + 2;
    let height = (tag.1 + tag.3).ceil() as usize + 2;
    let text_origin = (
        (tag.0 + TAG_PADDING.0) as i64,
        (tag.1 + TAG_PADDING.1) as i64,
    );

    let mut pixels = Vec::with_capacity(width * height);
    for py in 0..height {
        for px in 0..width {
            let text_alpha = {
                let (tx, ty) = (px as i64 - text_origin.0, py as i64 - text_origin.1);
                if (0..text.width as i64).contains(&tx) && (0..text.height as i64).contains(&ty) {
                    text.coverage[ty as usize * text.width + tx as usize] as f32 / 255.0
                } else {
                    0.0
                }
            };
            let mut sum = Rgba::CLEAR;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let s = sample((x, y), color, tag, text_alpha);
                    sum = Rgba(sum.0 + s.0, sum.1 + s.1, sum.2 + s.2, sum.3 + s.3);
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let byte = |v: f32| (v / n * 255.0).round().clamp(0.0, 255.0) as u8;
            pixels.push([byte(sum.0), byte(sum.1), byte(sum.2), byte(sum.3)]);
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// color at one sample point: shadow, name tag with text, outlined arrow
fn sample(
    (x, y): (f32, f32),
    color: (u8, u8, u8),
    (tag_x, tag_y, tag_w, tag_h): (f32, f32, f32, f32),
    text_alpha: f32,
) -> Rgba {
    let local = (x - TIP.0 as f32, y - TIP.1 as f32);
    let mut c = Rgba::CLEAR;

    let shadow = arrow_distance((local.0 - SHADOW_OFFSET.0, local.1 - SHADOW_OFFSET.1));
    let falloff = (1.0 - (shadow - OUTLINE) / SHADOW_SPREAD).clamp(0.0, 1.0);
    c = c.under(Rgba::solid((0, 0, 0), SHADOW_ALPHA * falloff));

    if rounded_rect_contains((x, y), (tag_x, tag_y, tag_w, tag_h), tag_h / 2.0) {
        c = c.under(Rgba::solid(color, TAG_ALPHA));
        c = c.under(Rgba::solid((255, 255, 255), text_alpha));
    }

    let d = arrow_distance(local);
    if d <= 0.0 {
        c = c.under(Rgba::solid(color, 1.0));
    } else if d <= OUTLINE {
        c = c.under(Rgba::solid((255, 255, 255), 1.0));
    }
    c
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

fn rounded_rect_contains((x, y): (f32, f32), (rx, ry, w, h): (f32, f32, f32, f32), r: f32) -> bool {
    let cx = x.clamp(rx + r, rx + w - r);
    let cy = y.clamp(ry + r, ry + h - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
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

    #[test]
    fn color_is_stable_per_name() {
        assert_eq!(color_for("Jason-MacAir"), color_for("Jason-MacAir"));
        assert!(PALETTE.contains(&color_for("DESKTOP-SN73G0N")));
    }

    #[test]
    fn arrow_is_filled_and_outlined() {
        let image = render((234, 88, 12), &text(40, 14));
        // inside the arrow, below the tip
        assert_eq!(pixel(&image, (TIP.0 + 3, TIP.1 + 10)), [234, 88, 12, 255]);
        // just left of the arrow's left edge: white outline
        let outline = pixel(&image, (TIP.0 - 1, TIP.1 + 10));
        assert!(outline[3] > 200 && outline[0] > 200 && outline[2] > 200);
    }

    #[test]
    fn tag_holds_the_text_and_corners_stay_clear() {
        let image = render((234, 88, 12), &text(40, 14));
        assert!(image.width >= TIP.0 as usize + 13 + 40 + 12);
        // text pixels are opaque white over the tag
        let text_px = pixel(&image, (TIP.0 + 13 + 6 + 5, TIP.1 + 19 + 2 + 5));
        assert_eq!(text_px, [255, 255, 255, 255]);
        // far corner is transparent
        assert_eq!(pixel(&image, (image.width as i32 - 1, 0)), [0, 0, 0, 0]);
    }

    #[test]
    fn pixels_are_premultiplied() {
        let image = render((234, 88, 12), &text(40, 14));
        assert!(
            image
                .pixels
                .iter()
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
        );
    }
}
