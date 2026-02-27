use clap::Parser;

/// Polymarket live-sports betting bot
#[derive(Parser, Debug, Clone)]
#[command(name = "livesports-bot", version, about)]
pub struct Config {
    /// Run in dry-run mode (no real trades placed)
    #[arg(long, env = "DRY_RUN", default_value = "false")]
    pub dry_run: bool,

    /// Initial simulated balance for dry-run mode (USD)
    #[arg(long, env = "INITIAL_BALANCE", default_value = "100.0")]
    pub initial_balance: f64,

    /// Dashboard listen address
    #[arg(long, env = "DASHBOARD_ADDR", default_value = "0.0.0.0:8080")]
    pub dashboard_addr: String,

    /// SQLite database path
    #[arg(long, env = "DATABASE_PATH", default_value = "livesports.db")]
    pub database_path: String,

    /// Polymarket API base URL
    #[arg(
        long,
        env = "POLYMARKET_API_URL",
        default_value = "https://gamma-api.polymarket.com"
    )]
    pub polymarket_api_url: String,

    /// Polymarket CLOB (Central Limit Order Book) URL
    #[arg(
        long,
        env = "POLYMARKET_CLOB_URL",
        default_value = "https://clob.polymarket.com"
    )]
    pub polymarket_clob_url: String,

    /// Polymarket WebSocket URL
    #[arg(
        long,
        env = "POLYMARKET_WS_URL",
        default_value = "wss://ws-subscriptions-clob.polymarket.com/ws/market"
    )]
    pub polymarket_ws_url: String,

    /// Polymarket API key (required for live trading)
    #[arg(long, env = "POLYMARKET_API_KEY")]
    pub polymarket_api_key: Option<String>,

    /// Polymarket private key for signing orders
    #[arg(long, env = "POLYMARKET_PRIVATE_KEY")]
    pub polymarket_private_key: Option<String>,

    /// Live scores API URL (e.g., TheSportsDB or similar)
    #[arg(
        long,
        env = "LIVE_SCORES_API_URL",
        default_value = "https://www.thesportsdb.com/api/v2/json/live"
    )]
    pub live_scores_api_url: String,

    /// Live scores API key
    #[arg(long, env = "LIVE_SCORES_API_KEY")]
    pub live_scores_api_key: Option<String>,

    /// AllSportsAPI key for WebSocket live scores (optional; enables real-time push)
    #[arg(long, env = "ALLSPORTSAPI_KEY")]
    pub allsportsapi_key: Option<String>,

    /// Polymarket Sports WebSocket URL for real-time score feed (no auth needed)
    #[arg(
        long,
        env = "POLYMARKET_SPORTS_WS_URL",
        default_value = "wss://sports-api.polymarket.com/ws"
    )]
    pub polymarket_sports_ws_url: String,

    /// Maximum fraction of bankroll to bet (Kelly multiplier, 0.0–1.0)
    #[arg(long, env = "KELLY_FRACTION", default_value = "0.25")]
    pub kelly_fraction: f64,

    /// Stop-loss threshold as fraction of position size (e.g. 0.5 = 50% loss)
    #[arg(long, env = "STOP_LOSS_FRACTION", default_value = "0.5")]
    pub stop_loss_fraction: f64,

    /// Take-profit threshold as fraction of position size (e.g. 0.3 = 30% gain)
    #[arg(long, env = "TAKE_PROFIT_FRACTION", default_value = "0.3")]
    pub take_profit_fraction: f64,

    /// Minimum edge required to place a bet (e.g. 0.05 = 5%)
    #[arg(long, env = "MIN_EDGE", default_value = "0.05")]
    pub min_edge: f64,

    /// Expected one-way fee in basis points for execution.
    #[arg(long, env = "EXPECTED_FEE_BPS", default_value = "10.0")]
    pub expected_fee_bps: f64,

    /// Expected one-way slippage in basis points for execution.
    #[arg(long, env = "EXPECTED_SLIPPAGE_BPS", default_value = "20.0")]
    pub expected_slippage_bps: f64,

    /// Skip entries if event-to-decision latency exceeds this value.
    #[arg(long, env = "LATENCY_MAX_SCORE_AGE_MS", default_value = "3500")]
    pub latency_max_score_age_ms: u64,

    /// Minimum modeled move required for latency-alpha entry gating.
    #[arg(long, env = "LATENCY_MIN_EXPECTED_MOVE", default_value = "0.02")]
    pub latency_min_expected_move: f64,

    /// Minimum residual move required (modeled move - observed move).
    #[arg(long, env = "LATENCY_MIN_RESIDUAL_MOVE", default_value = "0.01")]
    pub latency_min_residual_move: f64,

    /// Max fraction of modeled move that can already be priced-in.
    #[arg(long, env = "LATENCY_MAX_PRICED_IN_RATIO", default_value = "0.75")]
    pub latency_max_priced_in_ratio: f64,

    /// Maximum allowed age of WS quotes for exit decisions.
    #[arg(long, env = "WS_PRICE_MAX_AGE_MS", default_value = "2500")]
    pub ws_price_max_age_ms: u64,

    /// Max allowed absolute divergence between WS and REST entry quotes.
    #[arg(long, env = "MAX_ENTRY_QUOTE_DIVERGENCE", default_value = "0.08")]
    pub max_entry_quote_divergence: f64,

    /// Adaptive edge add-on cap driven by telemetry (latency/feed quality).
    #[arg(long, env = "ADAPTIVE_MIN_EDGE_MAX_ADDON", default_value = "0.03")]
    pub adaptive_min_edge_max_addon: f64,

    /// Tightening factor for adaptive divergence guard (0.0 = none).
    #[arg(long, env = "ADAPTIVE_DIVERGENCE_TIGHTENING", default_value = "0.35")]
    pub adaptive_divergence_tightening: f64,

    /// Maximum fraction of total equity that can be exposed to a single event.
    #[arg(long, env = "MAX_EVENT_EXPOSURE_FRACTION", default_value = "0.20")]
    pub max_event_exposure_fraction: f64,

    /// Maximum fraction of total equity that can be exposed to a single sport.
    #[arg(long, env = "MAX_SPORT_EXPOSURE_FRACTION", default_value = "0.50")]
    pub max_sport_exposure_fraction: f64,

    /// Maximum fraction of total equity that can be exposed to a single league.
    #[arg(long, env = "MAX_LEAGUE_EXPOSURE_FRACTION", default_value = "0.35")]
    pub max_league_exposure_fraction: f64,

    /// Maximum fraction of total equity that can be exposed to a single team across events.
    #[arg(long, env = "MAX_TEAM_EXPOSURE_FRACTION", default_value = "0.25")]
    pub max_team_exposure_fraction: f64,

    /// Maximum number of simultaneously open positions for one event.
    #[arg(long, env = "MAX_POSITIONS_PER_EVENT", default_value = "2")]
    pub max_positions_per_event: u32,

    /// Maximum covariance-adjusted effective exposure fraction of equity.
    #[arg(long, env = "MAX_EFFECTIVE_EXPOSURE_FRACTION", default_value = "0.30")]
    pub max_effective_exposure_fraction: f64,

    /// Correlation coefficient for positions tied to same event.
    #[arg(long, env = "CORRELATION_SAME_EVENT", default_value = "1.0")]
    pub correlation_same_event: f64,

    /// Correlation coefficient for positions tied to overlapping teams.
    #[arg(long, env = "CORRELATION_SAME_TEAM", default_value = "0.70")]
    pub correlation_same_team: f64,

    /// Correlation coefficient for positions in same league.
    #[arg(long, env = "CORRELATION_SAME_LEAGUE", default_value = "0.35")]
    pub correlation_same_league: f64,

    /// Correlation coefficient for positions in same sport.
    #[arg(long, env = "CORRELATION_SAME_SPORT", default_value = "0.20")]
    pub correlation_same_sport: f64,

    /// Circuit breaker: stop opening new positions if daily drawdown exceeds this fraction.
    #[arg(long, env = "MAX_DAILY_DRAWDOWN_FRACTION", default_value = "0.15")]
    pub max_daily_drawdown_fraction: f64,

    /// Circuit breaker: maximum number of new positions opened per day (UTC).
    #[arg(long, env = "MAX_TRADES_PER_DAY", default_value = "40")]
    pub max_trades_per_day: u32,

    /// Feed-health breaker: max EWMA REST fallback rate before pausing entries.
    #[arg(
        long,
        env = "FEED_HEALTH_MAX_REST_FALLBACK_RATE",
        default_value = "0.70"
    )]
    pub feed_health_max_rest_fallback_rate: f64,

    /// Feed-health breaker: max EWMA WS age (ms) before pausing entries.
    #[arg(long, env = "FEED_HEALTH_MAX_WS_AGE_MS", default_value = "4000")]
    pub feed_health_max_ws_age_ms: f64,

    /// Minimum sweep samples before feed-health breaker can trigger.
    #[arg(long, env = "FEED_HEALTH_MIN_SAMPLES", default_value = "6")]
    pub feed_health_min_samples: u64,

    /// Seconds to pause new entries when feed-health breaker triggers.
    #[arg(long, env = "FEED_HEALTH_COOLDOWN_SECS", default_value = "45")]
    pub feed_health_cooldown_secs: u64,

    /// If degradation persists this long, force-flatten open positions.
    #[arg(long, env = "FEED_HEALTH_FLATTEN_AFTER_SECS", default_value = "180")]
    pub feed_health_flatten_after_secs: u64,

    /// Max age for an open position before time-based forced flatten.
    #[arg(long, env = "MAX_POSITION_AGE_SECS", default_value = "14400")]
    pub max_position_age_secs: u64,

    /// Enable periodic outcome-based model calibration.
    #[arg(long, env = "CALIBRATION_ENABLED", default_value = "true")]
    pub calibration_enabled: bool,

    /// How often to run calibration retraining.
    #[arg(long, env = "CALIBRATION_INTERVAL_SECS", default_value = "3600")]
    pub calibration_interval_secs: u64,

    /// Minimum closed trades per sport required to fit calibration.
    #[arg(long, env = "CALIBRATION_MIN_SAMPLES_PER_SPORT", default_value = "50")]
    pub calibration_min_samples_per_sport: usize,

    /// Relative improvement required (logloss or brier) to promote a new calibration.
    #[arg(
        long,
        env = "CALIBRATION_MIN_RELATIVE_IMPROVEMENT",
        default_value = "0.005"
    )]
    pub calibration_min_relative_improvement: f64,

    /// Max optimization iterations for calibration fitting.
    #[arg(long, env = "CALIBRATION_MAX_ITERS", default_value = "400")]
    pub calibration_max_iters: usize,

    /// Learning rate for calibration optimization.
    #[arg(long, env = "CALIBRATION_LEARNING_RATE", default_value = "0.2")]
    pub calibration_learning_rate: f64,

    /// L2 regularization coefficient for calibration optimization.
    #[arg(long, env = "CALIBRATION_L2", default_value = "0.001")]
    pub calibration_l2: f64,

    /// Score-event dedup window for cross-provider duplicate suppression.
    #[arg(long, env = "SCORE_EVENT_DEDUP_WINDOW_SECS", default_value = "20")]
    pub score_event_dedup_window_secs: u64,

    /// Live scores polling interval in seconds
    #[arg(long, env = "POLL_INTERVAL_SECS", default_value = "5")]
    pub poll_interval_secs: u64,

    /// Retain score events for at most this many days.
    #[arg(long, env = "SCORE_EVENTS_RETENTION_DAYS", default_value = "14")]
    pub score_events_retention_days: i64,

    /// Retain balance history snapshots for at most this many days.
    #[arg(long, env = "BALANCE_HISTORY_RETENTION_DAYS", default_value = "30")]
    pub balance_history_retention_days: i64,
}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.dry_run {
            if self.polymarket_api_key.is_none() {
                anyhow::bail!(
                    "POLYMARKET_API_KEY is required in live trading mode. Use --dry-run for simulation."
                );
            }
            if self.polymarket_private_key.is_none() {
                anyhow::bail!(
                    "POLYMARKET_PRIVATE_KEY is required in live trading mode. Use --dry-run for simulation."
                );
            }
        }
        if !(0.0..=1.0).contains(&self.kelly_fraction) {
            anyhow::bail!("kelly_fraction must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.stop_loss_fraction) {
            anyhow::bail!("stop_loss_fraction must be between 0.0 and 1.0");
        }
        if !(0.0..=10.0).contains(&self.take_profit_fraction) {
            anyhow::bail!("take_profit_fraction must be between 0.0 and 10.0");
        }
        if self.initial_balance <= 0.0 {
            anyhow::bail!("initial_balance must be positive");
        }
        if !(0.0..=1_000.0).contains(&self.expected_fee_bps) {
            anyhow::bail!("expected_fee_bps must be between 0 and 1000");
        }
        if !(0.0..=1_000.0).contains(&self.expected_slippage_bps) {
            anyhow::bail!("expected_slippage_bps must be between 0 and 1000");
        }
        if !(0.0..=1.0).contains(&self.latency_min_expected_move) {
            anyhow::bail!("latency_min_expected_move must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.latency_min_residual_move) {
            anyhow::bail!("latency_min_residual_move must be between 0.0 and 1.0");
        }
        if !(0.0..=2.0).contains(&self.latency_max_priced_in_ratio) {
            anyhow::bail!("latency_max_priced_in_ratio must be between 0.0 and 2.0");
        }
        if self.ws_price_max_age_ms == 0 || self.ws_price_max_age_ms > 60_000 {
            anyhow::bail!("ws_price_max_age_ms must be between 1 and 60000");
        }
        if !(0.0..=0.5).contains(&self.max_entry_quote_divergence) {
            anyhow::bail!("max_entry_quote_divergence must be between 0.0 and 0.5");
        }
        if !(0.0..=0.5).contains(&self.adaptive_min_edge_max_addon) {
            anyhow::bail!("adaptive_min_edge_max_addon must be between 0.0 and 0.5");
        }
        if !(0.0..=1.0).contains(&self.adaptive_divergence_tightening) {
            anyhow::bail!("adaptive_divergence_tightening must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.max_event_exposure_fraction) {
            anyhow::bail!("max_event_exposure_fraction must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.max_sport_exposure_fraction) {
            anyhow::bail!("max_sport_exposure_fraction must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.max_league_exposure_fraction) {
            anyhow::bail!("max_league_exposure_fraction must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.max_team_exposure_fraction) {
            anyhow::bail!("max_team_exposure_fraction must be between 0.0 and 1.0");
        }
        if self.max_positions_per_event == 0 {
            anyhow::bail!("max_positions_per_event must be positive");
        }
        if !(0.0..=1.0).contains(&self.max_effective_exposure_fraction) {
            anyhow::bail!("max_effective_exposure_fraction must be between 0.0 and 1.0");
        }
        for (name, value) in [
            ("correlation_same_event", self.correlation_same_event),
            ("correlation_same_team", self.correlation_same_team),
            ("correlation_same_league", self.correlation_same_league),
            ("correlation_same_sport", self.correlation_same_sport),
        ] {
            if !(0.0..=1.0).contains(&value) {
                anyhow::bail!("{} must be between 0.0 and 1.0", name);
            }
        }
        if !(0.0..=1.0).contains(&self.max_daily_drawdown_fraction) {
            anyhow::bail!("max_daily_drawdown_fraction must be between 0.0 and 1.0");
        }
        if self.max_trades_per_day == 0 {
            anyhow::bail!("max_trades_per_day must be positive");
        }
        if !(0.0..=1.0).contains(&self.feed_health_max_rest_fallback_rate) {
            anyhow::bail!("feed_health_max_rest_fallback_rate must be between 0.0 and 1.0");
        }
        if !(100.0..=60_000.0).contains(&self.feed_health_max_ws_age_ms) {
            anyhow::bail!("feed_health_max_ws_age_ms must be between 100 and 60000");
        }
        if self.feed_health_min_samples == 0 {
            anyhow::bail!("feed_health_min_samples must be positive");
        }
        if self.feed_health_cooldown_secs == 0 {
            anyhow::bail!("feed_health_cooldown_secs must be positive");
        }
        if self.feed_health_flatten_after_secs == 0 {
            anyhow::bail!("feed_health_flatten_after_secs must be positive");
        }
        if self.max_position_age_secs == 0 || self.max_position_age_secs > 7 * 24 * 60 * 60 {
            anyhow::bail!("max_position_age_secs must be between 1 and 604800");
        }
        if self.calibration_interval_secs == 0 || self.calibration_interval_secs > 7 * 24 * 60 * 60
        {
            anyhow::bail!("calibration_interval_secs must be between 1 and 604800");
        }
        if self.calibration_min_samples_per_sport < 10
            || self.calibration_min_samples_per_sport > 1_000_000
        {
            anyhow::bail!("calibration_min_samples_per_sport must be between 10 and 1000000");
        }
        if !(0.0..=0.5).contains(&self.calibration_min_relative_improvement) {
            anyhow::bail!("calibration_min_relative_improvement must be between 0.0 and 0.5");
        }
        if self.calibration_max_iters == 0 || self.calibration_max_iters > 10_000 {
            anyhow::bail!("calibration_max_iters must be between 1 and 10000");
        }
        if !(0.0001..=5.0).contains(&self.calibration_learning_rate) {
            anyhow::bail!("calibration_learning_rate must be between 0.0001 and 5.0");
        }
        if !(0.0..=1.0).contains(&self.calibration_l2) {
            anyhow::bail!("calibration_l2 must be between 0.0 and 1.0");
        }
        if self.score_event_dedup_window_secs == 0 || self.score_event_dedup_window_secs > 600 {
            anyhow::bail!("score_event_dedup_window_secs must be between 1 and 600");
        }
        if self.score_events_retention_days <= 0 {
            anyhow::bail!("score_events_retention_days must be positive");
        }
        if self.balance_history_retention_days <= 0 {
            anyhow::bail!("balance_history_retention_days must be positive");
        }
        Ok(())
    }
}
