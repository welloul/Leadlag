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

/// Normalize venue symbol to a canonical form for cross-venue keying.
/// Strips common suffixes like "USDT", "USDC" so Binance's "ZECUSDT"
/// matches Hyperliquid's "ZEC" in the simulator.
fn normalize_symbol(sym: &eal::Symbol) -> eal::Symbol {
    let s = &sym.0;
    if let Some(stripped) = s.strip_suffix("USDT") {
        return eal::Symbol::new(stripped);
    }
    if let Some(stripped) = s.strip_suffix("USDC") {
        return eal::Symbol::new(stripped);
    }
    sym.clone()
}

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

    // Subscribe to L2 order book data for ALL symbols on ALL venues.
    // The simulator needs real book data for fills — synthetic books are stale.
    // Collect all receivers (don't overwrite — each symbol gets its own channel).
    let mut book_receivers: Vec<(eal::Symbol, VenueId, crossbeam_channel::Receiver<std::sync::Arc<eal::BookUpdate>>)> = Vec::new();
    for sym in &symbols {
        match exchange_a.subscribe_book(sym).await {
            Ok(rx) => {
                info!("Book subscription: {:?} {}", VenueId::EXCHANGE_A, sym);
                book_receivers.push((sym.clone(), VenueId::EXCHANGE_A, rx));
            }
            Err(e) => {
                warn!("Book subscription failed for {:?} {}: {}", VenueId::EXCHANGE_A, sym, e);
            }
        }
        match exchange_b.subscribe_book(sym).await {
            Ok(rx) => {
                info!("Book subscription: {:?} {}", VenueId::EXCHANGE_B, sym);
                book_receivers.push((sym.clone(), VenueId::EXCHANGE_B, rx));
            }
            Err(e) => {
                warn!("Book subscription failed for {:?} {}: {}", VenueId::EXCHANGE_B, sym, e);
            }
        }
    }
    info!("Book subscriptions: {} active", book_receivers.len());

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

    // Per-symbol performance tracking
    let mut symbol_fills: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut symbol_rejects: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

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

            // Update Exchange A's book at its actual price.
            // Normalize symbol (strip USDT suffix) so Binance and HL share the same key.
            // Only update the venue that sent the tick — never seed other venues with fake data.
            let norm_sym = normalize_symbol(&tick.symbol);
            simulator.update_book_from_tick(&norm_sym, tick.price, tick.venue);

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

                // Check target venue book staleness.
                // Normalize symbol for cross-venue keying (Binance: ZECUSDT → ZEC).
                // Allow stale books <2s old. Never seed with fake data.
                let sig_sym = normalize_symbol(&signal.symbol);
                let staleness_ms = simulator.book_staleness_ns(&sig_sym, signal.target_venue)
                    .map(|ns| ns as f64 / 1e6);
                let has_book = simulator.is_venue_liquid(&sig_sym, signal.target_venue);

                match (has_book, staleness_ms) {
                    (false, _) => {
                        tracing::debug!("SKIP: no book for {:?} {}", signal.target_venue, sig_sym);
                        *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                    }
                    (true, Some(ms)) if ms > 400.0 => {
                        tracing::debug!("SKIP: book {:.0}ms stale (>400ms) for {:?} {}", ms, signal.target_venue, sig_sym);
                        *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                    }
                    _ => {
                        let exec_price = simulator.get_mid_price(&sig_sym, signal.target_venue)
                            .unwrap_or(tick.price);
                        match oms.process_signal(&signal, exec_price, &simulator).await {
                            Ok(ack) => {
                                info!("Order submitted: {}", ack.order_id);
                                *symbol_fills.entry(sig_sym.0.clone()).or_insert(0) += 1;
                            }
                            Err(e) => {
                                warn!("Order rejected: {}", e);
                                *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                            }
                        }
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

            // Update Exchange B's book at its actual price.
            // Normalize symbol so Binance and HL share the same key.
            let norm_sym = normalize_symbol(&tick.symbol);
            simulator.update_book_from_tick(&norm_sym, tick.price, tick.venue);

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

                // Check target venue book staleness — allow stale <2s, never seed fake data
                let sig_sym = normalize_symbol(&signal.symbol);
                let staleness_ms = simulator.book_staleness_ns(&sig_sym, signal.target_venue)
                    .map(|ns| ns as f64 / 1e6);
                let has_book = simulator.is_venue_liquid(&sig_sym, signal.target_venue);

                match (has_book, staleness_ms) {
                    (false, _) => {
                        tracing::debug!("SKIP: no book for {:?} {}", signal.target_venue, sig_sym);
                        *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                    }
                    (true, Some(ms)) if ms > 400.0 => {
                        tracing::debug!("SKIP: book {:.0}ms stale (>400ms) for {:?} {}", ms, signal.target_venue, sig_sym);
                        *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                    }
                    _ => {
                        let exec_price = simulator.get_mid_price(&sig_sym, signal.target_venue)
                            .unwrap_or(tick.price);
                        match oms.process_signal(&signal, exec_price, &simulator).await {
                            Ok(ack) => {
                                info!("Order submitted: {}", ack.order_id);
                                *symbol_fills.entry(sig_sym.0.clone()).or_insert(0) += 1;
                            }
                            Err(e) => {
                                warn!("Order rejected: {}", e);
                                *symbol_rejects.entry(sig_sym.0.clone()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }

        // Process L2 order book updates from all venues.
        // Feed real book data into simulator (per-venue books) and pipeline (OBI signals).
        for (_sym, _venue, ref book_rx) in &book_receivers {
            if let Ok(book) = book_rx.try_recv() {
                // Feed real book into simulator
                simulator.update_book((*book).clone());

                // Feed into signal pipeline for OBI detection
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
            let sim_metrics = simulator.metrics();
            info!(
                "HEARTBEAT | A ticks: {} | B ticks: {} | total: {} | signals: {} | {}",
                tick_count_a, tick_count_b, tick_count, signal_count, sim_metrics.summary()
            );

            // Per-symbol performance tracking
            let mut sym_stats: Vec<String> = Vec::new();
            for sym in &symbols {
                let fills = symbol_fills.get(&sym.0).unwrap_or(&0);
                let rejects = symbol_rejects.get(&sym.0).unwrap_or(&0);
                let total = fills + rejects;
                let rate = if total > 0 { *fills * 100 / total } else { 0 };
                sym_stats.push(format!("{}: {}/{} ({}%)", sym.0, fills, rejects, rate));
            }
            info!("  SYMBOLS: {}", sym_stats.join(" | "));

            // Time-based exit: close positions older than exit_timeout_ms
            let exit_signals = oms.check_time_exits();
            for exit_signal in exit_signals {
                let exec_price = simulator.get_mid_price(&exit_signal.symbol, exit_signal.target_venue)
                    .unwrap_or(0.0);
                if exec_price > 0.0 {
                    match oms.process_exit_signal(&exit_signal, exec_price, &simulator).await {
                        Ok(ack) => {
                            info!("TIME EXIT submitted: {} {} on {:?} @ ${:.4}",
                                exit_signal.side, exit_signal.symbol, exit_signal.target_venue, exec_price);
                        }
                        Err(e) => {
                            warn!("TIME EXIT rejected: {}", e);
                        }
                    }
                }
            }

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
