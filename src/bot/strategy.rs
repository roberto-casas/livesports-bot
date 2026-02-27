use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::models::{Market, Position, ScoreEvent};
use crate::db::{models::LiveGame, Database, ModelCalibration};
use crate::polymarket::{MarketCache, PolymarketClient, PriceFeed};

use super::calibration::{apply_platt, fit_platt, PlattCalibration};
use super::kelly::{edge, kelly_stake};
use super::position::{compute_levels, evaluate_position, PositionAction};

/// The core bot engine.  Runs continuously; evaluates live score events,
/// finds matching Polymarket markets, and manages positions.
pub struct BotEngine {
    config: Config,
    db: Database,
    polymarket: PolymarketClient,
    /// Pre-loaded market cache — searched first on score events (sub-μs).
    /// Falls through to REST API on cache miss.
    market_cache: MarketCache,
    /// Current simulated balance (USD) in dry-run mode
    balance: f64,
    /// Last observed YES price per market for latency alpha gating.
    last_yes_price: HashMap<String, f64>,
    /// Rolling per-sport latency/price-reaction telemetry.
    latency_stats: HashMap<String, LatencyStats>,
    /// Daily circuit-breaker state (UTC day).
    daily_risk: DailyRiskState,
    /// Outcome token asset IDs per (market, outcome), used for WS pricing.
    outcome_asset_ids: HashMap<(String, String), String>,
    /// Tracks which assets were already subscribed on the CLOB WS feed.
    subscribed_asset_ids: HashSet<String>,
    /// Real-time CLOB price stream.
    price_feed: PriceFeed,
    /// Feed-health state for entry circuit-breaker decisions.
    feed_health: FeedHealthState,
    /// Recent event keys for cross-provider duplicate suppression.
    recent_event_keys: HashMap<String, DateTime<Utc>>,
    /// Last observed score per provider event ID.
    last_score_by_event: HashMap<String, (i32, i32, DateTime<Utc>)>,
    /// Per-sport Platt calibration models.
    probability_calibrations: HashMap<String, PlattCalibration>,
}

#[derive(Debug, Clone, Default)]
struct LatencyStats {
    samples: u64,
    ewma_processing_ms: f64,
    ewma_priced_in_ratio: f64,
    ewma_residual_move: f64,
}

#[derive(Debug, Clone)]
struct DailyRiskState {
    day: NaiveDate,
    day_start_equity: f64,
    trades_today: u32,
}

#[derive(Debug, Clone, Default)]
struct FeedHealthState {
    samples: u64,
    ewma_rest_fallback_rate: f64,
    ewma_ws_age_ms: f64,
    block_entries_until: Option<DateTime<Utc>>,
    degraded_since: Option<DateTime<Utc>>,
}

impl BotEngine {
    pub fn new(
        config: Config,
        db: Database,
        polymarket: PolymarketClient,
        market_cache: MarketCache,
    ) -> Result<Self> {
        let balance = db.get_balance()?;
        let balance = if balance <= 0.0 {
            config.initial_balance
        } else {
            balance
        };
        let open_positions = db.list_open_positions()?;
        let open_notional: f64 = open_positions.iter().map(|p| p.size_usd).sum();
        let equity_now = balance + open_notional;
        let today = Utc::now().date_naive();
        let day_start = Self::day_start_utc(today);
        let day_start_equity = db
            .first_balance_on_or_after(day_start)?
            .unwrap_or(equity_now);
        let trades_today = db.count_positions_opened_since(day_start)?;
        let probability_calibrations = db
            .load_model_calibrations()?
            .into_iter()
            .map(|c| {
                (
                    Self::normalize_sport_key(&c.sport),
                    PlattCalibration { a: c.a, b: c.b },
                )
            })
            .collect::<HashMap<_, _>>();
        let price_feed = PriceFeed::new(&config.polymarket_ws_url);
        if !probability_calibrations.is_empty() {
            info!(
                "Loaded {} probability calibration model(s)",
                probability_calibrations.len()
            );
        }
        Ok(BotEngine {
            config,
            db,
            polymarket,
            market_cache,
            balance,
            last_yes_price: HashMap::new(),
            latency_stats: HashMap::new(),
            daily_risk: DailyRiskState {
                day: today,
                day_start_equity,
                trades_today,
            },
            outcome_asset_ids: HashMap::new(),
            subscribed_asset_ids: HashSet::new(),
            price_feed,
            feed_health: FeedHealthState::default(),
            recent_event_keys: HashMap::new(),
            last_score_by_event: HashMap::new(),
            probability_calibrations,
        })
    }

    /// Minimum absolute probability shift required to treat a score event as
    /// materially important for pricing.
    fn probability_delta_threshold(sport: &str) -> f64 {
        match sport {
            "soccer" | "football" | "football_eu" => 0.04,
            "american_football" | "nfl" => 0.03,
            "basketball" | "nba" => 0.015,
            "baseball" | "mlb" => 0.025,
            "ice_hockey" | "nhl" => 0.025,
            "tennis" => 0.05,
            _ => 0.03,
        }
    }

    fn normalize_sport_key(sport: &str) -> String {
        sport.trim().to_lowercase()
    }

    fn calibrate_probability(&self, sport: &str, raw_prob: f64) -> f64 {
        let key = Self::normalize_sport_key(sport);
        if let Some(model) = self.probability_calibrations.get(&key).copied() {
            apply_platt(raw_prob, model)
        } else {
            raw_prob.clamp(0.0, 1.0)
        }
    }

    pub async fn retrain_probability_calibration(&mut self) -> Result<()> {
        if !self.config.calibration_enabled {
            return Ok(());
        }
        let candidates = self.db.list_calibration_candidates()?;
        if candidates.is_empty() {
            return Ok(());
        }

        let mut unique_market_ids: HashSet<String> = HashSet::new();
        for c in &candidates {
            unique_market_ids.insert(c.market_id.clone());
        }
        let polymarket = self.polymarket.clone();
        let outcome_futures: Vec<_> = unique_market_ids
            .into_iter()
            .map(|market_id| {
                let polymarket = polymarket.clone();
                async move {
                    let resolved = polymarket
                        .get_market_resolved_outcome(&market_id)
                        .await
                        .ok()
                        .flatten();
                    (market_id, resolved)
                }
            })
            .collect();
        let mut resolved_by_market = HashMap::new();
        for (market_id, resolved) in futures_util::future::join_all(outcome_futures).await {
            if let Some(outcome) = resolved {
                resolved_by_market.insert(market_id, outcome);
            }
        }
        if resolved_by_market.is_empty() {
            info!("Calibration skipped: no resolved market outcomes available");
            return Ok(());
        }

        let mut grouped: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        let mut unresolved_rows = 0usize;
        for row in candidates {
            if !(0.0..=1.0).contains(&row.model_prob_raw) {
                continue;
            }
            let Some(resolved_outcome) = resolved_by_market.get(&row.market_id) else {
                unresolved_rows += 1;
                continue;
            };
            let label = if row.outcome.eq_ignore_ascii_case(resolved_outcome) {
                1.0
            } else {
                0.0
            };
            grouped
                .entry(Self::normalize_sport_key(&row.sport))
                .or_default()
                .push((row.model_prob_raw, label));
        }
        if unresolved_rows > 0 {
            info!(
                "Calibration skipped {} trade rows without resolved market outcomes",
                unresolved_rows
            );
        }

        for (sport, sport_samples) in grouped {
            if sport_samples.len() < self.config.calibration_min_samples_per_sport {
                continue;
            }
            let Some(fit) = fit_platt(
                &sport_samples,
                self.config.calibration_max_iters,
                self.config.calibration_learning_rate,
                self.config.calibration_l2,
            ) else {
                continue;
            };
            let ll_before = fit.metrics.logloss_before.max(1e-9);
            let br_before = fit.metrics.brier_before.max(1e-9);
            let ll_improvement =
                ((fit.metrics.logloss_before - fit.metrics.logloss_after) / ll_before).max(0.0);
            let br_improvement =
                ((fit.metrics.brier_before - fit.metrics.brier_after) / br_before).max(0.0);

            if ll_improvement < self.config.calibration_min_relative_improvement
                && br_improvement < self.config.calibration_min_relative_improvement
            {
                info!(
                    "Calibration candidate rejected for {}: rel_improve logloss={:.4}, brier={:.4}",
                    sport, ll_improvement, br_improvement
                );
                continue;
            }

            let model = ModelCalibration {
                sport: sport.clone(),
                a: fit.calibration.a,
                b: fit.calibration.b,
                samples: sport_samples.len() as i64,
                logloss_before: fit.metrics.logloss_before,
                logloss_after: fit.metrics.logloss_after,
                brier_before: fit.metrics.brier_before,
                brier_after: fit.metrics.brier_after,
                fitted_at: Utc::now(),
            };
            self.db.upsert_model_calibration(&model)?;
            self.probability_calibrations.insert(
                sport.clone(),
                PlattCalibration {
                    a: model.a,
                    b: model.b,
                },
            );
            info!(
                "Calibration promoted for {}: samples={}, a={:.4}, b={:.4}, logloss {:.4}->{:.4}, brier {:.4}->{:.4}",
                sport,
                model.samples,
                model.a,
                model.b,
                model.logloss_before,
                model.logloss_after,
                model.brier_before,
                model.brier_after
            );
        }
        Ok(())
    }

