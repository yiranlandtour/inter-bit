use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use dex_blockchain_expert::state_machine::{
    HighPerformanceStateMachine, Transaction, TransactionSignature,
};
use dex_blockchain_expert::storage::OptimizedStateStorage;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

fn bench_transaction_execution(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("transaction_execution");
    group.measurement_time(Duration::from_secs(10));
    
    // 测试不同批量大小的事务执行
    for tx_count in [100, 500, 1000, 5000, 10000].iter() {
        group.throughput(Throughput::Elements(*tx_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(tx_count),
            tx_count,
            |b, &tx_count| {
                b.to_async(&rt).iter(|| async {
                    let storage = Arc::new(
                        OptimizedStateStorage::new("/tmp/bench_state").unwrap()
                    );
                    let state_machine = HighPerformanceStateMachine::new(storage);
                    
                    let transactions: Vec<Transaction> = (0..tx_count)
                        .map(|i| {
                            let mut from = [0u8; 20];
                            from[0] = (i % 256) as u8;
                            from[1] = ((i / 256) % 256) as u8;
                            
                            Transaction::new(
                                from,
                                Some([1; 20]),
                                1000,
                                vec![],
                                i as u64,
                                21000,
                                1,
                            )
                        })
                        .collect();
                    
                    let receipts = state_machine
                        .execute_transactions_parallel(transactions)
                        .await
                        .unwrap();
                    
                    black_box(receipts);
                });
            },
        );
    }
    
    group.finish();
}

fn bench_parallel_execution(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("parallel_execution");
    
    // 测试并行执行性能
    group.bench_function("parallel_1000_tx", |b| {
        b.to_async(&rt).iter(|| async {
            let storage = Arc::new(
                OptimizedStateStorage::new("/tmp/bench_parallel").unwrap()
            );
            let state_machine = HighPerformanceStateMachine::new(storage);
            
            // 创建无冲突的事务（可以完全并行）
            let transactions: Vec<Transaction> = (0..1000)
                .map(|i| {
                    let mut from = [0u8; 20];
                    from[0] = (i % 256) as u8;
                    from[1] = ((i / 256) % 256) as u8;
                    
                    let mut to = [0u8; 20];
                    to[0] = ((i + 1000) % 256) as u8;
                    to[1] = (((i + 1000) / 256) % 256) as u8;
                    
                    Transaction::new(
                        from,
                        Some(to),
                        100,
                        vec![],
                        0,
                        21000,
                        1,
                    )
                })
                .collect();
            
            let receipts = state_machine
                .execute_transactions_parallel(transactions)
                .await
                .unwrap();
            
            black_box(receipts);
        });
    });
    
    // 测试有冲突的事务
    group.bench_function("conflicting_1000_tx", |b| {
        b.to_async(&rt).iter(|| async {
            let storage = Arc::new(
                OptimizedStateStorage::new("/tmp/bench_conflict").unwrap()
            );
            let state_machine = HighPerformanceStateMachine::new(storage);
            
            // 创建有冲突的事务（都从同一账户发送）
            let transactions: Vec<Transaction> = (0..1000)
                .map(|i| {
                    Transaction::new(
                        [0; 20], // 同一发送方
                        Some([1; 20]),
                        100,
                        vec![],
                        i as u64,
                        21000,
                        1,
                    )
                })
                .collect();
            
            let receipts = state_machine
                .execute_transactions_parallel(transactions)
                .await
                .unwrap();
            
            black_box(receipts);
        });
    });
    
    group.finish();
}

fn bench_state_access(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("state_access");
    
    group.bench_function("account_read", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::state_machine::{StateDB, Account};
            use primitive_types::U256;
            
            let storage = Arc::new(
                OptimizedStateStorage::new("/tmp/bench_read").unwrap()
            );
            let state_db = StateDB::new(storage);
            
            // 预先写入一些账户
            for i in 0..100 {
                let mut addr = [0u8; 20];
                addr[0] = i as u8;
                let account = Account::new(U256::from(1000000));
                state_db.set_account(addr, account).await;
            }
            
            // 读取账户
            for i in 0..100 {
                let mut addr = [0u8; 20];
                addr[0] = i as u8;
                let account = state_db.get_account(addr).await;
                black_box(account);
            }
        });
    });
    
    group.bench_function("storage_read", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::state_machine::StateDB;
            use primitive_types::H256;
            
            let storage = Arc::new(
                OptimizedStateStorage::new("/tmp/bench_storage").unwrap()
            );
            let state_db = StateDB::new(storage);
            
            // 预先写入存储值
            let addr = [1u8; 20];
            for i in 0..100 {
                let mut key = [0u8; 32];
                key[0] = i as u8;
                let value = H256::from_low_u64_be(i);
                state_db.set_storage(addr, H256::from(key), value).await;
            }
            
            // 读取存储
            for i in 0..100 {
                let mut key = [0u8; 32];
                key[0] = i as u8;
                let value = state_db.get_storage(addr, H256::from(key)).await;
                black_box(value);
            }
        });
    });
    
    group.finish();
}

