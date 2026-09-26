//! Pixel helpers for screenshots: GDI gives bottom-up or top-down BGRA; PNG wants RGBA.

/// Converts top-down BGRA rows into RGBA with full alpha.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.as_chunks::<4>().0 {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    out
}

/// The size an image scaled by `scale` ends up, never smaller than 1×1.
pub fn scaled_size(width: u32, height: u32, scale: f64) -> (u32, u32) {
    let w = ((width as f64) * scale).round().max(1.0) as u32;
    let h = ((height as f64) * scale).round().max(1.0) as u32;
    (w, h)
}

/// Resizes RGBA pixels by averaging the source pixels each destination pixel covers (a box
/// filter), so text stays readable at small scales.
pub fn scale_rgba(rgba: &[u8], width: u32, height: u32, scale: f64) -> (Vec<u8>, u32, u32) {
    if scale >= 1.0 || width == 0 || height == 0 {
        return (rgba.to_vec(), width, height);
    }
    let (nw, nh) = scaled_size(width, height, scale);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for dy in 0..nh {
        let sy0 = (dy as u64 * height as u64 / nh as u64) as u32;
        let sy1 = (((dy + 1) as u64 * height as u64).div_ceil(nh as u64) as u32).clamp(sy0 + 1, height);
        for dx in 0..nw {
            let sx0 = (dx as u64 * width as u64 / nw as u64) as u32;
            let sx1 = (((dx + 1) as u64 * width as u64).div_ceil(nw as u64) as u32).clamp(sx0 + 1, width);
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = ((sy * width + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u64;
                    }
                    n += 1;
                }
            }
            let o = ((dy * nw + dx) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / n.max(1)) as u8;
            }
        }
    }
    (out, nw, nh)
}

/// Crops RGBA pixels to a rectangle (clamped to the image).
pub fn crop_rgba(rgba: &[u8], width: u32, height: u32, x: i32, y: i32, w: i32, h: i32) -> (Vec<u8>, u32, u32) {
    let x0 = x.clamp(0, width as i32) as u32;
    let y0 = y.clamp(0, height as i32) as u32;
    let x1 = (x + w).clamp(0, width as i32) as u32;
    let y1 = (y + h).clamp(0, height as i32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return (rgba.to_vec(), width, height);
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y0..y1 {
        let start = ((row * width + x0) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    (out, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bgra() {
        assert_eq!(
            bgra_to_rgba(&[1, 2, 3, 0, 10, 20, 30, 7]),
            vec![3, 2, 1, 255, 30, 20, 10, 255]
        );
    }

    #[test]
    fn scales_down_by_averaging() {
        // 2×2 → 1×1 averages all four.
        let px = [0, 0, 0, 255, 100, 100, 100, 255, 200, 200, 200, 255, 100, 100, 100, 255];
        let (out, w, h) = scale_rgba(&px, 2, 2, 0.5);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![100, 100, 100, 255]);
        let (same, w, h) = scale_rgba(&px, 2, 2, 1.0);
        assert_eq!((same.len(), w, h), (16, 2, 2));
        assert_eq!(scaled_size(3456, 2234, 0.3), (1037, 670));
        assert_eq!(scaled_size(10, 10, 0.01), (1, 1));
        let big = vec![7u8; 1000 * 10 * 4];
        let (out, w, h) = scale_rgba(&big, 1000, 10, 0.33);
        assert_eq!((w, h), (330, 3));
        assert!(out.iter().all(|&b| b == 7));
    }

    #[test]
    fn crops() {
        let mut px = vec![];
        for i in 0..16u8 {
            px.extend_from_slice(&[i, i, i, 255]);
        }
        let (out, w, h) = crop_rgba(&px, 4, 4, 1, 1, 2, 2);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out.chunks(4).map(|c| c[0]).collect::<Vec<_>>(), vec![5, 6, 9, 10]);
        let (_, w, h) = crop_rgba(&px, 4, 4, -5, -5, 100, 100);
        assert_eq!((w, h), (4, 4));
    }
}
