use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A Polymarket prediction market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    /// Polymarket market condition ID
    pub id: String,
    pub question: String,
    pub sport: Option<String>,
    pub league: Option<String>,
    pub event_name: Option<String>,
    /// Current YES token price (0.0–1.0)
    pub yes_price: Option<f64>,
    /// Current NO token price (0.0–1.0)
    pub no_price: Option<f64>,
    /// Total traded volume in USD
    pub volume: Option<f64>,
    /// "active" | "closed" | "resolved"
    pub status: String,
    pub fetched_at: DateTime<Utc>,
}

/// An open or closed betting position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: Option<i64>,
    /// Polymarket market condition ID
    pub market_id: String,
    /// Which outcome: "YES" or "NO"
    pub outcome: String,
    /// "buy" or "sell"
    pub side: String,
    /// USD amount committed
    pub size_usd: f64,
    /// Price at which we entered (0.0–1.0)
    pub entry_price: f64,
    /// Price at which we trigger stop-loss exit
    pub stop_loss_price: f64,
    /// Price at which we trigger take-profit exit
    pub take_profit_price: f64,
    /// "open" | "closed_profit" | "closed_loss" | "closed_stop_loss"
    pub status: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    /// Whether this position was placed in dry-run mode
    pub dry_run: bool,
    pub sport: Option<String>,
    pub league: Option<String>,
    pub event_name: Option<String>,
}

/// A detected live score change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreEvent {
    pub id: Option<i64>,
    /// External event/game ID from the live-score provider
    pub event_id: String,
    pub sport: String,
    pub league: String,
    pub home_team: String,
    pub away_team: String,
    pub home_score: i32,
    pub away_score: i32,
    /// Minute/period when the change occurred
    pub minute: Option<i32>,
    /// e.g. "goal", "touchdown", "basket", "penalty_goal", "red_card"
    pub event_type: String,
    pub detected_at: DateTime<Utc>,
}

/// Raw live game state as fetched from the score provider
#[derive(Debug, Clone, PartialEq)]
pub struct LiveGame {
    pub event_id: String,
    pub sport: String,
    pub league: String,
    pub home_team: String,
    pub away_team: String,
    pub home_score: i32,
    pub away_score: i32,
    pub minute: Option<i32>,
    pub status: GameStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameStatus {
    NotStarted,
    InProgress,
    HalfTime,
    Finished,
}
