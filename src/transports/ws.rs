//! WebSocket Transport

extern crate websocket;

use futures::sync::{mpsc, oneshot};
use futures::{self, Async, Future, IntoFuture, Poll, Sink, Stream};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::{atomic, Arc};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::timer::Delay;
use websocket::client::r#async::{Client, ClientNew};
use websocket::url::Url;
use websocket::{ClientBuilder, OwnedMessage};

use super::api::SubscriptionId;
use super::error::{Error, ErrorKind};
use super::helpers;
use super::transports::shared::Response;
use super::transports::Result;
use super::{BatchTransport, DuplexTransport, RequestId, Transport};

impl From<websocket::WebSocketError> for Error {
    fn from(err: websocket::WebSocketError) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

impl From<websocket::client::ParseError> for Error {
    fn from(err: websocket::client::ParseError) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

type Pending = oneshot::Sender<Result<Vec<Result<rpc::Value>>>>;

type Subscription = mpsc::UnboundedSender<rpc::Value>;

/// A future representing pending WebSocket request, resolves to a response.
pub type WsTask<F> = Response<F, Vec<Result<rpc::Value>>>;

/// WebSocket transport
pub struct WebSocket {
    inner: Inner,
    retry_interval: Duration,

    id: Arc<atomic::AtomicUsize>,
    url: Url,
    pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
    subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,

    write_sender: mpsc::UnboundedSender<OwnedMessage>,
    write_receiver: Option<mpsc::UnboundedReceiver<OwnedMessage>>,

    shutdown_sender: mpsc::UnboundedSender<()>,
    shutdown_receiver: Option<mpsc::UnboundedReceiver<()>>,
}

impl WebSocket {
    /// Create new WebSocket transport
    pub fn new(url: &str) -> Result<Self> {
        trace!("Connecting to: {:?}", url);

        let url: Url = url.parse()?;
        let pending: Arc<Mutex<BTreeMap<RequestId, Pending>>> = Default::default();
        let subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>> = Default::default();
        let (write_sender, write_receiver) = mpsc::unbounded();
        let (shutdown_sender, shutdown_receiver) = mpsc::unbounded();

        Ok(WebSocket {
            retry_interval: Duration::from_millis(200),
            inner: Inner::connect(&url),

            id: Arc::new(atomic::AtomicUsize::new(1)),
            url,
            pending,
            subscriptions,

            write_sender,
            write_receiver: Some(write_receiver),

            shutdown_sender,
            shutdown_receiver: Some(shutdown_receiver),
        })
    }

    /// Close WebSocket transport
    pub fn close(self) {
        if !self.shutdown_sender.is_closed() {
            self.shutdown_sender
                .unbounded_send(())
                .expect("channel is alive");
        }
    }

    fn send_request<F, O>(&self, id: RequestId, request: rpc::Request, extract: F) -> WsTask<F>
    where
        F: Fn(Vec<Result<rpc::Value>>) -> O,
    {
        let request = helpers::to_string(&request);
        debug!("[{}] Calling: {}", id, request);
        let (tx, rx) = futures::oneshot();
        self.pending.lock().insert(id, tx);

        let result = self
            .write_sender
            .unbounded_send(OwnedMessage::Text(request))
            .map_err(|_| ErrorKind::Transport("Error sending request".into()).into());

        Response::new(id, result, rx, extract)
    }
}

impl Transport for WebSocket {
    type Out = WsTask<fn(Vec<Result<rpc::Value>>) -> Result<rpc::Value>>;

    fn prepare(&self, method: &str, params: Vec<rpc::Value>) -> (RequestId, rpc::Call) {
        let id = self.id.fetch_add(1, atomic::Ordering::AcqRel);
        let request = helpers::build_request(id, method, params);

        (id, request)
    }

    fn send(&self, id: RequestId, request: rpc::Call) -> Self::Out {
        self.send_request(id, rpc::Request::Single(request), |response| match response
            .into_iter()
            .next()
        {
            Some(res) => res,
            None => Err(ErrorKind::InvalidResponse("Expected single, got batch.".into()).into()),
        })
    }
}

impl BatchTransport for WebSocket {
    type Batch = WsTask<fn(Vec<Result<rpc::Value>>) -> Result<Vec<Result<rpc::Value>>>>;

    fn send_batch<T>(&self, requests: T) -> Self::Batch
    where
        T: IntoIterator<Item = (RequestId, rpc::Call)>,
    {
        let mut it = requests.into_iter();
        let (id, first) = it
            .next()
            .map(|x| (x.0, Some(x.1)))
            .unwrap_or_else(|| (0, None));
        let requests = first.into_iter().chain(it.map(|x| x.1)).collect();
        self.send_request(id, rpc::Request::Batch(requests), Ok)
    }
}

impl DuplexTransport for WebSocket {
    type NotificationStream = Box<Stream<Item = rpc::Value, Error = Error> + Send + 'static>;

