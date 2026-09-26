//! The tray icon, drawn in code so the app ships no image files.

/// What the icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    /// Not paired: an outline.
    Unpaired,
    /// Paired and idle: a solid mark (a template image on macOS, so it follows the menu bar).
    Idle,
    /// A Silicon is using the computer: an orange mark with a dot.
    InUse,
    /// Something needs the Carbon (setup, reconnecting, update): an amber outline.
    Attention,
}

pub const SIZE: u32 = 36;

/// RGBA pixels for `state`, `SIZE`×`SIZE`.
pub fn rgba(state: IconState) -> Vec<u8> {
    let n = SIZE as f32;
    let c = n / 2.0;
    let outer = n * 0.42;
    let ring = n * 0.11;
    let (r, g, b) = match state {
        IconState::Unpaired | IconState::Idle => (0u8, 0u8, 0u8),
        IconState::InUse => (0xF9, 0x73, 0x16),
        IconState::Attention => (0xD9, 0x77, 0x06),
    };
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - c;
            let dy = y as f32 + 0.5 - c;
            let d = (dx * dx + dy * dy).sqrt();
            // Anti-aliased coverage of a disc edge at radius `rad`.
            let cover = |rad: f32| (rad - d + 0.5).clamp(0.0, 1.0);
            let alpha = match state {
                IconState::Idle => {
                    // A solid disc with a small extend-like gap across the middle.
                    let disc = cover(outer);
                    let gap = if dy.abs() < n * 0.05 && dx.abs() < outer * 0.62 { 0.0 } else { 1.0 };
                    disc * gap
                }
                IconState::InUse => {
                    let disc = cover(outer);
                    let hole = cover(outer * 0.38);
                    (disc - hole).max(0.0)
                }
                IconState::Unpaired | IconState::Attention => {
                    let o = cover(outer);
                    let i = cover(outer - ring);
                    (o - i).max(0.0)
                }
            };
            let i = ((y * SIZE + x) * 4) as usize;
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

    #[test]
    fn icons_have_pixels_and_transparent_corners() {
        for s in [IconState::Unpaired, IconState::Idle, IconState::InUse, IconState::Attention] {
            let px = rgba(s);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert_eq!(px[3], 0, "{s:?} corner should be transparent");
            assert!(px.chunks(4).filter(|p| p[3] > 200).count() > 50, "{s:?} should draw something");
        }
    }
}
