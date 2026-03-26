//! TokioParasite: Lead-Lag Arbitrage Bot
//!
//! Entry point for the bot. Orchestrates all modules:
//! - Configuration loading
//! - Exchange connectivity (EAL)
//! - Signal processing pipeline
//! - Order management (OMS)
//! - Paper trading simulation
//! - Persistence and logging

mod config;
mod eal;
mod logging;
mod oms;
mod persist;
mod signal;

use config::Settings;
use eal::{BinanceExchange, HyperliquidExchange, MarketData, MockExchange, VenueId};
use logging::init_logging;
use oms::OrderManagementSystem;
use persist::{StateStore, TelemetryWriter};
use signal::SignalPipeline;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load settings
    let settings = Settings::load()?;
    init_logging(&settings.app.log_level);

    info!("Starting TokioParasite Lead-Lag Bot");
    info!("Log level: {}", settings.app.log_level);
    info!("Paper trading: {}", settings.simulation.enabled);
    info!("CPU pinning: {}", settings.app.cpu_pinning);

    // Initialize storage
    persist::init_storage(&settings.storage)?;

    // Initialize state store
    let state_store = StateStore::open(&settings.storage.state_db_path)?;
    info!("State store opened at {}", settings.storage.state_db_path);

    // Initialize telemetry writer
    let mut telemetry = TelemetryWriter::new(&settings.storage.telemetry_path)?;
    info!("Telemetry writer initialized at {}", settings.storage.telemetry_path);

    // Initialize kill switches
    let kill_switch_a = Arc::new(AtomicBool::new(false));
    let kill_switch_b = Arc::new(AtomicBool::new(false));

    // Initialize OMS
    let mut oms = OrderManagementSystem::new(settings.risk.clone(), settings.strategy.clone());
    oms.register_kill_switch(VenueId::EXCHANGE_A, kill_switch_a.clone());
    oms.register_kill_switch(VenueId::EXCHANGE_B, kill_switch_b.clone());
    info!("OMS initialized with risk limits");

    // Initialize signal pipeline
    let mut pipeline = SignalPipeline::<256>::new(settings.strategy.clone());
    info!("Signal pipeline initialized with {} symbols", settings.strategy.symbols.len());

    // Initialize exchanges based on configuration
    let (exchange_a, exchange_b): (Box<dyn MarketData>, Box<dyn MarketData>) =
        if settings.simulation.use_real_data {
            info!("Using real market data feeds (Binance, Hyperliquid)");
            (
                Box::new(BinanceExchange::new()),
                Box::new(HyperliquidExchange::new()),
            )
        } else {
            info!("Using mock exchanges for paper trading");
            (
                Box::new(MockExchange::new(VenueId::EXCHANGE_A)),
                Box::new(MockExchange::new(VenueId::EXCHANGE_B)),
            )
        };

    // Subscribe to market data
    let symbols: Vec<eal::Symbol> = settings
        .strategy
        .symbols
        .iter()
        .map(|s| eal::Symbol::new(s))
        .collect();

    let tick_rx_a = exchange_a.subscribe_ticks(&symbols).await?;
    let tick_rx_b = exchange_b.subscribe_ticks(&symbols).await?;
    info!("Subscribed to market data for {} symbols", symbols.len());

    // Main event loop
    info!("Entering main event loop...");
    let mut tick_count = 0u64;

    loop {
        // Check kill switches
        if kill_switch_a.load(Ordering::SeqCst) {
            warn!("Kill switch A activated!");
            break;
        }
        if kill_switch_b.load(Ordering::SeqCst) {
            warn!("Kill switch B activated!");
            break;
        }

        // Process ticks from exchange A
        if let Ok(tick) = tick_rx_a.try_recv() {
            tick_count += 1;
            telemetry.log_tick(&tick);

            // Process through time grid and signal pipeline
            // (Simplified for now - full implementation would use TimeGrid)
            if tick_count % 100 == 0 {
                info!("Processed {} ticks", tick_count);
            }
        }

        // Process ticks from exchange B
        if let Ok(tick) = tick_rx_b.try_recv() {
            tick_count += 1;
            telemetry.log_tick(&tick);
        }

        // Small sleep to prevent busy-waiting
        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
    }

    // Shutdown
    info!("Shutting down TokioParasite...");
    telemetry.shutdown();
    state_store.flush()?;
    info!("Shutdown complete. Processed {} ticks total.", tick_count);

    Ok(())
}