fn bench_evm_execution(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("evm_execution");
    
    group.bench_function("simple_transfer", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::evm_compat::EvmExecutor;
            use primitive_types::U256;
            
            let executor = EvmExecutor::new(1);
            
            // 设置账户余额
            executor.set_balance([0; 20], U256::from(1_000_000)).await;
            
            // 执行转账
            let result = executor.execute_transaction(
                [0; 20],
                Some([1; 20]),
                U256::from(1000),
                vec![],
                21000,
                U256::from(1),
            ).await.unwrap();
            
            black_box(result);
        });
    });
    
    group.bench_function("contract_call", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::evm_compat::EvmExecutor;
            use primitive_types::U256;
            
            let executor = EvmExecutor::new(1);
            
            // 简单的合约字节码（返回42）
            let bytecode = vec![
                0x60, 0x2a, // PUSH1 42
                0x60, 0x00, // PUSH1 0
                0x52,       // MSTORE
                0x60, 0x20, // PUSH1 32
                0x60, 0x00, // PUSH1 0
                0xf3,       // RETURN
            ];
            
            let contract_addr = [2; 20];
            executor.set_code(contract_addr, bytecode).await;
            executor.set_balance([0; 20], U256::from(1_000_000)).await;
            
            // 调用合约
            let result = executor.call(
                [0; 20],
                contract_addr,
                vec![],
                U256::zero(),
            ).await.unwrap();
            
            black_box(result);
        });
    });
    
    group.finish();
}

fn bench_storage_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("storage_operations");
    
    group.bench_function("layered_get", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::storage::Storage;
            
            let storage = OptimizedStateStorage::new("/tmp/bench_layered").unwrap();
            
            // 写入一些数据
            for i in 0..100 {
                let key = format!("key_{}", i);
                let value = vec![i as u8; 100];
                storage.put(&key, &value).await.unwrap();
            }
            
            // 读取数据
            for i in 0..100 {
                let key = format!("key_{}", i);
                let value = storage.get(&key).await;
                black_box(value);
            }
        });
    });
    
    group.bench_function("batch_write", |b| {
        b.to_async(&rt).iter(|| async {
            use dex_blockchain_expert::storage::Storage;
            
            let storage = OptimizedStateStorage::new("/tmp/bench_batch").unwrap();
            
            let items: Vec<(String, Vec<u8>)> = (0..1000)
                .map(|i| {
                    (format!("key_{}", i), vec![i as u8; 100])
                })
                .collect();
            
            storage.batch_put(items).await.unwrap();
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_transaction_execution,
    bench_parallel_execution,
    bench_state_access,
    bench_evm_execution,
    bench_storage_operations
);
criterion_main!(benches);