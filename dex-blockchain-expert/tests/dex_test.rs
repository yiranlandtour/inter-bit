use dex_blockchain_expert::dex::{
    DexEngine,
    Order,
    OrderSide,
    OrderType,
    TradePath,
};
use primitive_types::{H256, U256};
use tokio;

#[tokio::test]
async fn test_orderbook_matching() {
    let dex = DexEngine::new();
    
    // Create a buy order
    let buy_order = Order {
        id: H256::random(),
        trader: [1u8; 20],
        token_a: H256::from_low_u64_be(1),
        token_b: H256::from_low_u64_be(2),
        amount_a: U256::from(1000),
        amount_b: U256::from(2000),
        price: U256::from(2),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        timestamp: 1000,
        expiry: 2000,
    };
    
    // Create a matching sell order
    let sell_order = Order {
        id: H256::random(),
        trader: [2u8; 20],
        token_a: H256::from_low_u64_be(1),
        token_b: H256::from_low_u64_be(2),
        amount_a: U256::from(1000),
        amount_b: U256::from(2000),
        price: U256::from(2),
        side: OrderSide::Sell,
        order_type: OrderType::Limit,
        timestamp: 1001,
        expiry: 2001,
    };
    
    // Place orders
    let result1 = dex.place_limit_order(buy_order).await;
    assert!(result1.is_ok());
    
    let result2 = dex.place_limit_order(sell_order).await;
    assert!(result2.is_ok());
    
    // Orders should be matched
    let trades = dex.get_recent_trades(H256::from_low_u64_be(1), H256::from_low_u64_be(2)).await;
    assert!(!trades.is_empty(), "Orders should be matched");
}

#[tokio::test]
async fn test_amm_swap() {
    let dex = DexEngine::new();
    
    // Create AMM pool
    let token_a = H256::from_low_u64_be(1);
    let token_b = H256::from_low_u64_be(2);
    let liquidity_provider = [3u8; 20];
    
    let pool_id = dex.create_amm_pool(
        token_a,
        token_b,
        U256::from(1000000), // 1M token A
        U256::from(2000000), // 2M token B
        liquidity_provider,
    ).await.unwrap();
    
    // Execute swap
    let swap_result = dex.swap_exact_input(
        token_a,
        token_b,
        U256::from(1000),
        U256::from(1900), // Minimum output
        [4u8; 20],
    ).await;
    
    assert!(swap_result.is_ok());
    let output = swap_result.unwrap();
    
    // Check slippage
    assert!(output >= U256::from(1900), "Output should meet minimum");
    assert!(output < U256::from(2000), "Should have some slippage");
}

#[tokio::test]
async fn test_smart_routing() {
    let dex = DexEngine::new();
    
    // Create multiple pools for routing
    let token_a = H256::from_low_u64_be(1);
    let token_b = H256::from_low_u64_be(2);
    let token_c = H256::from_low_u64_be(3);
    
    // Pool A-B
    dex.create_amm_pool(
        token_a,
        token_b,
        U256::from(1000000),
        U256::from(1000000),
        [1u8; 20],
    ).await.unwrap();
    
    // Pool B-C
    dex.create_amm_pool(
        token_b,
        token_c,
        U256::from(1000000),
        U256::from(1000000),
        [2u8; 20],
    ).await.unwrap();
    
    // Pool A-C (direct but with worse price)
    dex.create_amm_pool(
        token_a,
        token_c,
        U256::from(1000000),
        U256::from(500000), // Worse rate
        [3u8; 20],
    ).await.unwrap();
    
    // Find best path from A to C
    let path = dex.find_best_path(
        token_a,
        token_c,
        U256::from(10000),
    ).await.unwrap();
    
    // Should route through B for better price
    assert_eq!(path.hops.len(), 2, "Should use 2-hop path through token B");
    assert_eq!(path.hops[0].token_out, token_b);
    assert_eq!(path.hops[1].token_out, token_c);
}

#[tokio::test]
async fn test_mev_protection() {
    let dex = DexEngine::new();
    
    // Enable MEV protection
    dex.enable_batch_auction(std::time::Duration::from_millis(100)).await;
    
    let token_a = H256::from_low_u64_be(1);
    let token_b = H256::from_low_u64_be(2);
    
    // Submit multiple orders within batch window
    let mut order_ids = Vec::new();
    for i in 0..10 {
        let order = Order {
            id: H256::random(),
            trader: [i as u8; 20],
            token_a,
            token_b,
            amount_a: U256::from(100),
            amount_b: U256::from(200),
            price: U256::from(2),
            side: if i % 2 == 0 { OrderSide::Buy } else { OrderSide::Sell },
            order_type: OrderType::Limit,
            timestamp: 1000 + i,
            expiry: 2000,
        };
        
        let result = dex.place_limit_order(order).await;
        assert!(result.is_ok());
        order_ids.push(result.unwrap());
    }
    
    // Wait for batch to execute
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    
    // All orders in same batch should have same execution price
    let trades = dex.get_recent_trades(token_a, token_b).await;
    
    if trades.len() > 1 {
        let first_price = trades[0].price;
        for trade in &trades[1..] {
            assert_eq!(trade.price, first_price, "Batch orders should have uniform price");
        }
    }
}