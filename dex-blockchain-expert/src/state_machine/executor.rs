use super::*;
use crate::storage::OptimizedStateStorage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use futures::stream::{self, StreamExt};
use crossbeam_channel::{bounded, Sender, Receiver};
use parking_lot::Mutex;

const MAX_PARALLEL_WORKERS: usize = 16;
const BATCH_SIZE: usize = 100;

pub struct HighPerformanceStateMachine {
    state_db: Arc<StateDB>,
    storage: Arc<OptimizedStateStorage>,
    parallel_executor: Arc<ParallelExecutor>,
    tx_pool: Arc<RwLock<Vec<Transaction>>>,
    receipts: Arc<RwLock<HashMap<H256, TransactionReceipt>>>,
    metrics: Arc<ExecutionMetrics>,
}

#[derive(Debug, Default)]
pub struct ExecutionMetrics {
    pub total_transactions: AtomicU64,
    pub successful_transactions: AtomicU64,
    pub failed_transactions: AtomicU64,
    pub total_gas_used: AtomicU64,
    pub average_execution_time_us: AtomicU64,
}

impl HighPerformanceStateMachine {
    pub fn new(storage: Arc<OptimizedStateStorage>) -> Self {
        let state_db = Arc::new(StateDB::new(storage.clone()));
        let parallel_executor = Arc::new(ParallelExecutor::new(MAX_PARALLEL_WORKERS));
        
        Self {
            state_db,
            storage,
            parallel_executor,
            tx_pool: Arc::new(RwLock::new(Vec::new())),
            receipts: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(ExecutionMetrics::default()),
        }
    }