    /// Add-on to the minimum probability shift requirement for lower-confidence
    /// external score snapshots.
    fn score_event_quality_shift_addon(event: &ScoreEvent) -> f64 {
        let consensus = event.provider_consensus_count.unwrap_or(1);
        let mut addon = match consensus {
            c if c >= 3 => 0.0,
            2 => 0.005,
            _ => 0.015,
        };

        // Non-primary providers get a small additional conservatism buffer.
        if let Some(src) = event.source_provider.as_deref() {
            if !src.to_lowercase().contains("polymarket") {
                addon += 0.003;
            }
        }
        addon
    }

    fn normalize_text(s: &str) -> String {
        s.to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn event_teams(event_name: &str) -> Vec<String> {
        event_name
            .split(" vs ")
            .map(Self::normalize_text)
            .filter(|t| !t.is_empty())
            .collect()
    }

    fn position_teams(pos: &Position) -> Vec<String> {
        pos.event_name
            .as_deref()
            .map(Self::event_teams)
            .unwrap_or_default()
    }

    fn teams_overlap(a: &[String], b: &[String]) -> bool {
        a.iter().any(|ta| b.iter().any(|tb| ta == tb))
    }

    fn pairwise_correlation(
        &self,
        market_id_a: &str,
        sport_a: Option<&str>,
        league_a: Option<&str>,
        event_a: Option<&str>,
        teams_a: &[String],
        market_id_b: &str,
        sport_b: Option<&str>,
        league_b: Option<&str>,
        event_b: Option<&str>,
        teams_b: &[String],
    ) -> f64 {
        if market_id_a == market_id_b {
            return self.config.correlation_same_event;
        }
        if event_a.is_some() && event_b.is_some() && event_a == event_b {
            return self.config.correlation_same_event;
        }
        if !teams_a.is_empty() && !teams_b.is_empty() && Self::teams_overlap(teams_a, teams_b) {
            return self.config.correlation_same_team;
        }
        if league_a.is_some() && league_b.is_some() && league_a == league_b {
            return self.config.correlation_same_league;
        }
        if sport_a.is_some() && sport_b.is_some() && sport_a == sport_b {
            return self.config.correlation_same_sport;
        }
        0.0
    }

    fn effective_exposure_fraction_with_candidate(
        &self,
        open_positions: &[Position],
        candidate_market_id: &str,
        candidate_sport: Option<&str>,
        candidate_league: Option<&str>,
        candidate_event_name: Option<&str>,
        candidate_stake_usd: f64,
        total_equity: f64,
    ) -> f64 {
        #[derive(Clone)]
        struct Node {
            market_id: String,
            sport: Option<String>,
            league: Option<String>,
            event_name: Option<String>,
            teams: Vec<String>,
            weight: f64,
        }

        if total_equity <= 0.0 {
            return 0.0;
        }

        let mut nodes: Vec<Node> = open_positions
            .iter()
            .map(|p| Node {
                market_id: p.market_id.clone(),
                sport: p.sport.clone(),
                league: p.league.clone(),
                event_name: p.event_name.clone(),
                teams: Self::position_teams(p),
                weight: p.size_usd / total_equity,
            })
            .collect();

        nodes.push(Node {
            market_id: candidate_market_id.to_string(),
            sport: candidate_sport.map(ToString::to_string),
            league: candidate_league.map(ToString::to_string),
            event_name: candidate_event_name.map(ToString::to_string),
            teams: candidate_event_name
                .map(Self::event_teams)
                .unwrap_or_default(),
            weight: candidate_stake_usd / total_equity,
        });

        let mut variance = 0.0f64;
        for (i, a) in nodes.iter().enumerate() {
            for (j, b) in nodes.iter().enumerate() {
                let rho = if i == j {
                    1.0
                } else {
                    self.pairwise_correlation(
                        &a.market_id,
                        a.sport.as_deref(),
                        a.league.as_deref(),
                        a.event_name.as_deref(),
                        &a.teams,
                        &b.market_id,
                        b.sport.as_deref(),
                        b.league.as_deref(),
                        b.event_name.as_deref(),
                        &b.teams,
                    )
                };
                variance += a.weight * b.weight * rho;
            }
        }

        variance.max(0.0).sqrt()
    }

    fn contains_team(text: &str, team: &str) -> bool {
        let team_norm = Self::normalize_text(team);
        if team_norm.is_empty() {
            return false;
        }
        if text.contains(&team_norm) {
            return true;
        }
        // Fallback to first significant token for abbreviations.
        team_norm
            .split_whitespace()
            .find(|t| t.len() >= 4)
            .is_some_and(|token| text.contains(token))
    }

    /// Infer whether YES corresponds to the home team winning.
    /// Returns:
    /// - Some(true): YES = home wins
    /// - Some(false): YES = away wins
    /// - None: ambiguous or non-winner market
    fn infer_yes_is_home(market: &Market, home_team: &str, away_team: &str) -> Option<bool> {
        let question = Self::normalize_text(&market.question);

        // Skip non moneyline/winner markets to avoid semantic mismatches.
        let reject_keywords = [
            "over",
            "under",
            "total",
            "spread",
            "handicap",
            "points",
            "goals",
            "corners",
            "cards",
            "player",
            "first",
            "next",
            "race",
            "exact score",
            "both teams",
            "clean sheet",
        ];
        if reject_keywords.iter().any(|k| question.contains(k)) {
            return None;
        }

        let winner_keywords = ["win", "winner", "beat", "beats", "moneyline"];
        if !winner_keywords.iter().any(|k| question.contains(k)) {
            return None;
        }

        let home_in_q = Self::contains_team(&question, home_team);
        let away_in_q = Self::contains_team(&question, away_team);
        match (home_in_q, away_in_q) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            (false, false) => None,
            (true, true) => {
                let home_q = Self::normalize_text(home_team);
                let away_q = Self::normalize_text(away_team);
                let yes_home_patterns = [
                    format!("will {} win", home_q),
                    format!("{} to win", home_q),
                    format!("{} wins", home_q),
                    format!("{} beat", home_q),
                ];
                if yes_home_patterns.iter().any(|p| question.contains(p)) {
                    return Some(true);
                }
                let yes_away_patterns = [
                    format!("will {} win", away_q),
                    format!("{} to win", away_q),
                    format!("{} wins", away_q),
                    format!("{} beat", away_q),
                ];
                if yes_away_patterns.iter().any(|p| question.contains(p)) {
                    return Some(false);
                }
                None
            }
        }
    }

