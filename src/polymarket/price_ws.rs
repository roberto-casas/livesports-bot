//! Polymarket CLOB WebSocket client for real-time price streaming.
//!
//! Connects to `wss://ws-subscriptions-clob.polymarket.com/ws/market` and
//! receives push updates for subscribed market tokens. This replaces REST
//! polling in `manage_positions()` — prices are always fresh in shared memory.
//!
//! Usage:
//! ```ignore
//! let price_feed = PriceFeed::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");
//! price_feed.subscribe(&["token_id_1", "token_id_2"]).await;
//! let price = price_feed.get_price("token_id_1").await; // instant, no network
//! ```

#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

/// Real-time price feed from Polymarket CLOB WebSocket.
///
/// Maintains a shared price map updated by a background WebSocket task.
/// Price lookups are instant reads from shared memory.
pub struct PriceFeed {
    /// asset_id → best_bid price (0.0–1.0)
    prices: Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    /// Channel to send subscription requests to the background task
    subscribe_tx: mpsc::Sender<SubscriptionRequest>,
}

/// A snapshot of the best bid/ask for a token.
#[derive(Debug, Clone)]
pub struct PriceSnapshot {
    pub best_bid: f64,
    pub best_ask: f64,
    /// Midpoint price: (best_bid + best_ask) / 2
    pub mid_price: f64,
    pub last_updated_ms: u64,
}

enum SubscriptionRequest {
    Subscribe(Vec<String>),
    Unsubscribe(Vec<String>),
}

impl PriceFeed {
    /// Create a new PriceFeed and spawn the background WebSocket listener.
    pub fn new(ws_url: &str) -> Self {
        let prices: Arc<RwLock<HashMap<String, PriceSnapshot>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (subscribe_tx, subscribe_rx) = mpsc::channel(64);

        let prices_clone = Arc::clone(&prices);
        let ws_url = ws_url.to_string();

        tokio::spawn(async move {
            price_ws_loop(&ws_url, prices_clone, subscribe_rx).await;
        });

        PriceFeed {
            prices,
            subscribe_tx,
        }
    }

    /// Subscribe to price updates for the given asset IDs (token IDs).
    pub async fn subscribe(&self, asset_ids: &[&str]) {
        let ids: Vec<String> = asset_ids.iter().map(|s| s.to_string()).collect();
        let _ = self
            .subscribe_tx
            .send(SubscriptionRequest::Subscribe(ids))
            .await;
    }

    /// Get the latest price for an asset. Returns `None` if we haven't received
    /// any data for this asset yet.
    pub async fn get_price(&self, asset_id: &str) -> Option<PriceSnapshot> {
        let prices = self.prices.read().await;
        prices.get(asset_id).cloned()
    }

    /// Get the mid-price for an asset, or `None` if unavailable.
    pub async fn get_mid_price(&self, asset_id: &str) -> Option<f64> {
        self.get_price(asset_id).await.map(|p| p.mid_price)
    }
}

