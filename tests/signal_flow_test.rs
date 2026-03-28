//! Integration tests for the signal processing pipeline.
//!
//! Tests the complete flow from tick ingestion to signal generation.

use tokioparasite::eal::{MarketData, MockExchange, Symbol, Tick, VenueId};
use tokioparasite::signal::{SignalPipeline, TimeGrid};

/// Helper function to create a tick.
fn make_tick(venue: VenueId, symbol: &str, price: f64, ts_ns: u64) -> Tick {
    Tick {
        venue,
        symbol: Symbol::new(symbol),
        price,
        size: 1.0,
        exchange_ts_ns: ts_ns,
        local_ts_ns: ts_ns,
    }
}

#[test]
fn test_signal_pipeline_generates_signal_on_correlation() {
    // Create a signal pipeline with 256 tick window
    let settings = tokioparasite::config::StrategySettings {
        active_strategy: "correlation_hysteresis".to_string(),
        symbols: vec!["BTC".to_string()],
        window_size_ticks: 256,
        min_correlation_r: 0.85,
        hysteresis_buffer: 0.05, // Lower threshold for easier flip
        enable_obi: false,
        obi_weight: 0.0,
        impulse_threshold_bps: 5,
        lag_threshold_bps: 1,
        impulse_window_ms: 5,
        signal_timeout_ms: 10,
        min_trade_size_filter: 0.001,
        spread_filter_bps: 10,
        obi_strong_threshold: 0.7,
        obi_neutral_threshold: 0.2,
        obi_depth: 5,
        obi_spike_threshold: 0.3,
    };

    let mut pipeline = SignalPipeline::<256>::new(settings);
    let mut timegrid = TimeGrid::new(5_000_000); // 5ms grid

    // Generate correlated price data with a clear lead-lag relationship
    // Use NON-LINEAR SIN WAVE for realistic lag detection
    // Only correct lag → high correlation, wrong lag → low correlation
    let base_price = 100.0;
    let mut signal_generated = false;
    let mut history: Vec<f64> = Vec::new();

    // Run for enough ticks to ensure hysteresis can detect a flip
    for i in 0..2000 {
        let ts = i * 5_000_000; // 5ms intervals

        // Exchange A price: NON-LINEAR SIN WAVE (realistic market data)
        let price_a = base_price + (i as f64 * 0.01).sin() * 0.5;
        history.push(price_a);

        // Exchange B price: FLIP LEADER at i=500
        // Phase 1 (i < 500): B lags A by 10 ticks (A leads)
        // Phase 2 (i >= 500): A lags B by 10 ticks (B leads)
        // This creates a DRAMATIC role flip that hysteresis can easily detect
        let price_b = if i < 500 {
            // B lags A by 10 ticks → A leads
            if history.len() > 10 {
                history[history.len() - 11] // B = A[t-10]
            } else {
                price_a
            }
        } else {
            // A lags B by 10 ticks → B leads
            // B = A[t+10] (future price)
            base_price + ((i as f64 + 10.0) * 0.01).sin() * 0.5
        };

        // Ingest ticks from both exchanges
        // Both arrive at same timestamp (same time grid bin)
        // The lag is in the PRICE, not the timestamp
        let tick_a = make_tick(VenueId::EXCHANGE_A, "BTC", price_a, ts);
        let tick_b = make_tick(VenueId::EXCHANGE_B, "BTC", price_b, ts);

        let result_a = timegrid.ingest_tick(&tick_a);
        let result_b = timegrid.ingest_tick(&tick_b);

        // Process aligned pairs through pipeline
        for pair in result_a.iter() {
            if let Some(signal) = pipeline.process_pair(&Symbol::new("BTC"), pair) {
                signal_generated = true;
                assert_eq!(signal.symbol.to_string(), "BTC");
                assert!(signal.correlation_r >= 0.85);
                break;
            }
        }

        for pair in result_b.iter() {
            if let Some(signal) = pipeline.process_pair(&Symbol::new("BTC"), pair) {
                signal_generated = true;
                assert_eq!(signal.symbol.to_string(), "BTC");
                assert!(signal.correlation_r >= 0.85);
                break;
            }
        }

        if signal_generated {
            break;
        }
    }

    assert!(
        signal_generated,
        "Expected signal to be generated from correlated data"
    );
}

