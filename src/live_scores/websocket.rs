//! WebSocket-based live-score providers for real-time push delivery.
//!
//! Instead of polling a REST API every N seconds, these providers connect to
//! WebSocket endpoints and receive score updates as they happen — eliminating
//! polling delay entirely.
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
//! Included parsers:
//! - **AllSportsAPI** (`wss://wss.allsportsapi.com/live_events`) — multi-sport
//! - **Polymarket Sports WS** (`wss://sports-api.polymarket.com/ws`) — no auth needed

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
    pub subscribe_message: Option<String>,
    /// Parser that converts raw WS text frames into LiveGame snapshots
    pub parse_fn: ParseFn,
    /// Seconds between client-side ping frames
    pub ping_interval_secs: u64,
}

/// A push-based score provider that receives live scores via WebSocket.
///
/// The background task maintains a persistent connection with auto-reconnect.
/// `fetch_live_games()` returns the latest snapshot from shared memory — no
/// network call at all.
pub struct WebSocketProvider {
    name: String,
    snapshot: Arc<RwLock<HashMap<String, LiveGame>>>,
}

impl WebSocketProvider {
    /// Create a new WebSocket provider and spawn the background listener.
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
                backoff_secs = 1;

                let (mut write, mut read) = ws_stream.split();

                // Send subscription message if configured
                if let Some(sub_msg) = subscribe_msg {
                    if let Err(e) = write.send(Message::Text(sub_msg.to_string())).await {
                        error!("[{}] Failed to send subscribe message: {}", name, e);
                        continue;
                    }
                    info!("[{}] Subscription message sent", name);
                }

                let mut ping_interval =
                    tokio::time::interval(std::time::Duration::from_secs(ping_interval_secs));

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    // Handle text-based ping (Polymarket Sports WS sends "ping")
                                    if text.trim() == "ping" {
                                        let _ = write.send(Message::Text("pong".to_string())).await;
                                        continue;
                                    }
                                    let games = parse_fn(&text);
                                    if !games.is_empty() {
                                        let mut snap = snapshot.write().await;
                                        for game in games {
                                            snap.insert(game.event_id.clone(), game);
                                        }
                                        snap.retain(|_, g| g.status != GameStatus::Finished);
                                    }
                                }
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => {
                                    warn!("[{}] Server closed WebSocket", name);
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
                                _ => {}
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

        warn!("[{}] Reconnecting in {}s...", name, backoff_secs);
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(max_backoff);
    }
}

// ── AllSportsAPI Parser ──────────────────────────────────────────────────────

