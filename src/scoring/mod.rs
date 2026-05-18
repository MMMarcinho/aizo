use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Configuration for the time-decay mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    /// Score halves every N inactive days.
    pub half_life_days: f64,
    /// Minimum decay floor — effective weight never reaches zero from time alone.
    pub floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            half_life_days: 30.0,
            floor: 0.1,
        }
    }
}

/// All scoring components for a single preference entry.
#[derive(Debug, Clone)]
pub struct ScoringResult {
    pub decay_coefficient: f64, // d(t) ∈ [floor, 1.0]
    pub score_exponent: f64,    // α ∈ [0.0, 1.0]
    pub effective_weight: f64,  // w = s · d(t)^α
}

/// Compute all scoring components for a preference entry.
///
/// Effective weight formula:
///   w = s · d(t)^α
///
/// where:
///   d(t) = φ + (1 − φ) · exp(−λt),   λ = ln2 / t½
///   α    = (10 − s) / 10
///
/// The exponent α ties decay resistance to score: higher-scoring preferences
/// decay more slowly (α → 0 as s → 10), while low-scoring entries decay at
/// full speed (α → 1 as s → 0).
pub fn compute(base_score: f64, last_seen: &str, cfg: &DecayConfig) -> ScoringResult {
    let d = decay_coefficient(last_seen, cfg);
    let alpha = score_exponent(base_score);
    ScoringResult {
        decay_coefficient: d,
        score_exponent: alpha,
        effective_weight: base_score * d.powf(alpha),
    }
}

/// d(t) = φ + (1 − φ) · exp(−λt),  λ = ln2 / t½
pub fn decay_coefficient(last_seen: &str, cfg: &DecayConfig) -> f64 {
    let Ok(last) = DateTime::parse_from_rfc3339(last_seen) else {
        eprintln!("aizo: warning: unparseable last_seen {:?}, treating as now", last_seen);
        return 1.0;
    };
    let days = Utc::now()
        .signed_duration_since(last)
        .num_seconds()
        .max(0) as f64
        / 86_400.0;
    let lambda = std::f64::consts::LN_2 / cfg.half_life_days;
    let raw = (-lambda * days).exp();
    cfg.floor + (1.0 - cfg.floor) * raw
}

/// α = (10 − s) / 10
///
/// High score (s = 10) → α = 0.0 → decay has no effect (d^0 = 1).
/// Low score  (s = 0)  → α = 1.0 → full decay applies.
pub fn score_exponent(base_score: f64) -> f64 {
    (10.0 - base_score.clamp(0.0, 10.0)) / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DecayConfig {
        DecayConfig { half_life_days: 30.0, floor: 0.1 }
    }

    #[test]
    fn score_exponent_boundaries() {
        assert_eq!(score_exponent(10.0), 0.0);
        assert_eq!(score_exponent(0.0), 1.0);
        assert!((score_exponent(5.0) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn decay_at_t0() {
        let now = chrono::Utc::now().to_rfc3339();
        let d = decay_coefficient(&now, &cfg());
        assert!((d - 1.0).abs() < 0.001, "d(0) should be ~1.0, got {d}");
    }

    #[test]
    fn decay_approaches_floor() {
        let ancient = "2000-01-01T00:00:00+00:00";
        let d = decay_coefficient(ancient, &cfg());
        assert!((d - cfg().floor).abs() < 0.001, "d(∞) should be ~floor={}, got {d}", cfg().floor);
    }

    #[test]
    fn score10_never_decays() {
        let now = chrono::Utc::now().to_rfc3339();
        let ancient = "2000-01-01T00:00:00+00:00";
        let r_now = compute(10.0, &now, &cfg());
        let r_old = compute(10.0, ancient, &cfg());
        assert!((r_now.effective_weight - 10.0).abs() < f64::EPSILON);
        assert!((r_old.effective_weight - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn invalid_timestamp_returns_1() {
        let d = decay_coefficient("not-a-date", &cfg());
        assert_eq!(d, 1.0);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn cfg() -> DecayConfig {
        DecayConfig { half_life_days: 30.0, floor: 0.1 }
    }

    fn past(days: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }

    proptest! {
        // Effective weight is always in [0, score]
        #[test]
        fn weight_bounded_by_score(s in 0.0f64..=10.0, t in 0i64..3650) {
            let r = compute(s, &past(t), &cfg());
            prop_assert!(r.effective_weight >= 0.0 - f64::EPSILON);
            prop_assert!(r.effective_weight <= s + f64::EPSILON);
        }

        // Decay coefficient is always in [floor, 1.0]
        #[test]
        fn decay_in_floor_to_one(t in 0i64..3650) {
            let d = decay_coefficient(&past(t), &cfg());
            prop_assert!(d >= cfg().floor - f64::EPSILON);
            prop_assert!(d <= 1.0 + f64::EPSILON);
        }

        // Older entries always have lower or equal weight (monotone decay)
        #[test]
        fn weight_monotone_over_time(s in 0.1f64..9.9, t1 in 0i64..1000, extra in 0i64..1000) {
            let t2 = t1 + extra;
            let r1 = compute(s, &past(t1), &cfg());
            let r2 = compute(s, &past(t2), &cfg());
            prop_assert!(r1.effective_weight >= r2.effective_weight - f64::EPSILON,
                "t1={t1} w={:.4} should be >= t2={t2} w={:.4}", r1.effective_weight, r2.effective_weight);
        }

        // Score 10 is always exactly 10 regardless of time
        #[test]
        fn score10_never_decays_prop(t in 0i64..3650) {
            let r = compute(10.0, &past(t), &cfg());
            prop_assert!((r.effective_weight - 10.0).abs() < f64::EPSILON);
        }

        // Score 0 is always exactly 0 regardless of time
        #[test]
        fn score0_always_zero_prop(t in 0i64..3650) {
            let r = compute(0.0, &past(t), &cfg());
            prop_assert_eq!(r.effective_weight, 0.0);
        }

        // Score exponent is always in [0, 1]
        #[test]
        fn exponent_in_range(s in 0.0f64..=10.0) {
            let alpha = score_exponent(s);
            prop_assert!(alpha >= 0.0 - f64::EPSILON);
            prop_assert!(alpha <= 1.0 + f64::EPSILON);
        }

        // Any valid half-life produces d(t_half) ≈ floor + (1-floor)*0.5
        #[test]
        fn half_life_property(hl in 1.0f64..365.0) {
            let c = DecayConfig { half_life_days: hl, floor: 0.1 };
            let ms = (hl * 86_400_000.0) as i64;
            let at_half = (chrono::Utc::now() - chrono::Duration::milliseconds(ms)).to_rfc3339();
            let d = decay_coefficient(&at_half, &c);
            let expected = c.floor + (1.0 - c.floor) * 0.5;
            prop_assert!((d - expected).abs() < 0.01,
                "hl={hl} d={d:.4} expected≈{expected:.4}");
        }

        // Higher floor means higher minimum effective weight for any non-zero score
        #[test]
        fn higher_floor_higher_min_weight(s in 0.1f64..=10.0, floor in 0.0f64..0.9) {
            let c_low  = DecayConfig { half_life_days: 1.0, floor };
            let c_high = DecayConfig { half_life_days: 1.0, floor: floor + 0.05 };
            let ancient = "2000-01-01T00:00:00+00:00";
            let w_low  = compute(s, ancient, &c_low).effective_weight;
            let w_high = compute(s, ancient, &c_high).effective_weight;
            prop_assert!(w_high >= w_low - f64::EPSILON);
        }
    }
}
