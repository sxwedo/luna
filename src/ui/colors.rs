/// Design system color constants following modern frontend aesthetics (Zinc/Slate base, single saturated accents).
pub mod palette {
    pub const EMERALD: (u8, u8, u8) = (52, 211, 153); // Safe / healthy (>60%)
    pub const AMBER: (u8, u8, u8) = (251, 191, 36); // Moderate (25% - 60%)
    pub const ROSE: (u8, u8, u8) = (244, 63, 94); // Danger / near empty (<25%)
    pub const TRACK: (u8, u8, u8) = (51, 65, 85); // Dark rail background
    pub const BORDER: (u8, u8, u8) = (71, 85, 105); // Card outline
    pub const MUTED: (u8, u8, u8) = (148, 163, 184); // Secondary text / resets in
    pub const WHITE: (u8, u8, u8) = (248, 250, 252); // Primary headings
    pub const ACCENT_BLUE: (u8, u8, u8) = (56, 189, 248); // Antigravity blue
    pub const ACCENT_TEAL: (u8, u8, u8) = (45, 212, 191); // Zhipu teal
}

/// Resting chrome of a consuming title — same family as the border, so the
/// highlight can leave the letters and keep travelling without a color jump.
pub const ORBIT_TITLE_REST: (u8, u8, u8) = (80, 95, 120);
/// Peak of the travelling highlight (muse sky-cyan / white).
pub const ORBIT_BRIGHT: (u8, u8, u8) = (210, 245, 255);
/// First title column on a rounded card (`╭─` then text).
pub const ORBIT_TITLE_ORIGIN: usize = 2;

const ORBIT_RADIUS: f64 = 8.0;
/// Cells per 100ms tick (~2.8 cols/frame → a typical card loops in ~6s).
const ORBIT_SPEED: f64 = 2.8;

/// Colorize text with truecolor RGB.
pub fn rgb(text: &str, (r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, text)
}

/// Colorize background with truecolor RGB.
pub fn bg_rgb(text: &str, (r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m{}\x1b[0m", r, g, b, text)
}

/// Get semantic quota color based on remaining percentage.
pub fn quota_color(percent: f64) -> (u8, u8, u8) {
    if percent >= 60.0 {
        palette::EMERALD
    } else if percent >= 25.0 {
        palette::AMBER
    } else {
        palette::ROSE
    }
}

/// Perimeter length of a rounded rectangle in terminal cells.
pub fn perimeter_len(width: usize, height: usize) -> usize {
    if width < 2 || height < 2 {
        return 0;
    }
    2 * (width + height - 2)
}

/// Clockwise index along the card chrome, starting at the top-left corner.
///
/// ```text
/// 0 → top → right → bottom (right-to-left) → left (bottom-to-top) → 0
/// ```
pub fn perimeter_index(x: usize, y: usize, width: usize, height: usize) -> usize {
    if width < 2 || height < 2 {
        return 0;
    }
    let w = width;
    let h = height;
    if y == 0 {
        x.min(w - 1)
    } else if x == w - 1 && y < h - 1 {
        w + (y - 1)
    } else if y == h - 1 {
        w + (h - 2) + (w - 1 - x)
    } else {
        w + (h - 2) + w + (h - 2 - y)
    }
}

/// Quadratic-falloff intensity in `[0, 1]` for a cell on a closed orbit.
///
/// `origin` is the path index where the highlight should sit at frame 0
/// (title start), so the first thing you see is the letters lighting up,
/// then the beam continues onto the remaining top edge and around.
pub fn orbit_intensity(path_index: usize, path_len: usize, frame: usize, origin: usize) -> f64 {
    if path_len == 0 {
        return 0.0;
    }
    let origin = (origin % path_len) as f64;
    let center = (origin + frame as f64 * ORBIT_SPEED) % path_len as f64;
    let i = path_index as f64;
    let d = (i - center).abs();
    let dist = d.min(path_len as f64 - d);
    let t = (1.0 - dist / ORBIT_RADIUS).max(0.0);
    t * t
}

/// Mix a rest color toward the shared sky-cyan peak.
pub fn orbit_color(base: (u8, u8, u8), intensity: f64) -> (u8, u8, u8) {
    let t = intensity.clamp(0.0, 1.0);
    let r = base.0 as f64 + (ORBIT_BRIGHT.0 as f64 - base.0 as f64) * t;
    let g = base.1 as f64 + (ORBIT_BRIGHT.1 as f64 - base.1 as f64) * t;
    let b = base.2 as f64 + (ORBIT_BRIGHT.2 as f64 - base.2 as f64) * t;
    (r as u8, g as u8, b as u8)
}

/// Truecolor glyph with optional bold at the highlight core.
pub fn orbit_paint_char(ch: char, color: (u8, u8, u8), intensity: f64) -> String {
    let (r, g, b) = color;
    if intensity > 0.4 {
        format!("\x1b[38;2;{};{};{}m\x1b[1m{}\x1b[0m", r, g, b, ch)
    } else {
        format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, ch)
    }
}

/// Convenience: intensity + mix + paint for one chrome cell.
pub fn orbit_cell(
    ch: char,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    frame: usize,
    base: (u8, u8, u8),
) -> String {
    let path_len = perimeter_len(width, height);
    let idx = perimeter_index(x, y, width, height);
    let t = orbit_intensity(idx, path_len, frame, ORBIT_TITLE_ORIGIN);
    orbit_paint_char(ch, orbit_color(base, t), t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn perimeter_covers_every_border_cell_once() {
        let w = 10;
        let h = 5;
        let len = perimeter_len(w, h);
        assert_eq!(len, 26);

        let mut seen = HashSet::new();
        for x in 0..w {
            assert!(seen.insert(perimeter_index(x, 0, w, h)));
            assert!(seen.insert(perimeter_index(x, h - 1, w, h)));
        }
        for y in 1..h - 1 {
            assert!(seen.insert(perimeter_index(0, y, w, h)));
            assert!(seen.insert(perimeter_index(w - 1, y, w, h)));
        }
        assert_eq!(seen.len(), len);
    }

    #[test]
    fn orbit_starts_on_the_title_and_wraps() {
        let len = perimeter_len(20, 8);
        // Frame 0: peak sits on the first title column.
        let peak = orbit_intensity(ORBIT_TITLE_ORIGIN, len, 0, ORBIT_TITLE_ORIGIN);
        assert!((peak - 1.0).abs() < 1e-9);

        let far = orbit_intensity(ORBIT_TITLE_ORIGIN + 16, len, 0, ORBIT_TITLE_ORIGIN);
        assert!(far < 0.05);

        // A later frame still conserves a single peak somewhere on the loop.
        let mut max: f64 = 0.0;
        for i in 0..len {
            max = max.max(orbit_intensity(i, len, 40, ORBIT_TITLE_ORIGIN));
        }
        assert!(max > 0.9);
    }
}
