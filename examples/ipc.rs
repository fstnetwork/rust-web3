extern crate futures;
extern crate tokio;
extern crate web3;

use futures::Future;
use tokio::runtime::Runtime;
use tokio_uds::UnixStream;

fn main() {
    let mut runtime = Runtime::new().unwrap();
    let handle = runtime.reactor();

    // let stream = {
    //     let std_stream = std::os::unix::net::UnixStream::connect("./jsonrpc.ipc").unwrap();
    //     UnixStream::from_std(std_stream, handle).unwrap()
    // };

    // let ipc = web3::transports::Ipc::with_stream(stream).unwrap();
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

    runtime.block_on(ipc).unwrap();
}
