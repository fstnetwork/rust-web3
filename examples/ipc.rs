extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::timer::Delay;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let ipc = web3::transports::Ipc::new("./jsonrpc.ipc").unwrap();
    let web3 = web3::Web3::new(ipc.clone());
    println!("Calling accounts.");

    runtime.spawn(web3.eth().block_number().then(|block_number| {
        println!("Block number: {:?}", block_number);
        Ok(())
    }));
    runtime.spawn(web3.eth().accounts().then(|accounts| {
        println!("Accounts: {:?}", accounts);
        Ok(())
    }));

    let f = web3.eth().block_number().then(|block_number| {
        println!("Block number: {:?}", block_number);
        Ok(())
    });

    runtime.spawn(Delay::new(Instant::now() + Duration::from_millis(200)).then(|_| f));

    runtime.spawn({
        let ipc = ipc.clone();

        Delay::new(std::time::Instant::now() + std::time::Duration::from_secs(1)).then(|_| {
            ipc.close();
            Ok(())
        })
    });

    runtime.block_on(ipc).unwrap();
}