    fn round_trip_cost_edge(&self) -> f64 {
        let one_way_bps = self.config.expected_fee_bps + self.config.expected_slippage_bps;
        2.0 * one_way_bps / 10_000.0
    }

    fn position_net_pnl(pos: &Position, current_price: f64) -> f64 {
        let shares = pos.size_usd / pos.entry_price;
        let gross = shares * current_price - pos.size_usd;
        let estimated_cost = pos.size_usd * (pos.estimated_round_trip_cost_bps / 10_000.0);
        gross - estimated_cost
    }

    fn estimated_round_trip_cost_bps(&self) -> f64 {
        2.0 * (self.config.expected_fee_bps + self.config.expected_slippage_bps)
    }

    fn liquidity_edge_buffer(volume: Option<f64>) -> f64 {
        match volume.unwrap_or(0.0) {
            v if v >= 500_000.0 => 0.0,
            v if v >= 100_000.0 => 0.005,
            v if v >= 25_000.0 => 0.01,
            _ => 0.02,
        }
    }

    fn update_latency_stats(
        &mut self,
        sport: &str,
        processing_ms: f64,
        priced_in_ratio: f64,
        residual_move: f64,
    ) {
        const EWMA_ALPHA: f64 = 0.2;
        let stats = self.latency_stats.entry(sport.to_string()).or_default();
        if stats.samples == 0 {
            stats.ewma_processing_ms = processing_ms;
            stats.ewma_priced_in_ratio = priced_in_ratio;
            stats.ewma_residual_move = residual_move;
        } else {
            stats.ewma_processing_ms =
                (1.0 - EWMA_ALPHA) * stats.ewma_processing_ms + EWMA_ALPHA * processing_ms;
            stats.ewma_priced_in_ratio =
                (1.0 - EWMA_ALPHA) * stats.ewma_priced_in_ratio + EWMA_ALPHA * priced_in_ratio;
            stats.ewma_residual_move =
                (1.0 - EWMA_ALPHA) * stats.ewma_residual_move + EWMA_ALPHA * residual_move;
        }
        stats.samples += 1;
        if stats.samples % 100 == 0 {
            info!(
                "LatencyStats [{}] samples={} ewma_ms={:.0} ewma_priced_in={:.3} ewma_residual={:.3}",
                sport,
                stats.samples,
                stats.ewma_processing_ms,
                stats.ewma_priced_in_ratio,
                stats.ewma_residual_move
            );
        }
    }

    fn adaptive_latency_gate(&self, sport: &str) -> (f64, f64, f64) {
        let mut max_age_ms = self.config.latency_max_score_age_ms as f64;
        let mut min_residual = self.config.latency_min_residual_move;
        let mut max_priced_in = self.config.latency_max_priced_in_ratio;
        if let Some(stats) = self.latency_stats.get(sport) {
            if stats.samples >= 20 {
                if stats.ewma_priced_in_ratio > 0.90 {
                    max_age_ms *= 0.80;
                    min_residual += 0.005;
                    max_priced_in = (max_priced_in - 0.10).max(0.35);
                } else if stats.ewma_priced_in_ratio < 0.50
                    && stats.ewma_residual_move > self.config.latency_min_residual_move * 1.3
                {
                    max_age_ms *= 1.10;
                    max_priced_in = (max_priced_in + 0.05).min(1.20);
                }
                if stats.ewma_processing_ms > self.config.latency_max_score_age_ms as f64 {
                    max_age_ms *= 0.90;
                }
            }
        }
        (max_age_ms, min_residual, max_priced_in)
    }

    fn adaptive_edge_addon(&self, sport: &str) -> f64 {
        let mut priced_in_ratio = 0.7;
        let mut residual_move = self.config.latency_min_residual_move;
        if let Some(stats) = self.latency_stats.get(sport) {
            if stats.samples >= 10 {
                priced_in_ratio = stats.ewma_priced_in_ratio;
                residual_move = stats.ewma_residual_move;
            }
        }
        let fallback_rate = if self.feed_health.samples >= self.config.feed_health_min_samples {
            self.feed_health.ewma_rest_fallback_rate
        } else {
            0.0
        };
        let ws_age_ms = if self.feed_health.samples >= self.config.feed_health_min_samples {
            self.feed_health.ewma_ws_age_ms
        } else {
            0.0
        };
        Self::compute_adaptive_edge_addon(
            priced_in_ratio,
            residual_move,
            fallback_rate,
            ws_age_ms,
            self.config.latency_min_residual_move,
            self.config.ws_price_max_age_ms as f64,
            self.config.adaptive_min_edge_max_addon,
        )
    }

    fn adaptive_divergence_limit(&self, sport: &str) -> f64 {
        let fallback_rate = if self.feed_health.samples >= self.config.feed_health_min_samples {
            self.feed_health.ewma_rest_fallback_rate
        } else {
            0.0
        };
        let priced_in_ratio = self
            .latency_stats
            .get(sport)
            .filter(|s| s.samples >= 20)
            .map(|s| s.ewma_priced_in_ratio)
            .unwrap_or(0.7);
        Self::compute_adaptive_divergence_limit(
            self.config.max_entry_quote_divergence,
            self.config.adaptive_divergence_tightening,
            fallback_rate,
            priced_in_ratio,
        )
    }

    fn update_feed_health(
        &mut self,
        open_positions: usize,
        rest_fallback_count: usize,
        avg_ws_age_ms: f64,
    ) {
        if open_positions == 0 {
            return;
        }
        let fallback_rate = rest_fallback_count as f64 / open_positions as f64;
        const ALPHA: f64 = 0.2;
        if self.feed_health.samples == 0 {
            self.feed_health.ewma_rest_fallback_rate = fallback_rate;
            self.feed_health.ewma_ws_age_ms = avg_ws_age_ms;
        } else {
            self.feed_health.ewma_rest_fallback_rate =
                (1.0 - ALPHA) * self.feed_health.ewma_rest_fallback_rate + ALPHA * fallback_rate;
            self.feed_health.ewma_ws_age_ms =
                (1.0 - ALPHA) * self.feed_health.ewma_ws_age_ms + ALPHA * avg_ws_age_ms;
        }
        self.feed_health.samples += 1;

        if self.feed_health.samples >= self.config.feed_health_min_samples
            && Self::should_trip_feed_health_breaker(
                self.feed_health.ewma_rest_fallback_rate,
                self.feed_health.ewma_ws_age_ms,
                self.config.feed_health_max_rest_fallback_rate,
                self.config.feed_health_max_ws_age_ms,
            )
        {
            if self.feed_health.degraded_since.is_none() {
                self.feed_health.degraded_since = Some(Utc::now());
            }
            let until = Utc::now()
                + chrono::Duration::seconds(self.config.feed_health_cooldown_secs as i64);
            self.feed_health.block_entries_until = Some(until);
            warn!(
                "Feed-health breaker triggered: ewma_fallback={:.3}, ewma_ws_age_ms={:.0}, blocking entries until {}",
                self.feed_health.ewma_rest_fallback_rate,
                self.feed_health.ewma_ws_age_ms,
                until,
            );
        } else {
            self.feed_health.degraded_since = None;
        }
    }