    pub async fn execute_transactions_parallel(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<TransactionReceipt>, Error> {
        let start = Instant::now();
        let tx_count = transactions.len();
        
        // 分析事务依赖关系
        let dependency_graph = self.build_dependency_graph(&transactions).await;
        
        // 按依赖关系分组
        let execution_groups = self.group_by_dependency(transactions, dependency_graph);
        
        let mut all_receipts = Vec::new();
        
        // 按组顺序执行，组内并行
        for group in execution_groups {
            let group_receipts = self.execute_group_parallel(group).await?;
            all_receipts.extend(group_receipts);
        }
        
        // 更新指标
        let elapsed = start.elapsed().as_micros() as u64;
        self.metrics.average_execution_time_us.store(
            elapsed / tx_count as u64,
            Ordering::Relaxed
        );
        
        Ok(all_receipts)
    }

    async fn build_dependency_graph(
        &self,
        transactions: &[Transaction],
    ) -> HashMap<usize, Vec<usize>> {
        let mut graph = HashMap::new();
        let mut account_last_tx: HashMap<[u8; 20], usize> = HashMap::new();
        
        for (idx, tx) in transactions.iter().enumerate() {
            let mut deps = Vec::new();
            
            // 检查发送方依赖
            if let Some(&prev_idx) = account_last_tx.get(&tx.from) {
                deps.push(prev_idx);
            }
            account_last_tx.insert(tx.from, idx);
            
            // 检查接收方依赖（如果是合约调用）
            if let Some(to) = tx.to {
                if self.is_contract(to).await {
                    if let Some(&prev_idx) = account_last_tx.get(&to) {
                        deps.push(prev_idx);
                    }
                    account_last_tx.insert(to, idx);
                }
            }
            
            graph.insert(idx, deps);
        }
        
        graph
    }

    fn group_by_dependency(
        &self,
        transactions: Vec<Transaction>,
        dependency_graph: HashMap<usize, Vec<usize>>,
    ) -> Vec<Vec<Transaction>> {
        let mut groups = Vec::new();
        let mut processed = vec![false; transactions.len()];
        let mut current_group = Vec::new();
        
        // 使用拓扑排序分组
        loop {
            let mut found_any = false;
            
            for (idx, tx) in transactions.iter().enumerate() {
                if processed[idx] {
                    continue;
                }
                
                // 检查所有依赖是否已处理
                let deps = &dependency_graph[&idx];
                let all_deps_processed = deps.iter().all(|&dep| processed[dep]);
                
                if all_deps_processed {
                    current_group.push(tx.clone());
                    processed[idx] = true;
                    found_any = true;
                }
            }
            
            if !current_group.is_empty() {
                groups.push(current_group.clone());
                current_group.clear();
            }
            
            if !found_any {
                break;
            }
        }
        
        groups
    }

    async fn execute_group_parallel(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<TransactionReceipt>, Error> {
        let (tx_sender, tx_receiver) = bounded(transactions.len());
        let (receipt_sender, receipt_receiver) = bounded(transactions.len());
        
        // 发送事务到工作队列
        for tx in transactions {
            tx_sender.send(tx).unwrap();
        }
        drop(tx_sender);
        
        // 启动工作线程
        let workers = (0..MAX_PARALLEL_WORKERS.min(transactions.len()))
            .map(|_| {
                let tx_receiver = tx_receiver.clone();
                let receipt_sender = receipt_sender.clone();
                let state_db = self.state_db.clone();
                
                tokio::spawn(async move {
                    while let Ok(tx) = tx_receiver.recv() {
                        let receipt = Self::execute_single_transaction(&state_db, tx).await;
                        receipt_sender.send(receipt).unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        
        // 等待所有工作完成
        for worker in workers {
            worker.await.unwrap();
        }
        drop(receipt_sender);
        
        // 收集结果
        let mut receipts = Vec::new();
        while let Ok(receipt) = receipt_receiver.recv() {
            receipts.push(receipt?);
        }
        
        Ok(receipts)
    }

    async fn execute_single_transaction(
        state_db: &Arc<StateDB>,
        tx: Transaction,
    ) -> Result<TransactionReceipt, Error> {
        let start = Instant::now();
        
        // 验证事务
        Self::validate_transaction(state_db, &tx).await?;
        
        // 执行状态转换
        let mut state_changes = HashMap::new();
        let mut logs = Vec::new();
        let mut gas_used = 21000u64; // 基础gas费用
        
        // 扣除发送方余额
        let mut from_account = state_db.get_account(tx.from).await
            .ok_or(Error::StateNotFound)?;
        
        let total_cost = U256::from(tx.value) + U256::from(tx.gas_limit) * U256::from(tx.gas_price);
        if from_account.balance < total_cost {
            return Err(Error::InsufficientBalance);
        }
        
        from_account.balance -= total_cost;
        from_account.nonce += 1;
        state_changes.insert(tx.from, from_account.clone());
        
        // 处理接收方
        if let Some(to) = tx.to {
            if state_db.is_contract(to).await {
                // 执行合约（简化版）
                gas_used += Self::estimate_contract_gas(&tx.data);
                
                if gas_used > tx.gas_limit {
                    return Err(Error::GasLimitExceeded);
                }
                
                // 模拟合约执行产生的状态变化
                let mut to_account = state_db.get_account(to).await
                    .unwrap_or_else(|| Account::default());
                to_account.balance += U256::from(tx.value);
                state_changes.insert(to, to_account);
                
                // 模拟日志
                logs.push(format!("Contract executed: {:?}", to));
            } else {
                // 简单转账
                let mut to_account = state_db.get_account(to).await
                    .unwrap_or_else(|| Account::default());
                to_account.balance += U256::from(tx.value);
                state_changes.insert(to, to_account);
            }
        } else {
            // 合约创建
            gas_used += Self::estimate_contract_gas(&tx.data) * 2;
            
            if gas_used > tx.gas_limit {
                return Err(Error::GasLimitExceeded);
            }
            
            // 计算合约地址
            let contract_address = Self::calculate_contract_address(tx.from, from_account.nonce - 1);
            let mut contract_account = Account::default();
            contract_account.balance = U256::from(tx.value);
            contract_account.code_hash = Some(Self::hash_code(&tx.data));
            state_changes.insert(contract_address, contract_account);
            
            logs.push(format!("Contract created: {:?}", contract_address));
        }
        
        // 退还未使用的gas
        let gas_refund = tx.gas_limit - gas_used;
        if gas_refund > 0 {
            let mut from_account = state_changes.get_mut(&tx.from).unwrap();
            from_account.balance += U256::from(gas_refund) * U256::from(tx.gas_price);
        }
        
        // 应用状态变化
        for (address, account) in state_changes {
            state_db.set_account(address, account).await;
        }
        
        let receipt = TransactionReceipt {
            transaction_hash: tx.hash(),
            from: tx.from,
            to: tx.to,
            gas_used,
            status: true,
            logs,
            execution_time_us: start.elapsed().as_micros() as u64,
        };
        
        Ok(receipt)
    }

    async fn validate_transaction(
        state_db: &Arc<StateDB>,
        tx: &Transaction,
    ) -> Result<(), Error> {
        // 验证签名
        if !tx.verify_signature() {
            return Err(Error::InvalidTransaction("Invalid signature".to_string()));
        }
        
        // 验证nonce
        let account = state_db.get_account(tx.from).await
            .ok_or(Error::StateNotFound)?;
        
        if account.nonce != tx.nonce {
            return Err(Error::InvalidTransaction("Invalid nonce".to_string()));
        }
        
        // 验证余额
        let total_cost = U256::from(tx.value) + U256::from(tx.gas_limit) * U256::from(tx.gas_price);
        if account.balance < total_cost {
            return Err(Error::InsufficientBalance);
        }
        
        // 验证gas限制
        if tx.gas_limit < 21000 {
            return Err(Error::InvalidTransaction("Gas limit too low".to_string()));
        }
        
        Ok(())
    }

    async fn is_contract(&self, address: [u8; 20]) -> bool {
        self.state_db.get_account(address).await
            .map(|acc| acc.code_hash.is_some())
            .unwrap_or(false)
    }

    fn estimate_contract_gas(data: &[u8]) -> u64 {
        // 简化的gas估算
        let base_gas = 32000u64;
        let per_byte_gas = 68u64;
        base_gas + (data.len() as u64 * per_byte_gas)
    }

    fn calculate_contract_address(creator: [u8; 20], nonce: u64) -> [u8; 20] {
        use sha2::{Digest, Sha256};
        
        let mut data = Vec::new();
        data.extend_from_slice(&creator);
        data.extend_from_slice(&nonce.to_be_bytes());
        
        let hash = Sha256::digest(&data);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..32]);
        address
    }

    fn hash_code(code: &[u8]) -> H256 {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(code);
        H256::from_slice(&hash)
    }

    pub async fn get_metrics(&self) -> ExecutionMetrics {
        ExecutionMetrics {
            total_transactions: AtomicU64::new(
                self.metrics.total_transactions.load(Ordering::Relaxed)
            ),
            successful_transactions: AtomicU64::new(
                self.metrics.successful_transactions.load(Ordering::Relaxed)
            ),
            failed_transactions: AtomicU64::new(
                self.metrics.failed_transactions.load(Ordering::Relaxed)
            ),
            total_gas_used: AtomicU64::new(
                self.metrics.total_gas_used.load(Ordering::Relaxed)
            ),
            average_execution_time_us: AtomicU64::new(
                self.metrics.average_execution_time_us.load(Ordering::Relaxed)
            ),
        }
    }
}

#[async_trait]
impl StateMachine for HighPerformanceStateMachine {
    async fn execute_block(&self, transactions: Vec<Transaction>) -> Result<Vec<TransactionReceipt>, Error> {
        self.execute_transactions_parallel(transactions).await
    }

    async fn get_state(&self, address: [u8; 20]) -> Option<Account> {
        self.state_db.get_account(address).await
    }

    async fn get_storage(&self, address: [u8; 20], key: H256) -> H256 {
        self.state_db.get_storage(address, key).await
    }

    async fn commit_state(&self, block_hash: H256) -> Result<(), Error> {
        self.state_db.commit(block_hash).await
            .map_err(|e| Error::StorageError(e.to_string()))
    }

    async fn rollback_state(&self, block_hash: H256) -> Result<(), Error> {
        self.state_db.rollback(block_hash).await
            .map_err(|e| Error::StorageError(e.to_string()))
    }
}