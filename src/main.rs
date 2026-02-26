use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

mod bot;
mod config;
mod dashboard;
mod db;
mod live_scores;
mod polymarket;

use bot::BotEngine;
use config::Config;
use dashboard::AppState;
use db::Database;
use live_scores::{start_score_monitor, TheSportsDB};
use polymarket::PolymarketClient;
use live_scores::ScoreProvider;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise tracing / logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();
    config.validate()?;

    if config.dry_run {
        info!(
            "🟡 DRY RUN mode – no real trades will be placed (initial balance: ${:.2})",
            config.initial_balance
        );
    } else {
        info!("🔴 LIVE mode – real trades WILL be placed on Polymarket");
    }

    // Open database
    let db = Database::open(&config.database_path)?;
    info!("Database opened: {}", config.database_path);

    // Seed initial balance if not yet recorded
    if db.get_balance()? <= 0.0 {
        db.record_balance(config.initial_balance)?;
        info!("Initial balance recorded: ${:.2}", config.initial_balance);
    }

    // Build Polymarket client
    let polymarket = PolymarketClient::new(
        &config.polymarket_api_url,
        &config.polymarket_clob_url,
        config.polymarket_api_key.clone(),
    )?;

    // Build score providers (multiple for parallel redundancy).
    // Additional providers can be added here for faster signal.
    let mut score_providers: Vec<Arc<dyn ScoreProvider>> = Vec::new();
    score_providers.push(Arc::new(TheSportsDB::new(
        config.live_scores_api_key.as_deref(),
        None,
    )?));
    // TODO: Add more providers here for parallel data ingestion:
    // score_providers.push(Arc::new(ApiFootball::new(api_key)?));
    // score_providers.push(Arc::new(SportRadar::new(api_key)?));
    info!("Configured {} score provider(s)", score_providers.len());

    // Start the dashboard HTTP server
    let dashboard_state = AppState {
        db: db.clone(),
        dry_run: config.dry_run,
        initial_balance: config.initial_balance,
    };
    let app = dashboard::router(dashboard_state);
    let addr: SocketAddr = config.dashboard_addr.parse()?;
    info!("Dashboard listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Start bot engine in its own task
    let bot_config = config.clone();
    let bot_db = db.clone();
    let bot_polymarket = polymarket.clone();
    let poll_interval = Duration::from_secs(config.poll_interval_secs);

    tokio::spawn(async move {
        let mut rx = start_score_monitor(score_providers, poll_interval);

        let mut engine = match BotEngine::new(bot_config.clone(), bot_db.clone(), bot_polymarket) {
            Ok(e) => e,
            Err(err) => {
                error!("Failed to create bot engine: {}", err);
                return;
            }
        };

        // Background market-discovery task
        {
            let poly_clone = PolymarketClient::new(
                &bot_config.polymarket_api_url,
                &bot_config.polymarket_clob_url,
                bot_config.polymarket_api_key.clone(),
            );
            let db_clone = bot_db.clone();
            if let Ok(poly) = poly_clone {
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(300));
                    loop {
                        interval.tick().await;
                        match poly.fetch_sports_markets().await {
                            Ok(markets) => {
                                info!("Discovered {} Polymarket sports markets", markets.len());
                                for m in &markets {
                                    if let Err(e) = db_clone.upsert_market(m) {
                                        warn!("Failed to upsert market {}: {}", m.id, e);
                                    }
                                }
                            }
                            Err(e) => warn!("Market discovery failed: {}", e),
                        }
                    }
                });
            }
        }

        // Main event loop: process score changes + position management sweep
        let mut position_sweep_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                Some((score_event, live_game)) = rx.recv() => {
                    if let Err(e) = engine.on_score_event(&score_event, &live_game).await {
                        error!("Error processing score event: {}", e);
                    }
                }
                _ = position_sweep_interval.tick() => {
                    if let Err(e) = engine.manage_positions().await {
                        error!("Error managing positions: {}", e);
                    }
                }
            }
        }
    });

    // Run dashboard server (blocks until shutdown)
    axum::serve(listener, app).await?;

    Ok(())
}
