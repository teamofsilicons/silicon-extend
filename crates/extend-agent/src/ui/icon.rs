//! The tray icon, drawn in code so the app ships no image files.
//!
//! It is Silicon Extend's mark (web/public/brand/mark.svg) fitted to a 36 px grid: Interface's ring
//! of squares with its north-east square stepped out, reaching a square beyond it. The master uses a
//! 9-part square and a 4-part gutter; here they are 7 px and 3 px, so every square edge falls on a
//! pixel and only the turned centre square is anti-aliased.

/// What the icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    /// Not paired: the ring, with its reach faint (a template image on macOS).
    Unpaired,
    /// Paired and idle: the whole mark (a template image on macOS, so it follows the menu bar).
    Idle,
    /// A Silicon is using the computer: the whole mark in orange-red.
    InUse,
    /// Something needs the Carbon (setup, reconnecting, update): the ring in amber, its reach faint.
    Attention,
}

pub const SIZE: u32 = 36;

/// Unit square and gutter, in pixels.
const U: f32 = 7.0;
const G: f32 = 3.0;
/// The mark is 2G + 4U = 34 px square; one pixel of margin on each side.
const INSET: f32 = 1.0;
/// Top-left corners of the ring's seven squares: W, NW, N, E, SE, S, SW (the ring sits one unit down).
const RING: [(f32, f32); 7] = [
    (0.0, G + U + U),
    (G, G + U),
    (G + U, U),
    (2.0 * G + 2.0 * U, G + U + U),
    (G + 2.0 * U, G + 2.0 * U + U),
    (G + U, 2.0 * G + 2.0 * U + U),
    (G, G + 2.0 * U + U),
];
/// The reach: the north-east square, one gutter out (the row of N, the column of E).
const REACH: (f32, f32) = (2.0 * G + 2.0 * U, U);
/// The device it reaches: one unit further along the diagonal, touching it corner to corner.
const DEVICE: (f32, f32) = (2.0 * G + 3.0 * U, 0.0);
/// The turned square at the ring's centre: Interface's half-diagonal of 9.71 parts, scaled to U.
const CENTRE: (f32, f32) = (G + 1.5 * U, G + 1.5 * U + U);
const CENTRE_HALF_DIAGONAL: f32 = 9.71 / 9.0 * U;
/// How strongly the reach is drawn when the device isn't reached (unpaired, needs attention).
const FAINT: f32 = 0.35;

fn in_square((sx, sy): (f32, f32), x: f32, y: f32) -> bool {
    x >= sx && x < sx + U && y >= sy && y < sy + U
}

/// Which part of the mark a point (in mark coordinates) falls in.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    None,
    Ring,
    Reach,
}

fn part_at(x: f32, y: f32) -> Part {
    let in_centre = (x - CENTRE.0).abs() + (y - CENTRE.1).abs() <= CENTRE_HALF_DIAGONAL;
    if in_centre || RING.iter().any(|&s| in_square(s, x, y)) {
        Part::Ring
    } else if in_square(REACH, x, y) || in_square(DEVICE, x, y) {
        Part::Reach
    } else {
        Part::None
    }
}

/// RGBA pixels for `state`, `SIZE`×`SIZE`.
pub fn rgba(state: IconState) -> Vec<u8> {
    let (r, g, b) = match state {
        IconState::Unpaired | IconState::Idle => (0u8, 0u8, 0u8),
        IconState::InUse => (0xE0, 0x45, 0x2B),
        IconState::Attention => (0xD9, 0x77, 0x06),
    };
    let reach = match state {
        IconState::Idle | IconState::InUse => 1.0,
        IconState::Unpaired | IconState::Attention => FAINT,
    };
    // 4×4 samples per pixel: exact for the squares, smooth for the turned centre.
    const N: usize = 4;
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    for py in 0..SIZE {
        for pxl in 0..SIZE {
            let mut cover = 0.0f32;
            for sy in 0..N {
                for sx in 0..N {
                    let x = pxl as f32 + (sx as f32 + 0.5) / N as f32 - INSET;
                    let y = py as f32 + (sy as f32 + 0.5) / N as f32 - INSET;
                    cover += match part_at(x, y) {
                        Part::Ring => 1.0,
                        Part::Reach => reach,
                        Part::None => 0.0,
                    };
                }
            }
            let alpha = cover / (N * N) as f32;
            let i = ((py * SIZE + pxl) * 4) as usize;
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
            px[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    px
}

/// Template images let macOS draw the icon in the menu bar's own colour.
pub fn is_template(state: IconState) -> bool {
    matches!(state, IconState::Unpaired | IconState::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [IconState; 4] = [
        IconState::Unpaired,
        IconState::Idle,
        IconState::InUse,
        IconState::Attention,
    ];

    fn alpha(px: &[u8], x: u32, y: u32) -> u8 {
        px[((y * SIZE + x) * 4 + 3) as usize]
    }

    #[test]
    fn icons_have_pixels_and_transparent_corners() {
        for s in ALL {
            let px = rgba(s);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert_eq!(px[3], 0, "{s:?} corner should be transparent");
            assert!(
                px.chunks(4).filter(|p| p[3] > 200).count() > 50,
                "{s:?} should draw something"
            );
        }
    }

    #[test]
    fn the_mark_reaches_out_only_when_paired() {
        // Centre of the device square (top right) and of the ring's turned square (lower left).
        let device = ((INSET + DEVICE.0 + U / 2.0) as u32, (INSET + DEVICE.1 + U / 2.0) as u32);
        let centre = ((INSET + CENTRE.0) as u32, (INSET + CENTRE.1) as u32);
        // A pixel in the gutter between N and the reach: always empty.
        let gutter = ((INSET + G + 2.0 * U + 1.0) as u32, (INSET + U + 3.0) as u32);
        for s in ALL {
            let px = rgba(s);
            assert_eq!(alpha(&px, centre.0, centre.1), 255, "{s:?} ring should be solid");
            assert_eq!(alpha(&px, gutter.0, gutter.1), 0, "{s:?} gutter should be empty");
            let reach = alpha(&px, device.0, device.1);
            match s {
                IconState::Idle | IconState::InUse => assert_eq!(reach, 255, "{s:?} reach should be solid"),
                IconState::Unpaired | IconState::Attention => {
                    assert!(reach > 0 && reach < 128, "{s:?} reach should be faint, got {reach}")
                }
            }
        }
    }

    #[test]
    fn squares_land_on_whole_pixels() {
        // Every square edge is on the pixel grid, so a square's pixels are fully covered.
        let px = rgba(IconState::Idle);
        let (x0, y0) = ((INSET + DEVICE.0) as u32, (INSET + DEVICE.1) as u32);
        for y in y0..y0 + U as u32 {
            for x in x0..x0 + U as u32 {
                assert_eq!(alpha(&px, x, y), 255);
            }
        }
        assert_eq!(alpha(&px, x0 - 1, y0 + 1), 0, "left of the device square is empty");
    }
}
