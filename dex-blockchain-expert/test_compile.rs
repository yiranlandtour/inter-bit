// Simple test to check basic compilation
use dex_blockchain_expert::dex::smart_routing::Protocol;

fn main() {
    let _protocol = Protocol::AmmV2;
    println!("Protocol enum compiles: {:?}", _protocol);
}