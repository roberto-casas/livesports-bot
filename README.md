# livesports-bot

A Rust-based Polymarket sports betting bot with live score monitoring and a web dashboard.

## Features

- **Live Score Monitoring** – polls TheSportsDB (and any `ScoreProvider` implementation) for score changes in NFL, NBA, MLS, Premier League, NHL, MLB and more
- **Polymarket Integration** – searches for matching prediction markets and interacts with the Gamma API and CLOB API
- **Kelly Criterion Betting** – sizes each bet using fractional Kelly to balance risk and reward
- **Automatic Position Management** – stop-loss and take-profit thresholds close positions within seconds of price movement
- **Dry-Run Mode** – simulates trades with a configurable virtual balance (default $100) without touching real funds
- **SQLite Persistence** – all markets, positions, score events and balance history are stored locally
- **Web Dashboard** – live HTML/JS dashboard served on port 8080 showing P&L, positions, live events and balance chart

## Quick Start

### Prerequisites
- Rust 1.70+

### Build
```bash
cargo build --release
```

### Run in dry-run mode (default $100 virtual balance)
```bash
./target/release/livesports-bot --dry-run
```

### Run with a custom balance
```bash
./target/release/livesports-bot --dry-run --initial-balance 500
```

### Run in live mode (requires Polymarket credentials)
```bash
export POLYMARKET_API_KEY=your_api_key
export POLYMARKET_PRIVATE_KEY=your_private_key
./target/release/livesports-bot
```

Open `http://localhost:8080` to view the dashboard.

## Configuration

All options can be set via CLI flags or environment variables:

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--dry-run` | `DRY_RUN` | `false` | Simulate trades (no real funds) |
| `--initial-balance` | `INITIAL_BALANCE` | `100.0` | Starting virtual balance (USD) |
| `--dashboard-addr` | `DASHBOARD_ADDR` | `0.0.0.0:8080` | Dashboard listen address |
| `--database-path` | `DATABASE_PATH` | `livesports.db` | SQLite database path |
| `--polymarket-api-url` | `POLYMARKET_API_URL` | `https://gamma-api.polymarket.com` | Polymarket Gamma API |
| `--polymarket-clob-url` | `POLYMARKET_CLOB_URL` | `https://clob.polymarket.com` | Polymarket CLOB API |
| `--polymarket-api-key` | `POLYMARKET_API_KEY` | – | Required for live trading |
| `--polymarket-private-key` | `POLYMARKET_PRIVATE_KEY` | – | Required for live trading |
| `--live-scores-api-key` | `LIVE_SCORES_API_KEY` | `3` (free tier) | TheSportsDB API key |
| `--kelly-fraction` | `KELLY_FRACTION` | `0.25` | Fractional Kelly multiplier |
| `--stop-loss-fraction` | `STOP_LOSS_FRACTION` | `0.50` | Stop-loss as fraction of position |
| `--take-profit-fraction` | `TAKE_PROFIT_FRACTION` | `0.30` | Take-profit as fraction of entry |
| `--min-edge` | `MIN_EDGE` | `0.05` | Minimum edge (5%) to place a bet |
| `--poll-interval-secs` | `POLL_INTERVAL_SECS` | `5` | Score polling interval in seconds |

## Architecture

```
src/
├── main.rs              # Entry point, CLI, async runtime
├── config.rs            # Clap-based configuration
├── bot/
│   ├── kelly.rs         # Kelly criterion calculator
│   ├── position.rs      # Stop-loss / take-profit evaluation
│   └── strategy.rs      # BotEngine: orchestrates events → trades
├── polymarket/
│   └── client.rs        # Polymarket Gamma + CLOB API client
├── live_scores/
│   ├── provider.rs      # ScoreProvider trait
│   └── sports.rs        # TheSportsDB implementation + change detection
├── db/
│   ├── mod.rs           # SQLite CRUD layer
│   └── models.rs        # Rust data models
└── dashboard/
    └── mod.rs           # Axum HTTP server + embedded HTML dashboard
```

## Dashboard API

| Endpoint | Description |
|----------|-------------|
| `GET /` | Dashboard UI |
| `GET /api/stats` | Trading statistics (balance, P&L, win rate) |
| `GET /api/positions` | Recent positions (last 50) |
| `GET /api/markets` | Active Polymarket markets |
| `GET /api/score-events` | Recent live score events |
| `GET /api/balance-history` | Balance over time (for chart) |

## Testing

```bash
cargo test
```

22 unit tests cover:
- Kelly criterion edge cases
- Position stop-loss / take-profit evaluation
- Win probability estimation
- Live score change detection
- Sport-specific event classification (soccer goals, NFL touchdowns, NBA baskets, …)
