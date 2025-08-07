// Test compilation of specific modules without rocksdb
#[cfg(test)]
mod test {
    // Test basic module compilation
    use dex_blockchain_expert::state_machine::transaction::{Transaction, TransactionSignature};
    use dex_blockchain_expert::consensus::tendermint::{Block, Transaction as ConsensusTransaction};
    use dex_blockchain_expert::monitoring::{MonitoringSystem, Span, SerializableSpan};
    use primitive_types::{H256, U256};
    
    #[test]
    fn test_transaction_conversion() {
        let tx = Transaction::new(
            [0; 20],
            Some([1; 20]),
            1000,
            vec![],
            0,
            21000,
            1,
        );
        
        // Test that transaction can be hashed
        let _hash = tx.hash();
    }
    
    #[test]
    fn test_consensus_transaction() {
        let tx = ConsensusTransaction {
            nonce: 0,
            from: [0; 20],
            to: Some([1; 20]),
            value: 1000,
            data: vec![],
            gas_limit: 21000,
            gas_price: 1,
            signature: vec![0u8; 65],
        };
        
        // Test that consensus transaction can be hashed
        let _hash = tx.hash();
    }
    
    #[test]
    fn test_monitoring_span() {
        let span = Span {
            trace_id: "test".to_string(),
            span_id: "span1".to_string(),
            parent_id: None,
            operation: "test_op".to_string(),
            start_time: std::time::Instant::now(),
            duration: None,
            tags: std::collections::HashMap::new(),
        };
        
        // Test conversion to serializable span
        let _serializable: SerializableSpan = (&span).into();
    }
    
    #[test]
    fn test_block_hash() {
        let block = Block {
            height: 1,
            timestamp: 123456789,
            previous_hash: [0; 32],
            state_root: [0; 32],
            transactions: vec![],
            proposer: vec![1, 2, 3],
        };
        
        // Test that block can be hashed
        let _hash = block.hash();
    }
}