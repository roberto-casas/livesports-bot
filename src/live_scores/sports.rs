use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use tracing::debug;

use crate::db::models::{GameStatus, LiveGame};
use super::provider::ScoreProvider;

/// Live-score provider backed by TheSportsDB v2 free API.
/// Docs: <https://www.thesportsdb.com/api.php>
pub struct TheSportsDB {
    http: Client,
    api_key: String,
    /// Base URL for overriding in tests
    base_url: String,
}

impl TheSportsDB {
    pub fn new(api_key: Option<&str>, base_url: Option<&str>) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(TheSportsDB {
            http,
            // "3" is TheSportsDB's public free-tier key; replace with a paid key for higher limits
            api_key: api_key.unwrap_or("3").to_string(),
            base_url: base_url
                .unwrap_or("https://www.thesportsdb.com/api/v1/json")
                .to_string(),
        })
    }

    fn status_from_str(s: &str) -> GameStatus {
        match s.to_lowercase().as_str() {
            "not started" | "ns" => GameStatus::NotStarted,
            "half time" | "ht" | "halftime" => GameStatus::HalfTime,
            "match finished" | "ft" | "finished" | "aet" | "pen" => GameStatus::Finished,
            _ => GameStatus::InProgress,
        }
    }
}

#[async_trait]
impl ScoreProvider for TheSportsDB {
    fn name(&self) -> &str {
        "TheSportsDB"
    }

    async fn fetch_live_games(&self) -> Result<Vec<LiveGame>> {
        // TheSportsDB livescore endpoint (requires paid tier for real live scores;
        // free tier returns today's events which we treat as potentially live)
        let url = format!("{}/{}/livescore.php", self.base_url, self.api_key);
        debug!("Fetching live games from {}", url);

        let resp = self.http.get(&url).send().await
            .context("TheSportsDB request failed")?;

        if !resp.status().is_success() {
            anyhow::bail!("TheSportsDB error: {}", resp.status());
        }

        let raw: serde_json::Value = resp.json().await
            .context("Failed to parse TheSportsDB response")?;

        parse_livescore_response(&raw)
    }
}

fn parse_livescore_response(raw: &serde_json::Value) -> Result<Vec<LiveGame>> {
    let events = match raw["events"].as_array() {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let games = events
        .iter()
        .filter_map(|ev| {
            let event_id = ev["idEvent"].as_str()?.to_string();
            let sport = ev["strSport"].as_str().unwrap_or("soccer").to_lowercase();
            let league = ev["strLeague"].as_str().unwrap_or("unknown").to_string();
            let home_team = ev["strHomeTeam"].as_str()?.to_string();
            let away_team = ev["strAwayTeam"].as_str()?.to_string();

            let home_score: i32 = ev["intHomeScore"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| ev["intHomeScore"].as_i64().map(|v| v as i32))
                .unwrap_or(0);

            let away_score: i32 = ev["intAwayScore"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| ev["intAwayScore"].as_i64().map(|v| v as i32))
                .unwrap_or(0);

            let minute: Option<i32> = ev["intProgress"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| ev["strProgress"].as_str().and_then(|s| s.parse().ok()));

            let status_str = ev["strStatus"]
                .as_str()
                .unwrap_or("In Progress");
            let status = TheSportsDB::status_from_str(status_str);

            Some(LiveGame {
                event_id,
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
        .collect();

    Ok(games)
}

/// Detect changes between two game snapshots and return a description.
/// Returns `Some(event_type)` if a scoreline change is detected.
pub fn detect_score_change(prev: &LiveGame, curr: &LiveGame) -> Option<String> {
    if curr.home_score != prev.home_score || curr.away_score != prev.away_score {
        let event_type = classify_score_change(
            &curr.sport,
            prev.home_score,
            prev.away_score,
            curr.home_score,
            curr.away_score,
        );
        Some(event_type)
    } else {
        None
    }
}

fn classify_score_change(
    sport: &str,
    prev_home: i32,
    prev_away: i32,
    curr_home: i32,
    curr_away: i32,
) -> String {
    let home_delta = curr_home - prev_home;
    let away_delta = curr_away - prev_away;

    match sport {
        "soccer" | "football" => {
            if home_delta > 0 { "goal_home".to_string() }
            else if away_delta > 0 { "goal_away".to_string() }
            else { "score_change".to_string() }
        }
        "american_football" | "nfl" => {
            let delta = if home_delta != 0 { home_delta } else { away_delta };
            match delta.abs() {
                6 => "touchdown".to_string(),
                3 => "field_goal".to_string(),
                1 => "extra_point".to_string(),
                2 => "safety".to_string(),
                _ => "score_change".to_string(),
            }
        }
        "basketball" | "nba" => {
            let delta = if home_delta != 0 { home_delta } else { away_delta };
            match delta.abs() {
                3 => "three_pointer".to_string(),
                2 => "basket".to_string(),
                1 => "free_throw".to_string(),
                _ => "score_change".to_string(),
            }
        }
        "baseball" | "mlb" => "run".to_string(),
        "ice_hockey" | "nhl" => "goal".to_string(),
        "tennis" => "point".to_string(),
        _ => "score_change".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::GameStatus;

    fn game(home: i32, away: i32) -> LiveGame {
        LiveGame {
            event_id: "1".into(),
            sport: "soccer".into(),
            league: "Premier League".into(),
            home_team: "Arsenal".into(),
            away_team: "Chelsea".into(),
            home_score: home,
            away_score: away,
            minute: Some(45),
            status: GameStatus::InProgress,
        }
    }

    #[test]
    fn test_detect_no_change() {
        let g = game(1, 0);
        assert!(detect_score_change(&g, &g).is_none());
    }

    #[test]
    fn test_detect_home_goal() {
        let prev = game(0, 0);
        let curr = game(1, 0);
        let ev = detect_score_change(&prev, &curr);
        assert_eq!(ev, Some("goal_home".to_string()));
    }

    #[test]
    fn test_detect_away_goal() {
        let prev = game(0, 0);
        let curr = game(0, 1);
        let ev = detect_score_change(&prev, &curr);
        assert_eq!(ev, Some("goal_away".to_string()));
    }

    #[test]
    fn test_classify_nfl_touchdown() {
        let result = classify_score_change("nfl", 0, 0, 6, 0);
        assert_eq!(result, "touchdown");
    }

    #[test]
    fn test_classify_nba_three_pointer() {
        let result = classify_score_change("basketball", 0, 0, 3, 0);
        assert_eq!(result, "three_pointer");
    }

    #[test]
    fn test_status_from_str() {
        assert_eq!(TheSportsDB::status_from_str("FT"), GameStatus::Finished);
        assert_eq!(TheSportsDB::status_from_str("HT"), GameStatus::HalfTime);
        assert_eq!(TheSportsDB::status_from_str("NS"), GameStatus::NotStarted);
        assert_eq!(TheSportsDB::status_from_str("75"), GameStatus::InProgress);
    }
}
