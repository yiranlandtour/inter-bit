use super::{Protocol, Route, RoutingError};
use primitive_types::{H256, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct GasOptimizer {
    gas_prices: Arc<RwLock<GasPriceTracker>>,
    protocol_costs: HashMap<Protocol, GasCost>,
    optimization_strategies: Vec<Box<dyn OptimizationStrategy>>,
}

#[derive(Clone)]
struct GasPriceTracker {
    base_fee: U256,
    priority_fee: U256,
    max_fee: U256,
    last_update: u64,
    historical: Vec<GasPrice>,
}

#[derive(Clone)]
struct GasPrice {
    timestamp: u64,
    base_fee: U256,
    priority_fee: U256,
}

#[derive(Clone)]
struct GasCost {
    base_gas: u32,
    per_hop_gas: u32,
    approval_gas: u32,
    verification_gas: u32,
}

trait OptimizationStrategy: Send + Sync {
    fn optimize(&self, route: &mut Route, context: &OptimizationContext);
    fn name(&self) -> &str;
}

struct OptimizationContext {
    sender: Option<[u8; 20]>,
    gas_price: U256,
    block_number: u64,
    mempool_congestion: f64,
}

impl GasOptimizer {
    pub fn new() -> Self {
        let mut protocol_costs = HashMap::new();
        
        protocol_costs.insert(Protocol::OrderBook, GasCost {
            base_gas: 150_000,
            per_hop_gas: 30_000,
            approval_gas: 45_000,
            verification_gas: 20_000,
        });
        
        protocol_costs.insert(Protocol::AmmV2, GasCost {
            base_gas: 120_000,
            per_hop_gas: 60_000,
            approval_gas: 45_000,
            verification_gas: 10_000,
        });
        
        protocol_costs.insert(Protocol::AmmV3, GasCost {
            base_gas: 180_000,
            per_hop_gas: 80_000,
            approval_gas: 45_000,
            verification_gas: 15_000,
        });
        
        protocol_costs.insert(Protocol::StableSwap, GasCost {
            base_gas: 100_000,
            per_hop_gas: 40_000,
            approval_gas: 45_000,
            verification_gas: 10_000,
        });
        
        protocol_costs.insert(Protocol::ConcentratedLiquidity, GasCost {
            base_gas: 200_000,
            per_hop_gas: 90_000,
            approval_gas: 45_000,
            verification_gas: 20_000,
        });
        
        protocol_costs.insert(Protocol::RfqSystem, GasCost {
            base_gas: 80_000,
            per_hop_gas: 20_000,
            approval_gas: 0,
            verification_gas: 25_000,
        });

        let optimization_strategies: Vec<Box<dyn OptimizationStrategy>> = vec![
            Box::new(BatchingStrategy {}),
            Box::new(CallDataOptimization {}),
            Box::new(StorageOptimization {}),
            Box::new(MultiCallStrategy {}),
        ];

        Self {
            gas_prices: Arc::new(RwLock::new(GasPriceTracker {
                base_fee: U256::from(30_000_000_000u64),
                priority_fee: U256::from(2_000_000_000u64),
                max_fee: U256::from(100_000_000_000u64),
                last_update: 0,
                historical: Vec::new(),
            })),
            protocol_costs,
            optimization_strategies,
        }
    }

    pub async fn estimate_gas(&self, protocol: &Protocol) -> U256 {
        let cost = self.protocol_costs
            .get(protocol)
            .cloned()
            .unwrap_or(GasCost {
                base_gas: 100_000,
                per_hop_gas: 50_000,
                approval_gas: 45_000,
                verification_gas: 10_000,
            });

        U256::from(cost.base_gas)
    }

    pub async fn estimate_route_gas(&self, route: &Route) -> U256 {
        let mut total_gas = U256::zero();
        let mut seen_protocols = HashMap::new();

        for hop in &route.path {
            let protocol = &hop.protocol;
            let cost = self.protocol_costs.get(protocol).cloned().unwrap_or(GasCost {
                base_gas: 100_000,
                per_hop_gas: 50_000,
                approval_gas: 45_000,
                verification_gas: 10_000,
            });

            if !seen_protocols.contains_key(protocol) {
                total_gas = total_gas + U256::from(cost.base_gas);
                total_gas = total_gas + U256::from(cost.approval_gas);
                seen_protocols.insert(protocol.clone(), true);
            }

            total_gas = total_gas + U256::from(cost.per_hop_gas);
        }

        let verification_gas = self.calculate_verification_gas(route);
        total_gas = total_gas + verification_gas;

        let optimized_gas = self.apply_optimizations(total_gas, route).await;
        
        optimized_gas
    }

    pub async fn get_current_gas_price(&self) -> U256 {
        let tracker = self.gas_prices.read().await;
        tracker.base_fee + tracker.priority_fee
    }

    pub async fn get_gas_price_prediction(&self, blocks_ahead: u64) -> U256 {
        let tracker = self.gas_prices.read().await;
        
        if tracker.historical.is_empty() {
            return tracker.base_fee + tracker.priority_fee;
        }

        let trend = self.calculate_trend(&tracker.historical);
        let current = tracker.base_fee + tracker.priority_fee;
        
        let predicted = if trend > 0.0 {
            current + U256::from((trend * blocks_ahead as f64) as u64)
        } else {
            current.saturating_sub(U256::from((trend.abs() * blocks_ahead as f64) as u64))
        };

        predicted.min(tracker.max_fee)
    }

    pub async fn optimize_for_sender(
        &self,
        mut route: Route,
        sender: [u8; 20],
    ) -> Result<Route, RoutingError> {
        let gas_price = self.get_current_gas_price().await;
        let context = OptimizationContext {
            sender: Some(sender),
            gas_price,
            block_number: self.get_current_block(),
            mempool_congestion: self.estimate_congestion().await,
        };

        for strategy in &self.optimization_strategies {
            strategy.optimize(&mut route, &context);
        }

        route.gas_cost = self.estimate_route_gas(&route).await;
        
        Ok(route)
    }

    pub async fn suggest_gas_settings(&self) -> GasSuggestion {
        let tracker = self.gas_prices.read().await;
        let congestion = self.estimate_congestion().await;
        
        let slow = GasOption {
            max_fee: tracker.base_fee + U256::from(1_000_000_000u64),
            priority_fee: U256::from(1_000_000_000u64),
            estimated_time: 180,
        };

        let standard = GasOption {
            max_fee: tracker.base_fee + tracker.priority_fee,
            priority_fee: tracker.priority_fee,
            estimated_time: 30,
        };

        let fast = GasOption {
            max_fee: tracker.base_fee + tracker.priority_fee * U256::from(2u32),
            priority_fee: tracker.priority_fee * U256::from(2u32),
            estimated_time: 12,
        };

        let instant = GasOption {
            max_fee: tracker.base_fee + tracker.priority_fee * U256::from(5u32),
            priority_fee: tracker.priority_fee * U256::from(5u32),
            estimated_time: 6,
        };

        GasSuggestion {
            slow,
            standard,
            fast,
            instant,
            recommended: if congestion > 0.8 { fast } else { standard },
            congestion_level: congestion,
        }
    }

    fn calculate_verification_gas(&self, route: &Route) -> U256 {
        let mut verification_gas = U256::zero();
        
        for hop in &route.path {
            if let Some(cost) = self.protocol_costs.get(&hop.protocol) {
                verification_gas = verification_gas + U256::from(cost.verification_gas);
            }
        }

        if route.path.len() > 2 {
            verification_gas = verification_gas + U256::from(10_000 * (route.path.len() - 2));
        }

        verification_gas
    }

    async fn apply_optimizations(&self, base_gas: U256, route: &Route) -> U256 {
        let mut optimized = base_gas;
        
        if route.path.len() > 1 {
            let batching_savings = base_gas * U256::from(5u32) / U256::from(100u32);
            optimized = optimized.saturating_sub(batching_savings);
        }

        let same_protocol = route.path.windows(2).all(|w| w[0].protocol == w[1].protocol);
        if same_protocol {
            let protocol_savings = base_gas * U256::from(10u32) / U256::from(100u32);
            optimized = optimized.saturating_sub(protocol_savings);
        }

        if route.path.len() <= 2 {
            let simplicity_bonus = base_gas * U256::from(3u32) / U256::from(100u32);
            optimized = optimized.saturating_sub(simplicity_bonus);
        }

        optimized
    }

    fn calculate_trend(&self, historical: &[GasPrice]) -> f64 {
        if historical.len() < 2 {
            return 0.0;
        }

        let recent = &historical[historical.len().saturating_sub(10)..];
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xx = 0.0;
        let mut sum_xy = 0.0;
        let n = recent.len() as f64;

        for (i, price) in recent.iter().enumerate() {
            let x = i as f64;
            let y = (price.base_fee + price.priority_fee).as_u128() as f64;
            
            sum_x += x;
            sum_y += y;
            sum_xx += x * x;
            sum_xy += x * y;
        }

        let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_xx - sum_x * sum_x);
        
        slope
    }

    async fn estimate_congestion(&self) -> f64 {
        let tracker = self.gas_prices.read().await;
        let current_gas = tracker.base_fee + tracker.priority_fee;
        let max_gas = tracker.max_fee;
        
        if max_gas == U256::zero() {
            return 0.5;
        }
        
        let ratio = current_gas.as_u128() as f64 / max_gas.as_u128() as f64;
        ratio.min(1.0)
    }

    fn get_current_block(&self) -> u64 {
        15_000_000
    }

    pub async fn update_gas_price(&self, base_fee: U256, priority_fee: U256) {
        let mut tracker = self.gas_prices.write().await;
        
        tracker.historical.push(GasPrice {
            timestamp: self.get_current_timestamp(),
            base_fee: tracker.base_fee,
            priority_fee: tracker.priority_fee,
        });
        
        if tracker.historical.len() > 100 {
            tracker.historical.remove(0);
        }
        
        tracker.base_fee = base_fee;
        tracker.priority_fee = priority_fee;
        tracker.last_update = self.get_current_timestamp();
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

struct BatchingStrategy;

impl OptimizationStrategy for BatchingStrategy {
    fn optimize(&self, route: &mut Route, _context: &OptimizationContext) {
        if route.path.len() > 3 {
            route.gas_cost = route.gas_cost * U256::from(95u32) / U256::from(100u32);
        }
    }

    fn name(&self) -> &str {
        "Batching"
    }
}

struct CallDataOptimization;

impl OptimizationStrategy for CallDataOptimization {
    fn optimize(&self, route: &mut Route, _context: &OptimizationContext) {
        for hop in &mut route.path {
            hop.amount_in = hop.amount_in / U256::from(10u64.pow(10)) * U256::from(10u64.pow(10));
        }
    }

    fn name(&self) -> &str {
        "CallData"
    }
}

struct StorageOptimization;

impl OptimizationStrategy for StorageOptimization {
    fn optimize(&self, route: &mut Route, _context: &OptimizationContext) {
        let unique_tokens: std::collections::HashSet<_> = route.path.iter()
            .flat_map(|h| vec![h.token_in, h.token_out])
            .collect();
        
        if unique_tokens.len() <= 3 {
            route.gas_cost = route.gas_cost * U256::from(97u32) / U256::from(100u32);
        }
    }

    fn name(&self) -> &str {
        "Storage"
    }
}

struct MultiCallStrategy;

impl OptimizationStrategy for MultiCallStrategy {
    fn optimize(&self, route: &mut Route, _context: &OptimizationContext) {
        let protocols: std::collections::HashSet<_> = route.path.iter()
            .map(|h| h.protocol.clone())
            .collect();
        
        if protocols.len() == 1 {
            route.gas_cost = route.gas_cost * U256::from(90u32) / U256::from(100u32);
        }
    }

    fn name(&self) -> &str {
        "MultiCall"
    }
}

#[derive(Clone)]
pub struct GasSuggestion {
    pub slow: GasOption,
    pub standard: GasOption,
    pub fast: GasOption,
    pub instant: GasOption,
    pub recommended: GasOption,
    pub congestion_level: f64,
}

#[derive(Clone, Copy)]
pub struct GasOption {
    pub max_fee: U256,
    pub priority_fee: U256,
    pub estimated_time: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gas_estimation() {
        let optimizer = GasOptimizer::new();
        
        let gas = optimizer.estimate_gas(&Protocol::AmmV2).await;
        assert!(gas > U256::zero());
        
        let gas_v3 = optimizer.estimate_gas(&Protocol::AmmV3).await;
        assert!(gas_v3 > gas);
    }

    #[tokio::test]
    async fn test_gas_suggestions() {
        let optimizer = GasOptimizer::new();
        
        optimizer.update_gas_price(
            U256::from(30_000_000_000u64),
            U256::from(2_000_000_000u64),
        ).await;
        
        let suggestions = optimizer.suggest_gas_settings().await;
        
        assert!(suggestions.slow.max_fee < suggestions.standard.max_fee);
        assert!(suggestions.standard.max_fee < suggestions.fast.max_fee);
        assert!(suggestions.fast.max_fee < suggestions.instant.max_fee);
    }
}