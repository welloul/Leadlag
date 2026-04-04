//! - Configuration loading
//! - Exchange connectivity (EAL)
//! - Signal processing pipeline
//! - Order management (OMS)
//! - Paper trading simulation
//! - Persistence and logging

use crate::config;
use crate::eal;
use crate::oms;
use crate::persist;
use crate::signal;
use crate::sim;

use crate::config::Settings;
use crate::eal::{BinanceExchange, HyperliquidExchange, MarketData, MockExchange, VenueId, OrderExecution};
use crate::oms::OrderManagementSystem;
use crate::persist::{StateStore, TelemetryWriter};
use crate::signal::{SignalPipeline, TimeGrid};
use crate::sim::PaperSimulator;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{info, warn};


pub async fn run(settings: Settings) -> Result<(), Box<dyn std::error::Error>> {
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

    info!("Kill switches initialized");

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

    // Initialize paper simulator for order execution
    let mut simulator = PaperSimulator::new(settings.simulation.clone());
    
    // Asynchronous fill channel for limit order notifications
    let (fill_tx, fill_rx) = crossbeam_channel::unbounded::<eal::FillEvent>();

    // Initialize Paper Execution Engine
    simulator.set_fill_tx(fill_tx.clone());
    info!("Using PaperSimulator for execution");
    info!("Entering main event loop...");
    
    let mut oms = OrderManagementSystem::new(settings.risk.clone(), settings.strategy.clone());

    // Choose active executor for this session
    let executor: &dyn OrderExecution = &simulator;

    // Register kill switches manually for the OMS (pre-flight checks)
    oms.register_kill_switch(eal::VenueId::EXCHANGE_A, kill_switch_a.clone());
    oms.register_kill_switch(eal::VenueId::EXCHANGE_B, kill_switch_b.clone());
    info!("OMS initialized with risk limits");

    let mut tick_count = 0u64;
    let mut signal_count = 0u64;

    // Track last seen price per venue for book synthesis
    let mut last_price_a: Option<f64> = None;
    let mut last_price_b: Option<f64> = None;

    // Per-venue tick counters for heartbeat
    let mut tick_count_a = 0u64;
    let mut tick_count_b = 0u64;
    let mut last_heartbeat = std::time::Instant::now();
    let mut last_config_check = std::time::Instant::now();
    let mut last_exit_check = std::time::Instant::now(); // 500ms position exit timer

    // Per-symbol performance tracking
    let mut symbol_fills: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut symbol_rejects: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    
    // Edge Decay Tracking: Measures how long it takes for the laggard to "catch up" to the leader's move.
    // probe is created on signal, resolved on laggard reaching target price.
    struct DecayProbe {
        target_price: f64,
        side: eal::OrderSide,
        start_ts_ns: u64,
    }
    let mut edge_decay_probes: std::collections::HashMap<eal::Symbol, DecayProbe> = std::collections::HashMap::new();

    // Signal distribution tracking
    let mut high_conviction_count = 0u64;
    let mut medium_conviction_count = 0u64;

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
            
            // Resolve Alpha Decay probes (Laggard Catchup check)
            if let Some(probe) = edge_decay_probes.get(&tick.symbol.normalize()) {
                let reached = match probe.side {
                    eal::OrderSide::Buy => tick.price >= probe.target_price,
                    eal::OrderSide::Sell => tick.price <= probe.target_price,
                };
                if reached {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
                    let decay_ms = (now - probe.start_ts_ns) as f64 / 1_000_000.0;
                    info!("ALPHA_DECAY: {} | decay_ms={:.2} | target={:.4} | side={:?}", 
                        tick.symbol, decay_ms, probe.target_price, probe.side);
                    edge_decay_probes.remove(&tick.symbol.normalize());
                }
            }

            // Update Exchange A's book at its actual price.
            // Normalize symbol (strip USDT suffix) so Binance and HL share the same key.
            // Only update the venue that sent the tick — never seed other venues with fake data.
            let norm_sym = tick.symbol.normalize();
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
                        
                    let exec_tray: &dyn eal::OrderExecution = &simulator;
                    
                    match oms.process_signal(&signal, exec_price, exec_tray).await {
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

                // Start Edge Decay probe: Lead moved, wait for laggard to catch up
                edge_decay_probes.insert(signal.symbol.normalize(), DecayProbe {
                    target_price: tick.price,
                    side: signal.side,
                    start_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                });

                // Check target venue book staleness.
                // Normalize symbol for cross-venue keying (Binance: ZECUSDT → ZEC).
                // Allow stale books <2s old. Never seed with fake data.
                let sig_sym = signal.symbol.normalize();
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
                                info!("ORDER_ENTRY: {} {} | price={:.4} | r={:.2} | id={}", 
                                    signal.side, signal.symbol, exec_price, signal.correlation_r, ack.order_id);
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

            // Resolve Alpha Decay probes (Laggard B Catchup check)
            if let Some(probe) = edge_decay_probes.get(&tick.symbol.normalize()) {
                let reached = match probe.side {
                    eal::OrderSide::Buy => tick.price >= probe.target_price,
                    eal::OrderSide::Sell => tick.price <= probe.target_price,
                };
                if reached {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
                    let decay_ms = (now - probe.start_ts_ns) as f64 / 1_000_000.0;
                    info!("ALPHA_DECAY: {} | decay_ms={:.2} | target={:.4} | side={:?}", 
                        tick.symbol, decay_ms, probe.target_price, probe.side);
                    edge_decay_probes.remove(&tick.symbol.normalize());
                }
            }

            // Update Exchange B's book at its actual price.
            // Normalize symbol so Binance and HL share the same key.
            let norm_sym = tick.symbol.normalize();
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

                // Start Edge Decay probe: Lead moved, wait for laggard to catch up
                edge_decay_probes.insert(signal.symbol.normalize(), DecayProbe {
                    target_price: tick.price,
                    side: signal.side,
                    start_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                });

                // Check target venue book staleness — allow stale <2s, never seed fake data
                let sig_sym = signal.symbol.normalize();
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
        // Feed real book data into simulator (per-venue books) and pipeline (OBI + book-mid impulse).
        for (_sym, _venue, ref book_rx) in &book_receivers {
            if let Ok(book) = book_rx.try_recv() {
                // Feed real book into simulator
                simulator.update_book((*book).clone());

                // Resolve Alpha Decay probes on Book Updates (usually faster than ticks)
                if let Some(probe) = edge_decay_probes.get(&book.symbol.normalize()) {
                    if let Some(mid) = book.mid_price() {
                        let reached = match probe.side {
                            eal::OrderSide::Buy => mid >= probe.target_price,
                            eal::OrderSide::Sell => mid <= probe.target_price,
                        };
                        if reached {
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
                            let decay_ms = (now - probe.start_ts_ns) as f64 / 1_000_000.0;
                            info!("ALPHA_DECAY: {} | decay_ms={:.2} | target={:.4} | side={:?} (from book)", 
                                book.symbol, decay_ms, probe.target_price, probe.side);
                            edge_decay_probes.remove(&book.symbol.normalize());
                        }
                    }
                }

                // Book-mid impulse detection (more reliable than trade price)
                if let Some(signal) = pipeline.process_book_for_impulse(&book) {
                    signal_count += 1;
                    high_conviction_count += 1; // Book-mid signals are always HIGH quality
                    info!(
                        "BOOK_IMPULSE: {} {} on {:?} r={:.2} (book-mid)",
                        signal.side, signal.symbol, signal.target_venue, signal.correlation_r
                    );

                    let sig_sym = signal.symbol.normalize();
                    if simulator.is_venue_liquid(&sig_sym, signal.target_venue) {
                        let exec_price = simulator.get_mid_price(&sig_sym, signal.target_venue)
                            .unwrap_or(0.0);
                        
                        // Start Edge Decay probe on book-based impulse
                        edge_decay_probes.insert(sig_sym.clone(), DecayProbe {
                            target_price: book.best_bid().unwrap_or(0.0), // Approximate lead price
                            side: signal.side,
                            start_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                        });
                        if exec_price > 0.0 {
                            match oms.process_signal(&signal, exec_price, executor).await {
                                Ok(ack) => {
                                    info!("ORDER_ENTRY: {} {} | price={:.4} | r={:.2} | id={}", 
                                        signal.side, signal.symbol, exec_price, signal.correlation_r, ack.order_id);
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

                    let sig_sym = signal.symbol.normalize();
                    
                    // Start Edge Decay probe on OBI signals
                    edge_decay_probes.insert(sig_sym.clone(), DecayProbe {
                        target_price: book.mid_price().unwrap_or(0.0), // Current lead price
                        side: signal.side,
                        start_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                    });
                    let exec_price = simulator.get_mid_price(&sig_sym, signal.target_venue)
                        .unwrap_or_else(|| book.best_bid().unwrap_or(0.0));
                    match oms.process_signal(&signal, exec_price, executor).await {
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

        // Heartbeat every 5 seconds
        if last_heartbeat.elapsed().as_secs() >= 5 {
            info!(
                "HEARTBEAT | A ticks: {} | B ticks: {} | total: {} | signals: {}",
                tick_count_a, tick_count_b, tick_count, signal_count
            );

            let positions = oms.net_delta().positions();
            if !positions.is_empty() {
                info!("OMS POSITIONS (Total: {}):", positions.len());
                for pos in positions {
                    info!("  {:?} {} | size={:.4} | entry={:.4}", 
                        pos.venue, pos.symbol, pos.size, pos.entry_price);
                }
            }

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

            // Signal distribution
            let total_signals = high_conviction_count + medium_conviction_count;
            let high_pct = if total_signals > 0 { high_conviction_count * 100 / total_signals } else { 0 };
            let med_pct = if total_signals > 0 { medium_conviction_count * 100 / total_signals } else { 0 };
            info!("  SIGNALS: HIGH={} ({}%) MEDIUM={} ({}%)", high_conviction_count, high_pct, medium_conviction_count, med_pct);

            // (Exit checks moved to 500ms timer below — do NOT duplicate here)

            last_heartbeat = std::time::Instant::now();
        }

        // ── Position Exit & TTL check every 500ms ──────────────────────────────
        if last_exit_check.elapsed().as_millis() >= 500 {
            // (Live executors process rejected CLOIDs here. The PaperSimulator does not need to evict real cloids)

            // Cancel orders whose TTL has lapsed
            oms.check_pending_ttl(executor).await;

            // Close positions older than exit_timeout_ms
            let exit_signals = oms.check_time_exits();
            for exit_signal in exit_signals {
                let exec_price = simulator
                    .get_mid_price(&exit_signal.symbol, exit_signal.target_venue)
                    .unwrap_or(0.0);
                if exec_price > 0.0 {
                    match oms.process_exit_signal(&exit_signal, exec_price, executor).await {
                        Ok(ack) => {
                            info!(
                                "TIME_EXIT: {} {} | price={:.4} | id={}",
                                exit_signal.side, exit_signal.symbol, exec_price, ack.order_id
                            );
                        }
                        Err(e) => {
                            warn!("TIME_EXIT rejected: {}", e);
                        }
                    }
                }
            }

            last_exit_check = std::time::Instant::now();
        }

        // Hot-Reload Configuration (every 15s)
        if last_config_check.elapsed().as_secs() >= 15 {
            last_config_check = std::time::Instant::now();
            if let Ok(new_settings) = Settings::load() {
                // Update strategy engines
                oms.update_strategy_settings(new_settings.strategy.clone());
                pipeline.update_settings(new_settings.strategy.clone());
                
                // Optional: Update risk settings if needed
                // oms.update_risk(new_settings.risk.clone());
                
                info!("CONFIGURATION_RELOAD: Successfully updated strategy parameters from 'settings.toml'");
            }
        }

        // Process asynchronous fills (LIMIT ORDERS)
        while let Ok(fill) = fill_rx.try_recv() {
            if let Some(tp_order) = oms.process_fill(&fill) {
                let tp_price = tp_order.price.unwrap_or(0.0);
                info!("ENTRY FILLED: Submitting TP Limit for {} {} @ {:.4}", 
                    tp_order.symbol, tp_order.side, tp_price);
                
                match executor.submit_order(&tp_order).await {
                    Ok(ack) => { 
                        info!("TP_SUBMITTED: {} {} | price={:.4} | id={}",
                            tp_order.side, tp_order.symbol, tp_price, ack.order_id);
                        *symbol_fills.entry(tp_order.symbol.0.clone()).or_insert(0) += 1; 
                    }
                    Err(e) => { warn!("TP Order rejected: {}", e); }
                }
            }
        }

        // Yield to scheduler — avoid blocking the async runtime.
        tokio::task::yield_now().await;
    }

    // Shutdown
    info!("Shutting down TokioParasite...");
    telemetry.shutdown();
    state_store.flush()?;
    info!("Shutdown complete. Processed {} ticks total.", tick_count);

    Ok(())
}
