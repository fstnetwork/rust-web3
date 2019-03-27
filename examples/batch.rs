extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use tokio::runtime::Runtime;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let http = web3::transports::Http::new("http://localhost:8545").unwrap();
    let web3 = web3::Web3::new(web3::transports::Batch::new(http.clone()));

    runtime.spawn(web3.eth().accounts().then(|accounts| {
        println!("Accounts: {:?}", accounts);
        Ok(())
    }));

    runtime.spawn(web3.eth().block_number().then(|block| {
        println!("Best Block: {:?}", block);
        Ok(())
    }));

    runtime.spawn(web3.transport().submit_batch().then(|results| {
        println!("Result: {:?}", results);
        Ok(())
    }));

    runtime.block_on(http).unwrap();
}
