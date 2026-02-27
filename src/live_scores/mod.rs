pub mod provider;
pub mod sports;
pub mod websocket;

pub use provider::ScoreProvider;
pub use sports::{detect_score_change, TheSportsDB};
pub use websocket::{WebSocketProvider, WebSocketProviderConfig};

use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::db::models::{LiveGame, ScoreEvent};

fn provider_weight(name: &str) -> f64 {
    let n = name.to_lowercase();
    if n.contains("polymarket") {
        1.0
    } else if n.contains("allsports") {
        0.95
    } else if n.contains("thesportsdb") {
        0.9
    } else {
        0.85
    }
}

fn consensus_score_key(game: &LiveGame) -> (i32, i32, Option<i32>, String) {
    (
        game.home_score,
        game.away_score,
        game.minute,
        format!("{:?}", game.status),
    )
}

fn select_consensus_game(candidates: Vec<(String, LiveGame)>) -> Option<(String, LiveGame, i32)> {
    if candidates.is_empty() {
        return None;
    }

    // Group by score snapshot key and count agreement.
    let mut groups: HashMap<(i32, i32, Option<i32>, String), Vec<(String, LiveGame)>> =
        HashMap::new();
    for (provider, game) in candidates {
        groups
            .entry(consensus_score_key(&game))
            .or_default()
            .push((provider, game));
    }

    let mut best_group: Option<Vec<(String, LiveGame)>> = None;
    let mut best_count = -1i32;
    let mut best_weight = -1.0f64;
    for group in groups.into_values() {
        let count = group.len() as i32;
        let weight_sum: f64 = group.iter().map(|(p, _)| provider_weight(p)).sum();
        if count > best_count || (count == best_count && weight_sum > best_weight) {
            best_count = count;
            best_weight = weight_sum;
            best_group = Some(group);
        }
    }

    let mut best_provider = String::new();
    let mut best_game: Option<LiveGame> = None;
    let mut best_provider_weight = -1.0f64;
    let mut best_minute = -1i32;
    for (provider, game) in best_group.unwrap_or_default() {
        let w = provider_weight(&provider);
        let minute = game.minute.unwrap_or(-1);
        if w > best_provider_weight || (w == best_provider_weight && minute > best_minute) {
            best_provider_weight = w;
            best_minute = minute;
            best_provider = provider;
            best_game = Some(game);
        }
    }

    best_game.map(|g| (best_provider, g, best_count.max(1)))
}

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
    let (tx, rx) = mpsc::channel(1024);

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
        let mut last_seen: HashMap<String, tokio::time::Instant> = HashMap::new();
        let provider_timeout = poll_interval.min(Duration::from_secs(2));
        let stale_after = Duration::from_secs(6 * 60 * 60);
        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            // Poll ALL providers concurrently — fastest response wins per game
            let fetch_futures: Vec<_> = providers
                .iter()
                .map(|p| {
                    let p = Arc::clone(p);
                    async move {
                        let res =
                            tokio::time::timeout(provider_timeout, p.fetch_live_games()).await;
                        let out = match res {
                            Ok(result) => result,
                            Err(_) => {
                                Err(anyhow::anyhow!("timed out after {:?}", provider_timeout))
                            }
                        };
                        (p.name().to_string(), out)
                    }
                })
                .collect();

            let results = futures_util::future::join_all(fetch_futures).await;

            // Merge results with provider-consensus selection.
            let mut by_event: HashMap<String, Vec<(String, LiveGame)>> = HashMap::new();
            for (provider_name, result) in results {
                match result {
                    Ok(games) => {
                        for game in games {
                            by_event
                                .entry(game.event_id.clone())
                                .or_default()
                                .push((provider_name.clone(), game));
                        }
                    }
                    Err(e) => {
                        warn!("Provider '{}' failed: {}", provider_name, e);
                    }
                }
            }

            let mut merged: HashMap<String, (String, i32, LiveGame)> = HashMap::new();
            for (event_id, candidates) in by_event {
                if let Some((provider, game, consensus_count)) = select_consensus_game(candidates) {
                    merged.insert(event_id, (provider, consensus_count, game));
                }
            }

            // Detect score changes against previous snapshot
            for (provider, consensus_count, game) in merged.values() {
                if let Some(prev) = prev_snapshot.get(&game.event_id) {
                    if let Some(event_type) = detect_score_change(prev, game) {
                        let ev = ScoreEvent {
                            id: None,
                            event_id: game.event_id.clone(),
                            source_provider: Some(provider.clone()),
                            provider_consensus_count: Some(*consensus_count),
                            sport: game.sport.clone(),
                            league: game.league.clone(),
                            home_team: game.home_team.clone(),
                            away_team: game.away_team.clone(),
                            prev_home_score: Some(prev.home_score),
                            prev_away_score: Some(prev.away_score),
                            home_score: game.home_score,
                            away_score: game.away_score,
                            minute: game.minute,
                            event_type,
                            detected_at: Utc::now(),
                        };
                        info!(
                            "Score change detected: {} {} {}-{} ({})",
                            ev.league, ev.event_id, ev.home_score, ev.away_score, ev.event_type
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
            let now = tokio::time::Instant::now();
            for (id, (_, _, game)) in merged {
                last_seen.insert(id.clone(), now);
                prev_snapshot.insert(id, game);
            }
            // Prune finished games to prevent unbounded snapshot growth
            prev_snapshot.retain(|_, g| g.status != crate::db::models::GameStatus::Finished);
            let stale_ids: Vec<String> = last_seen
                .iter()
                .filter_map(|(id, seen)| {
                    if seen.elapsed() > stale_after {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for id in stale_ids {
                last_seen.remove(&id);
                prev_snapshot.remove(&id);
            }
        }
    });

    rx
}

/// Convenience wrapper: start a monitor with a single provider.
#[allow(dead_code)]
pub fn start_score_monitor_single(
    provider: Arc<dyn ScoreProvider>,
    poll_interval: Duration,
) -> mpsc::Receiver<(ScoreEvent, LiveGame)> {
    start_score_monitor(vec![provider], poll_interval)
}
