extern crate tokio;
extern crate web3;

use tokio::runtime::Runtime;
use web3::futures::Future;

fn main() {
    let mut runtime = Runtime::new().unwrap();

    let ipc = web3::transports::Ipc::with_executor(
        "./jsonrpc.ipc",
        &runtime.executor(),
        &runtime.reactor(),
    )
    .unwrap();
    let web3 = web3::Web3::new(ipc);
    println!("Calling accounts.");

    let future = web3.eth().accounts().map(|accounts| {
        println!("Accounts: {:?}", accounts);
    });

    runtime.block_on(future).unwrap();
}
