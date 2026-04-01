use std::sync::Arc;
use hyperliquid::{Exchange, Hyperliquid};
use hyperliquid::types::{
    Chain,
    exchange::request::{OrderRequest, OrderType, Limit, Tif},
};
use ethers_signers::LocalWallet;
use std::str::FromStr;

#[tokio::main]
async fn main() {
    let exchange = Exchange::new(Chain::Arbitrum);
    let wallet = Arc::new(LocalWallet::from_str("0123456789012345678901234567890123456789012345678901234567890123").unwrap());
    let req = OrderRequest {
        asset: 0,
        is_buy: true,
        limit_px: "1".into(),
        sz: "1".into(),
        reduce_only: false,
        order_type: OrderType::Limit(Limit { tif: Tif::Ioc }),
        cloid: None,
    };
    // Let's just mock the call compilation
    // let result = exchange.place_order(wallet, vec![req], None).await;
}
