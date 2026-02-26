use anyhow::Result;
use chrono::Utc;
use std::collections::HashSet;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::models::{Position, ScoreEvent};
use crate::db::{Database, models::LiveGame};
use crate::polymarket::PolymarketClient;

use super::kelly::{edge, kelly_stake};
use super::position::{compute_levels, evaluate_position, PositionAction};

/// The core bot engine.  Runs continuously; evaluates live score events,
/// finds matching Polymarket markets, and manages positions.
pub struct BotEngine {
    config: Config,
    db: Database,
    polymarket: PolymarketClient,
    /// Current simulated balance (USD) in dry-run mode
    balance: f64,
}

impl BotEngine {
    pub fn new(config: Config, db: Database, polymarket: PolymarketClient) -> Result<Self> {
        let balance = db.get_balance()?;
        let balance = if balance <= 0.0 {
            config.initial_balance
        } else {
            balance
        };
        Ok(BotEngine {
            config,
            db,
            polymarket,
            balance,
        })
    }

    /// Determine which team scored by comparing the event type string.
    /// Returns `true` if the home team scored, `false` if away.
    fn home_team_scored(event: &ScoreEvent) -> bool {
        // Event types containing "home" explicitly indicate home scoring
        let et = event.event_type.to_lowercase();
        if et.contains("home") {
            return true;
        }
        if et.contains("away") {
            return false;
        }
        // Fallback: if score differential moved in home's favour relative to
        // a neutral baseline, assume home scored.  This covers generic events.
        // NOTE: We cannot compare to previous score here, but the score monitor
        // tags events with "_home"/"_away" suffixes so the above branches cover
        // the common path.
        event.home_score >= event.away_score
    }

    /// Process a single score-change event.
    ///
    /// 1. Identify which Polymarket market(s) relate to this event.
    /// 2. Compute edge using estimated win probability vs market price.
    /// 3. If edge > min_edge, size a bet with fractional Kelly and open a position.
    pub async fn on_score_event(&mut self, event: &ScoreEvent, game: &LiveGame) -> Result<()> {
        info!(
            "Score event: {} {} {}-{} ({}' {})",
            event.sport, event.league, event.home_score, event.away_score,
            event.minute.unwrap_or(0), event.event_type,
        );

        // Persist the score event
        self.db.insert_score_event(event)?;

        // Collect market IDs that already have open positions to avoid duplicates
        let open_market_ids: HashSet<String> = self
            .db
            .list_open_positions()?
            .into_iter()
            .map(|p| p.market_id)
            .collect();

        // Find candidate markets for this game
        let markets = self
            .polymarket
            .search_markets(&event.home_team, &event.away_team, &event.league)
            .await?;

        if markets.is_empty() {
            info!("No Polymarket markets found for this game");
            return Ok(());
        }

        for market in &markets {
            // Skip markets where we already have an open position
            if open_market_ids.contains(&market.id) {
                info!("Already have open position in '{}', skipping", market.question);
                continue;
            }

            // Upsert market into DB
            self.db.upsert_market(market)?;

            let yes_price = match market.yes_price {
                Some(p) if p > 0.0 && p < 1.0 => p,
                _ => continue,
            };

            // Determine which side to bet based on WHO scored, not just who leads.
            // This correctly handles tied scores (e.g., home scores to make it 1-1:
            // home has momentum, bet YES).
            let home_scored = Self::home_team_scored(event);
            let (bet_yes, true_win_prob) = if home_scored {
                let p = super::position::estimate_win_probability(event, game, true);
                (true, p)
            } else {
                let p = super::position::estimate_win_probability(event, game, false);
                (false, 1.0 - p)
            };

            let price = if bet_yes { yes_price } else { 1.0 - yes_price };
            let bet_edge = edge(true_win_prob, price);

            info!(
                "Market '{}': price={:.3}, true_prob={:.3}, edge={:.3}",
                market.question, price, true_win_prob, bet_edge
            );

            if bet_edge < self.config.min_edge {
                info!("Edge {:.3} below minimum {:.3}, skipping", bet_edge, self.config.min_edge);
                continue;
            }

            // Kelly-size the bet
            let stake_fraction = kelly_stake(true_win_prob, price, self.config.kelly_fraction);
            let stake_usd = self.balance * stake_fraction;

            if stake_usd < 1.0 {
                info!("Stake too small (${:.2}), skipping", stake_usd);
                continue;
            }

            // Guard: never let balance go negative
            if stake_usd > self.balance {
                warn!(
                    "Stake ${:.2} exceeds available balance ${:.2}, skipping",
                    stake_usd, self.balance
                );
                continue;
            }

            let (stop_loss, take_profit) = compute_levels(
                price,
                self.config.stop_loss_fraction,
                self.config.take_profit_fraction,
            );

            let outcome = if bet_yes { "YES" } else { "NO" }.to_string();

            info!(
                "Opening {} position in '{}': stake=${:.2}, entry={:.3}, SL={:.3}, TP={:.3}",
                outcome, market.question, stake_usd, price, stop_loss, take_profit
            );

            if !self.config.dry_run {
                // Live trade: place order on Polymarket
                match self
                    .polymarket
                    .place_order(&market.id, &outcome, stake_usd, price)
                    .await
                {
                    Ok(_) => info!("Order placed successfully"),
                    Err(e) => {
                        error!("Failed to place order: {}", e);
                        continue;
                    }
                }
            } else {
                info!("[DRY RUN] Would place order – no real funds used");
            }

            let pos = Position {
                id: None,
                market_id: market.id.clone(),
                outcome,
                side: "buy".into(),
                size_usd: stake_usd,
                entry_price: price,
                stop_loss_price: stop_loss,
                take_profit_price: take_profit,
                status: "open".into(),
                opened_at: Utc::now(),
                closed_at: None,
                exit_price: None,
                pnl: None,
                dry_run: self.config.dry_run,
                sport: Some(event.sport.clone()),
                league: Some(event.league.clone()),
                event_name: Some(format!("{} vs {}", event.home_team, event.away_team)),
            };

            let _id = self.db.insert_position(&pos)?;
            self.balance -= stake_usd;
            self.db.record_balance(self.balance)?;
        }

        Ok(())
    }

