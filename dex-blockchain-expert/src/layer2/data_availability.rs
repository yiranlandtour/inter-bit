use super::{DataAvailabilityConfig, Layer2Error, StorageType};
use primitive_types::H256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct DataAvailabilityLayer {
    config: DataAvailabilityConfig,
    storage: Arc<RwLock<HashMap<H256, StoredData>>>,
    erasure_coder: Option<ErasureCoder>,
    compressor: Option<Compressor>,
}

#[derive(Clone)]
struct StoredData {
    data: Vec<u8>,
    metadata: DataMetadata,
    shards: Vec<DataShard>,
}

#[derive(Clone, Serialize, Deserialize)]
struct DataMetadata {
    original_size: usize,
    compressed_size: Option<usize>,
    shard_count: usize,
    storage_locations: Vec<String>,
    timestamp: u64,
}

#[derive(Clone)]
struct DataShard {
    index: usize,
    data: Vec<u8>,
    parity: bool,
}

struct ErasureCoder {
    data_shards: usize,
    parity_shards: usize,
}

struct Compressor {
    compression_level: u32,
}

impl DataAvailabilityLayer {
    pub fn new(config: DataAvailabilityConfig) -> Self {
        let erasure_coder = if config.erasure_coding_enabled {
            Some(ErasureCoder {
                data_shards: 4,
                parity_shards: 2,
            })
        } else {
            None
        };

        let compressor = if config.compression_enabled {
            Some(Compressor {
                compression_level: 6,
            })
        } else {
            None
        };

        Self {
            config,
            storage: Arc::new(RwLock::new(HashMap::new())),
            erasure_coder,
            compressor,
        }
    }

    pub async fn store_data(&self, id: H256, data: Vec<u8>) -> Result<(), Layer2Error> {
        let original_size = data.len();
        
        let compressed_data = if let Some(compressor) = &self.compressor {
            let compressed = compressor.compress(&data)?;
            compressed
        } else {
            data.clone()
        };

        let shards = if let Some(coder) = &self.erasure_coder {
            coder.encode(&compressed_data)?
        } else {
            vec![DataShard {
                index: 0,
                data: compressed_data.clone(),
                parity: false,
            }]
        };

        let storage_locations = self.distribute_shards(&shards).await?;

        let metadata = DataMetadata {
            original_size,
            compressed_size: if self.compressor.is_some() {
                Some(compressed_data.len())
            } else {
                None
            },
            shard_count: shards.len(),
            storage_locations,
            timestamp: self.get_current_timestamp(),
        };

        let stored = StoredData {
            data: compressed_data,
            metadata,
            shards,
        };

        let mut storage = self.storage.write().await;
        storage.insert(id, stored);

        Ok(())
    }

    pub async fn retrieve_data(&self, id: H256) -> Result<Vec<u8>, Layer2Error> {
        let storage = self.storage.read().await;
        let stored = storage.get(&id).ok_or(Layer2Error::DataNotAvailable)?;

        let data = if self.erasure_coder.is_some() {
            let min_shards = stored.shards.len() * 2 / 3;
            let available_shards = self.collect_shards(&stored.shards, min_shards).await?;
            
            if let Some(coder) = &self.erasure_coder {
                coder.decode(&available_shards)?
            } else {
                stored.data.clone()
            }
        } else {
            stored.data.clone()
        };

        let decompressed = if self.compressor.is_some() {
            if let Some(compressor) = &self.compressor {
                compressor.decompress(&data)?
            } else {
                data
            }
        } else {
            data
        };

        Ok(decompressed)
    }

    pub async fn verify_availability(&self, id: H256) -> bool {
        let storage = self.storage.read().await;
        
        if let Some(stored) = storage.get(&id) {
            let required_shards = if self.erasure_coder.is_some() {
                stored.shards.len() * 2 / 3
            } else {
                1
            };

            self.check_shard_availability(&stored.shards, required_shards).await
        } else {
            false
        }
    }

    async fn distribute_shards(&self, shards: &[DataShard]) -> Result<Vec<String>, Layer2Error> {
        let mut locations = Vec::new();

        for (i, shard) in shards.iter().enumerate() {
            let location = match self.config.storage_type {
                StorageType::OnChain => {
                    format!("onchain://shard/{}", i)
                }
                StorageType::IPFS => {
                    let cid = self.store_to_ipfs(&shard.data).await?;
                    format!("ipfs://{}", cid)
                }
                StorageType::Celestia => {
                    let namespace = self.store_to_celestia(&shard.data).await?;
                    format!("celestia://{}", namespace)
                }
                StorageType::EigenDA => {
                    let blob_id = self.store_to_eigenda(&shard.data).await?;
                    format!("eigenda://{}", blob_id)
                }
                StorageType::Custom => {
                    format!("custom://shard/{}", i)
                }
            };

            locations.push(location);
        }

        Ok(locations)
    }

