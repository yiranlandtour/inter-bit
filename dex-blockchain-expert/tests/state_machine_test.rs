use dex_blockchain_expert::state_machine::{
    HighPerformanceStateMachine,
    ParallelExecutor,
    Transaction,
    TransactionReceipt,
    Account,
    StateDB,
};
use primitive_types::{H256, U256};
use std::sync::Arc;
use tokio;

#[tokio::test]
async fn test_high_throughput() {
    let state_machine = Arc::new(HighPerformanceStateMachine::new());
    
    // Generate 10,000 transactions
    let mut transactions = Vec::new();
    for i in 0..10000 {
        transactions.push(Transaction {
            from: [0u8; 20],
            to: [(i % 256) as u8; 20],
            value: U256::from(100),
            nonce: i as u64,
            gas_limit: 21000,
            gas_price: U256::from(1000000000), // 1 gwei
            data: vec![],
            signature: vec![0u8; 65],
        });
    }
    
    let start = std::time::Instant::now();
    let receipts = state_machine.execute_block(transactions).await.unwrap();
    let duration = start.elapsed();
    
    let throughput = 10000.0 / duration.as_secs_f64();
    println!("Throughput: {:.2} tx/s", throughput);
    
    assert_eq!(receipts.len(), 10000);
    assert!(throughput > 10000.0, "Should process >10,000 tx/s");
}

#[tokio::test]
async fn test_parallel_execution() {
    let executor = ParallelExecutor::new(8);
    
    // Create non-conflicting transactions
    let mut transactions = Vec::new();
    for i in 0..100 {
        transactions.push(Transaction {
            from: [i as u8; 20],
            to: [(i + 1) as u8; 20],
            value: U256::from(50),
            nonce: 0,
            gas_limit: 21000,
            gas_price: U256::from(1000000000),
            data: vec![],
            signature: vec![0u8; 65],
        });
    }
    
    let start = std::time::Instant::now();
    let results = executor.execute_parallel(transactions).await;
    let duration = start.elapsed();
    
    println!("Parallel execution time: {:?}", duration);
    assert_eq!(results.len(), 100);
    
    // Verify all transactions succeeded
    for result in results {
        assert!(result.receipt.success);
    }
}

#[tokio::test]
async fn test_state_consistency() {
    let state_db = Arc::new(StateDB::new());
    
    // Create an account
    let address = [1u8; 20];
    let initial_balance = U256::from(1000);
    
    state_db.set_balance(address, initial_balance).await;
    
    // Verify balance
    let balance = state_db.get_balance(address).await;
    assert_eq!(balance, initial_balance);
    
    // Update balance
    let new_balance = U256::from(500);
    state_db.set_balance(address, new_balance).await;
    
    // Verify update
    let balance = state_db.get_balance(address).await;
    assert_eq!(balance, new_balance);
}

#[tokio::test]
async fn test_transaction_validation() {
    let state_machine = HighPerformanceStateMachine::new();
    let state_db = Arc::new(StateDB::new());
    
    // Set up sender account
    let sender = [1u8; 20];
    state_db.set_balance(sender, U256::from(1000)).await;
    state_db.set_nonce(sender, 0).await;
    
    // Valid transaction
    let valid_tx = Transaction {
        from: sender,
        to: [2u8; 20],
        value: U256::from(100),
        nonce: 0,
        gas_limit: 21000,
        gas_price: U256::from(1000000000),
        data: vec![],
        signature: vec![0u8; 65],
    };
    
    let result = state_machine.validate_transaction(&state_db, &valid_tx).await;
    assert!(result.is_ok());
    
    // Invalid transaction (insufficient balance)
    let invalid_tx = Transaction {
        from: sender,
        to: [2u8; 20],
        value: U256::from(10000), // More than balance
        nonce: 0,
        gas_limit: 21000,
        gas_price: U256::from(1000000000),
        data: vec![],
        signature: vec![0u8; 65],
    };
    
    let result = state_machine.validate_transaction(&state_db, &invalid_tx).await;
    assert!(result.is_err());
}