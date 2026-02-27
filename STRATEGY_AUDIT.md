# LiveSports Bot Strategy Audit

Date: 2026-02-27  
Scope: Current implemented strategy in this repository (`src/bot`, `src/live_scores`, `src/polymarket`, `src/db`).

Update: Priorities 1-6 are implemented. Priority 5 now includes periodic outcome-based recalibration (Platt scaling) trained on resolved market outcomes from historical trades. Also implemented: adaptive thresholding, consensus-aware score quality gating, covariance-adjusted effective exposure cap, feed-health circuit breaker, event de-duplication, and cost-aware net PnL accounting.

## Note on "expert agent"

No dedicated trading/financial specialist skill is installed in this workspace. This review applies a trading and market-microstructure expert lens directly to your implementation.

## 1) Current Strategy (Implementation Snapshot)

### 1.1 Signal ingestion

- Multiple score providers are polled concurrently.
- Providers are timeout-bounded and merged.
- A `ScoreEvent` is produced only when score changes versus previous snapshot.
- Event queue is buffered (`mpsc` size 1024) and stale games are pruned.
- Event types encode side where possible (`goal_home`, `touchdown_away`, etc.), with correction detection (`score_correction`).

### 1.2 Trade trigger

- On each score event:
  - Event-level duplicate suppression is applied across providers using a dedup time window.
  - Non-duplicate events are persisted to DB.
  - `score_correction` events are ignored.
  - Minimum probability-shift trigger is tightened when provider consensus is weak.
  - Previous and current state are compared using sport-specific win probability models.
  - Trade trigger requires absolute probability shift to exceed sport threshold:
    - Soccer: 0.04
    - NFL: 0.03
    - NBA: 0.015
    - MLB/NHL: 0.025
    - Tennis: 0.05

### 1.3 Market selection

- Candidate markets come from in-memory `MarketCache`; fallback to Polymarket search API on miss.
- Heuristics skip non-winner markets (`over/under`, spreads, props, etc.).
- Logic infers if `YES` corresponds to home or away team.
- Existing open market IDs are skipped to avoid duplicate position in same market.

### 1.4 Pricing and edge

- Before betting, YES/NO prices are resolved WS-first (fresh-token quotes) with REST fallback.
- When entry uses WS quote, a REST cross-check guards against large quote divergence (`MAX_ENTRY_QUOTE_DIVERGENCE`).
- Entry divergence guard is adaptive (tightens when feed quality/latency telemetry deteriorates).
- Sport-level probability calibration is applied before edge and shift decisions.
- The bot computes:
  - `p_yes_now`, `p_no_now` from model + inferred mapping
  - `edge_yes = p_yes_now / yes_price - 1`
  - `edge_no = p_no_now / no_price - 1`
- Chooses higher-edge side; requires `edge >= min_edge` (default 5%).
- `min_edge` and latency gate parameters are adaptively tightened/relaxed from rolling telemetry.

### 1.5 Position sizing and risk

- Kelly fraction sizing (`kelly_fraction` default 0.25).
- Stake = `balance * stake_fraction`.
- Guardrails:
  - Skip if stake < $1
  - Skip if stake > available balance
  - Cap positions per event and cap team-level exposure across correlated markets.
- Initial stop/take levels:
  - Stop loss: `entry * (1 - stop_loss_fraction)` (default 50%)
  - Take profit: `entry * (1 + take_profit_fraction)` capped at `0.99` (default +30%)

### 1.6 Position management

- Every 5s, bot marks open positions using token-level CLOB WS quotes first.
- WS quotes are accepted only if fresh (`WS_PRICE_MAX_AGE_MS`, default 2500ms).
- REST token price fetch is used concurrently only for positions missing fresh WS quotes.
- Closes when stop-loss or take-profit is hit.
- Force-flattens when feed degradation persists or max position age is exceeded.
- Updates realized **net** PnL and balance in DB using estimated round-trip execution costs.
- Updates feed-health EWMA telemetry (fallback rate and WS age), which can pause new entries temporarily.

### 1.7 Models used

- Soccer: empirical lookup table by goal diff and minute.
- NBA/NFL/MLB/NHL: logistic/time-scaled models.
- Tennis: set-based simplified mapping.
- Unknown sports: fallback logistic model.
- Probability output clamped to `[0.03, 0.97]`.

### 1.8 Persistence and maintenance

- DB stores events, markets, positions, balance history.
- Positions also track execution-quote telemetry:
  - `entry_price_source`, `entry_ws_age_ms`
  - `ws_used_count`, `rest_fallback_count`, `last_ws_age_ms`
