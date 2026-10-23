//! The tray's icon on Windows and Linux: the dashboard's mark (`ui/public/
//! favicon.svg`, three strokes and a dot on a dark rounded square), drawn
//! at the size the platform asks for, with the middle stroke and the dot in
//! the state's colour and both larger than in the favicon, so the state
//! shows at 16 pixels. macOS takes a template image instead (`macos.rs`).

use super::Tone;

/// In the favicon's 512-unit square.
const BACKGROUND: [u8; 3] = [0x0f, 0x11, 0x15];
const BLUE: [u8; 3] = [0x5b, 0x8c, 0xff];

fn colour(tone: Tone) -> [u8; 3] {
    match tone {
        Tone::Ok => [0x3e, 0xcf, 0x8e],
        Tone::Busy => [0xf5, 0xa5, 0x24],
        Tone::Attention => [0xf0, 0x52, 0x4f],
        Tone::Idle => [0x8b, 0x92, 0x9e],
    }
}

/// One shape: how much of the point `(x, y)` (512 units) it covers.
enum Shape {
    /// A rounded square over the whole icon, corner radius.
    Square(f32),
    /// A line from `x0` to `x1` at `y`, with round caps of radius `r`.
    Stroke {
        x0: f32,
        x1: f32,
        y: f32,
        r: f32,
    },
    Dot {
        x: f32,
        y: f32,
        r: f32,
    },
}

impl Shape {
    fn inside(&self, x: f32, y: f32) -> bool {
        match *self {
            Shape::Square(r) => {
                let cx = x.clamp(r, 512.0 - r);
                let cy = y.clamp(r, 512.0 - r);
                (x - cx).powi(2) + (y - cy).powi(2) <= r * r
            }
            Shape::Stroke { x0, x1, y: sy, r } => {
                let cx = x.clamp(x0, x1);
                (x - cx).powi(2) + (y - sy).powi(2) <= r * r
            }
            Shape::Dot { x: dx, y: dy, r } => (x - dx).powi(2) + (y - dy).powi(2) <= r * r,
        }
    }
}

/// The icon as `size`×`size` pixels, RGBA with straight alpha, rows from
/// the top.
pub fn rgba(size: u32, tone: Tone) -> Vec<u8> {
    let state = colour(tone);
    // Painted in order, each over the last: (shape, colour, opacity).
    let shapes = [
        (Shape::Square(112.0), BACKGROUND, 1.0),
        (
            Shape::Stroke {
                x0: 120.0,
                x1: 392.0,
                y: 150.0,
                r: 30.0,
            },
            BLUE,
            1.0,
        ),
        (
            Shape::Stroke {
                x0: 120.0,
                x1: 248.0,
                y: 256.0,
                r: 30.0,
            },
            state,
            1.0,
        ),
        (
            Shape::Stroke {
                x0: 120.0,
                x1: 392.0,
                y: 362.0,
                r: 30.0,
            },
            BLUE,
            0.55,
        ),
        (
            Shape::Dot {
                x: 352.0,
                y: 256.0,
                r: 62.0,
            },
            state,
            1.0,
        ),
    ];
    const SAMPLES: u32 = 4;
    let unit = 512.0 / size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            // Straight colour and alpha, composited per sample, averaged.
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * unit;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * unit;
                    let mut c = [0.0f32; 3];
                    let mut alpha = 0.0f32;
                    for (shape, col, op) in &shapes {
                        if shape.inside(x, y) {
                            let k = *op;
                            for (ci, &cc) in c.iter_mut().zip(col) {
                                *ci = *ci * (1.0 - k) + cc as f32 * k;
                            }
                            alpha = alpha * (1.0 - k) + k;
                        }
                    }
                    // Premultiplied while summing, so edges do not darken.
                    r += c[0] * alpha;
                    g += c[1] * alpha;
                    b += c[2] * alpha;
                    a += alpha;
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            if a > 0.0 {
                out.extend([
                    (r / a).round() as u8,
                    (g / a).round() as u8,
                    (b / a).round() as u8,
                    (a / n * 255.0).round() as u8,
                ]);
            } else {
                out.extend([0, 0, 0, 0]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(img: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn the_dot_has_the_states_colour_and_the_corners_are_clear() {
        for size in [16, 22, 32, 48] {
            for tone in [Tone::Idle, Tone::Ok, Tone::Busy, Tone::Attention] {
                let img = rgba(size, tone);
                assert_eq!(img.len(), (size * size * 4) as usize);
                assert_eq!(pixel(&img, size, 0, 0)[3], 0, "corner at {size}");
                // The dot's centre, (352, 256) of 512.
                let p = pixel(&img, size, size * 352 / 512, size / 2);
                assert_eq!(p[..3], colour(tone), "{tone:?} at {size}");
                assert_eq!(p[3], 255);
            }
        }
    }
}
