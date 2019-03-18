extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use tokio::runtime::Runtime;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let http = web3::transports::Http::new(
        "http://localhost:8545",
        &runtime.executor(),
    )
    .unwrap();

    let web3 = web3::Web3::new(http);
    let future = web3.eth().block_number().map(|block_number| {
        println!("Block number: {:?}", block_number);
    });

    runtime.block_on(future).unwrap();
}
