use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use dex_blockchain_expert::consensus::tendermint::{TendermintConsensus, Block, Transaction};
use dex_blockchain_expert::consensus::validator::Validator;
use std::time::Duration;
use tokio::runtime::Runtime;

fn bench_consensus_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("consensus_latency");
    group.measurement_time(Duration::from_secs(10));
    
    for validator_count in [4, 7, 10, 13, 16, 19].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(validator_count),
            validator_count,
            |b, &validator_count| {
                let validators: Vec<Validator> = (0..validator_count)
                    .map(|i| Validator::new(vec![i as u8]))
                    .collect();
                
                b.to_async(&rt).iter(|| async {
                    let (consensus, mut block_rx) = TendermintConsensus::new(validators.clone());
                    
                    // 模拟共识过程
                    tokio::spawn(async move {
                        consensus.start().await;
                    });
                    
                    // 等待一个块
                    tokio::time::timeout(
                        Duration::from_millis(200),
                        block_rx.recv()
                    ).await
                });
            },
        );
    }
    
    group.finish();
}

fn bench_block_production(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("block_production");
    group.throughput(Throughput::Elements(1));
    
    group.bench_function("block_creation", |b| {
        b.to_async(&rt).iter(|| async {
            let transactions: Vec<Transaction> = (0..100)
                .map(|i| Transaction {
                    nonce: i,
                    from: [0; 20],
                    to: Some([1; 20]),
                    value: 1000,
                    data: vec![],
                    gas_limit: 21000,
                    gas_price: 1,
                    signature: vec![],
                })
                .collect();
            
            let block = Block {
                height: 1,
                timestamp: 0,
                previous_hash: [0; 32],
                state_root: [0; 32],
                transactions,
                proposer: vec![1, 2, 3],
            };
            
            black_box(block);
        });
    });
    
    group.finish();
}

fn bench_vote_aggregation(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let mut group = c.benchmark_group("vote_aggregation");
    
    for vote_count in [10, 50, 100, 200, 500].iter() {
        group.throughput(Throughput::Elements(*vote_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(vote_count),
            vote_count,
            |b, &vote_count| {
                b.to_async(&rt).iter(|| async {
                    use dex_blockchain_expert::consensus::tendermint::{Vote, VoteType};
                    
                    let votes: Vec<Vote> = (0..vote_count)
                        .map(|i| Vote {
                            height: 1,
                            round: 0,
                            vote_type: VoteType::Prevote,
                            block_hash: Some([0; 32]),
                            validator: vec![i as u8],
                            signature: vec![0; 64],
                        })
                        .collect();
                    
                    // 模拟投票聚合
                    let mut aggregated = std::collections::HashMap::new();
                    for vote in votes {
                        aggregated.entry((vote.height, vote.round, vote.vote_type.clone()))
                            .or_insert_with(Vec::new)
                            .push(vote);
                    }
                    
                    black_box(aggregated);
                });
            },
        );
    }
    
    group.finish();
}

fn bench_signature_verification(c: &mut Criterion) {
    let mut group = c.benchmark_group("signature_verification");
    
    group.bench_function("ed25519_verify", |b| {
        use ed25519_dalek::{Keypair, Signature, Signer, Verifier};
        use rand::rngs::OsRng;
        
        let keypair = Keypair::generate(&mut OsRng);
        let message = b"test message for signature verification";
        let signature = keypair.sign(message);
        
        b.iter(|| {
            black_box(keypair.public.verify(message, &signature).is_ok());
        });
    });
    
    group.bench_function("batch_verify_10", |b| {
        use ed25519_dalek::{Keypair, Signature, Signer};
        use rand::rngs::OsRng;
        
        let signatures: Vec<_> = (0..10)
            .map(|i| {
                let keypair = Keypair::generate(&mut OsRng);
                let message = format!("message {}", i);
                let signature = keypair.sign(message.as_bytes());
                (keypair.public, message, signature)
            })
            .collect();
        
        b.iter(|| {
            for (public_key, message, signature) in &signatures {
                use ed25519_dalek::Verifier;
                black_box(public_key.verify(message.as_bytes(), signature).is_ok());
            }
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_consensus_latency,
    bench_block_production,
    bench_vote_aggregation,
    bench_signature_verification
);
criterion_main!(benches);