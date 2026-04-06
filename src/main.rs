//! - Configuration loading
//! - Exchange connectivity (EAL)
//! - Signal processing pipeline
//! - Order management (OMS)
//! - Paper trading simulation
//! - Persistence and logging

pub mod config;
pub mod eal;
pub mod logging;
pub mod oms;
pub mod persist;
pub mod signal;
pub mod sim;
pub mod runners;

use config::Settings;
use logging::init_logging;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env if present
    dotenv::dotenv().ok();
    
    // Load settings
    let settings = Settings::load()?;
    init_logging(&settings.app.log_level);

    use config::{TradingMode, TargetExchange};

    match settings.app.trading_mode {
        TradingMode::Paper => {
            runners::paper::run(settings).await
        }
        TradingMode::Live => {
            match settings.app.target_exchange {
                TargetExchange::Hyperliquid => runners::hyperliquid::run(settings).await,
                TargetExchange::Okx => runners::okx::run(settings).await,
                TargetExchange::Mexc => runners::mexc::run(settings).await,
            }
        }
    }
}
