pub mod provider;
pub mod sports;

pub use provider::ScoreProvider;
pub use sports::{TheSportsDB, detect_score_change};

use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::db::models::{LiveGame, ScoreEvent};

/// Spawns a background task that polls live scores from **multiple providers
/// concurrently** at the configured interval and sends `ScoreEvent`s through
/// the returned channel whenever a score change is detected.
///
/// Multiple providers race in parallel; results are merged so the bot gets
/// the union of all games with the freshest data.
pub fn start_score_monitor(
    providers: Vec<Arc<dyn ScoreProvider>>,
    poll_interval: Duration,
) -> mpsc::Receiver<(ScoreEvent, LiveGame)> {
    let (tx, rx) = mpsc::channel(256);

    tokio::spawn(async move {
        let provider_names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        info!(
            "Score monitor started ({} providers: {:?}, interval={:?})",
            providers.len(),
            provider_names,
            poll_interval
        );

        // Previous snapshot: event_id -> LiveGame
        let mut prev_snapshot: HashMap<String, LiveGame> = HashMap::new();

        loop {
            // Poll ALL providers concurrently — fastest response wins per game
            let fetch_futures: Vec<_> = providers
                .iter()
                .map(|p| {
                    let p = Arc::clone(p);
                    async move { (p.name().to_string(), p.fetch_live_games().await) }
                })
                .collect();

            let results = futures_util::future::join_all(fetch_futures).await;

            // Merge results: for each event_id, keep the first occurrence.
            // Providers may overlap; this gives the union of all live games.
            let mut merged: HashMap<String, LiveGame> = HashMap::new();
            for (provider_name, result) in results {
                match result {
                    Ok(games) => {
                        for game in games {
                            merged.entry(game.event_id.clone()).or_insert(game);
                        }
                    }
                    Err(e) => {
                        warn!("Provider '{}' failed: {}", provider_name, e);
                    }
                }
            }

            // Detect score changes against previous snapshot
            for game in merged.values() {
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
                        // Log when events are dropped instead of silently ignoring
                        if let Err(e) = tx.try_send((ev, game.clone())) {
                            error!("Score event channel full, event DROPPED: {}", e);
                        }
                    }
                }
            }

            // Merge new data into snapshot instead of clearing — preserves
            // games that may be absent from a partial API response
            for (id, game) in merged {
                prev_snapshot.insert(id, game);
            }
            // Prune finished games to prevent unbounded snapshot growth
            prev_snapshot.retain(|_, g| g.status != crate::db::models::GameStatus::Finished);

            tokio::time::sleep(poll_interval).await;
        }
    });

    rx
}

/// Convenience wrapper: start a monitor with a single provider.
pub fn start_score_monitor_single(
    provider: Arc<dyn ScoreProvider>,
    poll_interval: Duration,
) -> mpsc::Receiver<(ScoreEvent, LiveGame)> {
    start_score_monitor(vec![provider], poll_interval)
}