/// Parse AllSportsAPI WebSocket messages.
///
/// Endpoint: `wss://wss.allsportsapi.com/live_events?APIkey=KEY&timezone=+00:00`
///
/// The server pushes a JSON array of match objects whenever any live match
/// updates (goal, minute change, stat change). Key fields:
/// - `event_key`: unique match ID
/// - `event_home_team` / `event_away_team`: team names
/// - `event_final_result`: score string like "1 - 2"
/// - `event_status`: minute string like "74" or "Finished" / "Half Time"
/// - `league_name`: league name
/// - `event_live`: "1" if live
pub fn parse_allsportsapi(text: &str) -> Vec<LiveGame> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

    let events = match val.as_array() {
        Some(a) => a.clone(),
        None => {
            // Some messages wrap in a top-level object
            match val.get("result").and_then(|r| r.as_array()) {
                Some(a) => a.clone(),
                None => {
                    // Single event object
                    if val.get("event_key").is_some() {
                        vec![val]
                    } else {
                        return vec![];
                    }
                }
            }
        }
    };

    events
        .iter()
        .filter_map(|ev| {
            let event_key = ev.get("event_key")?
                .as_str()
                .or_else(|| ev.get("event_key").and_then(|v| v.as_u64()).map(|_| ""))
                .unwrap_or("");

            let event_id = if event_key.is_empty() {
                ev.get("event_key")?.as_u64()?.to_string()
            } else {
                event_key.to_string()
            };

            let home_team = ev.get("event_home_team")?.as_str()?.to_string();
            let away_team = ev.get("event_away_team")?.as_str()?.to_string();

            // Parse score from "event_final_result": "1 - 2"
            let (home_score, away_score) = parse_score_string(
                ev.get("event_final_result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0 - 0"),
            );

            let league_name = ev
                .get("league_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // event_status: minute like "74" or status like "Finished", "Half Time"
            let status_str = ev
                .get("event_status")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let (status, minute) = parse_allsports_status(status_str);

            // Classify sport from league name heuristics or country
            let sport = classify_sport_from_league(&league_name);

            // Only include live events
            let is_live = ev
                .get("event_live")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                == "1";
            if !is_live && status == GameStatus::NotStarted {
                return None;
            }

            Some(LiveGame {
                event_id: format!("allsports_{}", event_id),
                sport,
                league: league_name,
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

/// Parse Polymarket Sports WebSocket messages.
///
/// Endpoint: `wss://sports-api.polymarket.com/ws` (no auth required)
///
/// Server sends JSON with fields: `slug`, `score`, `period`.
/// Server pings with text "ping" every 5s — must reply with "pong".
pub fn parse_polymarket_sports(text: &str) -> Vec<LiveGame> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

    // Handle both single update and array of updates
    let events: Vec<&serde_json::Value> = if val.is_array() {
        val.as_array().unwrap().iter().collect()
    } else if val.get("slug").is_some() {
        vec![&val]
    } else if let Some(data) = val.get("data") {
        if data.is_array() {
            data.as_array().unwrap().iter().collect()
        } else if data.get("slug").is_some() {
            vec![data]
        } else {
            return vec![];
        }
    } else {
        return vec![];
    };

    events
        .into_iter()
        .filter_map(|ev| {
            let slug = ev.get("slug")?.as_str()?;

            // Parse score field — format varies, try common patterns
            let score_str = ev.get("score").and_then(|v| v.as_str()).unwrap_or("");
            let (home_score, away_score) = parse_score_string(score_str);

            let period = ev
                .get("period")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Extract team names from slug (format: "team1-vs-team2" or similar)
            let (home_team, away_team) = parse_teams_from_slug(slug);

            let status = match period.to_lowercase().as_str() {
                "final" | "finished" | "ft" => GameStatus::Finished,
                "halftime" | "ht" | "half" => GameStatus::HalfTime,
                "not_started" | "ns" | "pregame" | "" => GameStatus::NotStarted,
                _ => GameStatus::InProgress,
            };

            // Try to extract minute from period (e.g., "Q3 5:42" or "75'")
            let minute = extract_minute_from_period(period);

            Some(LiveGame {
                event_id: format!("polymarket_{}", slug),
                sport: "unknown".to_string(), // Polymarket doesn't send sport type
                league: "Polymarket".to_string(),
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

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a score string like "1 - 2", "1-2", "1:2" into (home, away) integers.
fn parse_score_string(s: &str) -> (i32, i32) {
    // Try common separators
    for sep in [" - ", "-", ":"] {
        if let Some((h, a)) = s.split_once(sep) {
            if let (Ok(home), Ok(away)) = (h.trim().parse::<i32>(), a.trim().parse::<i32>()) {
                return (home, away);
            }
        }
    }
    (0, 0)
}

/// Parse AllSportsAPI event_status: "74" → InProgress + minute=74,
/// "Finished" → Finished, "Half Time" → HalfTime, etc.
fn parse_allsports_status(status: &str) -> (GameStatus, Option<i32>) {
    if let Ok(minute) = status.parse::<i32>() {
        return (GameStatus::InProgress, Some(minute));
    }
    match status.to_lowercase().as_str() {
        "finished" | "ft" | "after pen." | "after extra time" => (GameStatus::Finished, None),
        "half time" | "ht" => (GameStatus::HalfTime, None),
        "not started" | "ns" | "" => (GameStatus::NotStarted, None),
        "postponed" | "cancelled" | "abandoned" => (GameStatus::Finished, None),
        _ => (GameStatus::InProgress, None),
    }
}

/// Classify sport from league name using heuristics.
fn classify_sport_from_league(league: &str) -> String {
    let l = league.to_lowercase();
    if l.contains("nba") || l.contains("basketball") || l.contains("euroleague") {
        "basketball".to_string()
    } else if l.contains("nfl") || l.contains("american football") {
        "american_football".to_string()
    } else if l.contains("nhl") || l.contains("hockey") || l.contains("ice hockey") {
        "ice_hockey".to_string()
    } else if l.contains("mlb") || l.contains("baseball") {
        "baseball".to_string()
    } else if l.contains("tennis") || l.contains("atp") || l.contains("wta") {
        "tennis".to_string()
    } else {
        // Default to soccer — most leagues worldwide are football
        "soccer".to_string()
    }
}

/// Extract team names from a Polymarket slug like "team1-vs-team2-something".
fn parse_teams_from_slug(slug: &str) -> (String, String) {
    // Common patterns: "team-a-vs-team-b-2024", "team-a-team-b"
    if let Some(idx) = slug.find("-vs-") {
        let home = &slug[..idx];
        let away_start = idx + 4;
        let away = &slug[away_start..];
        // Clean up: replace hyphens with spaces, strip trailing date/numbers
        let home = slug_to_name(home);
        let away = slug_to_name(away);
        return (home, away);
    }
    (slug.to_string(), "Unknown".to_string())
}

/// Convert a slug segment like "manchester-united" into "Manchester United".
fn slug_to_name(slug: &str) -> String {
    slug.split('-')
        .filter(|s| !s.is_empty() && s.parse::<u32>().is_err()) // strip year numbers
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Try to extract a minute number from a period string.
/// E.g., "Q3 5:42" → None (basketball quarters), "75'" → 75, "2H 30" → 30
fn extract_minute_from_period(period: &str) -> Option<i32> {
    // Try parsing the whole thing as a number first
    if let Ok(m) = period.trim().trim_end_matches('\'').parse::<i32>() {
        return Some(m);
    }
    // Try last token
    if let Some(last) = period.split_whitespace().last() {
        if let Ok(m) = last.trim_end_matches('\'').parse::<i32>() {
            return Some(m);
        }
    }
    None
}

// ── Legacy parsers (API-Football, BetsAPI) ───────────────────────────────────

/// Parse function for API-Football v3 WebSocket events.
pub fn parse_api_football(text: &str) -> Vec<LiveGame> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

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
            let event_id = ev
                .get("id")
                .and_then(|id| {
                    id.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| id.as_u64().map(|n| n.to_string()))
                })?;

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
                .and_then(|s| {
                    s.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| s.as_i64().map(|v| v as i32))
                })
                .unwrap_or(0);
            let away_score: i32 = scores
                .get("away")
                .and_then(|s| {
                    s.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| s.as_i64().map(|v| v as i32))
                })
                .unwrap_or(0);

            let time_status = ev
                .get("time_status")
                .and_then(|v| v.as_str())
                .unwrap_or("1");
            let status = match time_status {
                "0" => GameStatus::NotStarted,
                "3" => GameStatus::Finished,
                _ => GameStatus::InProgress,
            };

            let minute = ev
                .get("timer")
                .and_then(|t| t.get("tm"))
                .and_then(|tm| {
                    tm.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| tm.as_i64().map(|v| v as i32))
                });

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_score_string() {
        assert_eq!(parse_score_string("1 - 2"), (1, 2));
        assert_eq!(parse_score_string("3-0"), (3, 0));
        assert_eq!(parse_score_string("1:1"), (1, 1));
        assert_eq!(parse_score_string(""), (0, 0));
        assert_eq!(parse_score_string("garbage"), (0, 0));
    }

    #[test]
    fn test_parse_allsports_status() {
        assert_eq!(
            parse_allsports_status("74"),
            (GameStatus::InProgress, Some(74))
        );
        assert_eq!(
            parse_allsports_status("Finished"),
            (GameStatus::Finished, None)
        );
        assert_eq!(
            parse_allsports_status("Half Time"),
            (GameStatus::HalfTime, None)
        );
        assert_eq!(
            parse_allsports_status(""),
            (GameStatus::NotStarted, None)
        );
    }

    #[test]
    fn test_classify_sport() {
        assert_eq!(classify_sport_from_league("NBA - Regular Season"), "basketball");
        assert_eq!(classify_sport_from_league("NFL"), "american_football");
        assert_eq!(classify_sport_from_league("Premier League"), "soccer");
        assert_eq!(classify_sport_from_league("NHL"), "ice_hockey");
        assert_eq!(classify_sport_from_league("MLB"), "baseball");
    }

    #[test]
    fn test_parse_teams_from_slug() {
        let (h, a) = parse_teams_from_slug("manchester-united-vs-chelsea-2024");
        assert_eq!(h, "Manchester United");
        assert_eq!(a, "Chelsea");
    }

    #[test]
    fn test_slug_to_name() {
        assert_eq!(slug_to_name("manchester-united"), "Manchester United");
        assert_eq!(slug_to_name("arsenal"), "Arsenal");
    }

    #[test]
    fn test_parse_allsportsapi_message() {
        let msg = r#"[{
            "event_key": "11205",
            "event_home_team": "Newcastle Jets",
            "event_away_team": "Brisbane Roar",
            "event_final_result": "1 - 2",
            "event_status": "74",
            "league_name": "A-League",
            "event_live": "1"
        }]"#;
        let games = parse_allsportsapi(msg);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].home_team, "Newcastle Jets");
        assert_eq!(games[0].away_team, "Brisbane Roar");
        assert_eq!(games[0].home_score, 1);
        assert_eq!(games[0].away_score, 2);
        assert_eq!(games[0].minute, Some(74));
        assert_eq!(games[0].status, GameStatus::InProgress);
    }

    #[test]
    fn test_parse_polymarket_sports_message() {
        let msg = r#"{"slug": "arsenal-vs-chelsea-epl", "score": "2 - 1", "period": "75'"}"#;
        let games = parse_polymarket_sports(msg);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].home_team, "Arsenal");
        assert_eq!(games[0].away_team, "Chelsea Epl");
        assert_eq!(games[0].home_score, 2);
        assert_eq!(games[0].away_score, 1);
        assert_eq!(games[0].status, GameStatus::InProgress);
    }

    #[test]
    fn test_extract_minute_from_period() {
        assert_eq!(extract_minute_from_period("75'"), Some(75));
        assert_eq!(extract_minute_from_period("45"), Some(45));
        assert_eq!(extract_minute_from_period("Q3 5:42"), None);
        assert_eq!(extract_minute_from_period(""), None);
    }
}
