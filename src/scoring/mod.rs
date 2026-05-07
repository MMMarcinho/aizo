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
