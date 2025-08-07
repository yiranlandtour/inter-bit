pub mod pathfinder;
pub mod liquidity_aggregator;
pub mod gas_optimizer;

use primitive_types::{H256, U256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, BinaryHeap};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub max_hops: usize,
    pub max_splits: usize,
    pub price_impact_threshold: f64,
    pub gas_price_threshold: U256,
    pub slippage_tolerance: f64,
    pub enable_multi_path: bool,
    pub enable_gas_optimization: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            max_hops: 4,
            max_splits: 3,
            price_impact_threshold: 0.05,
            gas_price_threshold: U256::from(100_000_000_000u64),
            slippage_tolerance: 0.005,
            enable_multi_path: true,
            enable_gas_optimization: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub id: H256,
    pub path: Vec<Hop>,
    pub input_amount: U256,
    pub output_amount: U256,
    pub price_impact: f64,
    pub gas_cost: U256,
    pub execution_price: U256,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hop {
    pub protocol: Protocol,
    pub pool_address: H256,
    pub token_in: H256,
    pub token_out: H256,
    pub amount_in: U256,
    pub amount_out: U256,
    pub fee: U256,
    pub reserves: (U256, U256),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Protocol {
    OrderBook,
    AmmV2,
    AmmV3,
    StableSwap,
    ConcentratedLiquidity,
    RfqSystem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitRoute {
    pub routes: Vec<Route>,
    pub split_percentages: Vec<f64>,
    pub total_output: U256,
    pub average_price: U256,
    pub total_gas: U256,
}

pub struct SmartRouter {
    config: RoutingConfig,
    pathfinder: Arc<pathfinder::PathFinder>,
    aggregator: Arc<liquidity_aggregator::LiquidityAggregator>,
    gas_optimizer: Arc<gas_optimizer::GasOptimizer>,
    route_cache: Arc<RwLock<HashMap<H256, CachedRoute>>>,
    pool_registry: Arc<RwLock<HashMap<H256, PoolInfo>>>,
}

#[derive(Debug, Clone)]
struct CachedRoute {
    route: Route,
    timestamp: u64,
    hit_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    pub address: H256,
    pub protocol: Protocol,
    pub token_a: H256,
    pub token_b: H256,
    pub fee_tier: u32,
    pub liquidity: U256,
    pub volume_24h: U256,
    pub tvl: U256,
}

impl SmartRouter {
    pub fn new(config: RoutingConfig) -> Self {
        Self {
            pathfinder: Arc::new(pathfinder::PathFinder::new(config.max_hops)),
            aggregator: Arc::new(liquidity_aggregator::LiquidityAggregator::new()),
            gas_optimizer: Arc::new(gas_optimizer::GasOptimizer::new()),
            route_cache: Arc::new(RwLock::new(HashMap::new())),
            pool_registry: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    pub async fn find_best_route(
        &self,
        token_in: H256,
        token_out: H256,
        amount_in: U256,
        sender: Option<[u8; 20]>,
    ) -> Result<Route, RoutingError> {
        let cache_key = self.compute_cache_key(token_in, token_out, amount_in);
        
        if let Some(cached) = self.get_cached_route(&cache_key).await {
            if self.is_route_valid(&cached.route).await {
                return Ok(cached.route);
            }
        }

        let pools = self.get_relevant_pools(token_in, token_out).await?;
        
        let all_paths = self.pathfinder.find_all_paths(
            token_in,
            token_out,
            &pools,
            self.config.max_hops,
        ).await?;

        let mut evaluated_routes = Vec::new();
        for path in all_paths {
            if let Ok(route) = self.evaluate_path(path, amount_in).await {
                evaluated_routes.push(route);
            }
        }

        if evaluated_routes.is_empty() {
            return Err(RoutingError::NoRouteFound);
        }

        let best_route = if self.config.enable_gas_optimization {
            self.optimize_for_gas(evaluated_routes, sender).await?
        } else {
            self.select_best_by_output(evaluated_routes)
        };

        self.cache_route(cache_key, best_route.clone()).await;
        
        Ok(best_route)
    }

    pub async fn find_split_route(
        &self,
        token_in: H256,
        token_out: H256,
        amount_in: U256,
    ) -> Result<SplitRoute, RoutingError> {
        if !self.config.enable_multi_path {
            let single_route = self.find_best_route(token_in, token_out, amount_in, None).await?;
            return Ok(SplitRoute {
                routes: vec![single_route.clone()],
                split_percentages: vec![1.0],
                total_output: single_route.output_amount,
                average_price: single_route.execution_price,
                total_gas: single_route.gas_cost,
            });
        }

        let pools = self.get_relevant_pools(token_in, token_out).await?;
        
        let paths = self.pathfinder.find_all_paths(
            token_in,
            token_out,
            &pools,
            self.config.max_hops,
        ).await?;

        let splits = self.calculate_optimal_split(
            paths,
            amount_in,
            self.config.max_splits,
        ).await?;

        Ok(splits)
    }

    async fn calculate_optimal_split(
        &self,
        paths: Vec<Vec<H256>>,
        amount_in: U256,
        max_splits: usize,
    ) -> Result<SplitRoute, RoutingError> {
        let mut best_split = SplitRoute {
            routes: Vec::new(),
            split_percentages: Vec::new(),
            total_output: U256::zero(),
            average_price: U256::zero(),
            total_gas: U256::zero(),
        };

        let split_count = std::cmp::min(paths.len(), max_splits);
        if split_count == 0 {
            return Err(RoutingError::NoRouteFound);
        }

        let base_amount = amount_in / U256::from(split_count);
        let mut remaining = amount_in - (base_amount * U256::from(split_count));

        for (i, path) in paths.iter().take(split_count).enumerate() {
            let split_amount = if i == 0 {
                base_amount + remaining
            } else {
                base_amount
            };

            if let Ok(route) = self.evaluate_path(path.clone(), split_amount).await {
                best_split.routes.push(route.clone());
                best_split.split_percentages.push(
                    split_amount.as_u128() as f64 / amount_in.as_u128() as f64
                );
                best_split.total_output = best_split.total_output + route.output_amount;
                best_split.total_gas = best_split.total_gas + route.gas_cost;
            }
        }

        if best_split.routes.is_empty() {
            return Err(RoutingError::NoRouteFound);
        }

        best_split.average_price = best_split.total_output * U256::from(10u64.pow(18)) / amount_in;

        Ok(best_split)
    }

    async fn evaluate_path(
        &self,
        path: Vec<H256>,
        amount_in: U256,
    ) -> Result<Route, RoutingError> {
        let mut hops = Vec::new();
        let mut current_amount = amount_in;
        let mut total_gas = U256::zero();

        for i in 0..path.len() - 1 {
            let pool_address = path[i];
            let pool_info = self.get_pool_info(pool_address).await?;
            
            let (amount_out, fee, reserves) = self.aggregator
                .calculate_swap_output(
                    &pool_info,
                    current_amount,
                ).await?;

            let gas_cost = self.gas_optimizer.estimate_gas(&pool_info.protocol).await;
            total_gas = total_gas + gas_cost;

            hops.push(Hop {
                protocol: pool_info.protocol.clone(),
                pool_address,
                token_in: pool_info.token_a,
                token_out: pool_info.token_b,
                amount_in: current_amount,
                amount_out,
                fee,
                reserves,
            });

            current_amount = amount_out;
        }

        let price_impact = self.calculate_price_impact(amount_in, current_amount, &hops);
        
        Ok(Route {
            id: H256::random(),
            path: hops,
            input_amount: amount_in,
            output_amount: current_amount,
            price_impact,
            gas_cost: total_gas,
            execution_price: current_amount * U256::from(10u64.pow(18)) / amount_in,
            confidence: self.calculate_confidence(&path, current_amount),
        })
    }

    fn calculate_price_impact(&self, amount_in: U256, amount_out: U256, hops: &[Hop]) -> f64 {
        if hops.is_empty() {
            return 0.0;
        }

        let mut impact = 0.0;
        for hop in hops {
            let reserve_ratio = hop.amount_in.as_u128() as f64 / hop.reserves.0.as_u128() as f64;
            impact += reserve_ratio * 0.997;
        }

        impact.min(1.0)
    }

    fn calculate_confidence(&self, _path: &[H256], output: U256) -> f64 {
        if output == U256::zero() {
            return 0.0;
        }
        0.95
    }

    async fn optimize_for_gas(
        &self,
        routes: Vec<Route>,
        sender: Option<[u8; 20]>,
    ) -> Result<Route, RoutingError> {
        let gas_price = self.gas_optimizer.get_current_gas_price().await;
        
        let mut best_route = routes[0].clone();
        let mut best_score = self.calculate_route_score(&best_route, gas_price);

        for route in routes.iter().skip(1) {
            let score = self.calculate_route_score(route, gas_price);
            if score > best_score {
                best_score = score;
                best_route = route.clone();
            }
        }

        if let Some(sender_address) = sender {
            best_route = self.gas_optimizer
                .optimize_for_sender(best_route, sender_address)
                .await?;
        }

        Ok(best_route)
    }

    fn calculate_route_score(&self, route: &Route, gas_price: U256) -> f64 {
        let output_value = route.output_amount.as_u128() as f64;
        let gas_cost_value = (route.gas_cost * gas_price).as_u128() as f64;
        let impact_penalty = route.price_impact * output_value;
        
        (output_value - gas_cost_value - impact_penalty).max(0.0)
    }

    fn select_best_by_output(&self, routes: Vec<Route>) -> Route {
        routes.into_iter()
            .max_by_key(|r| r.output_amount)
            .unwrap()
    }

    async fn get_relevant_pools(
        &self,
        token_in: H256,
        token_out: H256,
    ) -> Result<Vec<PoolInfo>, RoutingError> {
        let registry = self.pool_registry.read().await;
        
        let mut relevant_pools = Vec::new();
        for pool in registry.values() {
            if (pool.token_a == token_in && pool.token_b == token_out) ||
               (pool.token_a == token_out && pool.token_b == token_in) ||
               pool.token_a == token_in || pool.token_b == token_in ||
               pool.token_a == token_out || pool.token_b == token_out {
                relevant_pools.push(pool.clone());
            }
        }

        if relevant_pools.is_empty() {
            return Err(RoutingError::NoPoolsAvailable);
        }

        relevant_pools.sort_by(|a, b| b.liquidity.cmp(&a.liquidity));
        
        Ok(relevant_pools)
    }

    async fn get_pool_info(&self, address: H256) -> Result<PoolInfo, RoutingError> {
        let registry = self.pool_registry.read().await;
        registry.get(&address)
            .cloned()
            .ok_or(RoutingError::PoolNotFound)
    }

    fn compute_cache_key(&self, token_in: H256, token_out: H256, amount: U256) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        hasher.update(&token_in.0);
        hasher.update(&token_out.0);
        hasher.update(&amount.to_little_endian());
        
        H256::from_slice(&hasher.finalize())
    }

    async fn get_cached_route(&self, key: &H256) -> Option<CachedRoute> {
        let mut cache = self.route_cache.write().await;
        if let Some(cached) = cache.get_mut(key) {
            cached.hit_count += 1;
            return Some(cached.clone());
        }
        None
    }

    async fn cache_route(&self, key: H256, route: Route) {
        let mut cache = self.route_cache.write().await;
        
        if cache.len() > 1000 {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, v)| v.timestamp)
                .map(|(k, _)| *k);
            
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }

        cache.insert(key, CachedRoute {
            route,
            timestamp: self.get_current_timestamp(),
            hit_count: 0,
        });
    }

    async fn is_route_valid(&self, route: &Route) -> bool {
        for hop in &route.path {
            if let Ok(pool) = self.get_pool_info(hop.pool_address).await {
                if pool.liquidity < hop.amount_in {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub async fn register_pool(&self, pool: PoolInfo) {
        let mut registry = self.pool_registry.write().await;
        registry.insert(pool.address, pool);
    }
}

#[derive(Debug)]
pub enum RoutingError {
    NoRouteFound,
    NoPoolsAvailable,
    PoolNotFound,
    InsufficientLiquidity,
    ExcessivePriceImpact,
    PathTooLong,
    GasOptimizationFailed,
}

impl std::error::Error for RoutingError {}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            RoutingError::NoRouteFound => write!(f, "No route found"),
            RoutingError::NoPoolsAvailable => write!(f, "No pools available"),
            RoutingError::PoolNotFound => write!(f, "Pool not found"),
            RoutingError::InsufficientLiquidity => write!(f, "Insufficient liquidity"),
            RoutingError::ExcessivePriceImpact => write!(f, "Excessive price impact"),
            RoutingError::PathTooLong => write!(f, "Path too long"),
            RoutingError::GasOptimizationFailed => write!(f, "Gas optimization failed"),
        }
    }
}