- Positions also persist entry model probabilities (`entry_model_prob_raw`, `entry_model_prob`) for calibration.
- Per-sport calibration coefficients and fit diagnostics are persisted in `model_calibrations`.
- Dashboard/API now expose aggregate and per-sport quote quality telemetry distributions.
- Hourly retention maintenance:
  - score events retention (default 14 days)
  - balance history retention (default 30 days)
- Calibration retraining runs periodically and promotes only improved models.

## 2) Validity Assessment (Trading/Quant Lens)

## Strengths

- Good event-driven structure: score delta is transformed into probability delta.
- Side-aware classification and correction filtering materially reduce false triggers.
- Market semantic filtering is a major correctness improvement vs naive search matching.
- Kelly sizing + minimum edge + capital guardrails is structurally sound.
- Memory and retention controls are now in place for long-running operation.

## Weaknesses / residual risks

- **Fill-quality model still basic**: strategy still assumes close to displayed executable prices; partial fills and queue position are not modeled.
- **Latency alpha adaptation is heuristic**: thresholds now adapt from EWMA telemetry, but not yet learned from formal optimization/backtest loops.
- **Model calibration has improved but remains simple**: periodic Platt scaling is online and outcome-based, but no richer non-linear or covariate-aware calibration yet.
- **Correlation control is improved but still heuristic**: effective exposure now uses configurable pairwise correlations, but this is not a fully estimated covariance matrix from empirical returns.
- **Exit policy is partially dynamic**: fixed TP/SL remains, with new time-based forced exits and feed-health flattening; still no full model-based hold/exit optimizer.
- **Thin books remain fallback-prone**: low-liquidity markets can still rely heavily on REST and trigger feed-health pauses.

## 3) Changes Most Likely to Improve Results

### Priority 1: Convert raw edge to net edge

- Require: `net_edge = model_edge - expected_costs`.
- Costs should include:
  - half-spread + estimated slippage + explicit fees + cancel/requote penalty.
- Make `min_edge` dynamic:
  - higher in low-liquidity markets or wide spreads,
  - lower only in deep/high-turnover markets.

### Priority 2: Build a latency-alpha gate (critical for score repricing strategy)

- Track per event:
  - `t_score_detected`, `t_first_price_move`, `delta_ms`.
- Keep a rolling distribution by sport/league/market-type.
- Enter only when expected residual lag is positive (market likely still stale).
- Add a max-age gate (e.g., skip if event older than N seconds relative to first seen price adjustment).

### Priority 3: Introduce portfolio risk budget

- Add caps:
  - max % bankroll per event,
  - max % bankroll per sport/league,
  - max correlated exposure to same team/game.
- Add daily drawdown circuit breaker and max trades/day.

### Priority 4: Improve exits with time-aware logic (Implemented)

- Implemented:
  - forced flatten on prolonged feed degradation,
  - max-position-age forced flatten (`MAX_POSITION_AGE_SECS`),
  - cost-aware net PnL accounting on all exit paths.
- Remaining:
  - trailing TP by game state certainty,
  - hold/exit based on updated model net edge.

### Priority 5: Continuous model calibration (Implemented)

- Implemented:
  - aggregate and per-sport CLV telemetry (`/api/stats`, dashboard),
  - periodic per-sport Platt scaling fits,
  - training labels from resolved market outcomes (not PnL proxy),
  - promotion gate requiring measurable out-of-sample improvement in logloss or brier.
- Next quality upgrades (optional):
  - isotonic or spline calibration for sports with larger sample sizes,
  - richer feature-based calibration (time remaining, score delta, league strata).

### Priority 6: Upgrade price path to WS-first

- Use token-level WS subscription for position marks and entry checks.
- Keep REST as fallback only.
- This directly improves the "anticipate post-score repricing" objective.

## 4) Concrete Metrics to Track Weekly

- Signal quality:
  - hit rate by sport/event type
  - mean and median model edge at entry
  - CLV (your entry vs later reference price)
- Execution quality:
  - expected vs realized fill
  - slippage bps by market liquidity bucket
  - missed opportunities due to latency
- Risk quality:
  - max drawdown
  - exposure concentration (team/game/sport)
  - PnL distribution tails

## 5) Suggested Next Implementation Sprint

1. Build a calibration/backtest pipeline that re-fits adaptive coefficients from realized outcomes/CLV.
2. Add real fill ingestion (partial fills, avg fill price, reject reasons) from exchange APIs.
3. Add covariance-aware portfolio risk budgeting across correlated leagues/teams/market archetypes.
