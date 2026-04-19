//! Discrete modulation levels and soft-decision confidence.
//!
//! Chosen centered away from BT.601/709 limited-range clipping zones [0,16]
//! and [235,255] to survive scaler range conversion.

pub const LEVELS_Y: [u8; 4] = [32, 96, 160, 224];
pub const LEVELS_U: [u8; 2] = [80, 176];
pub const LEVELS_V: [u8; 2] = [80, 176];

/// Quantise a luma sample to the closest of 4 levels.
/// Returns (symbol ∈ 0..=3, confidence ∈ [0.0, 1.0]).
pub fn quantise_y(sample: u8) -> (u8, f32) {
    quantise(sample, &LEVELS_Y)
}

pub fn quantise_uv(sample: u8) -> (u8, f32) {
    quantise(sample, &LEVELS_U)
}

fn quantise(sample: u8, levels: &[u8]) -> (u8, f32) {
    debug_assert!(!levels.is_empty());
    let mut best_idx = 0usize;
    let mut best_dist = i32::MAX;
    let mut second_dist = i32::MAX;
    for (i, &lvl) in levels.iter().enumerate() {
        let d = (sample as i32 - lvl as i32).abs();
        if d < best_dist {
            second_dist = best_dist;
            best_dist = d;
            best_idx = i;
        } else if d < second_dist {
            second_dist = d;
        }
    }
    // Confidence = 1 - (best_dist / second_dist); max 1.0, min 0.0.
    let confidence = if second_dist == 0 {
        0.0
    } else {
        1.0 - (best_dist as f32 / second_dist as f32).clamp(0.0, 1.0)
    };
    (best_idx as u8, confidence)
}

/// Paint symbol `sym` on an RGB24 pixel as pure luma (same in R, G, B).
#[inline]
pub fn level_y_as_rgb(sym: u8) -> [u8; 3] {
    let y = LEVELS_Y[sym as usize % LEVELS_Y.len()];
    [y, y, y]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_levels_quantise_with_full_confidence() {
        for (i, &lvl) in LEVELS_Y.iter().enumerate() {
            let (s, c) = quantise_y(lvl);
            assert_eq!(s, i as u8);
            assert!(c > 0.99, "confidence should be ~1.0 at exact level, got {c}");
        }
    }

    #[test]
    fn midway_between_levels_gives_low_confidence() {
        // Midway between 32 and 96 is 64; distance to both is 32.
        let (_, c) = quantise_y(64);
        assert!(c < 0.1, "confidence at midway should be near 0, got {c}");
    }

    #[test]
    fn slight_noise_gives_high_confidence() {
        // 32 + 5 = 37 is much closer to 32 (5) than to 96 (59).
        let (s, c) = quantise_y(37);
        assert_eq!(s, 0);
        assert!(c > 0.9, "confidence should be high for close sample, got {c}");
    }

    #[test]
    fn level_y_as_rgb_paints_luma() {
        assert_eq!(level_y_as_rgb(0), [32, 32, 32]);
        assert_eq!(level_y_as_rgb(3), [224, 224, 224]);
    }
}