    fn feed_health_blocking_entries(&self) -> bool {
        self.feed_health
            .block_entries_until
            .is_some_and(|until| until > Utc::now())
    }

    fn cleanup_event_quality_maps(&mut self) {
        let ttl = chrono::Duration::seconds(self.config.score_event_dedup_window_secs as i64 * 3);
        let cutoff = Utc::now() - ttl;
        self.recent_event_keys.retain(|_, ts| *ts >= cutoff);
        self.last_score_by_event
            .retain(|_, (_, _, ts)| *ts >= cutoff);
        if self.recent_event_keys.len() > 100_000 {
            self.recent_event_keys.clear();
        }
        if self.last_score_by_event.len() > 100_000 {
            self.last_score_by_event.clear();
        }
    }

    fn event_dedup_key(event: &ScoreEvent) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            event.event_id,
            event.home_score,
            event.away_score,
            event.minute.unwrap_or(-1),
            event.event_type,
            event.league
        )
    }

    fn should_skip_event(&mut self, event: &ScoreEvent) -> bool {
        self.cleanup_event_quality_maps();
        let now = Utc::now();
        let dedup_window =
            chrono::Duration::seconds(self.config.score_event_dedup_window_secs as i64);
        let key = Self::event_dedup_key(event);

        if let Some(prev) = self.recent_event_keys.get(&key) {
            if *prev + dedup_window >= now {
                info!(
                    "Skipping duplicate score event within dedup window: {}",
                    key
                );
                return true;
            }
        }

        if let Some((home, away, ts)) = self.last_score_by_event.get(&event.event_id) {
            if *home == event.home_score && *away == event.away_score && *ts + dedup_window >= now {
                info!(
                    "Skipping repeated score state for event {} within dedup window",
                    event.event_id
                );
                return true;
            }
        }

        self.recent_event_keys.insert(key, now);
        self.last_score_by_event.insert(
            event.event_id.clone(),
            (event.home_score, event.away_score, now),
        );
        false
    }

    fn compute_adaptive_edge_addon(
        priced_in_ratio: f64,
        residual_move: f64,
        fallback_rate: f64,
        ws_age_ms: f64,
        base_min_residual: f64,
        ws_max_age_ms: f64,
        addon_cap: f64,
    ) -> f64 {
        let mut addon = 0.0;
        addon += ((priced_in_ratio - 0.70).max(0.0) * 0.02).min(0.02);
        addon += ((base_min_residual - residual_move).max(0.0) * 0.50).min(0.02);
        addon += (fallback_rate.clamp(0.0, 1.0) * 0.02).min(0.02);
        if ws_age_ms > ws_max_age_ms {
            addon += 0.01;
        }
        addon.min(addon_cap).max(0.0)
    }

    fn compute_adaptive_divergence_limit(
        base_limit: f64,
        tightening: f64,
        fallback_rate: f64,
        priced_in_ratio: f64,
    ) -> f64 {
        let mut limit = base_limit;
        limit *= (1.0 - tightening * fallback_rate.clamp(0.0, 1.0)).max(0.5);
        let priced = (priced_in_ratio - 0.7).max(0.0).min(1.0);
        limit *= (1.0 - 0.5 * tightening * priced).max(0.6);
        limit.max(0.01)
    }

    fn should_trip_feed_health_breaker(
        ewma_fallback_rate: f64,
        ewma_ws_age_ms: f64,
        max_fallback_rate: f64,
        max_ws_age_ms: f64,
    ) -> bool {
        ewma_fallback_rate > max_fallback_rate || ewma_ws_age_ms > max_ws_age_ms
    }

    fn should_force_flatten_positions(&self) -> bool {
        self.feed_health.degraded_since.is_some_and(|since| {
            since + chrono::Duration::seconds(self.config.feed_health_flatten_after_secs as i64)
                <= Utc::now()
        })
    }

    fn should_time_exit(opened_at: DateTime<Utc>, now: DateTime<Utc>, max_age_secs: u64) -> bool {
        (now - opened_at).num_seconds().max(0) as u64 >= max_age_secs
    }

    fn day_start_utc(day: NaiveDate) -> DateTime<Utc> {
        DateTime::<Utc>::from_naive_utc_and_offset(
            day.and_hms_opt(0, 0, 0).expect("valid day start"),
            Utc,
        )
    }

    fn refresh_daily_risk_state(&mut self, current_equity: f64) -> Result<()> {
        let today = Utc::now().date_naive();
        if today == self.daily_risk.day {
            return Ok(());
        }
        let day_start = Self::day_start_utc(today);
        let trades_today = self.db.count_positions_opened_since(day_start)?;
        self.daily_risk = DailyRiskState {
            day: today,
            day_start_equity: current_equity,
            trades_today,
        };
        info!(
            "Daily risk state reset: day={}, start_equity=${:.2}, trades_today={}",
            self.daily_risk.day, self.daily_risk.day_start_equity, self.daily_risk.trades_today
        );
        Ok(())
    }

    fn daily_drawdown_fraction(&self, current_equity: f64) -> f64 {
        if self.daily_risk.day_start_equity <= 0.0 {
            return 0.0;
        }
        ((self.daily_risk.day_start_equity - current_equity) / self.daily_risk.day_start_equity)
            .max(0.0)
    }

    fn outcome_asset_key(market_id: &str, outcome: &str) -> (String, String) {
        (market_id.to_string(), outcome.to_uppercase())
    }

    async fn ensure_asset_subscription(
        &mut self,
        market_id: &str,
        outcome: &str,
    ) -> Option<String> {
        let key = Self::outcome_asset_key(market_id, outcome);
        if let Some(asset_id) = self.outcome_asset_ids.get(&key) {
            return Some(asset_id.clone());
        }

        match self
            .polymarket
            .get_market_asset_id(market_id, outcome)
            .await
        {
            Ok(asset_id) => {
                if self.subscribed_asset_ids.insert(asset_id.clone()) {
                    self.price_feed.subscribe(&[asset_id.as_str()]).await;
                    info!(
                        "PriceFeed subscribed: market={}, outcome={}, asset_id={}",
                        market_id, outcome, asset_id
                    );
                }
                self.outcome_asset_ids.insert(key, asset_id.clone());
                Some(asset_id)
            }
            Err(e) => {
                warn!(
                    "Failed to resolve asset id for market={} outcome={}: {}",
                    market_id, outcome, e
                );
                None
            }
        }
    }

    async fn ensure_asset_subscription_with_hint(
        &mut self,
        market_id: &str,
        outcome: &str,
        asset_id_hint: Option<&str>,
    ) -> Option<String> {
        if let Some(asset_id) = asset_id_hint.filter(|id| !id.trim().is_empty()) {
            let asset_id = asset_id.to_string();
            let key = Self::outcome_asset_key(market_id, outcome);
            self.outcome_asset_ids
                .entry(key)
                .or_insert(asset_id.clone());
            if self.subscribed_asset_ids.insert(asset_id.clone()) {
                self.price_feed.subscribe(&[asset_id.as_str()]).await;
                info!(
                    "PriceFeed subscribed from persisted asset_id: market={}, outcome={}, asset_id={}",
                    market_id, outcome, asset_id
                );
            }
            return Some(asset_id);
        }
        self.ensure_asset_subscription(market_id, outcome).await
    }

    /// Prune stale WS subscriptions so token/price maps do not grow unbounded.
    async fn cleanup_price_feed_subscriptions(&mut self) -> Result<()> {
        let open = self.db.list_open_positions()?;
        let required: HashSet<String> = open
            .iter()
            .filter_map(|pos| {
                if let Some(asset_id) = pos.asset_id.clone() {
                    return Some(asset_id);
                }
                let key = Self::outcome_asset_key(&pos.market_id, &pos.outcome);
                self.outcome_asset_ids.get(&key).cloned()
            })
            .collect();

        let to_unsubscribe: Vec<String> = self
            .subscribed_asset_ids
            .iter()
            .filter(|id| !required.contains(*id))
            .cloned()
            .collect();

        if !to_unsubscribe.is_empty() {
            let refs: Vec<&str> = to_unsubscribe.iter().map(String::as_str).collect();
            self.price_feed.unsubscribe(&refs).await;
            for id in &to_unsubscribe {
                self.subscribed_asset_ids.remove(id);
            }
            self.outcome_asset_ids
                .retain(|_, asset_id| required.contains(asset_id));
            info!(
                "PriceFeed pruned {} inactive asset subscriptions",
                to_unsubscribe.len()
            );
        }

        Ok(())
    }

    /// Process a single score-change event.
    ///
    /// 1. Identify which Polymarket market(s) relate to this event.
    /// 2. Compute edge using estimated win probability vs market price.
    /// 3. If edge > min_edge, size a bet with fractional Kelly and open a position.
    pub async fn on_score_event(&mut self, event: &ScoreEvent, game: &LiveGame) -> Result<()> {
        info!(
            "Score event: {} {} {}-{} (prev {}-{}, {}' {})",
            event.sport,
            event.league,
            event.home_score,
            event.away_score,
            event.prev_home_score.unwrap_or(event.home_score),
            event.prev_away_score.unwrap_or(event.away_score),
            event.minute.unwrap_or(0),
            event.event_type,
        );

        if self.should_skip_event(event) {
            return Ok(());
        }

        if event.event_type.contains("correction") {
            warn!("Skipping score correction event '{}'", event.event_type);
            return Ok(());
        }

        // Persist de-duplicated score events.
        self.db.insert_score_event(event)?;

        let mut prev_game = game.clone();
        prev_game.home_score = event.prev_home_score.unwrap_or(game.home_score);
        prev_game.away_score = event.prev_away_score.unwrap_or(game.away_score);

        if prev_game.home_score == game.home_score && prev_game.away_score == game.away_score {
            info!("Score event missing previous state; skipping probability-shift trade trigger");
            return Ok(());
        }

        let p_home_prev_raw =
            super::win_probability::estimate_win_probability(event, &prev_game, true);
        let p_home_now_raw = super::win_probability::estimate_win_probability(event, game, true);
        let p_home_prev = self.calibrate_probability(&event.sport, p_home_prev_raw);
        let p_home_now = self.calibrate_probability(&event.sport, p_home_now_raw);
        let probability_shift = (p_home_now - p_home_prev).abs();
        let base_min_shift = Self::probability_delta_threshold(&event.sport);
        let quality_addon = Self::score_event_quality_shift_addon(event);
        let min_shift = base_min_shift + quality_addon;
        if probability_shift < min_shift {
            info!(
                "Probability shift {:.3} below threshold {:.3} (base {:.3} + quality {:.3}) for {}, skipping",
                probability_shift, min_shift, base_min_shift, quality_addon, event.sport
            );
            return Ok(());
        }

        let mut open_positions = self.db.list_open_positions()?;
        let total_open_notional: f64 = open_positions.iter().map(|p| p.size_usd).sum();
        let current_equity = self.balance + total_open_notional;

        self.refresh_daily_risk_state(current_equity)?;

        let daily_drawdown = self.daily_drawdown_fraction(current_equity);
        if daily_drawdown >= self.config.max_daily_drawdown_fraction {
            warn!(
                "Daily circuit breaker active: drawdown {:.3} >= {:.3}",
                daily_drawdown, self.config.max_daily_drawdown_fraction
            );
            return Ok(());
        }
        if self.daily_risk.trades_today >= self.config.max_trades_per_day {
            warn!(
                "Daily circuit breaker active: trades_today {} >= {}",
                self.daily_risk.trades_today, self.config.max_trades_per_day
            );
            return Ok(());
        }
        if self.feed_health_blocking_entries() {
            warn!("Feed-health breaker active: skipping new entries");
            return Ok(());
        }

        // Collect market IDs that already have open positions to avoid duplicates
        let mut open_market_ids: HashSet<String> =
            open_positions.iter().map(|p| p.market_id.clone()).collect();
        let event_key = format!("{} vs {}", event.home_team, event.away_team);

        // Find candidate markets — cache first (sub-μs), REST fallback (~1.5s)
        let mut markets = self
            .market_cache
            .search(&event.home_team, &event.away_team, &event.league)
            .await;

        if markets.is_empty() {
            info!("Cache miss, falling back to REST API for market search");
            let raw_markets = self
                .polymarket
                .search_markets(&event.home_team, &event.away_team, &event.league)
                .await?;

            // The REST endpoint is an unfiltered full-text search that can return
            // non-sports markets (e.g. "Will Jesus Christ return before GTA VI?")
            // when team-name tokens overlap with unrelated question text.
            // Keep only markets where at least one team name actually appears.
            markets = raw_markets
                .into_iter()
                .filter(|m| {
                    let q = Self::normalize_text(&m.question);
                    Self::contains_team(&q, &event.home_team)
                        || Self::contains_team(&q, &event.away_team)
                })
                .collect();

            // Backfill with insert_many — never wipes the tag-filtered sports
            // markets already loaded by the background discovery task.
            if !markets.is_empty() {
                self.market_cache.insert_many(markets.clone()).await;
            }
        }

        if markets.is_empty() {
            info!("No Polymarket markets found for this game");
            return Ok(());
        }

        for market in &markets {
            // Skip markets where we already have an open position
            if open_market_ids.contains(&market.id) {
                info!(
                    "Already have open position in '{}', skipping",
                    market.question
                );
                continue;
            }

            let Some(yes_is_home) =
                Self::infer_yes_is_home(market, &event.home_team, &event.away_team)
            else {
                info!("Skipping non-winner/ambiguous market '{}'", market.question);
                continue;
            };

            // Upsert market into DB
            self.db.upsert_market(market)?;

            // WS-first entry pricing with freshness guard; REST only as fallback.
            let now_ms = Utc::now().timestamp_millis().max(0) as u64;
            let mut yes_price_opt = market.yes_price.filter(|p| *p > 0.0 && *p < 1.0);
            let mut no_price_opt = market.no_price.filter(|p| *p > 0.0 && *p < 1.0);
            let mut yes_source = if yes_price_opt.is_some() {
                "cache".to_string()
            } else {
                "none".to_string()
            };
            let mut no_source = if no_price_opt.is_some() {
                "cache".to_string()
            } else {
                "none".to_string()
            };
            let mut yes_ws_age_ms: Option<u64> = None;
            let mut no_ws_age_ms: Option<u64> = None;

            let yes_asset_id = self.ensure_asset_subscription(&market.id, "YES").await;
            let no_asset_id = self.ensure_asset_subscription(&market.id, "NO").await;

            if let Some(asset_id) = yes_asset_id.as_ref() {
                if let Some(snapshot) = self.price_feed.get_price(asset_id).await {
                    let age_ms = now_ms.saturating_sub(snapshot.last_updated_ms);
                    if snapshot.mid_price > 0.0
                        && snapshot.mid_price < 1.0
                        && age_ms <= self.config.ws_price_max_age_ms
                    {
                        yes_price_opt = Some(snapshot.mid_price);
                        yes_source = "ws".to_string();
                        yes_ws_age_ms = Some(age_ms);
                    }
                }
            }
            if let Some(asset_id) = no_asset_id.as_ref() {
                if let Some(snapshot) = self.price_feed.get_price(asset_id).await {
                    let age_ms = now_ms.saturating_sub(snapshot.last_updated_ms);
                    if snapshot.mid_price > 0.0
                        && snapshot.mid_price < 1.0
                        && age_ms <= self.config.ws_price_max_age_ms
                    {
                        no_price_opt = Some(snapshot.mid_price);
                        no_source = "ws".to_string();
                        no_ws_age_ms = Some(age_ms);
                    }
                }
            }

            if yes_source != "ws" || no_source != "ws" {
                match self.polymarket.get_market_prices(&market.id).await {
                    Ok((yes_fresh, no_fresh)) => {
                        if yes_source != "ws" {
                            if let Some(p) = yes_fresh.filter(|p| *p > 0.0 && *p < 1.0) {
                                yes_price_opt = Some(p);
                                yes_source = "rest".to_string();
                                yes_ws_age_ms = None;
                            }
                        }
                        if no_source != "ws" {
                            if let Some(p) = no_fresh.filter(|p| *p > 0.0 && *p < 1.0) {
                                no_price_opt = Some(p);
                                no_source = "rest".to_string();
                                no_ws_age_ms = None;
                            }
                        }
                    }
                    Err(e) => warn!("Live market price refresh failed for {}: {}", market.id, e),
                }
            }

            let yes_price = match yes_price_opt {
                Some(p) if p > 0.0 && p < 1.0 => p,
                _ => continue,
            };
            let no_price = match no_price_opt {
                Some(p) if p > 0.0 && p < 1.0 => p,
                _ => {
                    if no_source == "none" {
                        no_source = "derived".to_string();
                    }
                    (1.0 - yes_price).clamp(0.01, 0.99)
                }
            };

            let p_yes_now = if yes_is_home {
                p_home_now
            } else {
                1.0 - p_home_now
            };
            let p_yes_now_raw = if yes_is_home {
                p_home_now_raw
            } else {
                1.0 - p_home_now_raw
            };
            let p_yes_prev = if yes_is_home {
                p_home_prev
            } else {
                1.0 - p_home_prev
            };
            let p_no_now = 1.0 - p_yes_now;
            let p_no_now_raw = 1.0 - p_yes_now_raw;
            let yes_edge = edge(p_yes_now, yes_price);
            let no_edge = edge(p_no_now, no_price);

            // Latency alpha gate: require evidence that score has not been fully
            // priced by the market yet.
            let expected_yes_move = (p_yes_now - p_yes_prev).abs();
            let prev_yes_price = self.last_yes_price.get(&market.id).copied();
            let observed_yes_move = prev_yes_price
                .map(|prev| (yes_price - prev).abs())
                .unwrap_or(0.0);
            self.last_yes_price.insert(market.id.clone(), yes_price);
            if self.last_yes_price.len() > 50_000 {
                warn!("Price baseline map exceeded 50k entries, resetting");
                self.last_yes_price.clear();
            }

            // Warm-up one observation per market so we can measure observed move.
            if prev_yes_price.is_none() {
                info!(
                    "Skipping '{}' for latency warm-up (no prior quote baseline)",
                    market.question
                );
                continue;
            }

            let residual_yes_move = (expected_yes_move - observed_yes_move).max(0.0);
            let priced_in_ratio = if expected_yes_move > 1e-6 {
                (observed_yes_move / expected_yes_move).clamp(0.0, 5.0)
            } else {
                1.0
            };
            let processing_ms = (Utc::now() - event.detected_at).num_milliseconds().max(0) as f64;

            self.update_latency_stats(
                &event.sport,
                processing_ms,
                priced_in_ratio,
                residual_yes_move,
            );

            let (adaptive_max_age_ms, adaptive_min_residual, adaptive_max_priced_in) =
                self.adaptive_latency_gate(&event.sport);
            let latency_pass = processing_ms <= adaptive_max_age_ms
                && expected_yes_move >= self.config.latency_min_expected_move
                && residual_yes_move >= adaptive_min_residual
                && priced_in_ratio <= adaptive_max_priced_in;
            if !latency_pass {
                info!(
                    "Latency gate skip '{}': age_ms={:.0}/{:.0}, expected_move={:.3}, observed_move={:.3}, residual_move={:.3}/{:.3}, priced_in={:.3}/{:.3}",
                    market.question,
                    processing_ms,
                    adaptive_max_age_ms,
                    expected_yes_move,
                    observed_yes_move,
                    residual_yes_move,
                    adaptive_min_residual,
                    priced_in_ratio,
                    adaptive_max_priced_in
                );
                continue;
            }

            // Net-edge model with costs and liquidity-adjusted buffer.
            let cost_edge = self.round_trip_cost_edge();
            let liquidity_buffer = Self::liquidity_edge_buffer(market.volume);
            let adaptive_min_edge = self.config.min_edge + self.adaptive_edge_addon(&event.sport);
            let threshold_edge = adaptive_min_edge + cost_edge + liquidity_buffer;
            let yes_net_edge = yes_edge - cost_edge - liquidity_buffer;
            let no_net_edge = no_edge - cost_edge - liquidity_buffer;

            let (outcome, true_win_prob_raw, true_win_prob, price, bet_edge, net_edge) =
                if yes_net_edge >= no_net_edge {
                    (
                        "YES".to_string(),
                        p_yes_now_raw,
                        p_yes_now,
                        yes_price,
                        yes_edge,
                        yes_net_edge,
                    )
                } else {
                    (
                        "NO".to_string(),
                        p_no_now_raw,
                        p_no_now,
                        no_price,
                        no_edge,
                        no_net_edge,
                    )
                };

            info!(
                "Market '{}': p_yes {:.3}->{:.3}, edge_yes={:.3}, edge_no={:.3}, net_yes={:.3}, net_no={:.3}, chosen {} raw_edge={:.3}, net_edge={:.3}, threshold={:.3}",
                market.question, p_yes_prev, p_yes_now, yes_edge, no_edge, yes_net_edge, no_net_edge, outcome, bet_edge, net_edge, threshold_edge
            );

            if bet_edge < threshold_edge {
                info!(
                    "Raw edge {:.3} below dynamic threshold {:.3}, skipping",
                    bet_edge, threshold_edge
                );
                continue;
            }

            let chosen_source = if outcome == "YES" {
                yes_source.as_str()
            } else {
                no_source.as_str()
            };
            if chosen_source == "ws" {
                match self.polymarket.get_token_price(&market.id, &outcome).await {
                    Ok(rest_price) if rest_price > 0.0 && rest_price < 1.0 => {
                        let divergence = (price - rest_price).abs();
                        let divergence_limit = self.adaptive_divergence_limit(&event.sport);
                        if divergence > divergence_limit {
                            warn!(
                                "Entry quote divergence too high for {} {}: ws={:.5}, rest={:.5}, diff={:.5} > {:.5}; skipping",
                                market.id,
                                outcome,
                                price,
                                rest_price,
                                divergence,
                                divergence_limit
                            );
                            continue;
                        }
                    }
                    Ok(rest_price) => warn!(
                        "Ignoring entry quote divergence check due to invalid REST price for {} {}: {:.5}",
                        market.id, outcome, rest_price
                    ),
                    Err(e) => warn!(
                        "Entry quote divergence check unavailable for {} {}: {}",
                        market.id, outcome, e
                    ),
                }
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

            // Portfolio exposure caps (event / sport / league), based on notional
            // exposure over current equity proxy (cash + open notional).
            let total_open_notional: f64 = open_positions.iter().map(|p| p.size_usd).sum();
            let total_equity = (self.balance + total_open_notional).max(1.0);
            let event_exposure = open_positions
                .iter()
                .filter(|p| p.event_name.as_deref() == Some(event_key.as_str()))
                .map(|p| p.size_usd)
                .sum::<f64>()
                + stake_usd;
            let sport_exposure = open_positions
                .iter()
                .filter(|p| p.sport.as_deref() == Some(event.sport.as_str()))
                .map(|p| p.size_usd)
                .sum::<f64>()
                + stake_usd;
            let league_exposure = open_positions
                .iter()
                .filter(|p| p.league.as_deref() == Some(event.league.as_str()))
                .map(|p| p.size_usd)
                .sum::<f64>()
                + stake_usd;

            let event_frac = event_exposure / total_equity;
            if event_frac > self.config.max_event_exposure_fraction {
                info!(
                    "Event exposure cap hit for '{}': {:.3} > {:.3}",
                    event_key, event_frac, self.config.max_event_exposure_fraction
                );
                continue;
            }
            let sport_frac = sport_exposure / total_equity;
            if sport_frac > self.config.max_sport_exposure_fraction {
                info!(
                    "Sport exposure cap hit for '{}': {:.3} > {:.3}",
                    event.sport, sport_frac, self.config.max_sport_exposure_fraction
                );
                continue;
            }
            let league_frac = league_exposure / total_equity;
            if league_frac > self.config.max_league_exposure_fraction {
                info!(
                    "League exposure cap hit for '{}': {:.3} > {:.3}",
                    event.league, league_frac, self.config.max_league_exposure_fraction
                );
                continue;
            }
            let positions_for_event = open_positions
                .iter()
                .filter(|p| p.event_name.as_deref() == Some(event_key.as_str()))
                .count() as u32
                + 1;
            if positions_for_event > self.config.max_positions_per_event {
                info!(
                    "Per-event position count cap hit for '{}': {} > {}",
                    event_key, positions_for_event, self.config.max_positions_per_event
                );
                continue;
            }
            let team_exposure = open_positions
                .iter()
                .filter(|p| {
                    p.event_name.as_ref().is_some_and(|name| {
                        let normalized = Self::normalize_text(name);
                        Self::contains_team(&normalized, &event.home_team)
                            || Self::contains_team(&normalized, &event.away_team)
                    })
                })
                .map(|p| p.size_usd)
                .sum::<f64>()
                + stake_usd;
            let team_frac = team_exposure / total_equity;
            if team_frac > self.config.max_team_exposure_fraction {
                info!(
                    "Team exposure cap hit for '{} / {}': {:.3} > {:.3}",
                    event.home_team,
                    event.away_team,
                    team_frac,
                    self.config.max_team_exposure_fraction
                );
                continue;
            }
            let effective_frac = self.effective_exposure_fraction_with_candidate(
                &open_positions,
                &market.id,
                Some(event.sport.as_str()),
                Some(event.league.as_str()),
                Some(event_key.as_str()),
                stake_usd,
                total_equity,
            );
            if effective_frac > self.config.max_effective_exposure_fraction {
                info!(
                    "Effective exposure cap hit for '{}': {:.3} > {:.3}",
                    market.question, effective_frac, self.config.max_effective_exposure_fraction
                );
                continue;
            }

            let (stop_loss, take_profit) = compute_levels(
                price,
                self.config.stop_loss_fraction,
                self.config.take_profit_fraction,
            );

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

            let (entry_price_source, entry_ws_age_ms, asset_id) = if outcome == "YES" {
                (
                    yes_source.clone(),
                    yes_ws_age_ms.map(|v| v as i64),
                    yes_asset_id,
                )
            } else {
                (
                    no_source.clone(),
                    no_ws_age_ms.map(|v| v as i64),
                    no_asset_id,
                )
            };

            let pos = Position {
                id: None,
                market_id: market.id.clone(),
                asset_id,
                outcome,
                side: "buy".into(),
                size_usd: stake_usd,
                entry_price: price,
                entry_price_source: Some(entry_price_source),
                entry_model_prob_raw: Some(true_win_prob_raw),
                entry_model_prob: Some(true_win_prob),
                entry_ws_age_ms,
                estimated_round_trip_cost_bps: self.estimated_round_trip_cost_bps(),
                stop_loss_price: stop_loss,
                take_profit_price: take_profit,
                status: "open".into(),
                opened_at: Utc::now(),
                closed_at: None,
                exit_price: None,
                pnl: None,
                dry_run: self.config.dry_run,
                ws_used_count: 0,
                rest_fallback_count: 0,
                last_ws_age_ms: None,
                sport: Some(event.sport.clone()),
                league: Some(event.league.clone()),
                event_name: Some(format!("{} vs {}", event.home_team, event.away_team)),
                market_slug: market.slug.clone(),
            };

            let _id = self.db.insert_position(&pos)?;
            self.balance -= stake_usd;
            self.db.record_balance(self.balance)?;
            open_market_ids.insert(market.id.clone());
            open_positions.push(pos);
            self.daily_risk.trades_today = self.daily_risk.trades_today.saturating_add(1);
        }

        Ok(())
    }

    /// Sweep all open positions and close those that hit stop-loss or take-profit.
    ///
    /// Uses WS mid-prices first for minimal latency; falls back to concurrent
    /// REST fetches when WS price is unavailable.
    pub async fn manage_positions(&mut self) -> Result<()> {
        let open = self.db.list_open_positions()?;
        if open.is_empty() {
            return Ok(());
        }
        let now = Utc::now();
        let open_len = open.len();

        let mut resolved_prices: Vec<Option<f64>> = vec![None; open.len()];
        let mut quote_sources: Vec<&str> = vec!["none"; open.len()];
        let mut ws_age_millis: Vec<Option<i64>> = vec![None; open.len()];
        let mut used_rest_fallback: Vec<bool> = vec![false; open.len()];
        let mut rest_fallback_indices = Vec::new();
        let now_ms = now.timestamp_millis().max(0) as u64;

        for (idx, pos) in open.iter().enumerate() {
            let ws_price = if let Some(asset_id) = self
                .ensure_asset_subscription_with_hint(
                    &pos.market_id,
                    &pos.outcome,
                    pos.asset_id.as_deref(),
                )
                .await
            {
                self.price_feed.get_price(&asset_id).await
            } else {
                None
            };

            match ws_price {
                Some(snapshot) => {
                    let age_ms = now_ms.saturating_sub(snapshot.last_updated_ms);
                    if snapshot.mid_price > 0.0
                        && snapshot.mid_price < 1.0
                        && age_ms <= self.config.ws_price_max_age_ms
                    {
                        resolved_prices[idx] = Some(snapshot.mid_price);
                        quote_sources[idx] = "ws";
                        ws_age_millis[idx] = Some(age_ms as i64);
                    } else {
                        used_rest_fallback[idx] = true;
                        warn!(
                            "WS quote unusable for market {} outcome {} (mid={:.5}, age={}ms)",
                            pos.market_id, pos.outcome, snapshot.mid_price, age_ms
                        );
                        rest_fallback_indices.push(idx);
                    }
                }
                None => {
                    used_rest_fallback[idx] = true;
                    rest_fallback_indices.push(idx);
                }
            }
        }

        if !rest_fallback_indices.is_empty() {
            let price_futures: Vec<_> = rest_fallback_indices
                .iter()
                .map(|&idx| {
                    let pos = &open[idx];
                    self.polymarket
                        .get_token_price(&pos.market_id, &pos.outcome)
                })
                .collect();
            let rest_results = futures_util::future::join_all(price_futures).await;

            for (idx, price_result) in rest_fallback_indices
                .into_iter()
                .zip(rest_results.into_iter())
            {
                let pos = &open[idx];
                match price_result {
                    Ok(p) if p > 0.0 && p < 1.0 => {
                        resolved_prices[idx] = Some(p);
                        quote_sources[idx] = "rest";
                    }
                    Ok(p) => {
                        warn!(
                            "Ignoring out-of-range price for market {} outcome {}: {:.5}",
                            pos.market_id, pos.outcome, p
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get fallback REST price for market {} outcome {}: {}",
                            pos.market_id, pos.outcome, e
                        );
                    }
                }
            }
        }

        let rest_fallback_count = used_rest_fallback.iter().filter(|&&b| b).count();
        let avg_ws_age_ms = {
            let mut sum = 0.0;
            let mut n = 0.0;
            for age in ws_age_millis.iter().flatten() {
                sum += *age as f64;
                n += 1.0;
            }
            if n > 0.0 {
                sum / n
            } else {
                self.feed_health.ewma_ws_age_ms
            }
        };
        self.update_feed_health(open_len, rest_fallback_count, avg_ws_age_ms);
        let force_flatten = self.should_force_flatten_positions();

        for (idx, pos) in open.into_iter().enumerate() {
            let pos_id = match pos.id {
                Some(id) => id,
                None => continue,
            };

            if let Err(e) = self.db.record_position_quote_telemetry(
                pos_id,
                quote_sources[idx],
                ws_age_millis[idx],
                used_rest_fallback[idx],
            ) {
                warn!(
                    "Failed to record quote telemetry for position {}: {}",
                    pos_id, e
                );
            }

            let current_price = match resolved_prices.get(idx).and_then(|p| *p) {
                Some(p) => p,
                None => {
                    warn!(
                        "No usable price for position {} (market {} outcome {})",
                        pos_id, pos.market_id, pos.outcome
                    );
                    continue;
                }
            };

            if force_flatten {
                let pnl = Self::position_net_pnl(&pos, current_price);
                warn!(
                    "Feed-health flatten: closing position {} at {:.3}, pnl=${:.2}",
                    pos_id, current_price, pnl
                );
                if !self.config.dry_run {
                    if let Err(e) = self
                        .polymarket
                        .close_position(&pos.market_id, &pos.outcome, pos.size_usd)
                        .await
                    {
                        error!(
                            "Failed to close position {} during feed-health flatten: {}",
                            pos_id, e
                        );
                        continue;
                    }
                }
                self.db
                    .close_position(pos_id, "closed_feed_health", current_price, pnl)?;
                self.balance += pos.size_usd + pnl;
                self.db.record_balance(self.balance)?;
                continue;
            }
            if Self::should_time_exit(pos.opened_at, now, self.config.max_position_age_secs) {
                let pnl = Self::position_net_pnl(&pos, current_price);
                warn!(
                    "Time-based flatten: closing position {} at {:.3}, pnl=${:.2}",
                    pos_id, current_price, pnl
                );
                if !self.config.dry_run {
                    if let Err(e) = self
                        .polymarket
                        .close_position(&pos.market_id, &pos.outcome, pos.size_usd)
                        .await
                    {
                        error!("Failed to close timed position {}: {}", pos_id, e);
                        continue;
                    }
                }
                self.db
                    .close_position(pos_id, "closed_time_exit", current_price, pnl)?;
                self.balance += pos.size_usd + pnl;
                self.db.record_balance(self.balance)?;
                continue;
            }

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

        self.cleanup_price_feed_subscriptions().await?;

        Ok(())
    }

    #[allow(dead_code)]
    pub fn balance(&self) -> f64 {
        self.balance
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::BotEngine;
    use crate::db::models::ScoreEvent;

    #[test]
    fn adaptive_edge_addon_increases_with_worse_signals() {
        let calm =
            BotEngine::compute_adaptive_edge_addon(0.6, 0.03, 0.1, 1500.0, 0.01, 2500.0, 0.03);
        let stressed =
            BotEngine::compute_adaptive_edge_addon(1.1, 0.002, 0.8, 6000.0, 0.01, 2500.0, 0.03);
        assert!(stressed > calm);
        assert!(stressed <= 0.03 + 1e-9);
    }

    #[test]
    fn adaptive_divergence_limit_tightens_with_risk() {
        let base = BotEngine::compute_adaptive_divergence_limit(0.08, 0.35, 0.0, 0.6);
        let tighter = BotEngine::compute_adaptive_divergence_limit(0.08, 0.35, 0.8, 1.2);
        assert!(tighter < base);
        assert!(tighter >= 0.01);
    }

    #[test]
    fn feed_health_breaker_logic_works() {
        assert!(!BotEngine::should_trip_feed_health_breaker(
            0.2, 1200.0, 0.7, 4000.0
        ));
        assert!(BotEngine::should_trip_feed_health_breaker(
            0.8, 1200.0, 0.7, 4000.0
        ));
        assert!(BotEngine::should_trip_feed_health_breaker(
            0.2, 5000.0, 0.7, 4000.0
        ));
    }

    #[test]
    fn score_event_quality_addon_penalizes_low_consensus() {
        let high = ScoreEvent {
            id: None,
            event_id: "ev1".into(),
            source_provider: Some("PolymarketSportsWS".into()),
            provider_consensus_count: Some(3),
            sport: "soccer".into(),
            league: "EPL".into(),
            home_team: "A".into(),
            away_team: "B".into(),
            prev_home_score: Some(0),
            prev_away_score: Some(0),
            home_score: 1,
            away_score: 0,
            minute: Some(20),
            event_type: "goal_home".into(),
            detected_at: Utc::now(),
        };
        let low = ScoreEvent {
            provider_consensus_count: Some(1),
            source_provider: Some("TheSportsDB".into()),
            ..high.clone()
        };
        let high_addon = BotEngine::score_event_quality_shift_addon(&high);
        let low_addon = BotEngine::score_event_quality_shift_addon(&low);
        assert!(high_addon < low_addon);
    }

    #[test]
    fn should_time_exit_after_max_age() {
        let now = Utc::now();
        let opened = now - chrono::Duration::seconds(3601);
        assert!(BotEngine::should_time_exit(opened, now, 3600));
        assert!(!BotEngine::should_time_exit(opened, now, 7200));
    }
}
