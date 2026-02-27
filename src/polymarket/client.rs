use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use tracing::{debug, info};

use crate::db::models::Market;

/// Client for the Polymarket Gamma (markets) API and CLOB (order book) API.
#[derive(Clone)]
pub struct PolymarketClient {
    http: Client,
    api_url: String,
    clob_url: String,
    api_key: Option<String>,
}

impl PolymarketClient {
    pub fn new(api_url: &str, clob_url: &str, api_key: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .connect_timeout(std::time::Duration::from_secs(2))
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(4)
            .build()
            .context("Failed to build HTTP client")?;
        Ok(PolymarketClient {
            http,
            api_url: api_url.to_string(),
            clob_url: clob_url.to_string(),
            api_key,
        })
    }

    /// Search for open markets matching the given teams and league.
    pub async fn search_markets(
        &self,
        home_team: &str,
        away_team: &str,
        league: &str,
    ) -> Result<Vec<Market>> {
        // Build a search query that looks for markets about this match
        let query = format!("{} {} {}", home_team, away_team, league);
        let url = format!(
            "{}/markets?active=true&closed=false&limit=20&q={}",
            self.api_url,
            urlencoding::encode(&query)
        );

        debug!("Searching Polymarket markets: {}", url);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("Polymarket API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Polymarket API error {}: {}", status, body);
        }

        let raw: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Polymarket response")?;

        let markets = parse_markets(&raw, league)?;
        info!(
            "Found {} Polymarket markets for '{} vs {}'",
            markets.len(),
            home_team,
            away_team
        );
        Ok(markets)
    }

    /// Get the current price (0.0–1.0) for an outcome token.
    pub async fn get_token_price(&self, market_id: &str, outcome: &str) -> Result<f64> {
        let raw = self.fetch_market_raw(market_id).await?;
        extract_price(&raw, outcome)
    }

    /// Get both YES/NO prices for a market.
    pub async fn get_market_prices(&self, market_id: &str) -> Result<(Option<f64>, Option<f64>)> {
        let raw = self.fetch_market_raw(market_id).await?;
        Ok(parse_token_prices(&raw))
    }

    /// Resolve token asset ID for a given market outcome ("YES"/"NO").
    pub async fn get_market_asset_id(&self, market_id: &str, outcome: &str) -> Result<String> {
        let raw = self.fetch_market_raw(market_id).await?;
        extract_asset_id(&raw, outcome)
    }

    /// Return resolved market winner outcome ("YES"/"NO") if market is resolved.
    pub async fn get_market_resolved_outcome(&self, market_id: &str) -> Result<Option<String>> {
        let raw = self.fetch_market_raw(market_id).await?;
        Ok(parse_resolved_outcome(&raw))
    }

    /// Place a market order on Polymarket CLOB.
    pub async fn place_order(
        &self,
        market_id: &str,
        outcome: &str,
        size_usd: f64,
        price: f64,
    ) -> Result<String> {
        let api_key = self.api_key.as_deref().unwrap_or_default();

        info!(
            "Placing order: market={}, outcome={}, size=${:.2}, price={:.3}",
            market_id, outcome, size_usd, price
        );

        let order = serde_json::json!({
            "market": market_id,
            "outcome": outcome,
            "price": price,
            "size": size_usd,
            "side": "buy",
            "orderType": "limit",
        });

        let url = format!("{}/order", self.clob_url);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&order)
            .send()
            .await
            .context("Failed to place Polymarket order")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Order placement failed {}: {}", status, body);
        }

        let result: serde_json::Value = resp.json().await?;
        let order_id = result["orderId"].as_str().unwrap_or("unknown").to_string();
        info!("Order placed, id={}", order_id);
        Ok(order_id)
    }

    /// Close/sell an existing position.
    pub async fn close_position(
        &self,
        market_id: &str,
        outcome: &str,
        size_usd: f64,
    ) -> Result<()> {
        let api_key = self.api_key.as_deref().unwrap_or_default();

        info!(
            "Closing position: market={}, outcome={}, size=${:.2}",
            market_id, outcome, size_usd
        );

        let order = serde_json::json!({
            "market": market_id,
            "outcome": outcome,
            "size": size_usd,
            "side": "sell",
            "orderType": "market",
        });

        let url = format!("{}/order", self.clob_url);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&order)
            .send()
            .await
            .context("Failed to close Polymarket position")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Position close failed {}: {}", status, body);
        }

        Ok(())
    }

    /// Fetch all active sports markets from Polymarket (for background market discovery).
    /// All sport tags are fetched **concurrently** to minimise total latency.
    pub async fn fetch_sports_markets(&self) -> Result<Vec<Market>> {
        let sports_tags = [
            "nfl",
            "nba",
            "soccer",
            "mls",
            "premier-league",
            "nhl",
            "mlb",
        ];

        let fetch_futures: Vec<_> = sports_tags
            .iter()
            .map(|tag| {
                let url = format!(
                    "{}/markets?active=true&closed=false&limit=50&tag={}",
                    self.api_url, tag
                );
                let http = self.http.clone();
                let tag = tag.to_string();
                async move {
                    let resp = match http.get(&url).send().await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!("Failed to fetch {} markets: {}", tag, e);
                            return Vec::new();
                        }
                    };
                    if resp.status().is_success() {
                        if let Ok(raw) = resp.json::<serde_json::Value>().await {
                            if let Ok(markets) = parse_markets(&raw, &tag) {
                                return markets;
                            }
                        }
                    }
                    Vec::new()
                }
            })
            .collect();

        let results = futures_util::future::join_all(fetch_futures).await;
        let all_markets: Vec<Market> = results.into_iter().flatten().collect();
        Ok(all_markets)
    }

    async fn fetch_market_raw(&self, market_id: &str) -> Result<serde_json::Value> {
        let url = format!("{}/markets/{}", self.api_url, market_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch market price")?;

        if !resp.status().is_success() {
            anyhow::bail!("Polymarket price fetch error: {}", resp.status());
        }

        let raw: serde_json::Value = resp.json().await?;
        Ok(raw)
    }
}