    /// Sweep all open positions and close those that hit stop-loss or take-profit.
    ///
    /// Fetches current prices for ALL open positions concurrently to minimise
    /// latency, then evaluates SL/TP sequentially.
    pub async fn manage_positions(&mut self) -> Result<()> {
        let open = self.db.list_open_positions()?;
        if open.is_empty() {
            return Ok(());
        }

        // Fetch all prices concurrently to cut N sequential round-trips to 1
        let price_futures: Vec<_> = open
            .iter()
            .map(|pos| self.polymarket.get_token_price(&pos.market_id, &pos.outcome))
            .collect();
        let prices = futures_util::future::join_all(price_futures).await;

        for (pos, price_result) in open.into_iter().zip(prices.into_iter()) {
            let pos_id = match pos.id {
                Some(id) => id,
                None => continue,
            };

            let current_price = match price_result {
                Ok(p) => p,
                Err(e) => {
                    warn!("Failed to get price for market {}: {}", pos.market_id, e);
                    continue;
                }
            };

            match evaluate_position(&pos, current_price) {
                PositionAction::TakeProfit { exit_price, pnl } => {
                    info!(
                        "Taking profit on position {}: exit={:.3}, pnl=+${:.2}",
                        pos_id, exit_price, pnl
                    );
                    if !self.config.dry_run {
                        if let Err(e) = self
                            .polymarket
                            .close_position(&pos.market_id, &pos.outcome, pos.size_usd)
                            .await
                        {
                            error!("Failed to close position {}: {}", pos_id, e);
                            continue;
                        }
                    }
                    self.db
                        .close_position(pos_id, "closed_profit", exit_price, pnl)?;
                    self.balance += pos.size_usd + pnl;
                    self.db.record_balance(self.balance)?;
                }
                PositionAction::StopLoss { exit_price, pnl } => {
                    warn!(
                        "Stop-loss triggered on position {}: exit={:.3}, pnl=${:.2}",
                        pos_id, exit_price, pnl
                    );
                    if !self.config.dry_run {
                        if let Err(e) = self
                            .polymarket
                            .close_position(&pos.market_id, &pos.outcome, pos.size_usd)
                            .await
                        {
                            error!("Failed to close position {}: {}", pos_id, e);
                            continue;
                        }
                    }
                    self.db
                        .close_position(pos_id, "closed_stop_loss", exit_price, pnl)?;
                    self.balance += pos.size_usd + pnl; // pnl is negative
                    self.db.record_balance(self.balance)?;
                }
                PositionAction::Hold => {}
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn balance(&self) -> f64 {
        self.balance
    }
}
