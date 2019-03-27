extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use tokio::runtime::Runtime;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let ws = web3::transports::WebSocket::new("ws://127.0.0.1:8546").unwrap();
    let web3 = web3::Web3::new(ws.clone());

    runtime.spawn(web3.eth().block_number().then(|block_number| {
        println!("Block number: {:?}", block_number);
        Ok(())
    }));
    runtime.spawn(web3.eth().accounts().then(|accounts| {
        println!("Accounts: {:?}", accounts);
        Ok(())
    }));

    for _ in 0..3 {
        runtime.spawn(web3.eth().block_number().then(|block_number| {
            println!("Block number: {:?}", block_number);
            Ok(())
        }));
    }

    runtime.block_on(ws).unwrap();
}