    async fn collect_shards(
        &self,
        shards: &[DataShard],
        min_required: usize,
    ) -> Result<Vec<DataShard>, Layer2Error> {
        let mut collected = Vec::new();

        for shard in shards {
            if self.is_shard_available(shard).await {
                collected.push(shard.clone());
                
                if collected.len() >= min_required {
                    break;
                }
            }
        }

        if collected.len() < min_required {
            Err(Layer2Error::DataNotAvailable)
        } else {
            Ok(collected)
        }
    }

    async fn check_shard_availability(&self, shards: &[DataShard], required: usize) -> bool {
        let mut available = 0;
        
        for shard in shards {
            if self.is_shard_available(shard).await {
                available += 1;
                
                if available >= required {
                    return true;
                }
            }
        }

        false
    }

    async fn is_shard_available(&self, _shard: &DataShard) -> bool {
        true
    }

    async fn store_to_ipfs(&self, _data: &[u8]) -> Result<String, Layer2Error> {
        Ok("QmExample".to_string())
    }

    async fn store_to_celestia(&self, _data: &[u8]) -> Result<String, Layer2Error> {
        Ok("namespace-example".to_string())
    }

    async fn store_to_eigenda(&self, _data: &[u8]) -> Result<String, Layer2Error> {
        Ok("blob-example".to_string())
    }

    fn get_current_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

impl ErasureCoder {
    fn encode(&self, data: &[u8]) -> Result<Vec<DataShard>, Layer2Error> {
        let shard_size = (data.len() + self.data_shards - 1) / self.data_shards;
        let mut shards = Vec::new();

        for i in 0..self.data_shards {
            let start = i * shard_size;
            let end = std::cmp::min(start + shard_size, data.len());
            
            shards.push(DataShard {
                index: i,
                data: data[start..end].to_vec(),
                parity: false,
            });
        }

        for i in 0..self.parity_shards {
            let parity_data = self.generate_parity(&shards, i);
            shards.push(DataShard {
                index: self.data_shards + i,
                data: parity_data,
                parity: true,
            });
        }

        Ok(shards)
    }

    fn decode(&self, shards: &[DataShard]) -> Result<Vec<u8>, Layer2Error> {
        let mut data_shards: Vec<_> = shards.iter()
            .filter(|s| !s.parity)
            .collect();
        
        data_shards.sort_by_key(|s| s.index);

        let mut result = Vec::new();
        for shard in data_shards {
            result.extend_from_slice(&shard.data);
        }

        Ok(result)
    }

    fn generate_parity(&self, data_shards: &[DataShard], parity_index: usize) -> Vec<u8> {
        if data_shards.is_empty() {
            return Vec::new();
        }

        let max_len = data_shards.iter().map(|s| s.data.len()).max().unwrap_or(0);
        let mut parity = vec![0u8; max_len];

        for shard in data_shards {
            for (i, &byte) in shard.data.iter().enumerate() {
                parity[i] ^= byte.rotate_left(parity_index as u32);
            }
        }

        parity
    }
}

impl Compressor {
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>, Layer2Error> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(self.compression_level));
        encoder.write_all(data).map_err(|_| Layer2Error::CompressionError)?;
        encoder.finish().map_err(|_| Layer2Error::CompressionError)
    }

    fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, Layer2Error> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)
            .map_err(|_| Layer2Error::CompressionError)?;
        
        Ok(decompressed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_data_storage_retrieval() {
        let config = DataAvailabilityConfig {
            storage_type: StorageType::OnChain,
            compression_enabled: true,
            erasure_coding_enabled: true,
            replication_factor: 3,
        };

        let da_layer = DataAvailabilityLayer::new(config);
        
        let data = vec![1u8; 1000];
        let id = H256::random();
        
        let store_result = da_layer.store_data(id, data.clone()).await;
        assert!(store_result.is_ok());
        
        let retrieve_result = da_layer.retrieve_data(id).await;
        assert!(retrieve_result.is_ok());
        
        let retrieved = retrieve_result.unwrap();
        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_availability_check() {
        let config = DataAvailabilityConfig {
            storage_type: StorageType::OnChain,
            compression_enabled: false,
            erasure_coding_enabled: false,
            replication_factor: 1,
        };

        let da_layer = DataAvailabilityLayer::new(config);
        
        let data = vec![42u8; 100];
        let id = H256::random();
        
        da_layer.store_data(id, data).await.unwrap();
        
        let available = da_layer.verify_availability(id).await;
        assert!(available);
        
        let unavailable_id = H256::random();
        let not_available = da_layer.verify_availability(unavailable_id).await;
        assert!(!not_available);
    }
}