/// Background WebSocket connection loop for Polymarket CLOB prices.
async fn price_ws_loop(
    ws_url: &str,
    prices: Arc<RwLock<HashMap<String, PriceSnapshot>>>,
    mut subscribe_rx: mpsc::Receiver<SubscriptionRequest>,
) {
    let mut backoff_secs = 1u64;
    let max_backoff = 30u64;
    // Accumulate subscriptions across reconnects
    let mut subscribed_ids: Vec<String> = Vec::new();

    loop {
        info!("[PriceFeed] Connecting to Polymarket CLOB WS: {}", ws_url);

        match tokio_tungstenite::connect_async(ws_url).await {
            Ok((ws_stream, _)) => {
                info!("[PriceFeed] Connected");
                backoff_secs = 1;

                let (mut write, mut read) = ws_stream.split();

                // Re-subscribe to previously tracked assets after reconnect
                if !subscribed_ids.is_empty() {
                    let sub_msg = build_subscribe_message(&subscribed_ids);
                    if let Err(e) = write.send(Message::Text(sub_msg)).await {
                        error!("[PriceFeed] Re-subscribe failed: {}", e);
                        continue;
                    }
                    info!(
                        "[PriceFeed] Re-subscribed to {} assets",
                        subscribed_ids.len()
                    );
                }

                let mut ping_interval =
                    tokio::time::interval(std::time::Duration::from_secs(25));

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    parse_and_update_prices(&text, &prices).await;
                                }
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => {
                                    warn!("[PriceFeed] Server closed connection");
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("[PriceFeed] WS error: {}", e);
                                    break;
                                }
                                None => {
                                    warn!("[PriceFeed] Stream ended");
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Some(req) = subscribe_rx.recv() => {
                            match req {
                                SubscriptionRequest::Subscribe(ids) => {
                                    subscribed_ids.extend(ids.clone());
                                    subscribed_ids.sort();
                                    subscribed_ids.dedup();
                                    let sub_msg = build_subscribe_message(&ids);
                                    if let Err(e) = write.send(Message::Text(sub_msg)).await {
                                        error!("[PriceFeed] Subscribe send failed: {}", e);
                                    }
                                }
                                SubscriptionRequest::Unsubscribe(ids) => {
                                    subscribed_ids.retain(|id| !ids.contains(id));
                                    let unsub_msg = build_unsubscribe_message(&ids);
                                    if let Err(e) = write.send(Message::Text(unsub_msg)).await {
                                        error!("[PriceFeed] Unsubscribe send failed: {}", e);
                                    }
                                }
                            }
                        }
                        _ = ping_interval.tick() => {
                            if let Err(e) = write.send(Message::Ping(vec![])).await {
                                error!("[PriceFeed] Ping failed: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("[PriceFeed] Connection failed: {}", e);
            }
        }

        warn!("[PriceFeed] Reconnecting in {}s...", backoff_secs);
        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(max_backoff);
    }
}

fn build_subscribe_message(asset_ids: &[String]) -> String {
    serde_json::json!({
        "assets_ids": asset_ids,
        "type": "market",
        "custom_feature_enabled": true
    })
    .to_string()
}

fn build_unsubscribe_message(asset_ids: &[String]) -> String {
    serde_json::json!({
        "assets_ids": asset_ids,
        "type": "market",
        "operation": "unsubscribe"
    })
    .to_string()
}

/// Parse a CLOB WS message and update the shared price map.
///
/// Event types we care about:
/// - `price_change`: individual price level updates with best_bid/best_ask
/// - `best_bid_ask`: direct best bid/ask update
/// - `book`: full orderbook snapshot
async fn parse_and_update_prices(
    text: &str,
    prices: &Arc<RwLock<HashMap<String, PriceSnapshot>>>,
) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    let event_type = val.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
    let timestamp = val
        .get("timestamp")
        .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()))
        .unwrap_or(0);

    match event_type {
        "price_change" => {
            if let Some(changes) = val.get("price_changes").and_then(|v| v.as_array()) {
                let mut price_map = prices.write().await;
                for change in changes {
                    if let Some(asset_id) = change.get("asset_id").and_then(|v| v.as_str()) {
                        let best_bid = parse_price_field(change, "best_bid");
                        let best_ask = parse_price_field(change, "best_ask");
                        if best_bid > 0.0 || best_ask > 0.0 {
                            let mid = if best_bid > 0.0 && best_ask > 0.0 {
                                (best_bid + best_ask) / 2.0
                            } else if best_bid > 0.0 {
                                best_bid
                            } else {
                                best_ask
                            };
                            price_map.insert(
                                asset_id.to_string(),
                                PriceSnapshot {
                                    best_bid,
                                    best_ask,
                                    mid_price: mid,
                                    last_updated_ms: timestamp,
                                },
                            );
                        }
                    }
                }
            }
        }
        "best_bid_ask" => {
            if let Some(changes) = val.get("changes").and_then(|v| v.as_array()) {
                let mut price_map = prices.write().await;
                for change in changes {
                    if let Some(asset_id) = change.get("asset_id").and_then(|v| v.as_str()) {
                        let best_bid = parse_price_field(change, "best_bid");
                        let best_ask = parse_price_field(change, "best_ask");
                        let mid = if best_bid > 0.0 && best_ask > 0.0 {
                            (best_bid + best_ask) / 2.0
                        } else if best_bid > 0.0 {
                            best_bid
                        } else {
                            best_ask
                        };
                        price_map.insert(
                            asset_id.to_string(),
                            PriceSnapshot {
                                best_bid,
                                best_ask,
                                mid_price: mid,
                                last_updated_ms: timestamp,
                            },
                        );
                    }
                }
            }
        }
        "book" => {
            // Full book snapshot — extract best bid/ask from the top of book
            if let Some(bids) = val.get("bids").and_then(|v| v.as_array()) {
                if let Some(asks) = val.get("asks").and_then(|v| v.as_array()) {
                    let asset_id = val.get("asset_id").and_then(|v| v.as_str());
                    if let Some(asset_id) = asset_id {
                        let best_bid = bids
                            .first()
                            .and_then(|b| parse_price_field_from_val(b, "price"))
                            .unwrap_or(0.0);
                        let best_ask = asks
                            .first()
                            .and_then(|a| parse_price_field_from_val(a, "price"))
                            .unwrap_or(0.0);
                        let mid = if best_bid > 0.0 && best_ask > 0.0 {
                            (best_bid + best_ask) / 2.0
                        } else {
                            0.0
                        };
                        let mut price_map = prices.write().await;
                        price_map.insert(
                            asset_id.to_string(),
                            PriceSnapshot {
                                best_bid,
                                best_ask,
                                mid_price: mid,
                                last_updated_ms: timestamp,
                            },
                        );
                    }
                }
            }
        }
        _ => {} // Ignore tick_size_change, new_market, market_resolved, etc.
    }
}

fn parse_price_field(val: &serde_json::Value, field: &str) -> f64 {
    val.get(field)
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0.0)
}

fn parse_price_field_from_val(val: &serde_json::Value, field: &str) -> Option<f64> {
    val.get(field).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}
