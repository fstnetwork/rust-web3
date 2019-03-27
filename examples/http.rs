extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::timer::Delay;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let http = web3::transports::Http::new("http://localhost:8545").unwrap();
    let web3 = web3::Web3::new(http.clone());

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
        let http = http.clone();

        Delay::new(std::time::Instant::now() + std::time::Duration::from_secs(1)).then(|_| {
            http.close();
            Ok(())
        })
    });

    runtime.block_on(http).unwrap();
}