// ── Parsing helpers ────────────────────────────────────────────────────────────

fn parse_markets(raw: &serde_json::Value, league_hint: &str) -> Result<Vec<Market>> {
    let items = match raw.as_array() {
        Some(a) => a,
        None => {
            // Some endpoints return { "markets": [...] }
            match raw.get("markets").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => return Ok(vec![]),
            }
        }
    };

    let markets: Vec<Market> = items
        .iter()
        .filter_map(|item| {
            let id = item["conditionId"]
                .as_str()
                .or_else(|| item["id"].as_str())?;
            let question = item["question"].as_str().unwrap_or("").to_string();
            let volume = item["volume"]
                .as_f64()
                .or_else(|| item["volume"].as_str().and_then(|s| s.parse().ok()));
            let status = if item["active"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "closed"
            };

            // Parse outcome prices from tokens array
            let (yes_price, no_price) = parse_token_prices(item);

            Some(Market {
                id: id.to_string(),
                question,
                sport: Some(league_hint.to_string()),
                league: Some(league_hint.to_string()),
                event_name: item["description"].as_str().map(str::to_string),
                yes_price,
                no_price,
                volume,
                status: status.to_string(),
                fetched_at: Utc::now(),
                slug: item["slug"].as_str().map(str::to_string),
                end_date: item["endDate"]
                    .as_str()
                    .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok()),
                liquidity: item["liquidity"]
                    .as_f64()
                    .or_else(|| item["liquidity"].as_str().and_then(|s| s.parse().ok())),
            })
        })
        .collect();

    Ok(markets)
}

fn parse_token_prices(item: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    // Polymarket tokens array: [{ "outcome": "Yes", "price": "0.65" }, ...]
    if let Some(tokens) = item["tokens"].as_array() {
        let mut yes_price = None;
        let mut no_price = None;
        for token in tokens {
            let outcome = token["outcome"].as_str().unwrap_or("").to_lowercase();
            let price = token["price"]
                .as_f64()
                .or_else(|| token["price"].as_str().and_then(|s| s.parse().ok()));
            if outcome == "yes" {
                yes_price = price;
            } else if outcome == "no" {
                no_price = price;
            }
        }
        return (yes_price, no_price);
    }

    // Fallback: outcomePrices field
    if let Some(prices) = item["outcomePrices"].as_array() {
        let yes_price = prices.first().and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        let no_price = prices.get(1).and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        return (yes_price, no_price);
    }

    (None, None)
}

fn parse_resolved_outcome(item: &serde_json::Value) -> Option<String> {
    let normalize = |s: &str| match s.trim().to_lowercase().as_str() {
        "yes" | "true" | "1" => Some("YES".to_string()),
        "no" | "false" | "0" => Some("NO".to_string()),
        _ => None,
    };

    // Common top-level resolution fields.
    for key in [
        "resolvedOutcome",
        "resolved_outcome",
        "winner",
        "winningOutcome",
        "outcome",
        "result",
    ] {
        if let Some(outcome) = item.get(key).and_then(|v| v.as_str()).and_then(normalize) {
            return Some(outcome);
        }
    }

    // Token-level winner flag.
    if let Some(tokens) = item.get("tokens").and_then(|v| v.as_array()) {
        for token in tokens {
            let is_winner = token
                .get("winner")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || token
                    .get("isWinner")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            if is_winner {
                if let Some(outcome) = token
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .and_then(normalize)
                {
                    return Some(outcome);
                }
            }
        }
    }

    None
}

