//! Pictures drawn larger than they are, by the method the Preview settings choose (`settings::Upscale`): the scaled
//! image and how the GPU samples it. Nearest and bilinear are the GPU's own; sharp bilinear prescales by repeating
//! each pixel four times, so blending only touches pixel edges; Scale2x (EPX) and Scale3x (`AdvMAME3x`) copy a
//! neighbor's color into a corner where two neighbors agree, rounding diagonal edges without inventing colors.

use eframe::egui::{Color32, ColorImage, TextureOptions};

use crate::settings::Upscale;

const SHARP_PRESCALE: usize = 4;

/// The image to upload and its sampling.
pub fn apply(image: ColorImage, method: Upscale) -> (ColorImage, TextureOptions) {
    match method {
        Upscale::Nearest => (image, TextureOptions::NEAREST),
        Upscale::Bilinear => (image, TextureOptions::LINEAR),
        Upscale::Sharp => (repeat(&image, SHARP_PRESCALE), TextureOptions::LINEAR),
        Upscale::Scale2x => (scale2x(&image), TextureOptions::NEAREST),
        Upscale::Scale3x => (scale3x(&image), TextureOptions::NEAREST),
    }
}

/// The pixel at (x, y), the edge repeated past the borders.
fn at(image: &ColorImage, x: isize, y: isize) -> Color32 {
    let [w, h] = image.size;
    let clamp = |v: isize, n: usize| v.clamp(0, n.cast_signed() - 1).cast_unsigned();
    image.pixels[clamp(y, h) * w + clamp(x, w)]
}

fn signed(v: usize) -> isize {
    v.cast_signed()
}

fn repeat(image: &ColorImage, k: usize) -> ColorImage {
    let [w, h] = image.size;
    let pixels = (0..h * k).flat_map(|y| (0..w * k).map(move |x| image.pixels[(y / k) * w + x / k])).collect();
    ColorImage::new([w * k, h * k], pixels)
}

/// EPX: each pixel P becomes four; a corner takes the color of the two neighbors meeting there when they agree and
/// the other two differ.
#[expect(clippy::many_single_char_names, reason = "the algorithm's own names: P, its neighbors A-D, corners E")]
fn scale2x(image: &ColorImage) -> ColorImage {
    let [w, h] = image.size;
    let mut out = vec![Color32::TRANSPARENT; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (signed(x), signed(y));
            let p = at(image, sx, sy);
            let (a, b, c, d) =
                (at(image, sx, sy - 1), at(image, sx + 1, sy), at(image, sx - 1, sy), at(image, sx, sy + 1));
            let mut e = [p; 4];
            if a != d && c != b {
                if c == a {
                    e[0] = c;
                }
                if a == b {
                    e[1] = b;
                }
                if d == c {
                    e[2] = c;
                }
                if b == d {
                    e[3] = d;
                }
            }
            let row = 2 * w;
            out[2 * y * row + 2 * x] = e[0];
            out[2 * y * row + 2 * x + 1] = e[1];
            out[(2 * y + 1) * row + 2 * x] = e[2];
            out[(2 * y + 1) * row + 2 * x + 1] = e[3];
        }
    }
    ColorImage::new([w * 2, h * 2], out)
}

/// `AdvMAME3x`: the same rule over a 3x3 block, its edge centers taking a corner's color where the neighborhood
/// says the edge runs through them.
#[expect(clippy::many_single_char_names, reason = "the algorithm's own names: the 3x3 neighborhood A-I")]
fn scale3x(image: &ColorImage) -> ColorImage {
    let [w, h] = image.size;
    let mut out = vec![Color32::TRANSPARENT; w * h * 9];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (signed(x), signed(y));
            let n = |dx: isize, dy: isize| at(image, sx + dx, sy + dy);
            let (a, b, c) = (n(-1, -1), n(0, -1), n(1, -1));
            let (d, e, f) = (n(-1, 0), n(0, 0), n(1, 0));
            let (g, hh, i) = (n(-1, 1), n(0, 1), n(1, 1));
            let mut block = [e; 9];
            if b != hh && d != f {
                if d == b {
                    block[0] = d;
                }
                if (d == b && e != c) || (b == f && e != a) {
                    block[1] = b;
                }
                if b == f {
                    block[2] = f;
                }
                if (d == b && e != g) || (d == hh && e != a) {
                    block[3] = d;
                }
                if (b == f && e != i) || (hh == f && e != c) {
                    block[5] = f;
                }
                if d == hh {
                    block[6] = d;
                }
                if (d == hh && e != i) || (hh == f && e != g) {
                    block[7] = hh;
                }
                if hh == f {
                    block[8] = f;
                }
            }
            for (k, &color) in block.iter().enumerate() {
                out[(3 * y + k / 3) * 3 * w + 3 * x + k % 3] = color;
            }
        }
    }
    ColorImage::new([w * 3, h * 3], out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize, on: &[(usize, usize)]) -> ColorImage {
        let mut pixels = vec![Color32::BLACK; w * h];
        for &(x, y) in on {
            pixels[y * w + x] = Color32::WHITE;
        }
        ColorImage::new([w, h], pixels)
    }

    #[test]
    fn scale2x_rounds_a_diagonal() {
        // A diagonal of white: the corners between two white neighbors turn white.
        let out = scale2x(&image(2, 2, &[(1, 0), (0, 1)]));
        assert_eq!(out.size, [4, 4]);
        assert_eq!(out.pixels[3], Color32::WHITE); // top right, the white pixel's own
        assert_eq!(out.pixels[5], Color32::WHITE); // the black (0,0)'s bottom right, between the two whites
    }

    #[test]
    fn flat_images_stay_flat() {
        let flat = image(3, 3, &[]);
        assert!(scale3x(&flat).pixels.iter().all(|&c| c == Color32::BLACK));
        assert!(scale2x(&flat).pixels.iter().all(|&c| c == Color32::BLACK));
    }
}
