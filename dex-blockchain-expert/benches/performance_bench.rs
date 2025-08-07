use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use dex_blockchain_expert::{
    state_machine::{HighPerformanceStateMachine, Transaction},
    dex::DexEngine,
    consensus::tendermint::TendermintConsensus,
};
use primitive_types::{H256, U256};
use tokio::runtime::Runtime;

fn benchmark_transaction_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let state_machine = HighPerformanceStateMachine::new();
    
    let mut group = c.benchmark_group("transaction_throughput");
    
    for tx_count in [100, 1000, 10000, 50000].iter() {
        group.throughput(Throughput::Elements(*tx_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(tx_count),
            tx_count,
            |b, &tx_count| {
                let transactions: Vec<Transaction> = (0..tx_count)
                    .map(|i| Transaction {
                        from: [(i % 256) as u8; 20],
                        to: [((i + 1) % 256) as u8; 20],
                        value: U256::from(100),
                        nonce: i as u64,
                        gas_limit: 21000,
                        gas_price: U256::from(1000000000),
                        data: vec![],
                        signature: vec![0u8; 65],
                    })
                    .collect();
                
                b.iter(|| {
                    rt.block_on(async {
                        state_machine.execute_block(black_box(transactions.clone())).await
                    })
                });
            },
        );
    }
    group.finish();
}

fn benchmark_consensus_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("consensus_latency");
    
    for validator_count in [4, 7, 10, 13].iter() {
        group.bench_with_input(
            BenchmarkId::new("tendermint", validator_count),
            validator_count,
            |b, &validator_count| {
                let consensus = TendermintConsensus::new(validator_count, 0);
                
                b.iter(|| {
                    rt.block_on(async {
                        let block = dex_blockchain_expert::consensus::types::Block {
                            header: dex_blockchain_expert::consensus::types::BlockHeader {
                                height: 1,
                                previous_hash: H256::zero(),
                                state_root: H256::random(),
                                timestamp: 1000,
                                proposer: [0u8; 20],
                            },
                            transactions: vec![],
                        };
                        consensus.propose_block(black_box(block)).await
                    })
                });
            },
        );
    }
    group.finish();
}

fn benchmark_dex_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dex = DexEngine::new();
    
    let mut group = c.benchmark_group("dex_operations");
    
    // Benchmark order placement
    group.bench_function("place_order", |b| {
        b.iter(|| {
            rt.block_on(async {
                let order = dex_blockchain_expert::dex::Order {
                    id: H256::random(),
                    trader: [1u8; 20],
                    token_a: H256::from_low_u64_be(1),
                    token_b: H256::from_low_u64_be(2),
                    amount_a: U256::from(1000),
                    amount_b: U256::from(2000),
                    price: U256::from(2),
                    side: dex_blockchain_expert::dex::OrderSide::Buy,
                    order_type: dex_blockchain_expert::dex::OrderType::Limit,
                    timestamp: 1000,
                    expiry: 2000,
                };
                dex.place_limit_order(black_box(order)).await
            })
        });
    });
    
    // Benchmark AMM swap
    group.bench_function("amm_swap", |b| {
        rt.block_on(async {
            // Setup pool
            dex.create_amm_pool(
                H256::from_low_u64_be(1),
                H256::from_low_u64_be(2),
                U256::from(1000000),
                U256::from(2000000),
                [1u8; 20],
            ).await.unwrap();
        });
        
        b.iter(|| {
            rt.block_on(async {
                dex.swap_exact_input(
                    H256::from_low_u64_be(1),
                    H256::from_low_u64_be(2),
                    black_box(U256::from(100)),
                    U256::from(0),
                    [2u8; 20],
                ).await
            })
        });
    });
    
    group.finish();
}

fn benchmark_storage_operations(c: &mut Criterion) {
    use dex_blockchain_expert::storage::OptimizedStateStorage;
    
    let rt = Runtime::new().unwrap();
    let storage = OptimizedStateStorage::new();
    
    let mut group = c.benchmark_group("storage");
    
    // Benchmark writes
    group.bench_function("write", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            rt.block_on(async {
                let key = format!("key_{}", counter);
                let value = format!("value_{}", counter);
                counter += 1;
                storage.put(black_box(key.as_bytes()), black_box(value.as_bytes())).await
            })
        });
    });
    
    // Benchmark reads
    rt.block_on(async {
        // Populate some data
        for i in 0..1000 {
            let key = format!("test_{}", i);
            let value = format!("data_{}", i);
            storage.put(key.as_bytes(), value.as_bytes()).await.unwrap();
        }
    });
    
    group.bench_function("read", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            rt.block_on(async {
                let key = format!("test_{}", counter % 1000);
                counter += 1;
                storage.get(black_box(key.as_bytes())).await
            })
        });
    });
    
    group.finish();
}

fn benchmark_parallel_execution(c: &mut Criterion) {
    use dex_blockchain_expert::state_machine::ParallelExecutor;
    
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("parallel_execution");
    
    for worker_count in [1, 2, 4, 8, 16].iter() {
        group.bench_with_input(
            BenchmarkId::new("workers", worker_count),
            worker_count,
            |b, &worker_count| {
                let executor = ParallelExecutor::new(worker_count);
                
                let transactions: Vec<Transaction> = (0..1000)
                    .map(|i| Transaction {
                        from: [i as u8; 20],
                        to: [(i + 1) as u8; 20],
                        value: U256::from(100),
                        nonce: 0,
                        gas_limit: 21000,
                        gas_price: U256::from(1000000000),
                        data: vec![],
                        signature: vec![0u8; 65],
                    })
                    .collect();
                
                b.iter(|| {
                    rt.block_on(async {
                        executor.execute_parallel(black_box(transactions.clone())).await
                    })
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    benchmark_transaction_throughput,
    benchmark_consensus_latency,
    benchmark_dex_operations,
    benchmark_storage_operations,
    benchmark_parallel_execution
);
criterion_main!(benches);