    fn subscribe(&self, id: &SubscriptionId) -> Self::NotificationStream {
        let (tx, rx) = mpsc::unbounded();
        if self.subscriptions.lock().insert(id.clone(), tx).is_some() {
            warn!("Replacing already-registered subscription with id {:?}", id)
        }
        Box::new(rx.map_err(|()| ErrorKind::Transport("No data available".into()).into()))
    }

    fn unsubscribe(&self, id: &SubscriptionId) {
        self.subscriptions.lock().remove(id);
    }
}

impl Future for WebSocket {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            if let Some(ref mut shutdown_receiver) = self.shutdown_receiver {
                match shutdown_receiver.poll() {
                    Ok(Async::Ready(_)) => {
                        self.inner = Inner::empty();
                    }
                    Ok(Async::NotReady) => {}
                    _ => {
                        unreachable!();
                    }
                }
            }

            match &mut self.inner {
                Inner::Empty => return Ok(Async::Ready(())),
                Inner::WaitForConnect { ref mut delay } => match delay.poll() {
                    Ok(Async::Ready(_)) => {
                        self.inner = Inner::connect(&self.url);
                    }
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    _ => return Ok(Async::NotReady),
                },
                Inner::Connecting { ref mut connector } => match connector.poll() {
                    Ok(Async::Ready((client, _))) => {
                        let write_receiver = std::mem::replace(&mut self.write_receiver, None);
                        self.inner = Inner::new_stream(
                            client,
                            self.pending.clone(),
                            self.subscriptions.clone(),
                            self.write_sender.clone(),
                            write_receiver.expect("write_receiver must be some; qed"),
                        );
                    }
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    Err(err) => {
                        warn!("error: {:?}, retry after {:?}", err, self.retry_interval);
                        self.inner = Inner::wait(self.retry_interval);
                    }
                },
                Inner::Connected { ref mut client } => match client.poll() {
                    Ok(Async::Ready(write_receiver)) => {
                        debug_assert!(self.write_receiver.is_none());
                        self.write_receiver = Some(write_receiver);
                        self.inner = Inner::wait(self.retry_interval);
                    }
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    Err(_err) => {}
                },
            }
        }
    }
}

impl ::std::fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "WebSocket {{ id: {:?}, url: {:?}, pending: {:?}, subscriptions: {:?}, write_sender: {:?} }}", self.id, self.url, self.pending, self.subscriptions, self.write_sender)
    }
}

impl Clone for WebSocket {
    fn clone(&self) -> WebSocket {
        WebSocket {
            retry_interval: self.retry_interval,
            inner: Inner::empty(),

            id: self.id.clone(),
            url: self.url.clone(),
            pending: self.pending.clone(),
            subscriptions: self.subscriptions.clone(),

            write_receiver: None,
            write_sender: self.write_sender.clone(),

            shutdown_sender: self.shutdown_sender.clone(),
            shutdown_receiver: None,
        }
    }
}

enum Inner {
    Empty,
    WaitForConnect {
        delay: Delay,
    },
    Connecting {
        connector: ClientNew<TcpStream>,
    },
    Connected {
        client: Box<Future<Item = mpsc::UnboundedReceiver<OwnedMessage>, Error = ()> + Send>,
    },
}

impl Inner {
    fn empty() -> Self {
        Inner::Empty
    }

    fn wait(delay: Duration) -> Self {
        Inner::WaitForConnect {
            delay: Delay::new(Instant::now() + delay),
        }
    }

    fn connect(url: &Url) -> Self {
        Inner::Connecting {
            connector: ClientBuilder::from_url(&url).async_connect_insecure(),
        }
    }

