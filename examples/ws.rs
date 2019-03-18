extern crate futures;
extern crate tokio;
extern crate web3;

use tokio::runtime::Runtime;

use futures::Future;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let ws = web3::transports::WebSocket::with_executor(
        "ws://localhost:8546",
        &runtime.executor(),
    )
    .unwrap();
    let web3 = web3::Web3::new(ws);
    let future = web3.eth().block_number().map(|block_number| {
        println!("Block number: {:?}", block_number);
    });

    runtime.block_on(future).unwrap();
}
