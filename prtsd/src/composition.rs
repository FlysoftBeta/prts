//! Premultiplied BGRA reference composition in device-pixel coordinates.
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Top-left, top-right, bottom-right, bottom-left radii in device pixels.
    pub corners: [f32; 4],
    pub opacity: f32,
    /// Zero for a Views-painted shadow; positive padding for native shadows.
    pub shadow: f32,
}
impl Placement {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width > 0 && self.height > 0 && self.width <= 16384 && self.height <= 16384,
            "Invalid composition extent"
        );
        ensure!(
            self.x.unsigned_abs() <= 100000 && self.y.unsigned_abs() <= 100000,
            "Invalid composition position"
        );
        ensure!(
            self.opacity.is_finite()
                && (0.0..=1.0).contains(&self.opacity)
                && self.shadow.is_finite()
                && (0.0..=64.).contains(&self.shadow)
                && self
                    .corners
                    .iter()
                    .all(|r| r.is_finite() && *r >= 0. && *r <= 8192.),
            "Invalid composition decoration"
        );
        Ok(())
    }
}
fn distance(p: [f32; 2], size: [f32; 2], corners: [f32; 4]) -> f32 {
    let half = [size[0] / 2., size[1] / 2.];
    let index = match (p[0] < half[0], p[1] < half[1]) {
        (true, true) => 0,
        (false, true) => 1,
        (false, false) => 2,
        (true, false) => 3,
    };
    let r = corners[index].min(half[0]).min(half[1]);
    let q = [
        (p[0] - half[0]).abs() - half[0] + r,
        (p[1] - half[1]).abs() - half[1] + r,
    ];
    q[0].max(0.).hypot(q[1].max(0.)) + q[0].max(q[1]).min(0.) - r
}
/// Fresh opaque base every time: closing a popup cannot retain blended pixels.
pub fn blend_cpu(
    base: &[u8],
    width: u32,
    height: u32,
    layers: &[(Placement, u32, u32, &[u8])],
) -> Result<Vec<u8>> {
    ensure!(
        width > 0
            && height > 0
            && u64::from(width) * u64::from(height) <= 64_000_000
            && base.len() == width as usize * height as usize * 4
            && layers.len() <= 8,
        "Invalid composition base/limit"
    );
    let mut out = base.to_vec();
    for (placement, sw, sh, pixels) in layers {
        placement.validate()?;
        ensure!(
            *sw > 0
                && *sh > 0
                && u64::from(*sw) * u64::from(*sh) <= 64_000_000
                && pixels.len() == *sw as usize * *sh as usize * 4,
            "Invalid layer pixels"
        );
        let size = [placement.width as f32, placement.height as f32];
        let pad = placement.shadow.ceil() as i32;
        let left = (placement.x - pad).max(0);
        let top = (placement.y - pad).max(0);
        let right = (placement.x + placement.width as i32 + pad).min(width as i32);
        let bottom = (placement.y + placement.height as i32 + pad).min(height as i32);
        for y in top..bottom {
            for x in left..right {
                let p = [
                    (x - placement.x) as f32 + 0.5,
                    (y - placement.y) as f32 + 0.5,
                ];
                let mask =
                    (0.5 - distance(p, size, placement.corners)).clamp(0., 1.) * placement.opacity;
                let mut src = [0.; 4];
                if p[0] >= 0. && p[1] >= 0. && p[0] < size[0] && p[1] < size[1] {
                    let sx = (p[0] / size[0] * *sw as f32) as u32;
                    let sy = (p[1] / size[1] * *sh as f32) as u32;
                    let at = (sy as usize * *sw as usize + sx as usize) * 4;
                    for c in 0..4 {
                        src[c] = pixels[at + c] as f32 / 255. * mask;
                    }
                }
                if placement.shadow > 0. {
                    let d = distance(
                        [p[0], p[1] - placement.shadow / 4.],
                        size,
                        placement.corners,
                    )
                    .max(0.);
                    let sigma = placement.shadow / 3.;
                    let alpha = 0.35 * (-d * d / (2. * sigma * sigma)).exp() * placement.opacity;
                    src[3] += alpha * (1. - src[3]);
                }
                let at = (y as usize * width as usize + x as usize) * 4;
                for c in 0..3 {
                    out[at + c] = ((src[c] + out[at + c] as f32 / 255. * (1. - src[3])) * 255.)
                        .round()
                        .clamp(0., 255.) as u8;
                }
                out[at + 3] = 255;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_edges_order_and_fresh_base() -> Result<()> {
        let base = [20, 40, 60, 255].repeat(16 * 16);
        let layer = [0, 0, 128, 128].repeat(8 * 8);
        let p = Placement {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
            corners: [4.; 4],
            opacity: 1.,
            shadow: 0.,
        };
        let result = blend_cpu(&base, 16, 16, &[(p, 8, 8, &layer)])?;
        let center = (8 * 16 + 8) * 4;
        assert_eq!(&result[center..center + 4], &[10, 20, 158, 255]);
        assert_eq!(
            &result[(4 * 16 + 4) * 4..(4 * 16 + 4) * 4 + 4],
            &[20, 40, 60, 255]
        );
        assert_eq!(blend_cpu(&base, 16, 16, &[])?, base);
        let opaque = [0, 255, 0, 255].repeat(8 * 8);
        let result = blend_cpu(&base, 16, 16, &[(p, 8, 8, &layer), (p, 8, 8, &opaque)])?;
        assert_eq!(&result[center..center + 4], &[0, 255, 0, 255]);
        Ok(())
    }
}
