use crate::db::models::Position;

/// Decision made by the position manager for a live position
#[derive(Debug, Clone, PartialEq)]
pub enum PositionAction {
    /// Hold – no action needed
    Hold,
    /// Exit for a profit
    TakeProfit { exit_price: f64, pnl: f64 },
    /// Exit at a stop-loss
    StopLoss { exit_price: f64, pnl: f64 },
}

/// Evaluate whether to close an open position based on the current market price.
///
/// # Arguments
/// * `pos`           – The open position.
/// * `current_price` – Current market price of the outcome token (0.0–1.0).
///
/// Returns a `PositionAction` indicating what to do.
pub fn evaluate_position(pos: &Position, current_price: f64) -> PositionAction {
    let shares = pos.size_usd / pos.entry_price; // number of outcome tokens held
    let current_value = shares * current_price;
    let gross_pnl = current_value - pos.size_usd;
    let estimated_cost = pos.size_usd * (pos.estimated_round_trip_cost_bps / 10_000.0);
    let pnl = gross_pnl - estimated_cost;

    if current_price >= pos.take_profit_price {
        PositionAction::TakeProfit {
            exit_price: current_price,
            pnl,
        }
    } else if current_price <= pos.stop_loss_price {
        PositionAction::StopLoss {
            exit_price: current_price,
            pnl,
        }
    } else {
        PositionAction::Hold
    }
}

/// Build stop-loss and take-profit prices for a new YES bet.
///
/// # Arguments
/// * `entry_price`          – Price at which we buy the YES token (0.0–1.0).
/// * `stop_loss_fraction`   – Fraction of position to risk (e.g. 0.5 → stop at 50% loss).
/// * `take_profit_fraction` – Fraction of position gain to target (e.g. 0.3 → exit at 30% gain).
///
/// Returns `(stop_loss_price, take_profit_price)`.
pub fn compute_levels(
    entry_price: f64,
    stop_loss_fraction: f64,
    take_profit_fraction: f64,
) -> (f64, f64) {
    // Stop-loss: if price drops by `stop_loss_fraction` of entry value
    let stop_loss_price = entry_price * (1.0 - stop_loss_fraction);
    // Take-profit: if price rises by `take_profit_fraction` of entry value
    let take_profit_price = (entry_price * (1.0 + take_profit_fraction)).min(0.99);
    (stop_loss_price, take_profit_price)
}

// Win probability estimation has moved to `super::win_probability` with
// sport-specific models (soccer lookup table, NBA/NFL/MLB logistic, etc.).

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::Utc;

    fn make_position(entry: f64, stop: f64, take: f64) -> Position {
        Position {
            id: None,
            market_id: "mkt1".into(),
            asset_id: None,
            outcome: "YES".into(),
            side: "buy".into(),
            size_usd: 10.0,
            entry_price: entry,
            entry_price_source: None,
            entry_model_prob_raw: None,
            entry_model_prob: None,
            entry_ws_age_ms: None,
            estimated_round_trip_cost_bps: 0.0,
            stop_loss_price: stop,
            take_profit_price: take,
            status: "open".into(),
            opened_at: Utc::now(),
            closed_at: None,
            exit_price: None,
            pnl: None,
            dry_run: true,
            ws_used_count: 0,
            rest_fallback_count: 0,
            last_ws_age_ms: None,
            sport: None,
            league: None,
            event_name: None,
        }
    }

    #[test]
    fn test_hold_action() {
        let pos = make_position(0.5, 0.3, 0.7);
        let action = evaluate_position(&pos, 0.55);
        assert_eq!(action, PositionAction::Hold);
    }

    #[test]
    fn test_take_profit_action() {
        let pos = make_position(0.5, 0.3, 0.7);
        let action = evaluate_position(&pos, 0.75);
        match action {
            PositionAction::TakeProfit { pnl, .. } => assert!(pnl > 0.0),
            other => panic!("Expected TakeProfit, got {:?}", other),
        }
    }

    #[test]
    fn test_stop_loss_action() {
        let pos = make_position(0.5, 0.3, 0.7);
        let action = evaluate_position(&pos, 0.25);
        match action {
            PositionAction::StopLoss { pnl, .. } => assert!(pnl < 0.0),
            other => panic!("Expected StopLoss, got {:?}", other),
        }
    }

    #[test]
    fn test_compute_levels() {
        let (stop, take) = compute_levels(0.5, 0.5, 0.3);
        assert_relative_eq!(stop, 0.25, epsilon = 1e-9);
        assert_relative_eq!(take, 0.65, epsilon = 1e-9);
    }

    #[test]
    fn test_compute_levels_take_profit_capped() {
        let (_, take) = compute_levels(0.95, 0.5, 0.3);
        assert!(take <= 0.99);
    }

    #[test]
    fn test_evaluate_position_accounts_for_estimated_cost() {
        let mut pos = make_position(0.5, 0.3, 0.7);
        pos.estimated_round_trip_cost_bps = 100.0; // 1%
        let action = evaluate_position(&pos, 0.75);
        match action {
            PositionAction::TakeProfit { pnl, .. } => {
                assert!(pnl < 5.0); // gross would be 5.0 on $10 at 0.5->0.75
                assert!(pnl > 4.8);
            }
            other => panic!("Expected TakeProfit, got {:?}", other),
        }
    }

    // Win probability tests are now in super::win_probability::tests
    // with comprehensive sport-specific test coverage.
}
