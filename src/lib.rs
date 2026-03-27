//! TokioParasite: Lead-Lag Arbitrage Bot
//!
//! A high-performance lead-lag arbitrage bot that uses cross-correlation
//! signal processing to detect price leadership between exchanges.

pub mod config;
pub mod eal;
pub mod logging;
pub mod oms;
pub mod persist;
pub mod signal;