extern crate futures;
extern crate tokio;
extern crate web3;

use futures::{Future, Stream};
use tokio::runtime::Runtime;

fn main() {
    let mut runtime = Runtime::new().unwrap();
    let ws = web3::transports::WebSocket::new("ws://127.0.0.1:8546").unwrap();
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

    runtime.block_on(ws);
    drop(web3);
}
