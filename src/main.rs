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
mod sim;

use config::Settings;
use eal::{BinanceExchange, HyperliquidExchange, MarketData, MockExchange, VenueId};
use logging::init_logging;
use oms::OrderManagementSystem;
use persist::{StateStore, TelemetryWriter};
use signal::{SignalPipeline, TimeGrid};
use sim::PaperSimulator;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load settings
    let settings = Settings::load()?;
    init_logging(&settings.app.log_level);

    info!("Starting TokioParasite Lead-Lag Bot");
    info!("Log level: {}", settings.app.log_level);
    info!("Active strategy: {}", settings.strategy.active_strategy);
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
    pipeline.set_precision(settings.app.tick_precision_ns);
    info!(
        "Signal pipeline initialized with {} symbols",
        settings.strategy.symbols.len()
    );

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
    info!("Subscribed to ticks for {} symbols", symbols.len());

    // Subscribe to order book data for OBI-based strategies
    let use_obi = settings.strategy.active_strategy == "impulse_obi";
    let (book_rx_a, book_rx_b) = if use_obi {
        info!("OBI strategy active — subscribing to order book data");
        // Try to subscribe per-symbol; fall back gracefully if not implemented
        let mut book_a = None;
        let mut book_b = None;
        for sym in &symbols {
            match exchange_a.subscribe_book(sym).await {
                Ok(rx) => {
                    info!("Book subscription successful for {:?} {}", VenueId::EXCHANGE_A, sym);
                    book_a = Some(rx);
                }
                Err(e) => {
                    warn!(
                        "Book subscription not available for {:?} {}: {} (OBI signals will be limited)",
                        VenueId::EXCHANGE_A, sym, e
                    );
                }
            }
            match exchange_b.subscribe_book(sym).await {
                Ok(rx) => {
                    info!("Book subscription successful for {:?} {}", VenueId::EXCHANGE_B, sym);
                    book_b = Some(rx);
                }
                Err(e) => {
                    warn!(
                        "Book subscription not available for {:?} {}: {} (OBI signals will be limited)",
                        VenueId::EXCHANGE_B, sym, e
                    );
                }
            }
        }
        (book_a, book_b)
    } else {
        (None, None)
    };

    // Initialize time grid for tick alignment
    let mut timegrid = TimeGrid::new(settings.app.tick_precision_ns);
    info!(
        "Time grid initialized with {}ns precision",
        settings.app.tick_precision_ns
    );

    // Initialize paper simulator for order execution (Issue 3 fix)
    let simulator = PaperSimulator::new(settings.simulation.clone());

    // Main event loop
    info!("Entering main event loop...");
    let mut tick_count = 0u64;
    let mut signal_count = 0u64;

    // Track last seen price per venue for book synthesis
    let mut last_price_a: Option<f64> = None;
    let mut last_price_b: Option<f64> = None;

    // Per-venue tick counters for heartbeat
    let mut tick_count_a = 0u64;
    let mut tick_count_b = 0u64;
    let mut last_heartbeat = std::time::Instant::now();

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
            tick_count_a += 1;
            telemetry.log_tick(&tick);
            last_price_a = Some(tick.price);

            // Update Exchange A's book at its actual price
            simulator.update_book_from_tick(&tick.symbol, tick.price, tick.venue);

            // Seed Exchange B's book from A's price if B hasn't sent any ticks yet.
            // In real trading, B's book exists independently. Here we approximate it.
            if last_price_b.is_none() {
                simulator.update_book_from_tick(&tick.symbol, tick.price, VenueId::EXCHANGE_B);
            }

            // Process through time grid (zero-cost, no heap allocation)
            let ingest_result = timegrid.ingest_tick(&tick);

            // Process through signal pipeline — route based on strategy
            for pair in ingest_result.iter() {
                if let Some(signal) = pipeline.process_pair(&tick.symbol, pair) {
                    signal_count += 1;
                    info!(
                        "Signal generated: {} {} at R={:.3}",
                        signal.side, signal.symbol, signal.correlation_r
                    );

                    telemetry.log_signal(
                        &signal.symbol.to_string(),
                        &signal.side.to_string(),
                        signal.correlation_r,
                        signal.lag_offset_ns,
                    );

                    // Use target venue's price, not source tick's price
                    let exec_price = simulator.get_mid_price(&signal.symbol, signal.target_venue)
                        .unwrap_or(tick.price);
                    match oms.process_signal(&signal, exec_price, &simulator).await {
                        Ok(ack) => {
                            info!("Order submitted: {}", ack.order_id);
                        }
                        Err(e) => {
                            warn!("Order rejected: {}", e);
                        }
                    }
                }
            }

            // Impulse-OBI: process tick directly for impulse detection
            if let Some(signal) = pipeline.process_tick(&tick) {
                signal_count += 1;
                info!(
                    "Impulse signal: {} {} (r={:.3})",
                    signal.side, signal.symbol, signal.correlation_r
                );

                telemetry.log_signal(
                    &signal.symbol.to_string(),
                    &signal.side.to_string(),
                    signal.correlation_r,
                    signal.lag_offset_ns,
                );

                // Use target venue's price for risk checks and execution
                let exec_price = simulator.get_mid_price(&signal.symbol, signal.target_venue)
                    .unwrap_or(tick.price);
                match oms.process_signal(&signal, exec_price, &simulator).await {
                    Ok(ack) => {
                        info!("Order submitted: {}", ack.order_id);
                    }
                    Err(e) => {
                        warn!("Order rejected: {}", e);
                    }
                }
            }

            if tick_count % 100 == 0 {
                info!(
                    "Processed {} ticks, generated {} signals",
                    tick_count, signal_count
                );
            }

            // Log position summary every 50 ticks
            if tick_count % 50 == 0 {
                let fills = simulator.fill_history();
                let total_fees = simulator.total_fees();

                if !fills.is_empty() {
                    // Aggregate positions from fill history
                    use std::collections::BTreeMap;
                    let mut pos_map: BTreeMap<String, (f64, f64, VenueId, eal::Symbol)> = BTreeMap::new();
                    for fill in &fills {
                        let key = format!("{:?}{}", fill.venue, fill.symbol);
                        let entry = pos_map.entry(key).or_insert((0.0, 0.0, fill.venue, fill.symbol.clone()));
                        let signed = match fill.side {
                            eal::OrderSide::Buy => fill.filled_size,
                            eal::OrderSide::Sell => -fill.filled_size,
                        };
                        if entry.0 == 0.0 {
                            entry.1 = fill.avg_price;
                        }
                        entry.0 += signed;
                    }

                    let mut net_pnl = 0.0;
                    let mut pos_summary = String::new();
                    for (_, (size, entry_price, venue, symbol)) in &pos_map {
                        let current_mid = simulator.get_mid_price(symbol, *venue)
                            .unwrap_or(*entry_price);
                        let unrealized = if *entry_price > 0.0 {
                            *size * (current_mid - *entry_price)
                        } else { 0.0 };
                        net_pnl += unrealized;
                        pos_summary.push_str(&format!(
                            "\n  {:?} {} | size={:.4} | entry={:.4} | mid={:.4} | uPnL={:.2}",
                            venue, symbol, size, entry_price, current_mid, unrealized
                        ));
                    }
                    info!(
                        "POSITIONS ({} fills, fees={:.4}):{}\n  NET PnL = {:.2}",
                        fills.len(), total_fees, pos_summary, net_pnl
                    );
                }
            }
        }

        // Process ticks from exchange B
        if let Ok(tick) = tick_rx_b.try_recv() {
            tick_count += 1;
            tick_count_b += 1;
            telemetry.log_tick(&tick);
            last_price_b = Some(tick.price);

            // Update Exchange B's book at its actual price
            simulator.update_book_from_tick(&tick.symbol, tick.price, tick.venue);

            // Seed Exchange A's book from B's price if A hasn't sent any ticks yet
            if last_price_a.is_none() {
                simulator.update_book_from_tick(&tick.symbol, tick.price, VenueId::EXCHANGE_A);
            }

            let ingest_result = timegrid.ingest_tick(&tick);

            for pair in ingest_result.iter() {
                if let Some(signal) = pipeline.process_pair(&tick.symbol, pair) {
                    signal_count += 1;
                    info!(
                        "Signal generated: {} {} at R={:.3}",
                        signal.side, signal.symbol, signal.correlation_r
                    );

                    telemetry.log_signal(
                        &signal.symbol.to_string(),
                        &signal.side.to_string(),
                        signal.correlation_r,
                        signal.lag_offset_ns,
                    );

                    // Use target venue's price, not source tick's price
                    let exec_price = simulator.get_mid_price(&signal.symbol, signal.target_venue)
                        .unwrap_or(tick.price);
                    match oms.process_signal(&signal, exec_price, &simulator).await {
                        Ok(ack) => {
                            info!("Order submitted: {}", ack.order_id);
                        }
                        Err(e) => {
                            warn!("Order rejected: {}", e);
                        }
                    }
                }
            }

            // Impulse-OBI: process tick directly
            if let Some(signal) = pipeline.process_tick(&tick) {
                signal_count += 1;
                info!(
                    "Impulse signal: {} {} (r={:.3})",
                    signal.side, signal.symbol, signal.correlation_r
                );

                telemetry.log_signal(
                    &signal.symbol.to_string(),
                    &signal.side.to_string(),
                    signal.correlation_r,
                    signal.lag_offset_ns,
                );

                // Use target venue's price for risk checks and execution
                let exec_price = simulator.get_mid_price(&signal.symbol, signal.target_venue)
                    .unwrap_or(tick.price);
                match oms.process_signal(&signal, exec_price, &simulator).await {
                    Ok(ack) => {
                        info!("Order submitted: {}", ack.order_id);
                    }
                    Err(e) => {
                        warn!("Order rejected: {}", e);
                    }
                }
            }
        }

        // Process book updates from exchange A (Issue 2 fix)
        if let Some(ref book_rx) = book_rx_a {
            if let Ok(book) = book_rx.try_recv() {
                if let Some(signal) = pipeline.process_book(&book) {
                    signal_count += 1;
                    info!(
                        "OBI signal: {} {} (r={:.3})",
                        signal.side, signal.symbol, signal.correlation_r
                    );

                    telemetry.log_signal(
                        &signal.symbol.to_string(),
                        &signal.side.to_string(),
                        signal.correlation_r,
                        signal.lag_offset_ns,
                    );

                    let price = book.best_bid().unwrap_or(0.0);
                    match oms.process_signal(&signal, price, &simulator).await {
                        Ok(ack) => {
                            info!("Order submitted: {}", ack.order_id);
                        }
                        Err(e) => {
                            warn!("Order rejected: {}", e);
                        }
                    }
                }
            }
        }

        // Process book updates from exchange B (Issue 2 fix)
        if let Some(ref book_rx) = book_rx_b {
            if let Ok(book) = book_rx.try_recv() {
                if let Some(signal) = pipeline.process_book(&book) {
                    signal_count += 1;
                    info!(
                        "OBI signal: {} {} (r={:.3})",
                        signal.side, signal.symbol, signal.correlation_r
                    );

                    telemetry.log_signal(
                        &signal.symbol.to_string(),
                        &signal.side.to_string(),
                        signal.correlation_r,
                        signal.lag_offset_ns,
                    );

                    let price = book.best_bid().unwrap_or(0.0);
                    match oms.process_signal(&signal, price, &simulator).await {
                        Ok(ack) => {
                            info!("Order submitted: {}", ack.order_id);
                        }
                        Err(e) => {
                            warn!("Order rejected: {}", e);
                        }
                    }
                }
            }
        }

        // Heartbeat every 5 seconds
        if last_heartbeat.elapsed().as_secs() >= 5 {
            info!(
                "HEARTBEAT | A ticks: {} | B ticks: {} | total: {} | signals: {}",
                tick_count_a, tick_count_b, tick_count, signal_count
            );
            last_heartbeat = std::time::Instant::now();
        }

        // Yield to scheduler — avoid blocking the async runtime.
        // On a dedicated hot-path OS thread, this would be std::hint::spin_loop().
        // (Issue 11: replaced sleep with yield_now for lower latency)
        tokio::task::yield_now().await;
    }

    // Shutdown
    info!("Shutting down TokioParasite...");
    telemetry.shutdown();
    state_store.flush()?;
    info!("Shutdown complete. Processed {} ticks total.", tick_count);

    Ok(())
}
