//! Generic WebSocket-based live-score provider.
//!
//! Instead of polling a REST API every N seconds, this provider connects to a
//! WebSocket endpoint and receives score updates as they happen — eliminating
//! the polling delay entirely.
//!
//! The provider is generic: it takes a `parse_fn` closure that converts raw WS
//! messages into `LiveGame` snapshots, so it can be used with **any** push-based
//! sports data API (API-Football, BetsAPI, Sportmonks, custom feeds, etc.).
//!
//! Architecture:
//! ```text
//!  WS Server ──push──▶ WebSocketProvider (background task)
//!                         │  parses messages → LiveGame
//!                         │  stores in shared snapshot map
//!                         ▼
//!              ScoreProvider::fetch_live_games()
//!                  reads snapshot (lock-free via tokio RwLock)
//! ```
//!
//! The provider exposes the standard `ScoreProvider` trait so it plugs directly
//! into the existing multi-provider score monitor.

#![allow(dead_code)]

use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::db::models::{GameStatus, LiveGame};
use super::provider::ScoreProvider;

/// A function that parses a raw WebSocket text message into zero or more `LiveGame`s.
pub type ParseFn = Arc<dyn Fn(&str) -> Vec<LiveGame> + Send + Sync>;

/// Configuration for a WebSocket score provider.
pub struct WebSocketProviderConfig {
    /// Display name for logging
    pub name: String,
    /// WebSocket URL to connect to (wss://...)
    pub url: String,
    /// Optional subscription message to send after connecting
    /// (many WS APIs require you to subscribe to specific events)
    pub subscribe_message: Option<String>,
    /// Parser that converts raw WS text frames into LiveGame snapshots
    pub parse_fn: ParseFn,
    /// Optional auth headers or query params (baked into the URL)
    pub ping_interval_secs: u64,
}

/// A push-based score provider that receives live scores via WebSocket.
///
/// The background task maintains a persistent connection with auto-reconnect.
/// The `fetch_live_games()` method returns the latest snapshot without any
/// network call — it's just a read from shared memory.
pub struct WebSocketProvider {
    name: String,
    /// Shared snapshot: event_id → LiveGame (updated by background task)
    snapshot: Arc<RwLock<HashMap<String, LiveGame>>>,
}

impl WebSocketProvider {
    /// Create a new WebSocket provider and spawn the background listener.
    ///
    /// The connection runs in a separate tokio task with automatic reconnection.
    pub fn new(config: WebSocketProviderConfig) -> Self {
        let snapshot: Arc<RwLock<HashMap<String, LiveGame>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let snap_clone = Arc::clone(&snapshot);
        let name_clone = config.name.clone();

        tokio::spawn(async move {
            ws_connection_loop(
                &name_clone,
                &config.url,
                config.subscribe_message.as_deref(),
                &config.parse_fn,
                snap_clone,
                config.ping_interval_secs,
            )
            .await;
        });

        WebSocketProvider {
            name: config.name,
            snapshot,
        }
    }
}

#[async_trait]
impl ScoreProvider for WebSocketProvider {
    fn name(&self) -> &str {
        &self.name
    }

    /// Returns the latest snapshot of live games — no network call, just a
    /// read lock on shared memory. This is effectively zero-latency.
    async fn fetch_live_games(&self) -> Result<Vec<LiveGame>> {
        let snap = self.snapshot.read().await;
        Ok(snap.values().cloned().collect())
    }
}

/// Persistent WebSocket connection loop with auto-reconnect and exponential
/// backoff.
async fn ws_connection_loop(
    name: &str,
    url: &str,
    subscribe_msg: Option<&str>,
    parse_fn: &ParseFn,
    snapshot: Arc<RwLock<HashMap<String, LiveGame>>>,
    ping_interval_secs: u64,
) {
    let mut backoff_secs = 1u64;
    let max_backoff = 30u64;

    loop {
        info!("[{}] Connecting to WebSocket: {}", name, url);

        match tokio_tungstenite::connect_async(url).await {
            Ok((ws_stream, _response)) => {
                info!("[{}] WebSocket connected", name);
                backoff_secs = 1; // reset backoff on successful connect

                let (mut write, mut read) = ws_stream.split();

                // Send subscription message if configured
                if let Some(sub_msg) = subscribe_msg {
                    if let Err(e) = write.send(Message::Text(sub_msg.to_string())).await {
                        error!("[{}] Failed to send subscribe message: {}", name, e);
                        continue;
                    }
                    info!("[{}] Subscription message sent", name);
                }

                // Set up ping interval to keep connection alive
                let mut ping_interval =
                    tokio::time::interval(std::time::Duration::from_secs(ping_interval_secs));

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    let games = parse_fn(&text);
                                    if !games.is_empty() {
                                        let mut snap = snapshot.write().await;
                                        for game in games {
                                            snap.insert(game.event_id.clone(), game);
                                        }
                                        // Prune finished games
                                        snap.retain(|_, g| g.status != GameStatus::Finished);
                                    }
                                }
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => {
                                    warn!("[{}] Server closed WebSocket connection", name);
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("[{}] WebSocket error: {}", name, e);
                                    break;
                                }
                                None => {
                                    warn!("[{}] WebSocket stream ended", name);
                                    break;
                                }
                                _ => {} // Binary, Pong, Frame — ignore
                            }
                        }
                        _ = ping_interval.tick() => {
                            if let Err(e) = write.send(Message::Ping(vec![])).await {
                                error!("[{}] Ping failed: {}", name, e);
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("[{}] WebSocket connection failed: {}", name, e);
            }
        }

        // Reconnect with exponential backoff
        warn!(
            "[{}] Reconnecting in {}s...",
            name, backoff_secs
        );
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(max_backoff);
    }
}

