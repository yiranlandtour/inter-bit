use super::{
    connector::HyperliquidConnector,
    types::*,
    HyperliquidError,
    VaultPerformance,
};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct VaultManager {
    connector: Arc<HyperliquidConnector>,
    vaults: Arc<RwLock<HashMap<[u8; 20], HyperliquidVault>>>,
    user_deposits: Arc<RwLock<HashMap<[u8; 20], Vec<VaultDeposit>>>>,
    vault_performance: Arc<RwLock<HashMap<[u8; 20], VaultPerformance>>>,
}

impl VaultManager {
    pub fn new(connector: Arc<HyperliquidConnector>) -> Self {
        Self {
            connector,
            vaults: Arc::new(RwLock::new(HashMap::new())),
            user_deposits: Arc::new(RwLock::new(HashMap::new())),
            vault_performance: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn deposit(
        &self,
        account: [u8; 20],
        vault_address: [u8; 20],
        amount: U256,
    ) -> Result<H256, HyperliquidError> {
        let vault = self.get_vault(vault_address).await?;
        
        if amount < vault.min_deposit {
            return Err(HyperliquidError::ApiError("Amount below minimum deposit".to_string()));
        }
        
        let account_info = self.connector.get_account_info(account).await?;
        if amount > account_info.balance {
            return Err(HyperliquidError::InsufficientBalance);
        }
        
        let shares_received = self.calculate_shares_for_deposit(&vault, amount);
        let current_time = self.get_timestamp();
        let unlock_time = current_time + vault.lock_period;
        
        let deposit = VaultDeposit {
            vault_address,
            depositor: account,
            amount,
            shares_received,
            share_price: vault.share_price,
            timestamp: current_time,
            unlock_time,
            tx_hash: H256::random(),
        };
        
        let mut user_deposits = self.user_deposits.write().await;
        user_deposits.entry(account)
            .or_insert_with(Vec::new)
            .push(deposit.clone());
        
        let mut vaults = self.vaults.write().await;
        if let Some(vault) = vaults.get_mut(&vault_address) {
            vault.total_value_locked += amount;
            vault.total_shares += shares_received;
        }
        
        Ok(deposit.tx_hash)
    }

    pub async fn withdraw(
        &self,
        account: [u8; 20],
        vault_address: [u8; 20],
        shares: U256,
    ) -> Result<H256, HyperliquidError> {
        let vault = self.get_vault(vault_address).await?;
        let user_shares = self.get_user_shares(account, vault_address).await?;
        
        if shares > user_shares {
            return Err(HyperliquidError::ApiError("Insufficient shares".to_string()));
        }
        
        let current_time = self.get_timestamp();
        let deposits = self.user_deposits.read().await;
        let user_deposits = deposits.get(&account)
            .ok_or(HyperliquidError::ApiError("No deposits found".to_string()))?;
        
        let eligible_deposit = user_deposits.iter()
            .find(|d| d.vault_address == vault_address && d.unlock_time <= current_time)
            .ok_or(HyperliquidError::ApiError("Shares still locked".to_string()))?;
        
        drop(deposits);
        
        let amount_to_withdraw = self.calculate_withdrawal_amount(&vault, shares);
        let performance_fee = (amount_to_withdraw * U256::from(vault.performance_fee)) / U256::from(10000);
        let amount_after_fees = amount_to_withdraw - performance_fee;
        
        let withdrawal = VaultWithdrawal {
            vault_address,
            withdrawer: account,
            shares_burned: shares,
            amount_received: amount_after_fees,
            share_price: vault.share_price,
            performance_fee_paid: performance_fee,
            timestamp: current_time,
            tx_hash: H256::random(),
        };
        
        let mut vaults = self.vaults.write().await;
        if let Some(vault) = vaults.get_mut(&vault_address) {
            vault.total_value_locked = if vault.total_value_locked > amount_to_withdraw {
                vault.total_value_locked - amount_to_withdraw
            } else {
                U256::zero()
            };
            vault.total_shares = if vault.total_shares > shares {
                vault.total_shares - shares
            } else {
                U256::zero()
            };
        }
        
        let mut user_deposits = self.user_deposits.write().await;
        if let Some(deposits) = user_deposits.get_mut(&account) {
            deposits.retain(|d| d.vault_address != vault_address || d.shares_received > shares);
        }
        
        Ok(withdrawal.tx_hash)
    }

    pub async fn get_performance(
        &self,
        vault_address: [u8; 20],
    ) -> Result<VaultPerformance, HyperliquidError> {
        let cache = self.vault_performance.read().await;
        if let Some(performance) = cache.get(&vault_address) {
            return Ok(performance.clone());
        }
        drop(cache);
        
        let vault = self.get_vault(vault_address).await?;
        
        let performance = VaultPerformance {
            vault_address,
            total_value_locked: vault.total_value_locked,
            apy: self.calculate_apy(&vault),
            sharpe_ratio: self.calculate_sharpe_ratio(&vault),
            max_drawdown: self.calculate_max_drawdown(&vault),
            total_trades: 0,  // Would be fetched from historical data
            win_rate: 0,      // Would be calculated from historical trades
        };
        
        let mut cache = self.vault_performance.write().await;
        cache.insert(vault_address, performance.clone());
        
        Ok(performance)
    }

    pub async fn get_vault(&self, vault_address: [u8; 20]) -> Result<HyperliquidVault, HyperliquidError> {
        let vaults = self.vaults.read().await;
        vaults.get(&vault_address)
            .cloned()
            .ok_or(HyperliquidError::ApiError("Vault not found".to_string()))
    }

    pub async fn create_vault(
        &self,
        manager: [u8; 20],
        name: String,
        description: String,
        strategy_type: VaultStrategy,
        performance_fee: u32,
        management_fee: u32,
        min_deposit: U256,
        lock_period: u64,
    ) -> Result<[u8; 20], HyperliquidError> {
        if performance_fee > 5000 {  // Max 50%
            return Err(HyperliquidError::ApiError("Performance fee too high".to_string()));
        }
        
        if management_fee > 500 {  // Max 5% annually
            return Err(HyperliquidError::ApiError("Management fee too high".to_string()));
        }
        
        let vault_address = self.generate_vault_address(&name);
        
        let vault = HyperliquidVault {
            address: vault_address,
            name,
            description,
            manager,
            total_value_locked: U256::zero(),
            share_price: U256::from(1_000_000),  // 1 USDC initial price
            total_shares: U256::zero(),
            performance_fee,
            management_fee,
            min_deposit,
            lock_period,
            strategy_type,
            created_at: self.get_timestamp(),
            last_rebalance: self.get_timestamp(),
        };
        
        let mut vaults = self.vaults.write().await;
        vaults.insert(vault_address, vault);
        
        Ok(vault_address)
    }

    pub async fn list_vaults(&self) -> Result<Vec<HyperliquidVault>, HyperliquidError> {
        let vaults = self.vaults.read().await;
        Ok(vaults.values().cloned().collect())
    }

    pub async fn get_user_vaults(&self, account: [u8; 20]) -> Result<Vec<VaultDeposit>, HyperliquidError> {
        let deposits = self.user_deposits.read().await;
        Ok(deposits.get(&account).cloned().unwrap_or_default())
    }

    pub async fn get_user_shares(
        &self,
        account: [u8; 20],
        vault_address: [u8; 20],
    ) -> Result<U256, HyperliquidError> {
        let deposits = self.user_deposits.read().await;
        let user_deposits = deposits.get(&account).unwrap_or(&Vec::new());
        
        let total_shares = user_deposits.iter()
            .filter(|d| d.vault_address == vault_address)
            .map(|d| d.shares_received)
            .fold(U256::zero(), |acc, shares| acc + shares);
        
        Ok(total_shares)
    }

    pub async fn rebalance_vault(
        &self,
        vault_address: [u8; 20],
        manager: [u8; 20],
    ) -> Result<(), HyperliquidError> {
        let vault = self.get_vault(vault_address).await?;
        
        if vault.manager != manager {
            return Err(HyperliquidError::Unauthorized);
        }
        
        let current_time = self.get_timestamp();
        let time_since_last_rebalance = current_time - vault.last_rebalance;
        
        if time_since_last_rebalance < 3600 {  // Minimum 1 hour between rebalances
            return Err(HyperliquidError::ApiError("Too soon to rebalance".to_string()));
        }
        
        // Implement actual rebalancing logic here
        // This would involve analyzing current positions and adjusting them
        // according to the vault's strategy
        
        let mut vaults = self.vaults.write().await;
        if let Some(vault) = vaults.get_mut(&vault_address) {
            vault.last_rebalance = current_time;
        }
        
        Ok(())
    }

    fn calculate_shares_for_deposit(&self, vault: &HyperliquidVault, amount: U256) -> U256 {
        if vault.total_shares == U256::zero() {
            amount  // First depositor gets 1:1 shares
        } else {
            (amount * vault.total_shares) / vault.total_value_locked
        }
    }

    fn calculate_withdrawal_amount(&self, vault: &HyperliquidVault, shares: U256) -> U256 {
        if vault.total_shares == U256::zero() {
            U256::zero()
        } else {
            (shares * vault.total_value_locked) / vault.total_shares
        }
    }

    fn calculate_apy(&self, _vault: &HyperliquidVault) -> u32 {
        // Simplified APY calculation
        // In production, this would analyze historical performance
        1500  // 15% APY
    }

    fn calculate_sharpe_ratio(&self, _vault: &HyperliquidVault) -> i32 {
        // Simplified Sharpe ratio calculation
        // In production, this would analyze risk-adjusted returns
        150  // 1.5 Sharpe ratio
    }

    fn calculate_max_drawdown(&self, _vault: &HyperliquidVault) -> u32 {
        // Simplified max drawdown calculation
        // In production, this would analyze historical drawdowns
        1000  // 10% max drawdown
    }

    fn generate_vault_address(&self, name: &str) -> [u8; 20] {
        let mut address = [0u8; 20];
        let name_bytes = name.as_bytes();
        let len = std::cmp::min(name_bytes.len(), 20);
        address[..len].copy_from_slice(&name_bytes[..len]);
        address
    }

    fn get_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}