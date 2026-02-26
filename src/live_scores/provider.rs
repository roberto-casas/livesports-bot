use anyhow::Result;
use async_trait::async_trait;

use crate::db::models::LiveGame;

/// Trait that every live-score provider must implement.
#[async_trait]
pub trait ScoreProvider: Send + Sync {
    /// Return a snapshot of all currently in-progress games.
    async fn fetch_live_games(&self) -> Result<Vec<LiveGame>>;

    /// Human-readable name for logging.
    fn name(&self) -> &str;
}
