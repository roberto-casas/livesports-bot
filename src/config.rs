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

    /// Live scores polling interval in seconds
    #[arg(long, env = "POLL_INTERVAL_SECS", default_value = "5")]
    pub poll_interval_secs: u64,
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
        Ok(())
    }
}