#[test]
fn test_signal_pipeline_no_signal_on_uncorrelated_data() {
    // Create a signal pipeline with 256 tick window
    let settings = tokioparasite::config::StrategySettings {
        active_strategy: "correlation_hysteresis".to_string(),
        symbols: vec!["BTC".to_string()],
        window_size_ticks: 256,
        min_correlation_r: 0.85,
        hysteresis_buffer: 0.10,
        enable_obi: false,
        obi_weight: 0.0,
        impulse_threshold_bps: 5,
        lag_threshold_bps: 1,
        impulse_window_ms: 5,
        signal_timeout_ms: 10,
        min_trade_size_filter: 0.001,
        spread_filter_bps: 10,
        obi_strong_threshold: 0.7,
        obi_neutral_threshold: 0.2,
        obi_depth: 5,
        obi_spike_threshold: 0.3,
    };

    let mut pipeline = SignalPipeline::<256>::new(settings);
    let mut timegrid = TimeGrid::new(5_000_000); // 5ms grid

    // Generate uncorrelated price data
    // Exchange A is constant, Exchange B varies - this should have zero correlation
    let mut signal_generated = false;
    let mut max_correlation = 0.0f64;

    // Run for enough ticks to ensure hysteresis is past initial state
    // and correlation is calculated with sufficient data
    for i in 0..500 {
        let ts = i * 5_000_000; // 5ms intervals

        // Exchange A price (constant)
        let price_a = 60000.0;

        // Exchange B price (varies)
        let price_b = 60000.0 + (i as f64) * 0.1;

        // Ingest ticks from both exchanges
        let tick_a = make_tick(VenueId::EXCHANGE_A, "BTC", price_a, ts);
        let tick_b = make_tick(VenueId::EXCHANGE_B, "BTC", price_b, ts);

        let result_a = timegrid.ingest_tick(&tick_a);
        let result_b = timegrid.ingest_tick(&tick_b);

        // Process aligned pairs through pipeline
        for pair in result_a.iter() {
            // Check correlation before signal generation
            if let Some(corr) = pipeline.current_correlation(&Symbol::new("BTC")) {
                max_correlation = max_correlation.max(corr);
            }

            if let Some(_signal) = pipeline.process_pair(&Symbol::new("BTC"), pair) {
                signal_generated = true;
                break;
            }
        }

        for pair in result_b.iter() {
            // Check correlation before signal generation
            if let Some(corr) = pipeline.current_correlation(&Symbol::new("BTC")) {
                max_correlation = max_correlation.max(corr);
            }

            if let Some(_signal) = pipeline.process_pair(&Symbol::new("BTC"), pair) {
                signal_generated = true;
                break;
            }
        }

        if signal_generated {
            break;
        }
    }

    // Uncorrelated data should not generate signals
    // Also verify that correlation is indeed low
    assert!(
        !signal_generated,
        "Expected no signal from uncorrelated data (max correlation: {})",
        max_correlation
    );
    assert!(
        max_correlation < 0.85,
        "Expected correlation to be below threshold, got {}",
        max_correlation
    );
}

#[test]
fn test_timegrid_alignment() {
    let mut timegrid = TimeGrid::new(5_000_000); // 5ms grid

    // First tick from exchange A at t=0
    let tick_a1 = make_tick(VenueId::EXCHANGE_A, "BTC", 60000.0, 0);
    let result1 = timegrid.ingest_tick(&tick_a1);
    assert_eq!(result1.count, 0); // No B price yet

    // First tick from exchange B at t=2ms (within first grid bin)
    let tick_b1 = make_tick(VenueId::EXCHANGE_B, "BTC", 60001.0, 2_000_000);
    let _result2 = timegrid.ingest_tick(&tick_b1);

    // POKE: Send a tick from the future to close the first bin
    let result3 = timegrid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, "BTC", 60000.0, 5_000_000));

    assert!(
        result3.count > 0,
        "Grid should have emitted the first bin after seeing t=5ms"
    );

    // Verify aligned pair has both prices
    let pair = &result3.pairs[0];
    assert_eq!(pair.price_a, 60000.0);
    assert_eq!(pair.price_b, 60001.0);
}

#[test]
fn test_mock_exchange_tick_injection() {
    let mock = MockExchange::new(VenueId::EXCHANGE_A);
    let symbol = Symbol::new("BTC");

    // Subscribe to ticks
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rx = rt
        .block_on(mock.subscribe_ticks(&[symbol.clone()]))
        .unwrap();

    // Inject a tick
    let tick = make_tick(VenueId::EXCHANGE_A, "BTC", 60000.0, 0);
    mock.inject_tick(tick);

    // Receive the tick
    let received = rx.recv().unwrap();
    assert_eq!(received.price, 60000.0);
    assert_eq!(received.symbol, symbol);
}
