//! Raster backdrop for the auth page, ported from the Go client's
//! `cmd/voxhold/background.go`. The reference caches this image and keeps the
//! form as native widgets; we do the same and regenerate only when the client
//! area or theme changes.

use image::RgbaImage;

struct Rgb([f64; 3]);

impl Rgb {
    fn blend(&mut self, color: [f64; 3], alpha: f64) {
        for i in 0..3 {
            self.0[i] = self.0[i] * (1.0 - alpha) + color[i] * alpha;
        }
    }
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn clamp255(v: f64) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

pub fn page_background(w: u32, h: u32, light: bool) -> RgbaImage {
    let (base, end) = if light {
        ([243.0, 242.0, 237.0], [242.0, 241.0, 236.0])
    } else {
        ([16.0, 16.0, 15.0], [30.0, 30.0, 27.0])
    };
    let (lime, violet) = if light {
        ([181.0, 207.0, 22.0], [124.0, 100.0, 223.0])
    } else {
        ([231.0, 255.0, 91.0], [169.0, 147.0, 255.0])
    };
    let grid = if light {
        [31.0, 32.0, 27.0]
    } else {
        [255.0, 255.0, 255.0]
    };
    let (grid_alpha, orbit_opacity) = if light { (0.16, 0.45) } else { (0.13, 0.64) };

    let wf = w as f64;
    let hf = h as f64;
    let scale = (wf / 1440.0).max(hf / 900.0);
    let off_x = (wf - 1440.0 * scale) / 2.0;
    let off_y = (hf - 900.0 * scale) / 2.0;
    let (cos, sin) = (28.0f64.to_radians().cos(), 28.0f64.to_radians().sin());
    let orbits = [
        (435.0, 217.0),
        (490.0, 270.0),
        (555.0, 331.0),
        (625.0, 398.0),
    ];
    let dots = [
        (-490.0, 0.0, 4.0, 0.0),
        (0.0, -331.0, 4.0, 1.0),
        (0.0, 270.0, 3.0, 1.0),
    ];

    let mut img = RgbaImage::new(w.max(1), h.max(1));
    for y in 0..h {
        for x in 0..w {
            let xf = x as f64;
            let yf = y as f64;
            let gradient = clamp01((0.55 * xf / wf + 0.45 * yf / hf - 0.2) / 0.8);
            let mut rgb = Rgb([
                base[0] * (1.0 - gradient) + end[0] * gradient,
                base[1] * (1.0 - gradient) + end[1] * gradient,
                base[2] * (1.0 - gradient) + end[2] * gradient,
            ]);
            let radial = |cx: f64, cy: f64, rx: f64, ry: f64, stop: f64| -> f64 {
                clamp01(
                    1.0 - (((xf - cx * wf) / (rx * wf)).powi(2)
                        + ((yf - cy * hf) / (ry * hf)).powi(2))
                    .sqrt()
                        / stop,
                )
            };
            rgb.blend(lime, 0.10 * radial(0.04, 0.80, 0.96, 0.80, 0.5657));
            rgb.blend(lime, 0.19 * radial(0.68, 1.09, 0.68, 1.09, 0.6505));
            rgb.blend(violet, 0.30 * radial(0.83, 0.14, 0.83, 0.86, 0.7071));
            if (x % 24 == 0 || x % 24 == 1) && (y % 24 == 0 || y % 24 == 1) {
                let mask = clamp01((0.62 * xf / wf + 0.38 * yf / hf - 0.22) / 0.53);
                rgb.blend(grid, grid_alpha * mask * 0.55);
            }
            let vx = (xf - off_x) / scale - 920.0;
            let vy = (yf - off_y) / scale - 468.0;
            let rx = cos * vx - sin * vy;
            let ry = sin * vx + cos * vy;
            for (ox, oy) in orbits {
                let d = ((rx / ox).powi(2) + (ry / oy).powi(2)).sqrt();
                let distance = (d - 1.0).abs() * ox.min(oy) * scale;
                if distance < 1.0 {
                    let mix = clamp01((rx / ox - ry / oy + 2.0) / 4.0);
                    let color = [
                        lime[0] * (1.0 - mix) + violet[0] * mix,
                        lime[1] * (1.0 - mix) + violet[1] * mix,
                        lime[2] * (1.0 - mix) + violet[2] * mix,
                    ];
                    rgb.blend(
                        color,
                        0.35 * orbit_opacity
                            * (0.50 * (1.0 - mix) + 0.68 * mix)
                            * distance.min(1.0),
                    );
                }
            }
            for (dx, dy, radius, accent) in dots {
                if ((rx - dx).powi(2) + (ry - dy).powi(2)).sqrt() <= radius {
                    let color = if accent == 1.0 { violet } else { lime };
                    rgb.blend(color, orbit_opacity * 0.8);
                }
            }
            img.put_pixel(
                x,
                y,
                image::Rgba([
                    clamp255(rgb.0[0]),
                    clamp255(rgb.0[1]),
                    clamp255(rgb.0[2]),
                    255,
                ]),
            );
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_is_not_flat() {
        let image = page_background(64, 48, false);
        assert_ne!(image.get_pixel(0, 0), image.get_pixel(40, 0));
    }

    #[test]
    fn light_and_dark_backdrops_differ() {
        let dark = page_background(32, 32, false);
        let light = page_background(32, 32, true);
        assert_ne!(dark.get_pixel(16, 16), light.get_pixel(16, 16));
    }
}
