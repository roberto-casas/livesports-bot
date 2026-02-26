use crate::db::models::{LiveGame, Position, ScoreEvent};

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
    let pnl = current_value - pos.size_usd;

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

/// Estimate the true probability of the winning team given a score change.
///
/// This is a simplified model based on goal/point advantage and time remaining.
/// In production you would integrate a proper prediction model or external odds feed.
///
/// # Arguments
/// * `event`    – The score change event.
/// * `game`     – The current game state.
/// * `for_home` – Whether we are estimating the home team's win probability.
pub fn estimate_win_probability(event: &ScoreEvent, game: &LiveGame, for_home: bool) -> f64 {
    let home_score = game.home_score;
    let away_score = game.away_score;
    let diff = (home_score - away_score) as f64;

    // Time fraction remaining (0 = just started, 1 = game over)
    let (_max_minutes, time_fraction) = match event.sport.as_str() {
        "soccer" | "football_eu" => {
            let max = 90.0_f64;
            let elapsed = event.minute.unwrap_or(0) as f64;
            (max, (elapsed / max).min(1.0))
        }
        "american_football" | "nfl" => {
            let max = 60.0_f64;
            let elapsed = event.minute.unwrap_or(0) as f64;
            (max, (elapsed / max).min(1.0))
        }
        "basketball" | "nba" => {
            let max = 48.0_f64;
            let elapsed = event.minute.unwrap_or(0) as f64;
            (max, (elapsed / max).min(1.0))
        }
        "baseball" | "mlb" => {
            // Use innings as proxy; 9 innings
            let max = 9.0_f64;
            let elapsed = event.minute.unwrap_or(0) as f64;
            (max, (elapsed / max).min(1.0))
        }
        _ => (90.0, 0.5),
    };

    // Base probability: sigmoid of goal difference scaled by time remaining
    // As time progresses, the current score matters more
    let base_p = 0.5 + diff * 0.15 * (0.5 + 0.5 * time_fraction);
    let home_win_p = base_p.min(0.97).max(0.03);

    if for_home {
        home_win_p
    } else {
        1.0 - home_win_p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::Utc;
    use crate::db::models::{GameStatus, LiveGame, ScoreEvent};

    fn make_position(entry: f64, stop: f64, take: f64) -> Position {
        Position {
            id: None,
            market_id: "mkt1".into(),
            outcome: "YES".into(),
            side: "buy".into(),
            size_usd: 10.0,
            entry_price: entry,
            stop_loss_price: stop,
            take_profit_price: take,
            status: "open".into(),
            opened_at: Utc::now(),
            closed_at: None,
            exit_price: None,
            pnl: None,
            dry_run: true,
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

    fn make_score_event(sport: &str, home: i32, away: i32, minute: i32) -> ScoreEvent {
        ScoreEvent {
            id: None,
            event_id: "ev1".into(),
            sport: sport.into(),
            league: "test".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            home_score: home,
            away_score: away,
            minute: Some(minute),
            event_type: "goal".into(),
            detected_at: Utc::now(),
        }
    }

    fn make_live_game(sport: &str, home: i32, away: i32) -> LiveGame {
        LiveGame {
            event_id: "ev1".into(),
            sport: sport.into(),
            league: "test".into(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            home_score: home,
            away_score: away,
            minute: Some(75),
            status: GameStatus::InProgress,
        }
    }

    #[test]
    fn test_win_prob_leading_team() {
        let ev = make_score_event("soccer", 2, 0, 75);
        let game = make_live_game("soccer", 2, 0);
        let p_home = estimate_win_probability(&ev, &game, true);
        assert!(p_home > 0.5, "Leading team should have p > 0.5, got {}", p_home);
    }

    #[test]
    fn test_win_prob_trailing_team() {
        let ev = make_score_event("soccer", 0, 2, 75);
        let game = make_live_game("soccer", 0, 2);
        let p_home = estimate_win_probability(&ev, &game, true);
        assert!(p_home < 0.5, "Trailing team should have p < 0.5, got {}", p_home);
    }
}
