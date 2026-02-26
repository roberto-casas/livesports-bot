pub mod provider;
pub mod sports;

pub use provider::ScoreProvider;
pub use sports::{TheSportsDB, detect_score_change};

use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::db::models::{LiveGame, ScoreEvent};

/// Spawns a background task that polls live scores at the configured interval
/// and sends `ScoreEvent`s through the returned channel whenever a score change
/// is detected.
pub fn start_score_monitor(
    provider: Arc<dyn ScoreProvider>,
    poll_interval: Duration,
) -> mpsc::Receiver<(ScoreEvent, LiveGame)> {
    let (tx, rx) = mpsc::channel(256);

    tokio::spawn(async move {
        info!("Score monitor started (provider={}, interval={:?})", provider.name(), poll_interval);

        // Previous snapshot: event_id → LiveGame
        let mut prev_snapshot: HashMap<String, LiveGame> = HashMap::new();

        loop {
            match provider.fetch_live_games().await {
                Ok(games) => {
                    for game in &games {
                        if let Some(prev) = prev_snapshot.get(&game.event_id) {
                            if let Some(event_type) = detect_score_change(prev, game) {
                                let ev = ScoreEvent {
                                    id: None,
                                    event_id: game.event_id.clone(),
                                    sport: game.sport.clone(),
                                    league: game.league.clone(),
                                    home_team: game.home_team.clone(),
                                    away_team: game.away_team.clone(),
                                    home_score: game.home_score,
                                    away_score: game.away_score,
                                    minute: game.minute,
                                    event_type,
                                    detected_at: Utc::now(),
                                };
                                info!(
                                    "Score change detected: {} {} {}-{} ({})",
                                    ev.league, ev.event_id,
                                    ev.home_score, ev.away_score,
                                    ev.event_type
                                );
                                // Best-effort send; drop if receiver is full
                                let _ = tx.try_send((ev, game.clone()));
                            }
                        }
                    }

                    // Update snapshot
                    prev_snapshot.clear();
                    for game in games {
                        prev_snapshot.insert(game.event_id.clone(), game);
                    }
                }
                Err(e) => {
                    error!("Failed to fetch live games: {}", e);
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    });

    rx
}
