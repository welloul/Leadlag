//! L2 order book matcher for paper trading.
//!
//! Simulates realistic order matching against the top N levels of the order book.
//! Calculates VWAP fills and slippage.

use crate::eal::{BookLevel, OrderSide};

/// Order book matcher for paper trading.
///
/// Matches orders against L2 order book depth with realistic slippage.
pub struct OrderBookMatcher {
    /// Bid levels (sorted descending by price).
    bids: Vec<BookLevel>,
    /// Ask levels (sorted ascending by price).
    asks: Vec<BookLevel>,
    /// Maximum depth to match against.
    max_depth: usize,
}

impl OrderBookMatcher {
    /// Create a new order book matcher.
    pub fn new(max_depth: usize) -> Self {
        Self {
            bids: Vec::new(),
            asks: Vec::new(),
            max_depth,
        }
    }

    /// Update the order book.
    pub fn update_book(&mut self, bids: Vec<BookLevel>, asks: Vec<BookLevel>) {
        self.bids = bids;
        self.asks = asks;
    }

    /// Match an order against the order book.
    ///
    /// Returns (filled_size, avg_price, slippage_bps).
    pub fn match_order(
        &self,
        side: OrderSide,
        size: f64,
        limit_price: Option<f64>,
    ) -> Result<(f64, f64, f64), crate::eal::ExecutionError> {
        let levels = match side {
            OrderSide::Buy => &self.asks,
            OrderSide::Sell => &self.bids,
        };

        if levels.is_empty() {
            return Err(crate::eal::ExecutionError::ExchangeError(
                "No liquidity".to_string(),
            ));
        }

        let mut remaining = size;
        let mut total_cost = 0.0;
        let mut total_filled = 0.0;
        let mut levels_used = 0;

        for level in levels.iter().take(self.max_depth) {
            if remaining <= 0.0 {
                break;
            }

            // Check limit price
            if let Some(limit) = limit_price {
                match side {
                    OrderSide::Buy => {
                        if level.price > limit {
                            break;
                        }
                    }
                    OrderSide::Sell => {
                        if level.price < limit {
                            break;
                        }
                    }
                }
            }

            let fill_size = remaining.min(level.size);
            total_cost += fill_size * level.price;
            total_filled += fill_size;
            remaining -= fill_size;
            levels_used += 1;
        }

        if total_filled == 0.0 {
            return Err(crate::eal::ExecutionError::ExchangeError(
                "No fill".to_string(),
            ));
        }

        let avg_price = total_cost / total_filled;

        // Calculate slippage relative to best price
        let best_price = match side {
            OrderSide::Buy => self.asks.first().map(|l| l.price).unwrap_or(avg_price),
            OrderSide::Sell => self.bids.first().map(|l| l.price).unwrap_or(avg_price),
        };

        let slippage_bps = ((avg_price - best_price) / best_price * 10000.0).abs();

        Ok((total_filled, avg_price, slippage_bps))
    }

    /// Get the best bid price.
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.price)
    }

    /// Get the best ask price.
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.price)
    }

    /// Get the mid price.
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_buy_fill() {
        let mut matcher = OrderBookMatcher::new(10);
        matcher.update_book(
            vec![BookLevel { price: 60000.0, size: 1.0 }],
            vec![BookLevel { price: 60001.0, size: 1.0 }],
        );

        let (filled, avg, slippage) = matcher
            .match_order(OrderSide::Buy, 0.5, None)
            .unwrap();

        assert_eq!(filled, 0.5);
        assert_eq!(avg, 60001.0);
        assert_eq!(slippage, 0.0);
    }

    #[test]
    fn test_multi_level_fill() {
        let mut matcher = OrderBookMatcher::new(10);
        matcher.update_book(
            vec![BookLevel { price: 60000.0, size: 1.0 }],
            vec![
                BookLevel { price: 60001.0, size: 0.3 },
                BookLevel { price: 60002.0, size: 0.3 },
                BookLevel { price: 60003.0, size: 0.4 },
            ],
        );

        let (filled, avg, slippage) = matcher
            .match_order(OrderSide::Buy, 0.5, None)
            .unwrap();

        assert_eq!(filled, 0.5);
        // VWAP = (0.3 * 60001 + 0.2 * 60002) / 0.5 = 60001.4
        assert!((avg - 60001.4).abs() < 0.01);
        assert!(slippage > 0.0);
    }

    #[test]
    fn test_limit_price_respected() {
        let mut matcher = OrderBookMatcher::new(10);
        matcher.update_book(
            vec![BookLevel { price: 60000.0, size: 1.0 }],
            vec![
                BookLevel { price: 60001.0, size: 0.3 },
                BookLevel { price: 60005.0, size: 0.3 },
            ],
        );

        // Limit at 60002 should only fill first level
        let (filled, avg, _) = matcher
            .match_order(OrderSide::Buy, 1.0, Some(60002.0))
            .unwrap();

        assert_eq!(filled, 0.3);
        assert_eq!(avg, 60001.0);
    }

    #[test]
    fn test_no_liquidity() {
        let matcher = OrderBookMatcher::new(10);
        let result = matcher.match_order(OrderSide::Buy, 0.5, None);
        assert!(result.is_err());
    }
}