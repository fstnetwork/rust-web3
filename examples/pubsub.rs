extern crate futures;
extern crate tokio;
extern crate web3;

use futures::{Future, Stream};
use tokio::runtime::Runtime;

fn main() {
    let runtime = Runtime::new().unwrap();
    let ws = web3::transports::WebSocket::new(
        "ws://localhost:8546",
        &runtime.executor(),
    )
    .unwrap();
    let web3 = web3::Web3::new(ws.clone());
    let mut sub = web3.eth_subscribe().subscribe_new_heads().wait().unwrap();

    println!("Got subscription id: {:?}", sub.id());

    (&mut sub)
        .take(5)
        .for_each(|x| {
            println!("Got: {:?}", x);
            Ok(())
        })
        .wait()
        .unwrap();

    sub.unsubscribe();

    drop(web3);
}
