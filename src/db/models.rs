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
    pub slug: Option<String>,
    pub end_date: Option<DateTime<Utc>>,
    pub liquidity: Option<f64>,
}

/// An open or closed betting position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: Option<i64>,
    /// Polymarket market condition ID
    pub market_id: String,
    /// Polymarket CLOB token asset ID for this outcome.
    pub asset_id: Option<String>,
    /// Which outcome: "YES" or "NO"
    pub outcome: String,
    /// "buy" or "sell"
    pub side: String,
    /// USD amount committed
    pub size_usd: f64,
    /// Price at which we entered (0.0–1.0)
    pub entry_price: f64,
    /// Source used for entry quote: "ws" | "rest" | "cache".
    pub entry_price_source: Option<String>,
    /// Raw model probability for chosen outcome at entry (before calibration).
    pub entry_model_prob_raw: Option<f64>,
    /// Effective model probability used for decision at entry (after calibration).
    pub entry_model_prob: Option<f64>,
    /// WS quote age at entry, when source is "ws".
    pub entry_ws_age_ms: Option<i64>,
    /// Estimated round-trip execution cost used for net PnL accounting.
    pub estimated_round_trip_cost_bps: f64,
    /// Price at which we trigger stop-loss exit
    pub stop_loss_price: f64,
    /// Price at which we trigger take-profit exit
    pub take_profit_price: f64,
    /// "open" | "closed_profit" | "closed_loss" | "closed_stop_loss" | "closed_feed_health" | "closed_time_exit"
    pub status: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    /// Whether this position was placed in dry-run mode
    pub dry_run: bool,
    /// Number of manage-position sweeps that used WS quote marks.
    pub ws_used_count: i64,
    /// Number of sweeps that required REST fallback marks.
    pub rest_fallback_count: i64,
    /// Last observed WS quote age during mark-to-market sweeps.
    pub last_ws_age_ms: Option<i64>,
    pub sport: Option<String>,
    pub league: Option<String>,
    pub event_name: Option<String>,
    pub market_slug: Option<String>,
}

/// A detected live score change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreEvent {
    pub id: Option<i64>,
    /// External event/game ID from the live-score provider
    pub event_id: String,
    /// Primary provider selected for this event snapshot.
    pub source_provider: Option<String>,
    /// Number of providers that agreed on the selected score snapshot.
    pub provider_consensus_count: Option<i32>,
    pub sport: String,
    pub league: String,
    pub home_team: String,
    pub away_team: String,
    /// Previous home score before this event, if known.
    pub prev_home_score: Option<i32>,
    /// Previous away score before this event, if known.
    pub prev_away_score: Option<i32>,
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
