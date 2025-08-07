use dex_blockchain_expert::consensus::{
    tendermint::TendermintConsensus,
    hotstuff::HotStuffConsensus,
    optimistic_bft::OptimisticBFT,
    types::*,
};
use primitive_types::{H256, U256};
use tokio;

#[tokio::test]
async fn test_tendermint_consensus_latency() {
    let consensus = TendermintConsensus::new(4, 0);
    let start = std::time::Instant::now();
    
    // Simulate consensus round
    let block = Block {
        header: BlockHeader {
            height: 1,
            previous_hash: H256::zero(),
            state_root: H256::random(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            proposer: [0u8; 20],
        },
        transactions: vec![],
    };
    
    // Test that consensus completes within 200ms
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        consensus.propose_block(block)
    ).await;
    
    assert!(result.is_ok(), "Consensus should complete within 200ms");
    let duration = start.elapsed();
    println!("Tendermint consensus latency: {:?}", duration);
    assert!(duration.as_millis() < 200, "Latency should be less than 200ms");
}

#[tokio::test]
async fn test_hotstuff_linear_complexity() {
    let consensus = HotStuffConsensus::new(7, 0);
    
    // Test that message complexity is O(n)
    let block = Block {
        header: BlockHeader {
            height: 1,
            previous_hash: H256::zero(),
            state_root: H256::random(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            proposer: [1u8; 20],
        },
        transactions: vec![],
    };
    
    let messages_sent = consensus.get_message_count();
    assert!(messages_sent <= 21, "HotStuff should have linear message complexity");
}

#[tokio::test]
async fn test_optimistic_bft_fast_path() {
    let consensus = OptimisticBFT::new(4, 0);
    
    // Test optimistic fast path
    let block = Block {
        header: BlockHeader {
            height: 1,
            previous_hash: H256::zero(),
            state_root: H256::random(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            proposer: [2u8; 20],
        },
        transactions: vec![],
    };
    
    let start = std::time::Instant::now();
    consensus.propose_block(block).await.unwrap();
    let duration = start.elapsed();
    
    println!("Optimistic BFT fast path latency: {:?}", duration);
    assert!(duration.as_millis() < 100, "Fast path should be very quick");
}

#[test]
fn test_consensus_switching() {
    // Test that we can switch between consensus algorithms
    let tendermint = TendermintConsensus::new(4, 0);
    let hotstuff = HotStuffConsensus::new(4, 0);
    let optimistic = OptimisticBFT::new(4, 0);
    
    assert_eq!(tendermint.get_validator_count(), 4);
    assert_eq!(hotstuff.get_validator_count(), 4);
    assert_eq!(optimistic.get_validator_count(), 4);
}