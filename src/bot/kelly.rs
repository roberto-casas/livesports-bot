/// Kelly Criterion betting size calculator.
///
/// The Kelly formula sizes a bet to maximise the expected logarithm of wealth,
/// which balances risk and reward optimally over the long run.
///
/// Standard formula:
///   f* = (b·p − q) / b
/// where
///   b  = net odds received on the bet (profit per unit staked, i.e. (1/price) − 1)
///   p  = estimated probability of winning
///   q  = 1 − p  (probability of losing)
///
/// We apply a *fractional* Kelly multiplier (0 < multiplier ≤ 1) to reduce
/// variance at the cost of slightly lower expected growth.

/// Calculate the Kelly stake fraction.
///
/// # Arguments
/// * `win_prob`   – Estimated probability that the bet wins (0.0–1.0).
/// * `market_price` – Current market price of the outcome token (0.0–1.0).
///                    This represents the implicit market probability.
/// * `kelly_fraction` – Fractional Kelly multiplier (0.0–1.0).
///
/// # Returns
/// The fraction of bankroll to stake (0.0–1.0).  Returns `0.0` when
/// expected value is non-positive (i.e. no edge).
pub fn kelly_stake(win_prob: f64, market_price: f64, kelly_fraction: f64) -> f64 {
    debug_assert!((0.0..=1.0).contains(&win_prob), "win_prob out of range");
    debug_assert!(
        (0.0..=1.0).contains(&market_price),
        "market_price out of range"
    );
    debug_assert!(
        (0.0..=1.0).contains(&kelly_fraction),
        "kelly_fraction out of range"
    );

    if market_price <= 0.0 || market_price >= 1.0 {
        return 0.0;
    }

    // Net odds per unit staked (e.g. price=0.4 → odds=1.5, meaning 1.5x profit)
    let b = (1.0 / market_price) - 1.0;
    let p = win_prob;
    let q = 1.0 - p;

    let f = (b * p - q) / b;

    if f <= 0.0 {
        return 0.0; // no edge
    }

    // Apply fractional Kelly and clamp to [0, 1]
    (f * kelly_fraction).min(1.0).max(0.0)
}

/// Calculate the edge (expected value) of a bet.
///
/// Edge = win_prob / market_price − 1
///
/// Positive edge means the market is underpricing the true probability.
pub fn edge(win_prob: f64, market_price: f64) -> f64 {
    if market_price <= 0.0 {
        return 0.0;
    }
    win_prob / market_price - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn test_kelly_no_edge() {
        // When market price equals true probability, edge = 0, stake = 0
        let stake = kelly_stake(0.5, 0.5, 1.0);
        assert_relative_eq!(stake, 0.0, epsilon = 1e-9);
    }

    #[test]
    fn test_kelly_positive_edge() {
        // True prob = 0.6, market price = 0.5 → clear positive edge
        let stake = kelly_stake(0.6, 0.5, 1.0);
        // b = 1.0, p = 0.6, q = 0.4 → f = (1*0.6 - 0.4)/1 = 0.2
        assert_relative_eq!(stake, 0.2, epsilon = 1e-9);
    }

    #[test]
    fn test_kelly_fractional_multiplier() {
        // Same as above but with 25% Kelly
        let stake = kelly_stake(0.6, 0.5, 0.25);
        assert_relative_eq!(stake, 0.05, epsilon = 1e-9);
    }

    #[test]
    fn test_kelly_negative_edge() {
        // Market overpriced → no bet
        let stake = kelly_stake(0.3, 0.5, 1.0);
        assert_relative_eq!(stake, 0.0, epsilon = 1e-9);
    }

    #[test]
    fn test_kelly_clamp_high() {
        // Extreme edge → clamp to 1.0
        let stake = kelly_stake(0.99, 0.01, 1.0);
        assert!(stake <= 1.0);
    }

    #[test]
    fn test_kelly_zero_price() {
        let stake = kelly_stake(0.5, 0.0, 1.0);
        assert_relative_eq!(stake, 0.0, epsilon = 1e-9);
    }

    #[test]
    fn test_edge_calculation() {
        // True prob 60%, market 50% → 20% edge
        assert_relative_eq!(edge(0.6, 0.5), 0.2, epsilon = 1e-9);
    }

    #[test]
    fn test_edge_no_edge() {
        assert_relative_eq!(edge(0.5, 0.5), 0.0, epsilon = 1e-9);
    }

    #[test]
    fn test_edge_negative() {
        assert!(edge(0.3, 0.5) < 0.0);
    }
}
