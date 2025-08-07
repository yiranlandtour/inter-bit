use super::{PoolInfo, Protocol, RoutingError};
use primitive_types::{H256, U256};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

pub struct PathFinder {
    max_hops: usize,
}

#[derive(Clone, Eq, PartialEq)]
struct Node {
    token: H256,
    cost: u128,
    path: Vec<H256>,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.cmp(&self.cost)
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PathFinder {
    pub fn new(max_hops: usize) -> Self {
        Self { max_hops }
    }

    pub async fn find_all_paths(
        &self,
        token_in: H256,
        token_out: H256,
        pools: &[PoolInfo],
        max_hops: usize,
    ) -> Result<Vec<Vec<H256>>, RoutingError> {
        let graph = self.build_graph(pools);
        
        let mut all_paths = Vec::new();
        
        all_paths.extend(self.dijkstra(&graph, token_in, token_out, max_hops)?);
        
        all_paths.extend(self.bfs_paths(&graph, token_in, token_out, max_hops)?);
        
        all_paths.extend(self.dfs_paths(&graph, token_in, token_out, max_hops, 5)?);
        
        self.deduplicate_paths(all_paths)
    }

    pub async fn find_shortest_path(
        &self,
        token_in: H256,
        token_out: H256,
        pools: &[PoolInfo],
    ) -> Result<Vec<H256>, RoutingError> {
        let graph = self.build_graph(pools);
        
        let paths = self.dijkstra(&graph, token_in, token_out, self.max_hops)?;
        
        paths.into_iter()
            .next()
            .ok_or(RoutingError::NoRouteFound)
    }

    fn dijkstra(
        &self,
        graph: &HashMap<H256, Vec<Edge>>,
        start: H256,
        end: H256,
        max_hops: usize,
    ) -> Result<Vec<Vec<H256>>, RoutingError> {
        let mut distances = HashMap::new();
        let mut heap = BinaryHeap::new();
        let mut paths = Vec::new();

        distances.insert(start, 0u128);
        heap.push(Node {
            token: start,
            cost: 0,
            path: vec![start],
        });

        while let Some(Node { token, cost, path }) = heap.pop() {
            if token == end && path.len() <= max_hops + 1 {
                paths.push(path);
                if paths.len() >= 3 {
                    break;
                }
                continue;
            }

            if path.len() > max_hops {
                continue;
            }

            if let Some(&d) = distances.get(&token) {
                if cost > d {
                    continue;
                }
            }

            if let Some(edges) = graph.get(&token) {
                for edge in edges {
                    let next_cost = cost + edge.weight;
                    
                    if !path.contains(&edge.to) {
                        let mut next_path = path.clone();
                        next_path.push(edge.to);
                        
                        if let Some(&d) = distances.get(&edge.to) {
                            if next_cost >= d && edge.to != end {
                                continue;
                            }
                        }
                        
                        distances.insert(edge.to, next_cost);
                        heap.push(Node {
                            token: edge.to,
                            cost: next_cost,
                            path: next_path,
                        });
                    }
                }
            }
        }

        if paths.is_empty() {
            Err(RoutingError::NoRouteFound)
        } else {
            Ok(paths)
        }
    }

    fn bfs_paths(
        &self,
        graph: &HashMap<H256, Vec<Edge>>,
        start: H256,
        end: H256,
        max_hops: usize,
    ) -> Result<Vec<Vec<H256>>, RoutingError> {
        let mut queue = VecDeque::new();
        let mut paths = Vec::new();
        
        queue.push_back((start, vec![start]));

        while let Some((current, path)) = queue.pop_front() {
            if current == end {
                paths.push(path);
                if paths.len() >= 3 {
                    break;
                }
                continue;
            }

            if path.len() > max_hops {
                continue;
            }

            if let Some(edges) = graph.get(&current) {
                for edge in edges {
                    if !path.contains(&edge.to) {
                        let mut new_path = path.clone();
                        new_path.push(edge.to);
                        queue.push_back((edge.to, new_path));
                    }
                }
            }
        }

        Ok(paths)
    }

    fn dfs_paths(
        &self,
        graph: &HashMap<H256, Vec<Edge>>,
        start: H256,
        end: H256,
        max_hops: usize,
        max_paths: usize,
    ) -> Result<Vec<Vec<H256>>, RoutingError> {
        let mut all_paths = Vec::new();
        let mut current_path = vec![start];
        let mut visited = HashSet::new();
        
        self.dfs_helper(
            graph,
            start,
            end,
            &mut current_path,
            &mut visited,
            &mut all_paths,
            max_hops,
            max_paths,
        );

        Ok(all_paths)
    }

    fn dfs_helper(
        &self,
        graph: &HashMap<H256, Vec<Edge>>,
        current: H256,
        end: H256,
        path: &mut Vec<H256>,
        visited: &mut HashSet<H256>,
        all_paths: &mut Vec<Vec<H256>>,
        max_hops: usize,
        max_paths: usize,
    ) {
        if all_paths.len() >= max_paths {
            return;
        }

        if current == end {
            all_paths.push(path.clone());
            return;
        }

        if path.len() > max_hops {
            return;
        }

        visited.insert(current);

        if let Some(edges) = graph.get(&current) {
            for edge in edges {
                if !visited.contains(&edge.to) {
                    path.push(edge.to);
                    self.dfs_helper(
                        graph,
                        edge.to,
                        end,
                        path,
                        visited,
                        all_paths,
                        max_hops,
                        max_paths,
                    );
                    path.pop();
                }
            }
        }

        visited.remove(&current);
    }

    fn build_graph(&self, pools: &[PoolInfo]) -> HashMap<H256, Vec<Edge>> {
        let mut graph: HashMap<H256, Vec<Edge>> = HashMap::new();

        for pool in pools {
            let weight = self.calculate_edge_weight(pool);
            
            graph.entry(pool.token_a)
                .or_insert_with(Vec::new)
                .push(Edge {
                    to: pool.token_b,
                    pool: pool.address,
                    weight,
                    liquidity: pool.liquidity,
                });

            graph.entry(pool.token_b)
                .or_insert_with(Vec::new)
                .push(Edge {
                    to: pool.token_a,
                    pool: pool.address,
                    weight,
                    liquidity: pool.liquidity,
                });
        }

        for edges in graph.values_mut() {
            edges.sort_by_key(|e| e.weight);
        }

        graph
    }

    fn calculate_edge_weight(&self, pool: &PoolInfo) -> u128 {
        let liquidity_score = if pool.liquidity > U256::zero() {
            U256::from(10u64.pow(18)) / pool.liquidity
        } else {
            U256::from(u128::MAX)
        };

        let fee_multiplier = match pool.protocol {
            Protocol::OrderBook => 100,
            Protocol::AmmV2 => 150,
            Protocol::AmmV3 => 120,
            Protocol::StableSwap => 80,
            Protocol::ConcentratedLiquidity => 110,
            Protocol::RfqSystem => 90,
        };

        let base_weight = liquidity_score.as_u128().saturating_add(pool.fee_tier as u128);
        
        base_weight.saturating_mul(fee_multiplier) / 100
    }

    fn deduplicate_paths(&self, paths: Vec<Vec<H256>>) -> Result<Vec<Vec<H256>>, RoutingError> {
        let mut unique_paths = HashSet::new();
        let mut result = Vec::new();

        for path in paths {
            let path_hash = self.hash_path(&path);
            if unique_paths.insert(path_hash) {
                result.push(path);
            }
        }

        if result.is_empty() {
            Err(RoutingError::NoRouteFound)
        } else {
            result.sort_by_key(|p| p.len());
            Ok(result)
        }
    }

    fn hash_path(&self, path: &[H256]) -> H256 {
        use sha3::{Digest, Keccak256};
        
        let mut hasher = Keccak256::new();
        for token in path {
            hasher.update(&token.0);
        }
        
        H256::from_slice(&hasher.finalize())
    }

    pub fn validate_path(&self, path: &[H256]) -> bool {
        if path.len() < 2 || path.len() > self.max_hops + 1 {
            return false;
        }

        let mut seen = HashSet::new();
        for token in path {
            if !seen.insert(token) {
                return false;
            }
        }

        true
    }
}

#[derive(Clone)]
struct Edge {
    to: H256,
    pool: H256,
    weight: u128,
    liquidity: U256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pathfinding() {
        let pathfinder = PathFinder::new(4);
        
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

        let paths = pathfinder.find_all_paths(
            H256::from_low_u64_be(100),
            H256::from_low_u64_be(300),
            &pools,
            3,
        ).await;

        assert!(paths.is_ok());
        let paths = paths.unwrap();
        assert!(!paths.is_empty());
        assert_eq!(paths[0].len(), 3);
    }

    #[test]
    fn test_path_validation() {
        let pathfinder = PathFinder::new(4);
        
        let valid_path = vec![
            H256::from_low_u64_be(1),
            H256::from_low_u64_be(2),
            H256::from_low_u64_be(3),
        ];
        assert!(pathfinder.validate_path(&valid_path));
        
        let invalid_path = vec![
            H256::from_low_u64_be(1),
            H256::from_low_u64_be(2),
            H256::from_low_u64_be(1),
        ];
        assert!(!pathfinder.validate_path(&invalid_path));
        
        let too_long = vec![
            H256::from_low_u64_be(1),
            H256::from_low_u64_be(2),
            H256::from_low_u64_be(3),
            H256::from_low_u64_be(4),
            H256::from_low_u64_be(5),
            H256::from_low_u64_be(6),
        ];
        assert!(!pathfinder.validate_path(&too_long));
    }
}