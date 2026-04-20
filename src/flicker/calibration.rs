//! Per-frame adaptive Y/U/V level calibration from pilot observations.
//!
//! Each pilot cell carries a KNOWN Mode C symbol (painted by the encoder
//! from `pilot_value_c`). The decoder reads raw YUV means from those cells
//! after VK transcode, groups them by expected y_sym/u_sym/v_sym, and takes
//! the median to produce actual level positions for this specific frame.
//!
//! Motivation: VK DCT quantization drifts chroma by ±20-30 LSB per
//! macroblock. Fixed thresholds (midpoint of static `LEVELS_U = [80, 176]`)
//! then catch bit-flips on every drifted cell. Recomputing thresholds from
//! pilots that endured the same drift as payload cells recovers symbol
//! accuracy.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalibratedLevels {
    pub y: [u8; 4],
    pub u: [u8; 2],
    pub v: [u8; 2],
}

impl CalibratedLevels {
    /// Fallback used when fewer than `MIN_OBSERVATIONS` samples exist for a
    /// given level — decoder reverts to static `LEVELS_*` from levels.rs.
    pub fn fallback() -> Self {
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        Self { y: LEVELS_Y, u: LEVELS_U, v: LEVELS_V }
    }
}

/// Need at least this many pilot observations per level slot before we
/// trust the calibrated value. Below this, that slot falls back to static.
pub const MIN_OBSERVATIONS: usize = 3;

/// Accumulate raw YUV samples grouped by expected symbol, then compute
/// per-level medians. `samples[sym]` = Vec of (y, u, v) means observed
/// for pilots whose expected Mode C symbol was `sym`.
pub fn calibrate(samples: &[Vec<(u8, u8, u8)>; 16]) -> CalibratedLevels {
    let fallback = CalibratedLevels::fallback();
    let mut out = fallback;

    for y_sym in 0..4 {
        let mut ys: Vec<u8> = (0..16)
            .filter(|s| (s >> 2) & 0b11 == y_sym)
            .flat_map(|s| samples[s].iter().map(|&(y, _, _)| y))
            .collect();
        if ys.len() >= MIN_OBSERVATIONS {
            ys.sort_unstable();
            out.y[y_sym] = ys[ys.len() / 2];
        }
    }

    for u_sym in 0..2 {
        let mut us: Vec<u8> = (0..16)
            .filter(|s| (s >> 1) & 0b1 == u_sym)
            .flat_map(|s| samples[s].iter().map(|&(_, u, _)| u))
            .collect();
        if us.len() >= MIN_OBSERVATIONS {
            us.sort_unstable();
            out.u[u_sym] = us[us.len() / 2];
        }
    }

    for v_sym in 0..2 {
        let mut vs: Vec<u8> = (0..16)
            .filter(|s| s & 0b1 == v_sym)
            .flat_map(|s| samples[s].iter().map(|&(_, _, v)| v))
            .collect();
        if vs.len() >= MIN_OBSERVATIONS {
            vs.sort_unstable();
            out.v[v_sym] = vs[vs.len() / 2];
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrate_returns_fallback_when_no_samples() {
        let empty: [Vec<(u8, u8, u8)>; 16] = Default::default();
        let cal = calibrate(&empty);
        assert_eq!(cal, CalibratedLevels::fallback());
    }

    #[test]
    fn calibrate_tracks_uniform_chroma_drift() {
        // Simulate: every cell's U was shifted +30 LSB by VK. Expected
        // calibration: U[0] and U[1] both shift by ~30.
        let mut samples: [Vec<(u8, u8, u8)>; 16] = Default::default();
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        for sym in 0..16u8 {
            let y_sym = (sym >> 2) & 0b11;
            let u_sym = (sym >> 1) & 0b1;
            let v_sym = sym & 0b1;
            let y = LEVELS_Y[y_sym as usize];
            let u = LEVELS_U[u_sym as usize].saturating_add(30);
            let v = LEVELS_V[v_sym as usize];
            for _ in 0..7 { samples[sym as usize].push((y, u, v)); }
        }
        let cal = calibrate(&samples);
        assert!((cal.u[0] as i32 - (LEVELS_U[0] as i32 + 30)).abs() <= 2,
            "calibrated U[0] should track +30 drift, got {}", cal.u[0]);
        assert!((cal.u[1] as i32 - (LEVELS_U[1] as i32 + 30)).abs() <= 2,
            "calibrated U[1] should track +30 drift, got {}", cal.u[1]);
        assert_eq!(cal.y, LEVELS_Y, "Y undrifted stays at static levels");
        assert_eq!(cal.v, LEVELS_V, "V undrifted stays at static levels");
    }

    #[test]
    fn calibrate_falls_back_when_only_one_symbol_sparse() {
        // symbol 0 has 2 samples (below MIN_OBSERVATIONS when grouped by y_sym=0)
        // — so Y[0] should fall back. But Y[1..3] with many samples should
        // not be touched (remain static since no drift injected).
        let mut samples: [Vec<(u8, u8, u8)>; 16] = Default::default();
        use crate::flicker::levels::{LEVELS_Y, LEVELS_U, LEVELS_V};
        samples[0] = vec![(LEVELS_Y[0], LEVELS_U[0], LEVELS_V[0]); 2];
        for sym in 4..16u8 {
            let y_sym = (sym >> 2) & 0b11;
            let u_sym = (sym >> 1) & 0b1;
            let v_sym = sym & 0b1;
            for _ in 0..7 {
                samples[sym as usize].push((
                    LEVELS_Y[y_sym as usize],
                    LEVELS_U[u_sym as usize],
                    LEVELS_V[v_sym as usize],
                ));
            }
        }
        let cal = calibrate(&samples);
        assert_eq!(cal.y[0], LEVELS_Y[0], "sparse Y[0] falls back");
    }
}
