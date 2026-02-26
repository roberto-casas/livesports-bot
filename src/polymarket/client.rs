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
    pub fn new(
        api_url: &str,
        clob_url: &str,
        api_key: Option<String>,
    ) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
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

        let raw: serde_json::Value = resp.json().await.context("Failed to parse Polymarket response")?;

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
        extract_price(&raw, outcome)
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
        let order_id = result["orderId"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
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
    pub async fn fetch_sports_markets(&self) -> Result<Vec<Market>> {
        // Polymarket sports tag IDs (these are approximate; adjust as needed)
        let sports_tags = ["nfl", "nba", "soccer", "mls", "premier-league", "nhl", "mlb"];

        let mut all_markets = Vec::new();

        for tag in &sports_tags {
            let url = format!(
                "{}/markets?active=true&closed=false&limit=50&tag={}",
                self.api_url, tag
            );

            let resp = match self.http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Failed to fetch {} markets: {}", tag, e);
                    continue;
                }
            };

            if resp.status().is_success() {
                if let Ok(raw) = resp.json::<serde_json::Value>().await {
                    if let Ok(markets) = parse_markets(&raw, tag) {
                        all_markets.extend(markets);
                    }
                }
            }
        }

        Ok(all_markets)
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
            let id = item["conditionId"].as_str().or_else(|| item["id"].as_str())?;
            let question = item["question"].as_str().unwrap_or("").to_string();
            let volume = item["volume"].as_f64().or_else(|| {
                item["volume"].as_str().and_then(|s| s.parse().ok())
            });
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
            v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        let no_price = prices.get(1).and_then(|v| {
            v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        return (yes_price, no_price);
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
