use dex_blockchain_expert::{
    consensus::tendermint::TendermintConsensus,
    consensus::validator::Validator,
    state_machine::HighPerformanceStateMachine,
    storage::OptimizedStateStorage,
    network::P2PNetwork,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting DEX Blockchain Node...");

    // 初始化存储层
    let storage = Arc::new(OptimizedStateStorage::new("./data/blockchain")?);
    info!("Storage layer initialized");

    // 初始化状态机
    let state_machine = Arc::new(HighPerformanceStateMachine::new(storage.clone()));
    info!("State machine initialized");

    // 初始化验证者
    let validators = vec![
        Validator::new(vec![1]),
        Validator::new(vec![2]),
        Validator::new(vec![3]),
        Validator::new(vec![4]),
    ];

    // 初始化共识层
    let (consensus, mut block_rx) = TendermintConsensus::new(validators);
    info!("Consensus layer initialized with {} validators", 4);

    // 初始化网络层
    let network = Arc::new(P2PNetwork::new(vec![1, 2, 3]));
    info!("P2P network initialized");

    // 启动共识
    let consensus_handle = tokio::spawn(async move {
        consensus.start().await;
    });

    // 启动区块处理
    let state_machine_clone = state_machine.clone();
    let block_processor = tokio::spawn(async move {
        while let Some(block) = block_rx.recv().await {
            info!(
                "Processing block at height {} with {} transactions",
                block.height,
                block.transactions.len()
            );

            // 转换交易格式
            let transactions: Vec<_> = block.transactions.into_iter()
                .map(|tx| {
                    use dex_blockchain_expert::state_machine::Transaction;
                    Transaction::new(
                        tx.from,
                        tx.to,
                        tx.value,
                        tx.data,
                        tx.nonce,
                        tx.gas_limit,
                        tx.gas_price,
                    )
                })
                .collect();

            match state_machine_clone.execute_block(transactions).await {
                Ok(receipts) => {
                    let successful = receipts.iter().filter(|r| r.status).count();
                    let total_gas: u64 = receipts.iter().map(|r| r.gas_used).sum();
                    
                    info!(
                        "Block executed: {}/{} transactions successful, total gas used: {}",
                        successful,
                        receipts.len(),
                        total_gas
                    );

                    // 提交状态
                    if let Err(e) = state_machine_clone.commit_state(
                        primitive_types::H256::from(block.hash)
                    ).await {
                        error!("Failed to commit state: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to execute block: {:?}", e);
                }
            }
        }
    });

    // 启动性能监控
    let state_machine_monitor = state_machine.clone();
    let storage_monitor = storage.clone();
    let network_monitor = network.clone();
    
    let monitor_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        
        loop {
            interval.tick().await;
            
            // 获取状态机指标
            let sm_metrics = state_machine_monitor.get_metrics().await;
            info!(
                "State Machine - Total TX: {}, Success: {}, Failed: {}, Avg Latency: {}μs",
                sm_metrics.total_transactions.load(std::sync::atomic::Ordering::Relaxed),
                sm_metrics.successful_transactions.load(std::sync::atomic::Ordering::Relaxed),
                sm_metrics.failed_transactions.load(std::sync::atomic::Ordering::Relaxed),
                sm_metrics.average_execution_time_us.load(std::sync::atomic::Ordering::Relaxed)
            );
            
            // 获取存储指标
            let storage_stats = storage_monitor.get_stats();
            info!(
                "Storage - L1 Hit Rate: {:.2}%, L2 Hit Rate: {:.2}%, Avg Read: {}μs, Avg Write: {}μs",
                storage_stats.l1_hit_rate * 100.0,
                storage_stats.l2_hit_rate * 100.0,
                storage_stats.avg_read_latency_us,
                storage_stats.avg_write_latency_us
            );
            
            // 获取网络指标
            let peer_count = network_monitor.get_peer_count().await;
            let tx_pool_size = network_monitor.get_tx_pool_size().await;
            info!(
                "Network - Peers: {}, TX Pool Size: {}",
                peer_count,
                tx_pool_size
            );
        }
    });

    // 模拟交易生成（用于测试）
    let network_tx = network.clone();
    let tx_generator = tokio::spawn(async move {
        let mut nonce = 0u64;
        
        loop {
            sleep(Duration::from_millis(100)).await;
            
            use dex_blockchain_expert::network::Transaction;
            
            // 生成随机交易
            let tx = Transaction {
                hash: [0; 32], // 会被重新计算
                from: [0; 20],
                to: Some([1; 20]),
                value: 1000,
                data: vec![],
                nonce,
                gas_limit: 21000,
                gas_price: 1,
            };
            
            network_tx.broadcast_transaction(tx).await;
            nonce += 1;
            
            if nonce % 100 == 0 {
                info!("Generated {} test transactions", nonce);
            }
        }
    });

    // 等待信号
    tokio::signal::ctrl_c().await?;
    
    info!("Shutting down...");
    
    // 清理
    consensus_handle.abort();
    block_processor.abort();
    monitor_handle.abort();
    tx_generator.abort();
    
    // 刷新存储
    storage.flush_batch().await?;
    
    info!("Shutdown complete");
    
    Ok(())
}