fn parse_token_asset_ids(item: &serde_json::Value) -> (Option<String>, Option<String>) {
    // Preferred: tokens array with explicit outcome mapping.
    if let Some(tokens) = item["tokens"].as_array() {
        let mut yes_id = None;
        let mut no_id = None;
        for token in tokens {
            let outcome = token["outcome"].as_str().unwrap_or("").to_lowercase();
            let asset_id = parse_id_field(token, "asset_id")
                .or_else(|| parse_id_field(token, "assetId"))
                .or_else(|| parse_id_field(token, "token_id"))
                .or_else(|| parse_id_field(token, "tokenId"))
                .or_else(|| parse_id_field(token, "clobTokenId"));
            if outcome == "yes" {
                yes_id = asset_id;
            } else if outcome == "no" {
                no_id = asset_id;
            }
        }
        if yes_id.is_some() || no_id.is_some() {
            return (yes_id, no_id);
        }
    }

    // Fallback: some payloads expose token IDs as a 2-element array aligned to
    // outcomes [YES, NO].
    if let Some(ids) = item["clobTokenIds"].as_array() {
        let yes_id = ids.first().and_then(parse_id_value);
        let no_id = ids.get(1).and_then(parse_id_value);
        return (yes_id, no_id);
    }

    (None, None)
}

fn extract_price(raw: &serde_json::Value, outcome: &str) -> Result<f64> {
    let (yes_price, no_price) = parse_token_prices(raw);
    match outcome.to_lowercase().as_str() {
        "yes" => yes_price.context("YES price not found in market data"),
        "no" => no_price.context("NO price not found in market data"),
        _ => anyhow::bail!("Unknown outcome: {}", outcome),
    }
}

fn extract_asset_id(raw: &serde_json::Value, outcome: &str) -> Result<String> {
    let (yes_id, no_id) = parse_token_asset_ids(raw);
    match outcome.to_lowercase().as_str() {
        "yes" => yes_id.context("YES asset id not found in market data"),
        "no" => no_id.context("NO asset id not found in market data"),
        _ => anyhow::bail!("Unknown outcome: {}", outcome),
    }
}

fn parse_id_field(obj: &serde_json::Value, field: &str) -> Option<String> {
    obj.get(field).and_then(parse_id_value)
}

fn parse_id_value(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        if !s.trim().is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(n) = v.as_u64() {
        return Some(n.to_string());
    }
    None
}

// Expose a simple URL encoding without pulling in another dep
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
                ' ' => out.push('+'),
                c => {
                    let bytes = c.to_string();
                    for b in bytes.as_bytes() {
                        out.push_str(&format!("%{:02X}", b));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::parse_resolved_outcome;

    #[test]
    fn parse_markets_extracts_slug_end_date_liquidity() {
        let raw = serde_json::json!([{
            "conditionId": "abc123",
            "question": "Will Arsenal win?",
            "active": true,
            "slug": "will-arsenal-win",
            "endDate": "2025-06-01T00:00:00Z",
            "liquidity": 12345.67,
            "volume": "50000",
            "tokens": [
                {"outcome": "Yes", "price": "0.65"},
                {"outcome": "No",  "price": "0.35"}
            ]
        }]);
        let markets = super::parse_markets(&raw, "soccer").unwrap();
        assert_eq!(markets.len(), 1);
        let m = &markets[0];
        assert_eq!(m.slug.as_deref(), Some("will-arsenal-win"));
        assert!(m.end_date.is_some());
        assert_eq!(m.liquidity, Some(12345.67));
    }

    #[test]
    fn parse_resolved_outcome_from_top_level() {
        let raw = serde_json::json!({ "resolvedOutcome": "Yes" });
        assert_eq!(parse_resolved_outcome(&raw).as_deref(), Some("YES"));
    }

    #[test]
    fn parse_resolved_outcome_from_tokens_winner_flag() {
        let raw = serde_json::json!({
            "tokens": [
                { "outcome": "No", "winner": false },
                { "outcome": "Yes", "winner": true }
            ]
        });
        assert_eq!(parse_resolved_outcome(&raw).as_deref(), Some("YES"));
    }
}
