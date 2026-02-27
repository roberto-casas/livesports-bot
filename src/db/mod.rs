use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

pub mod models;
use models::*;

/// Thread-safe SQLite connection pool (single connection with mutex)
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (or create) the SQLite database at the given path
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Database {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.run_migrations()?;
        Ok(db)
    }

    /// Run schema migrations (idempotent)
    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_SQL)?;
        ensure_column(&conn, "score_events", "prev_home_score", "INTEGER")?;
        ensure_column(&conn, "score_events", "prev_away_score", "INTEGER")?;
        ensure_column(&conn, "score_events", "source_provider", "TEXT")?;
        ensure_column(&conn, "score_events", "provider_consensus_count", "INTEGER")?;
        ensure_column(&conn, "positions", "asset_id", "TEXT")?;
        ensure_column(&conn, "positions", "entry_price_source", "TEXT")?;
        ensure_column(&conn, "positions", "entry_model_prob_raw", "REAL")?;
        ensure_column(&conn, "positions", "entry_model_prob", "REAL")?;
        ensure_column(&conn, "positions", "entry_ws_age_ms", "INTEGER")?;
        ensure_column(
            &conn,
            "positions",
            "estimated_round_trip_cost_bps",
            "REAL NOT NULL DEFAULT 0.0",
        )?;
        ensure_column(
            &conn,
            "positions",
            "ws_used_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "positions",
            "rest_fallback_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "positions", "last_ws_age_ms", "INTEGER")?;
        ensure_column(&conn, "markets", "slug", "TEXT")?;
        ensure_column(&conn, "markets", "end_date", "TEXT")?;
        ensure_column(&conn, "markets", "liquidity", "REAL")?;
        ensure_column(&conn, "positions", "market_slug", "TEXT")?;
        Ok(())
    }

    // ── Balance ──────────────────────────────────────────────────────────────

    /// Get the current balance from the latest balance_history entry
    pub fn get_balance(&self) -> Result<f64> {
        let conn = self.conn.lock().unwrap();
        let balance: f64 = conn
            .query_row(
                "SELECT balance FROM balance_history ORDER BY recorded_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0.0);
        Ok(balance)
    }

    /// Get the first recorded balance snapshot on or after `since`.
    pub fn first_balance_on_or_after(&self, since: DateTime<Utc>) -> Result<Option<f64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT balance
             FROM balance_history
             WHERE recorded_at >= ?1
             ORDER BY recorded_at ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![since])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    /// Record a balance snapshot
    pub fn record_balance(&self, balance: f64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO balance_history (balance, recorded_at) VALUES (?1, ?2)",
            params![balance, Utc::now()],
        )?;
        Ok(())
    }

    // ── Positions ─────────────────────────────────────────────────────────────

    /// Insert a new position
    pub fn insert_position(&self, pos: &Position) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO positions (
                market_id, asset_id, outcome, side, size_usd, entry_price,
                entry_price_source, entry_model_prob_raw, entry_model_prob,
                entry_ws_age_ms, estimated_round_trip_cost_bps,
                stop_loss_price, take_profit_price, status,
                opened_at, dry_run, ws_used_count, rest_fallback_count, last_ws_age_ms,
                sport, league, event_name
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                pos.market_id,
                pos.asset_id,
                pos.outcome,
                pos.side,
                pos.size_usd,
                pos.entry_price,
                pos.entry_price_source,
                pos.entry_model_prob_raw,
                pos.entry_model_prob,
                pos.entry_ws_age_ms,
                pos.estimated_round_trip_cost_bps,
                pos.stop_loss_price,
                pos.take_profit_price,
                pos.status,
                pos.opened_at,
                pos.dry_run,
                pos.ws_used_count,
                pos.rest_fallback_count,
                pos.last_ws_age_ms,
                pos.sport,
                pos.league,
                pos.event_name,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Update position status and close fields
    pub fn close_position(&self, id: i64, status: &str, exit_price: f64, pnl: f64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE positions SET status=?1, exit_price=?2, pnl=?3, closed_at=?4 WHERE id=?5",
            params![status, exit_price, pnl, Utc::now(), id],
        )?;
        Ok(())
    }

    /// Record quote-source telemetry for an open/managed position.
    pub fn record_position_quote_telemetry(
        &self,
        id: i64,
        source: &str,
        ws_age_ms: Option<i64>,
        rest_fallback: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE positions
             SET ws_used_count = ws_used_count + ?1,
                 rest_fallback_count = rest_fallback_count + ?2,
                 last_ws_age_ms = COALESCE(?3, last_ws_age_ms)
             WHERE id = ?4",
            params![
                if source == "ws" { 1i64 } else { 0i64 },
                if rest_fallback { 1i64 } else { 0i64 },
                ws_age_ms,
                id,
            ],
        )?;
        Ok(())
    }

    /// List open positions
    pub fn list_open_positions(&self) -> Result<Vec<Position>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, market_id, asset_id, outcome, side, size_usd, entry_price,
                    entry_price_source, entry_model_prob_raw, entry_model_prob,
                    entry_ws_age_ms, estimated_round_trip_cost_bps,
                    stop_loss_price, take_profit_price, status,
                    opened_at, closed_at, exit_price, pnl, dry_run,
                    ws_used_count, rest_fallback_count, last_ws_age_ms,
                    sport, league, event_name
             FROM positions WHERE status='open' ORDER BY opened_at DESC",
        )?;
        let positions = stmt
            .query_map([], map_position)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(positions)
    }

    /// List all positions (paginated)
    pub fn list_positions(&self, limit: i64, offset: i64) -> Result<Vec<Position>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, market_id, asset_id, outcome, side, size_usd, entry_price,
                    entry_price_source, entry_model_prob_raw, entry_model_prob,
                    entry_ws_age_ms, estimated_round_trip_cost_bps,
                    stop_loss_price, take_profit_price, status,
                    opened_at, closed_at, exit_price, pnl, dry_run,
                    ws_used_count, rest_fallback_count, last_ws_age_ms,
                    sport, league, event_name
             FROM positions ORDER BY opened_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let positions = stmt
            .query_map(params![limit, offset], map_position)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(positions)
    }

    /// Count positions opened on or after `since`.
    pub fn count_positions_opened_since(&self, since: DateTime<Utc>) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM positions
             WHERE opened_at >= ?1",
            params![since],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    // ── Markets ───────────────────────────────────────────────────────────────

    /// Upsert a market record
    pub fn upsert_market(&self, market: &Market) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO markets (id, question, sport, league, event_name,
                                  yes_price, no_price, volume, status, fetched_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
                yes_price=excluded.yes_price,
                no_price=excluded.no_price,
                volume=excluded.volume,
                status=excluded.status,
                fetched_at=excluded.fetched_at",
            params![
                market.id,
                market.question,
                market.sport,
                market.league,
                market.event_name,
                market.yes_price,
                market.no_price,
                market.volume,
                market.status,
                market.fetched_at,
            ],
        )?;
        Ok(())
    }

    /// List active markets
    pub fn list_active_markets(&self) -> Result<Vec<Market>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, question, sport, league, event_name,
                    yes_price, no_price, volume, status, fetched_at
             FROM markets WHERE status='active' ORDER BY volume DESC LIMIT 100",
        )?;
        let markets = stmt
            .query_map([], map_market)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(markets)
    }

    // ── Live score events ─────────────────────────────────────────────────────

    /// Insert a score event (goal, point change, etc.)
    pub fn insert_score_event(&self, ev: &ScoreEvent) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO score_events (
                event_id, sport, league, home_team, away_team,
                source_provider, provider_consensus_count,
                prev_home_score, prev_away_score, home_score, away_score,
                minute, event_type, detected_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                ev.event_id,
                ev.sport,
                ev.league,
                ev.home_team,
                ev.away_team,
                ev.source_provider,
                ev.provider_consensus_count,
                ev.prev_home_score,
                ev.prev_away_score,
                ev.home_score,
                ev.away_score,
                ev.minute,
                ev.event_type,
                ev.detected_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// List recent score events
    pub fn list_recent_score_events(&self, limit: i64) -> Result<Vec<ScoreEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, event_id, sport, league, home_team, away_team,
                    source_provider, provider_consensus_count,
                    prev_home_score, prev_away_score, home_score, away_score,
                    minute, event_type, detected_at
             FROM score_events ORDER BY detected_at DESC LIMIT ?1",
        )?;
        let events = stmt
            .query_map(params![limit], map_score_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    /// Delete score events older than the given number of days.
    pub fn prune_score_events(&self, keep_days: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(keep_days);
        let deleted = conn.execute(
            "DELETE FROM score_events WHERE detected_at < ?1",
            params![cutoff],
        )?;
        Ok(deleted)
    }

    /// Delete balance snapshots older than the given number of days.
    pub fn prune_balance_history(&self, keep_days: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(keep_days);
        let deleted = conn.execute(
            "DELETE FROM balance_history WHERE recorded_at < ?1",
            params![cutoff],
        )?;
        Ok(deleted)
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    /// Get aggregate trading stats
    pub fn get_stats(&self) -> Result<Stats> {
        let conn = self.conn.lock().unwrap();
        let total_trades: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE status != 'open'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let winning_trades: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions WHERE pnl > 0", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        let total_pnl: f64 = conn
            .query_row("SELECT COALESCE(SUM(pnl),0) FROM positions", [], |r| {
                r.get(0)
            })
            .unwrap_or(0.0);
        let open_positions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE status='open'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let balance = {
            conn.query_row(
                "SELECT balance FROM balance_history ORDER BY recorded_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0)
        };
        let ws_marks_total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(ws_used_count),0) FROM positions",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let rest_fallback_total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(rest_fallback_count),0) FROM positions",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let total_entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap_or(0);
        let ws_entry_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE entry_price_source = 'ws'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let avg_last_ws_age_ms: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG(last_ws_age_ms),0.0) FROM positions WHERE last_ws_age_ms IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        let avg_entry_ws_age_ms: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG(entry_ws_age_ms),0.0) FROM positions WHERE entry_ws_age_ms IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        let calibration_models_active: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_calibrations", [], |r| r.get(0))
            .unwrap_or(0);
        let calibration_last_fit_at: Option<DateTime<Utc>> = conn
            .query_row("SELECT MAX(fitted_at) FROM model_calibrations", [], |r| {
                r.get(0)
            })
            .unwrap_or(None);
        let avg_closed_clv_bps: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG((exit_price - entry_price) * 10000.0),0.0)
                 FROM positions
                 WHERE status != 'open' AND exit_price IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        let total_marks = ws_marks_total + rest_fallback_total;
        let rest_fallback_rate = if total_marks > 0 {
            rest_fallback_total as f64 / total_marks as f64
        } else {
            0.0
        };
        let ws_entry_rate = if total_entries > 0 {
            ws_entry_count as f64 / total_entries as f64
        } else {
            0.0
        };
        let mut sport_stats_stmt = conn.prepare(
            "SELECT COALESCE(sport, 'unknown') as sport,
                    COALESCE(SUM(ws_used_count),0) as ws_marks,
                    COALESCE(SUM(rest_fallback_count),0) as rest_marks,
                    COALESCE(AVG(last_ws_age_ms),0.0) as avg_ws_age
             FROM positions
             GROUP BY COALESCE(sport, 'unknown')
             ORDER BY (COALESCE(SUM(ws_used_count),0) + COALESCE(SUM(rest_fallback_count),0)) DESC",
        )?;
        let sport_quote_stats = sport_stats_stmt
            .query_map([], |row| {
                let ws_marks: i64 = row.get(1)?;
                let rest_marks: i64 = row.get(2)?;
                let denom = ws_marks + rest_marks;
                let rest_fallback_rate = if denom > 0 {
                    rest_marks as f64 / denom as f64
                } else {
                    0.0
                };
                Ok(SportQuoteStats {
                    sport: row.get(0)?,
                    ws_marks,
                    rest_fallback_marks: rest_marks,
                    rest_fallback_rate,
                    avg_ws_age_ms: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut sport_clv_stmt = conn.prepare(
            "SELECT COALESCE(sport, 'unknown') as sport,
                    COUNT(*) as n,
                    COALESCE(AVG((exit_price - entry_price) * 10000.0),0.0) as avg_clv_bps,
                    COALESCE(AVG(CASE WHEN pnl > 0 THEN 1.0 ELSE 0.0 END),0.0) as win_rate
             FROM positions
             WHERE status != 'open' AND exit_price IS NOT NULL
             GROUP BY COALESCE(sport, 'unknown')
             ORDER BY n DESC",
        )?;
        let sport_clv_stats = sport_clv_stmt
            .query_map([], |row| {
                Ok(SportClvStats {
                    sport: row.get(0)?,
                    trades: row.get(1)?,
                    avg_clv_bps: row.get(2)?,
                    win_rate: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(Stats {
            total_trades,
            winning_trades,
            total_pnl,
            open_positions,
            current_balance: balance,
            ws_marks_total,
            rest_fallback_total,
            rest_fallback_rate,
            ws_entry_rate,
            avg_last_ws_age_ms,
            avg_entry_ws_age_ms,
            calibration_models_active,
            calibration_last_fit_at,
            avg_closed_clv_bps,
            sport_quote_stats,
            sport_clv_stats,
        })
    }

    /// Get balance history for charting
    pub fn get_balance_history(&self, limit: i64) -> Result<Vec<BalanceSnapshot>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT balance, recorded_at FROM balance_history ORDER BY recorded_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(BalanceSnapshot {
                    balance: row.get(0)?,
                    recorded_at: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Calibration candidates from closed trades with recorded raw model probabilities.
    pub fn list_calibration_candidates(&self) -> Result<Vec<CalibrationCandidate>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(sport, 'unknown') as sport,
                    market_id,
                    outcome,
                    entry_model_prob_raw
             FROM positions
             WHERE status != 'open'
               AND entry_model_prob_raw IS NOT NULL
               AND outcome IN ('YES', 'NO')",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CalibrationCandidate {
                    sport: row.get(0)?,
                    market_id: row.get(1)?,
                    outcome: row.get(2)?,
                    model_prob_raw: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn load_model_calibrations(&self) -> Result<Vec<ModelCalibration>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sport, a, b, samples, logloss_before, logloss_after,
                    brier_before, brier_after, fitted_at
             FROM model_calibrations",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ModelCalibration {
                    sport: row.get(0)?,
                    a: row.get(1)?,
                    b: row.get(2)?,
                    samples: row.get(3)?,
                    logloss_before: row.get(4)?,
                    logloss_after: row.get(5)?,
                    brier_before: row.get(6)?,
                    brier_after: row.get(7)?,
                    fitted_at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_model_calibration(&self, model: &ModelCalibration) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO model_calibrations (
                sport, a, b, samples, logloss_before, logloss_after,
                brier_before, brier_after, fitted_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(sport) DO UPDATE SET
                a=excluded.a,
                b=excluded.b,
                samples=excluded.samples,
                logloss_before=excluded.logloss_before,
                logloss_after=excluded.logloss_after,
                brier_before=excluded.brier_before,
                brier_after=excluded.brier_after,
                fitted_at=excluded.fitted_at",
            params![
                model.sport,
                model.a,
                model.b,
                model.samples,
                model.logloss_before,
                model.logloss_after,
                model.brier_before,
                model.brier_after,
                model.fitted_at,
            ],
        )?;
        Ok(())
    }
}

// ── SQL helpers ────────────────────────────────────────────────────────────────

fn map_position(row: &rusqlite::Row) -> rusqlite::Result<Position> {
    Ok(Position {
        id: row.get(0)?,
        market_id: row.get(1)?,
        asset_id: row.get(2)?,
        outcome: row.get(3)?,
        side: row.get(4)?,
        size_usd: row.get(5)?,
        entry_price: row.get(6)?,
        entry_price_source: row.get(7)?,
        entry_model_prob_raw: row.get(8)?,
        entry_model_prob: row.get(9)?,
        entry_ws_age_ms: row.get(10)?,
        estimated_round_trip_cost_bps: row.get(11)?,
        stop_loss_price: row.get(12)?,
        take_profit_price: row.get(13)?,
        status: row.get(14)?,
        opened_at: row.get(15)?,
        closed_at: row.get(16)?,
        exit_price: row.get(17)?,
        pnl: row.get(18)?,
        dry_run: row.get(19)?,
        ws_used_count: row.get(20)?,
        rest_fallback_count: row.get(21)?,
        last_ws_age_ms: row.get(22)?,
        sport: row.get(23)?,
        league: row.get(24)?,
        event_name: row.get(25)?,
    })
}

fn map_market(row: &rusqlite::Row) -> rusqlite::Result<Market> {
    Ok(Market {
        id: row.get(0)?,
        question: row.get(1)?,
        sport: row.get(2)?,
        league: row.get(3)?,
        event_name: row.get(4)?,
        yes_price: row.get(5)?,
        no_price: row.get(6)?,
        volume: row.get(7)?,
        status: row.get(8)?,
        fetched_at: row.get(9)?,
    })
}

fn map_score_event(row: &rusqlite::Row) -> rusqlite::Result<ScoreEvent> {
    Ok(ScoreEvent {
        id: row.get(0)?,
        event_id: row.get(1)?,
        sport: row.get(2)?,
        league: row.get(3)?,
        home_team: row.get(4)?,
        away_team: row.get(5)?,
        source_provider: row.get(6)?,
        provider_consensus_count: row.get(7)?,
        prev_home_score: row.get(8)?,
        prev_away_score: row.get(9)?,
        home_score: row.get(10)?,
        away_score: row.get(11)?,
        minute: row.get(12)?,
        event_type: row.get(13)?,
        detected_at: row.get(14)?,
    })
}

/// SQLite schema (idempotent CREATE IF NOT EXISTS)
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS balance_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    balance     REAL    NOT NULL,
    recorded_at TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS markets (
    id          TEXT    PRIMARY KEY,
    question    TEXT    NOT NULL,
    sport       TEXT,
    league      TEXT,
    event_name  TEXT,
    yes_price   REAL,
    no_price    REAL,
    volume      REAL,
    status      TEXT    NOT NULL DEFAULT 'active',
    fetched_at  TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS positions (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id         TEXT    NOT NULL,
    asset_id          TEXT,
    outcome           TEXT    NOT NULL,
    side              TEXT    NOT NULL,
    size_usd          REAL    NOT NULL,
    entry_price       REAL    NOT NULL,
    entry_price_source TEXT,
    entry_model_prob_raw REAL,
    entry_model_prob REAL,
    entry_ws_age_ms   INTEGER,
    estimated_round_trip_cost_bps REAL NOT NULL DEFAULT 0.0,
    stop_loss_price   REAL    NOT NULL,
    take_profit_price REAL    NOT NULL,
    status            TEXT    NOT NULL DEFAULT 'open',
    opened_at         TEXT    NOT NULL,
    closed_at         TEXT,
    exit_price        REAL,
    pnl               REAL,
    dry_run           INTEGER NOT NULL DEFAULT 1,
    ws_used_count     INTEGER NOT NULL DEFAULT 0,
    rest_fallback_count INTEGER NOT NULL DEFAULT 0,
    last_ws_age_ms    INTEGER,
    sport             TEXT,
    league            TEXT,
    event_name        TEXT,
    FOREIGN KEY (market_id) REFERENCES markets(id)
);

CREATE TABLE IF NOT EXISTS score_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id    TEXT    NOT NULL,
    source_provider TEXT,
    provider_consensus_count INTEGER,
    sport       TEXT    NOT NULL,
    league      TEXT    NOT NULL,
    home_team   TEXT    NOT NULL,
    away_team   TEXT    NOT NULL,
    prev_home_score INTEGER,
    prev_away_score INTEGER,
    home_score  INTEGER NOT NULL,
    away_score  INTEGER NOT NULL,
    minute      INTEGER,
    event_type  TEXT    NOT NULL,
    detected_at TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS model_calibrations (
    sport       TEXT    PRIMARY KEY,
    a           REAL    NOT NULL,
    b           REAL    NOT NULL,
    samples     INTEGER NOT NULL,
    logloss_before REAL NOT NULL,
    logloss_after  REAL NOT NULL,
    brier_before   REAL NOT NULL,
    brier_after    REAL NOT NULL,
    fitted_at   TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_market ON positions(market_id);
CREATE INDEX IF NOT EXISTS idx_score_events_event ON score_events(event_id);
CREATE INDEX IF NOT EXISTS idx_model_calibrations_fitted_at ON model_calibrations(fitted_at);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub total_trades: i64,
    pub winning_trades: i64,
    pub total_pnl: f64,
    pub open_positions: i64,
    pub current_balance: f64,
    pub ws_marks_total: i64,
    pub rest_fallback_total: i64,
    pub rest_fallback_rate: f64,
    pub ws_entry_rate: f64,
    pub avg_last_ws_age_ms: f64,
    pub avg_entry_ws_age_ms: f64,
    pub calibration_models_active: i64,
    pub calibration_last_fit_at: Option<DateTime<Utc>>,
    pub avg_closed_clv_bps: f64,
    pub sport_quote_stats: Vec<SportQuoteStats>,
    pub sport_clv_stats: Vec<SportClvStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportQuoteStats {
    pub sport: String,
    pub ws_marks: i64,
    pub rest_fallback_marks: i64,
    pub rest_fallback_rate: f64,
    pub avg_ws_age_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportClvStats {
    pub sport: String,
    pub trades: i64,
    pub avg_clv_bps: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub balance: f64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCalibration {
    pub sport: String,
    pub a: f64,
    pub b: f64,
    pub samples: i64,
    pub logloss_before: f64,
    pub logloss_after: f64,
    pub brier_before: f64,
    pub brier_after: f64,
    pub fitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CalibrationCandidate {
    pub sport: String,
    pub market_id: String,
    pub outcome: String,
    pub model_prob_raw: f64,
}

fn ensure_column(conn: &Connection, table: &str, column: &str, column_type: &str) -> Result<()> {
    let pragma = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(&pragma)?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        let alter = format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            table, column, column_type
        );
        conn.execute_batch(&alter)?;
    }
    Ok(())
}