// ── Example parse functions for popular APIs ────────────────────────────────

/// Parse function for API-Football v3 WebSocket events.
///
/// API-Football sends JSON objects like:
/// ```json
/// {"fixture": {"id": 123, "status": {"elapsed": 45}},
///  "league": {"name": "Premier League"},
///  "teams": {"home": {"name": "Arsenal"}, "away": {"name": "Chelsea"}},
///  "goals": {"home": 1, "away": 0}}
/// ```
pub fn parse_api_football(text: &str) -> Vec<LiveGame> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

    // Handle both single event and array of events
    let events: Vec<&serde_json::Value> = if val.is_array() {
        val.as_array().unwrap().iter().collect()
    } else if val.get("fixture").is_some() {
        vec![&val]
    } else if let Some(response) = val.get("response").and_then(|r| r.as_array()) {
        response.iter().collect()
    } else {
        return vec![];
    };

    events
        .into_iter()
        .filter_map(|ev| {
            let fixture = ev.get("fixture")?;
            let event_id = fixture.get("id")?.as_u64()?.to_string();
            let elapsed = fixture
                .get("status")
                .and_then(|s| s.get("elapsed"))
                .and_then(|e| e.as_i64())
                .map(|e| e as i32);
            let short_status = fixture
                .get("status")
                .and_then(|s| s.get("short"))
                .and_then(|s| s.as_str())
                .unwrap_or("1H");

            let league_name = ev
                .get("league")
                .and_then(|l| l.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
                .to_string();

            let teams = ev.get("teams")?;
            let home_team = teams.get("home")?.get("name")?.as_str()?.to_string();
            let away_team = teams.get("away")?.get("name")?.as_str()?.to_string();

            let goals = ev.get("goals")?;
            let home_score = goals.get("home")?.as_i64()? as i32;
            let away_score = goals.get("away")?.as_i64()? as i32;

            let status = match short_status {
                "NS" => GameStatus::NotStarted,
                "HT" => GameStatus::HalfTime,
                "FT" | "AET" | "PEN" => GameStatus::Finished,
                _ => GameStatus::InProgress,
            };

            Some(LiveGame {
                event_id: format!("apifootball_{}", event_id),
                sport: "soccer".to_string(),
                league: league_name,
                home_team,
                away_team,
                home_score,
                away_score,
                minute: elapsed,
                status,
            })
        })
        .collect()
}

/// Parse function for BetsAPI live events.
///
/// BetsAPI WebSocket sends JSON with `results` array containing score data.
pub fn parse_betsapi(text: &str) -> Vec<LiveGame> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

    let results = match val.get("results").and_then(|r| r.as_array()) {
        Some(r) => r,
        None => return vec![],
    };

    results
        .iter()
        .filter_map(|ev| {
            let event_id = ev.get("id")?.as_str().or_else(|| {
                ev.get("id").and_then(|id| id.as_u64()).map(|_| "")
            })?.to_string();

            // Fallback for numeric IDs
            let event_id = if event_id.is_empty() {
                ev.get("id")?.as_u64()?.to_string()
            } else {
                event_id
            };

            let sport_id = ev.get("sport_id").and_then(|s| s.as_u64()).unwrap_or(1);
            let sport = match sport_id {
                1 => "soccer",
                18 => "basketball",
                12 => "american_football",
                16 => "baseball",
                17 => "ice_hockey",
                13 => "tennis",
                _ => "unknown",
            }
            .to_string();

            let league = ev
                .get("league")
                .and_then(|l| l.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
                .to_string();

            let home_team = ev.get("home")?.get("name")?.as_str()?.to_string();
            let away_team = ev.get("away")?.get("name")?.as_str()?.to_string();

            let scores = ev.get("scores")?;
            let home_score: i32 = scores
                .get("home")
                .and_then(|s| s.as_str().and_then(|s| s.parse().ok()).or_else(|| s.as_i64().map(|v| v as i32)))
                .unwrap_or(0);
            let away_score: i32 = scores
                .get("away")
                .and_then(|s| s.as_str().and_then(|s| s.parse().ok()).or_else(|| s.as_i64().map(|v| v as i32)))
                .unwrap_or(0);

            let time_status = ev.get("time_status")?.as_str().unwrap_or("1");
            let status = match time_status {
                "0" => GameStatus::NotStarted,
                "3" => GameStatus::Finished,
                _ => GameStatus::InProgress,
            };

            let minute = ev
                .get("timer")
                .and_then(|t| t.get("tm"))
                .and_then(|tm| tm.as_str().and_then(|s| s.parse().ok()).or_else(|| tm.as_i64().map(|v| v as i32)));

            Some(LiveGame {
                event_id: format!("betsapi_{}", event_id),
                sport,
                league,
                home_team,
                away_team,
                home_score,
                away_score,
                minute,
                status,
            })
        })
        .collect()
}
