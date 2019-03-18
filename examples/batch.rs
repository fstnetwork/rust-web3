extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use tokio::runtime::Runtime;

const MAX_PARALLEL_REQUESTS: usize = 64;

fn main() {
    let mut runtime = Runtime::new().unwrap();
    let executor = runtime.executor();

    let http = web3::transports::Http::with_executor(
        "http://localhost:8545",
        &executor,
        MAX_PARALLEL_REQUESTS,
    )
    .unwrap();

    let web3 = web3::Web3::new(web3::transports::Batch::new(http));
    let _ = web3.eth().accounts();

    let block = web3.eth().block_number().then(|block| {
        println!("Best Block: {:?}", block);
        Ok(())
    });

    let result = web3.transport().submit_batch().map(|accounts| {
        println!("Result: {:?}", accounts);
    });

    executor.spawn(block);
    runtime.block_on(result).unwrap();
}
