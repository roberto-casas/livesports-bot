use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
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
        Ok(())
    }

    // ── Balance ──────────────────────────────────────────────────────────────

    /// Get the current balance from the latest balance_history entry
    pub fn get_balance(&self) -> Result<f64> {
        let conn = self.conn.lock().unwrap();
        let balance: f64 = conn.query_row(
            "SELECT balance FROM balance_history ORDER BY recorded_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        ).unwrap_or(0.0);
        Ok(balance)
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
                market_id, outcome, side, size_usd, entry_price,
                stop_loss_price, take_profit_price, status,
                opened_at, dry_run, sport, league, event_name
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                pos.market_id,
                pos.outcome,
                pos.side,
                pos.size_usd,
                pos.entry_price,
                pos.stop_loss_price,
                pos.take_profit_price,
                pos.status,
                pos.opened_at,
                pos.dry_run,
                pos.sport,
                pos.league,
                pos.event_name,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Update position status and close fields
    pub fn close_position(
        &self,
        id: i64,
        status: &str,
        exit_price: f64,
        pnl: f64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE positions SET status=?1, exit_price=?2, pnl=?3, closed_at=?4 WHERE id=?5",
            params![status, exit_price, pnl, Utc::now(), id],
        )?;
        Ok(())
    }

    /// List open positions
    pub fn list_open_positions(&self) -> Result<Vec<Position>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, market_id, outcome, side, size_usd, entry_price,
                    stop_loss_price, take_profit_price, status,
                    opened_at, closed_at, exit_price, pnl, dry_run,
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
            "SELECT id, market_id, outcome, side, size_usd, entry_price,
                    stop_loss_price, take_profit_price, status,
                    opened_at, closed_at, exit_price, pnl, dry_run,
                    sport, league, event_name
             FROM positions ORDER BY opened_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let positions = stmt
            .query_map(params![limit, offset], map_position)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(positions)
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
                home_score, away_score, minute, event_type, detected_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                ev.event_id,
                ev.sport,
                ev.league,
                ev.home_team,
                ev.away_team,
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
                    home_score, away_score, minute, event_type, detected_at
             FROM score_events ORDER BY detected_at DESC LIMIT ?1",
        )?;
        let events = stmt
            .query_map(params![limit], map_score_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    /// Get aggregate trading stats
    pub fn get_stats(&self) -> Result<Stats> {
        let conn = self.conn.lock().unwrap();
        let total_trades: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions WHERE status != 'open'", [], |r| r.get(0))
            .unwrap_or(0);
        let winning_trades: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions WHERE pnl > 0", [], |r| r.get(0))
            .unwrap_or(0);
        let total_pnl: f64 = conn
            .query_row("SELECT COALESCE(SUM(pnl),0) FROM positions", [], |r| r.get(0))
            .unwrap_or(0.0);
        let open_positions: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions WHERE status='open'", [], |r| r.get(0))
            .unwrap_or(0);
        let balance = {
            conn.query_row(
                "SELECT balance FROM balance_history ORDER BY recorded_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            ).unwrap_or(0.0)
        };
        Ok(Stats {
            total_trades,
            winning_trades,
            total_pnl,
            open_positions,
            current_balance: balance,
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
}

// ── SQL helpers ────────────────────────────────────────────────────────────────

fn map_position(row: &rusqlite::Row) -> rusqlite::Result<Position> {
    Ok(Position {
        id: row.get(0)?,
        market_id: row.get(1)?,
        outcome: row.get(2)?,
        side: row.get(3)?,
        size_usd: row.get(4)?,
        entry_price: row.get(5)?,
        stop_loss_price: row.get(6)?,
        take_profit_price: row.get(7)?,
        status: row.get(8)?,
        opened_at: row.get(9)?,
        closed_at: row.get(10)?,
        exit_price: row.get(11)?,
        pnl: row.get(12)?,
        dry_run: row.get(13)?,
        sport: row.get(14)?,
        league: row.get(15)?,
        event_name: row.get(16)?,
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
        home_score: row.get(6)?,
        away_score: row.get(7)?,
        minute: row.get(8)?,
        event_type: row.get(9)?,
        detected_at: row.get(10)?,
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
    outcome           TEXT    NOT NULL,
    side              TEXT    NOT NULL,
    size_usd          REAL    NOT NULL,
    entry_price       REAL    NOT NULL,
    stop_loss_price   REAL    NOT NULL,
    take_profit_price REAL    NOT NULL,
    status            TEXT    NOT NULL DEFAULT 'open',
    opened_at         TEXT    NOT NULL,
    closed_at         TEXT,
    exit_price        REAL,
    pnl               REAL,
    dry_run           INTEGER NOT NULL DEFAULT 1,
    sport             TEXT,
    league            TEXT,
    event_name        TEXT,
    FOREIGN KEY (market_id) REFERENCES markets(id)
);

CREATE TABLE IF NOT EXISTS score_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id    TEXT    NOT NULL,
    sport       TEXT    NOT NULL,
    league      TEXT    NOT NULL,
    home_team   TEXT    NOT NULL,
    away_team   TEXT    NOT NULL,
    home_score  INTEGER NOT NULL,
    away_score  INTEGER NOT NULL,
    minute      INTEGER,
    event_type  TEXT    NOT NULL,
    detected_at TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_market ON positions(market_id);
CREATE INDEX IF NOT EXISTS idx_score_events_event ON score_events(event_id);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub total_trades: i64,
    pub winning_trades: i64,
    pub total_pnl: f64,
    pub open_positions: i64,
    pub current_balance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub balance: f64,
    pub recorded_at: DateTime<Utc>,
}
