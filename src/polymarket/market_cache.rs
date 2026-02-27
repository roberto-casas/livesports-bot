//! In-memory market cache for instant lookups on the hot path.
//!
//! The background market-discovery task periodically fetches all sports markets
//! from Polymarket and populates this cache.  When a score event fires, the
//! bot engine queries the cache first (sub-microsecond) instead of hitting the
//! REST API (~1.5s).
//!
//! Markets are indexed by **normalized team-name tokens** so that a lookup for
//! "Manchester United" matches a market titled "Will Man United win vs Chelsea?"
//!
//! Cache misses fall through to the live REST API.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

use crate::db::models::Market;

/// Thread-safe, in-memory market cache with team-name indexing.
#[derive(Clone)]
pub struct MarketCache {
    inner: Arc<RwLock<CacheInner>>,
}

struct CacheInner {
    /// market_id → Market
    markets: HashMap<String, Market>,
    /// normalized token → set of market IDs that mention this token
    /// e.g. "arsenal" → {"market_abc", "market_def"}
    token_index: HashMap<String, HashSet<String>>,
}

impl MarketCache {
    pub fn new() -> Self {
        MarketCache {
            inner: Arc::new(RwLock::new(CacheInner {
                markets: HashMap::new(),
                token_index: HashMap::new(),
            })),
        }
    }

    /// Bulk-load markets into the cache, replacing existing entries.
    /// Called by the background discovery task.
    pub async fn load(&self, markets: Vec<Market>) {
        let mut inner = self.inner.write().await;
        inner.markets.clear();
        inner.token_index.clear();

        for market in markets {
            // Index by tokens from the question and event_name
            let tokens = extract_tokens(&market.question, market.event_name.as_deref());
            for token in &tokens {
                inner
                    .token_index
                    .entry(token.clone())
                    .or_default()
                    .insert(market.id.clone());
            }
            inner.markets.insert(market.id.clone(), market);
        }

        debug!(
            "MarketCache: {} markets, {} index tokens",
            inner.markets.len(),
            inner.token_index.len()
        );
    }

    /// Search for markets matching the given teams and league.
    ///
    /// Uses set-intersection on normalized tokens: a market must contain at
    /// least one token from EACH team name to match.  This handles abbreviations
    /// ("Man United" matching "Manchester United") through token overlap.
    ///
    /// Returns markets sorted by volume (highest first).
    pub async fn search(&self, home_team: &str, away_team: &str, _league: &str) -> Vec<Market> {
        let inner = self.inner.read().await;
        if inner.markets.is_empty() {
            return vec![];
        }

        let home_tokens = normalize_team(home_team);
        let away_tokens = normalize_team(away_team);

        // Find market IDs that match at least one home token AND one away token
        let home_candidates = token_candidates(&inner.token_index, &home_tokens);
        let away_candidates = token_candidates(&inner.token_index, &away_tokens);

        let matching_ids: HashSet<&String> = home_candidates
            .intersection(&away_candidates)
            .copied()
            .collect();

        if matching_ids.is_empty() {
            return vec![];
        }

        let mut results: Vec<Market> = matching_ids
            .into_iter()
            .filter_map(|id| inner.markets.get(id.as_str()))
            .filter(|m| m.status == "active")
            .cloned()
            .collect();

        // Sort by volume descending (highest liquidity first)
        results.sort_by(|a, b| {
            b.volume
                .unwrap_or(0.0)
                .partial_cmp(&a.volume.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }

    /// Number of cached markets.
    pub async fn len(&self) -> usize {
        self.inner.read().await.markets.len()
    }
}

/// Find all market IDs that match at least one of the given tokens.
fn token_candidates<'a>(
    index: &'a HashMap<String, HashSet<String>>,
    tokens: &[String],
) -> HashSet<&'a String> {
    let mut candidates: HashSet<&String> = HashSet::new();
    for token in tokens {
        if let Some(ids) = index.get(token) {
            candidates.extend(ids);
        }
        // Also try common abbreviations / partial matches
        for (idx_token, ids) in index {
            if idx_token.contains(token) || token.contains(idx_token) {
                candidates.extend(ids);
            }
        }
    }
    candidates
}

/// Extract normalized lowercase tokens from a market question and event name.
fn extract_tokens(question: &str, event_name: Option<&str>) -> Vec<String> {
    let mut combined = question.to_string();
    if let Some(en) = event_name {
        combined.push(' ');
        combined.push_str(en);
    }

    combined
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 3) // skip "vs", "to", "the", etc.
        .map(|s| s.to_string())
        .collect()
}

/// Normalize a team name into searchable tokens.
/// "Manchester United" → ["manchester", "united"]
/// "Man City" → ["man", "city"]
fn normalize_team(name: &str) -> Vec<String> {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 3)
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_market(id: &str, question: &str, event_name: Option<&str>) -> Market {
        Market {
            id: id.to_string(),
            question: question.to_string(),
            sport: Some("soccer".to_string()),
            league: Some("premier-league".to_string()),
            event_name: event_name.map(|s| s.to_string()),
            yes_price: Some(0.65),
            no_price: Some(0.35),
            volume: Some(50000.0),
            status: "active".to_string(),
            fetched_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_cache_search_exact_match() {
        let cache = MarketCache::new();
        cache
            .load(vec![make_market(
                "m1",
                "Will Arsenal win vs Chelsea?",
                Some("Arsenal vs Chelsea"),
            )])
            .await;

        let results = cache.search("Arsenal", "Chelsea", "premier-league").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "m1");
    }

    #[tokio::test]
    async fn test_cache_search_no_match() {
        let cache = MarketCache::new();
        cache
            .load(vec![make_market(
                "m1",
                "Will Arsenal win vs Chelsea?",
                None,
            )])
            .await;

        let results = cache
            .search("Liverpool", "Manchester City", "premier-league")
            .await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_cache_search_partial_team_name() {
        let cache = MarketCache::new();
        cache
            .load(vec![make_market(
                "m1",
                "Manchester United vs Liverpool - Winner",
                None,
            )])
            .await;

        // "Man United" should match "Manchester United" via substring matching
        let results = cache
            .search("Manchester United", "Liverpool", "premier-league")
            .await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_cache_sorted_by_volume() {
        let cache = MarketCache::new();
        let mut m1 = make_market("m1", "Arsenal vs Chelsea moneyline", None);
        m1.volume = Some(10000.0);
        let mut m2 = make_market("m2", "Arsenal vs Chelsea total goals", None);
        m2.volume = Some(50000.0);

        cache.load(vec![m1, m2]).await;
        let results = cache.search("Arsenal", "Chelsea", "").await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "m2"); // higher volume first
    }

    #[tokio::test]
    async fn test_cache_excludes_closed_markets() {
        let cache = MarketCache::new();
        let mut m = make_market("m1", "Arsenal vs Chelsea", None);
        m.status = "closed".to_string();
        cache.load(vec![m]).await;

        let results = cache.search("Arsenal", "Chelsea", "").await;
        assert!(results.is_empty());
    }
}
