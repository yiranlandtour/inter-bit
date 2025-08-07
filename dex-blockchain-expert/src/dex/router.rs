// 智能路由器
// 负责寻找最优交易路径，最小化滑点和手续费

use super::{TradePath, TradeHop, DexError};
use primitive_types::{H256, U256};
use crate::dex::smart_routing::{SmartRouter as AdvancedRouter, RoutingConfig, PoolInfo, Protocol};
use std::sync::Arc;

pub struct SmartRouter {
    advanced_router: Arc<AdvancedRouter>,
}

impl SmartRouter {
    pub fn new() -> Self {
        let config = RoutingConfig::default();
        Self {
            advanced_router: Arc::new(AdvancedRouter::new(config)),
        }
    }

    pub async fn find_best_path(
        &self,
        token_in: H256,
        token_out: H256,
        amount_in: U256,
    ) -> Result<TradePath, DexError> {
        // 注册一些示例池用于测试
        self.register_sample_pools().await;
        
        // 使用高级路由器查找最优路径
        let route = self.advanced_router
            .find_best_route(token_in, token_out, amount_in, None)
            .await
            .map_err(|_| DexError::PathNotFound)?;
        
        // 转换路径格式
        let hops = route.path.iter()
            .map(|hop| TradeHop::AmmPool(hop.pool_address))
            .collect();
        
        Ok(TradePath {
            hops,
            expected_output: route.output_amount,
            price_impact: route.price_impact,
        })
    }
    
    async fn register_sample_pools(&self) {
        // 注册一些示例池用于路由
        let pools = vec![
            PoolInfo {
                address: H256::from_low_u64_be(1),
                protocol: Protocol::AmmV2,
                token_a: H256::from_low_u64_be(100),
                token_b: H256::from_low_u64_be(200),
                fee_tier: 3000,
                liquidity: U256::from(1_000_000u64),
                volume_24h: U256::from(100_000u64),
                tvl: U256::from(2_000_000u64),
            },
            PoolInfo {
                address: H256::from_low_u64_be(2),
                protocol: Protocol::AmmV3,
                token_a: H256::from_low_u64_be(200),
                token_b: H256::from_low_u64_be(300),
                fee_tier: 500,
                liquidity: U256::from(5_000_000u64),
                volume_24h: U256::from(500_000u64),
                tvl: U256::from(10_000_000u64),
            },
        ];
        
        for pool in pools {
            self.advanced_router.register_pool(pool).await;
        }
    }
}