    fn new_stream(
        client: Client<TcpStream>,
        pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
        subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,
        write_sender: mpsc::UnboundedSender<OwnedMessage>,
        write_receiver: mpsc::UnboundedReceiver<OwnedMessage>,
    ) -> Self {
        let (sink, stream) = client.split();

        let reader = stream
            .from_err::<Error>()
            .for_each(move |message| {
                trace!("Message received: {:?}", message);

                match message {
                    OwnedMessage::Close(e) => write_sender
                        .unbounded_send(OwnedMessage::Close(e))
                        .map_err(|_| {
                            ErrorKind::Transport("Error sending close message".into()).into()
                        }),
                    OwnedMessage::Ping(d) => write_sender
                        .unbounded_send(OwnedMessage::Pong(d))
                        .map_err(|_| {
                            ErrorKind::Transport("Error sending pong message".into()).into()
                        }),
                    OwnedMessage::Text(t) => {
                        if let Ok(notification) = helpers::to_notification_from_slice(t.as_bytes())
                        {
                            if let rpc::Params::Map(params) = notification.params {
                                let id = params.get("subscription");
                                let result = params.get("result");

                                if let (Some(&rpc::Value::String(ref id)), Some(result)) =
                                    (id, result)
                                {
                                    let id: SubscriptionId = id.clone().into();
                                    if let Some(stream) = subscriptions.lock().get(&id) {
                                        return stream.unbounded_send(result.clone()).map_err(
                                            |_| {
                                                ErrorKind::Transport(
                                                    "Error sending notification".into(),
                                                )
                                                .into()
                                            },
                                        );
                                    } else {
                                        warn!(
                                            "Got notification for unknown subscription (id: {:?})",
                                            id
                                        );
                                    }
                                } else {
                                    error!("Got unsupported notification (id: {:?})", id);
                                }
                            }

                            return Ok(());
                        }

                        let response = helpers::to_response_from_slice(t.as_bytes());
                        let outputs = match response {
                            Ok(rpc::Response::Single(output)) => vec![output],
                            Ok(rpc::Response::Batch(outputs)) => outputs,
                            _ => vec![],
                        };

                        let id = match outputs.get(0) {
                            Some(&rpc::Output::Success(ref success)) => success.id.clone(),
                            Some(&rpc::Output::Failure(ref failure)) => failure.id.clone(),
                            None => rpc::Id::Num(0),
                        };

                        if let rpc::Id::Num(num) = id {
                            if let Some(request) = pending.lock().remove(&(num as usize)) {
                                trace!("Responding to (id: {:?}) with {:?}", num, outputs);
                                if let Err(err) =
                                    request.send(helpers::to_results_from_outputs(outputs))
                                {
                                    warn!("Sending a response to deallocated channel: {:?}", err);
                                }
                            } else {
                                warn!("Got response for unknown request (id: {:?})", num);
                            }
                        } else {
                            warn!("Got unsupported response (id: {:?})", id);
                        }

                        Ok(())
                    }
                    _ => Ok(()),
                }
            })
            .map(|_| {})
            .map_err(|_| {})
            .into_future();

        let writer = sink
            .sink_from_err::<Error>()
            .send_all(write_receiver.map_err(|_| websocket::WebSocketError::NoDataAvailable))
            .map(|(_, write_receiver)| write_receiver.into_inner())
            .map_err(|_| {})
            .into_future();

        Inner::Connected {
            client: Box::new(
                reader
                    .join(writer)
                    .map(|(_, write_receiver)| write_receiver),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate tokio;
    extern crate websocket;

    use futures::{Future, Sink, Stream};
    use std::time::{Duration, Instant};
    use tokio::timer::Delay;
    use websocket::message::OwnedMessage;
    use websocket::r#async::server::Server;
    use websocket::server::InvalidConnection;

    use super::Transport;
    use super::WebSocket;

    #[test]
    fn should_send_a_request() {
        // given
        let mut runtime = tokio::runtime::Runtime::new().unwrap();
        let server = Server::bind("localhost:3000", &runtime.reactor()).unwrap();
        let f = {
            let executor = runtime.executor();
            server.incoming().take(1).map_err(|InvalidConnection { error, .. }| error).for_each(move |(upgrade, addr)| {
                trace!("Got a connection from {}", addr);
                let f = upgrade.accept().and_then(|(s, _)| {
                    let (sink, stream) = s.split();

                    stream
                        .take_while(|m| Ok(!m.is_close()))
                        .filter_map(|m| match m {
                            OwnedMessage::Ping(p) => Some(OwnedMessage::Pong(p)),
                            OwnedMessage::Pong(_) => None,
                            OwnedMessage::Text(t) => {
                                assert_eq!(t, r#"{"jsonrpc":"2.0","method":"eth_accounts","params":["1"],"id":1}"#);
                                Some(OwnedMessage::Text(r#"{"jsonrpc":"2.0","id":1,"result":"x"}"#.to_owned()))
                            }
                            _ => None,
                        })
                        .forward(sink)
                        .and_then(|(_, sink)| sink.send(OwnedMessage::Close(None)))
                });

                executor.spawn(f.map(|_| ()).map_err(|_| ()));

                Ok(())
            })
        };
        runtime.spawn(f.map_err(|_| ()));

        let ws = WebSocket::new("ws://localhost:3000").unwrap();

        // when
        runtime.spawn(
            ws.execute("eth_accounts", vec![rpc::Value::String("1".into())])
                .then(|v| {
                    assert_eq!(v, Ok(rpc::Value::String("x".into())));
                    Ok(())
                }),
        );

        runtime.spawn({
            let ws = ws.clone();
            Delay::new(Instant::now() + Duration::from_millis(100)).then(|_| {
                ws.close();
                Ok(())
            })
        });

        // then
        runtime.block_on(ws).unwrap();
    }
}
