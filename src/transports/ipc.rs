//! IPC Transport for *nix

#[cfg(unix)]
extern crate tokio_uds;

use futures::{
    self,
    sync::{mpsc, oneshot},
    Async, Future, IntoFuture, Poll, Stream,
};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic, Arc};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, ReadHalf, WriteHalf};
use tokio::timer::Delay;

#[cfg(unix)]
use tokio_uds::{ConnectFuture, UnixStream};

use super::api::SubscriptionId;
use super::error::{Error, ErrorKind};
use super::helpers;
use super::transports::shared::Response;
use super::transports::Result;
use super::{BatchTransport, DuplexTransport, RequestId, Transport};

macro_rules! try_nb {
    ($e:expr) => {
        match $e {
            Ok(t) => t,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Ok(futures::Async::NotReady);
            }
            Err(e) => {
                warn!("Unexpected IO error: {:?}", e);
                return Err(());
            }
        }
    };
}

type Pending = oneshot::Sender<Result<Vec<Result<rpc::Value>>>>;

type Subscription = mpsc::UnboundedSender<rpc::Value>;

/// A future representing pending IPC request, resolves to a response.
pub type IpcTask<F> = Response<F, Vec<Result<rpc::Value>>>;

/// Unix Domain Sockets (IPC) transport
pub struct Ipc {
    inner: Inner,
    retry_interval: Duration,
    path: PathBuf,

    id: Arc<atomic::AtomicUsize>,
    pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
    subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,
    write_receiver: Option<Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>>,
    write_sender: mpsc::UnboundedSender<Vec<u8>>,

    shutdown_sender: mpsc::UnboundedSender<()>,
    shutdown_receiver: Option<mpsc::UnboundedReceiver<()>>,
}

impl Ipc {
    /// IPC is only available on Unix. On other systems, this always returns an error.
    /// Create new IPC transport
    #[cfg(unix)]
    pub fn new<P>(path: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        trace!("Connecting to: {:?}", path.as_ref());
        let (shutdown_sender, shutdown_receiver) = mpsc::unbounded();
        let (write_sender, write_receiver) = mpsc::unbounded();
        let pending: Arc<Mutex<BTreeMap<RequestId, Pending>>> = Default::default();
        let subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>> = Default::default();

        let inner = Inner::connect(path.as_ref());
        let path = PathBuf::from(path.as_ref());

        Ok(Ipc {
            inner,
            retry_interval: Duration::from_millis(200),
            path,

            id: Default::default(),
            pending,
            subscriptions,

            write_sender,
            write_receiver: Some(Arc::new(Mutex::new(write_receiver))),

            shutdown_sender,
            shutdown_receiver: Some(shutdown_receiver),
        })
    }

    /// Creates new IPC transport from existing `UnixStream`
    #[cfg(unix)]
    pub fn with_stream<P>(stream: UnixStream, path: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let (shutdown_sender, shutdown_receiver) = mpsc::unbounded();
        let (write_sender, write_receiver) = mpsc::unbounded();
        let pending: Arc<Mutex<BTreeMap<RequestId, Pending>>> = Default::default();
        let subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>> = Default::default();
        let write_receiver = Arc::new(Mutex::new(write_receiver));

        Ok(Ipc {
            inner: Inner::new_stream(
                stream,
                pending.clone(),
                subscriptions.clone(),
                write_receiver.clone(),
            ),
            path: PathBuf::from(path.as_ref()),
            retry_interval: Duration::from_millis(200),

            id: Default::default(),
            pending,
            subscriptions,

            write_sender,
            write_receiver: Some(write_receiver),

            shutdown_sender,
            shutdown_receiver: Some(shutdown_receiver),
        })
    }

    #[cfg(not(unix))]
    pub fn new<P>(_path: P) -> Result<Self> {
        return Err(ErrorKind::Transport("IPC transport is only supported on Unix".into()).into());
    }

    /// Close IPC transport
    pub fn close(self) {
        if !self.shutdown_sender.is_closed() {
            self.shutdown_sender
                .unbounded_send(())
                .expect("channel is alive");
        }
    }

    fn send_request<F, O>(&self, id: RequestId, request: rpc::Request, extract: F) -> IpcTask<F>
    where
        F: Fn(Vec<Result<rpc::Value>>) -> O,
    {
        let request = helpers::to_string(&request);
        debug!("[{}] Calling: {}", id, request);
        let (tx, rx) = futures::oneshot();
        self.pending.lock().insert(id, tx);

        let result = self
            .write_sender
            .unbounded_send(request.into_bytes())
            .map_err(|_| ErrorKind::Io(io::ErrorKind::BrokenPipe.into()).into());

        Response::new(id, result, rx, extract)
    }
}

impl Future for Ipc {
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
                        info!("Connecting {:?}", self.path);
                        self.inner = Inner::connect(self.path.clone());
                    }
                    _ => {
                        return Ok(Async::NotReady);
                    }
                },
                Inner::Connecting { ref mut connector } => match connector.poll() {
                    Ok(Async::Ready(stream)) => {
                        trace!("{:?} connected!", self.path);
                        debug_assert!(self.write_receiver.is_some());
                        let write_receiver = match self.write_receiver {
                            Some(ref write_receiver) => write_receiver.clone(),
                            None => {
                                unreachable!();
                            }
                        };

                        self.inner = Inner::new_stream(
                            stream,
                            self.pending.clone(),
                            self.subscriptions.clone(),
                            write_receiver,
                        );
                    }
                    Err(err) => {
                        warn!("error: {:?}, retry after {:?}", err, self.retry_interval);
                        self.inner = Inner::wait(self.retry_interval);
                    }
                    _ => {
                        return Ok(Async::NotReady);
                    }
                },
                Inner::Connected { client } => match client.poll() {
                    Ok(_) => return Ok(Async::NotReady),
                    Err(err) => {
                        warn!("error: {:?}, retry after {:?}", err, self.retry_interval);
                        self.inner = Inner::wait(self.retry_interval);
                    }
                },
            }
        }
    }
}

impl Transport for Ipc {
    type Out = IpcTask<fn(Vec<Result<rpc::Value>>) -> Result<rpc::Value>>;

    fn prepare(&self, method: &str, params: Vec<rpc::Value>) -> (RequestId, rpc::Call) {
        let id = self.id.fetch_add(1, atomic::Ordering::AcqRel);
        let request = helpers::build_request(id, method, params);

        (id, request)
    }

    fn send(&self, id: RequestId, request: rpc::Call) -> Self::Out {
        self.send_request(id, rpc::Request::Single(request), single_response)
    }
}

fn single_response(response: Vec<Result<rpc::Value>>) -> Result<rpc::Value> {
    match response.into_iter().next() {
        Some(res) => res,
        None => Err(ErrorKind::InvalidResponse("Expected single, got batch.".into()).into()),
    }
}

impl BatchTransport for Ipc {
    type Batch = IpcTask<fn(Vec<Result<rpc::Value>>) -> Result<Vec<Result<rpc::Value>>>>;

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

impl DuplexTransport for Ipc {
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

impl ::std::fmt::Debug for Ipc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(
            f,
            "Ipc {{ id: {:?}, path: {:?}, pending: {:?}, subscriptions: {:?}, write_sender: {:?} }}",
            self.id, self.path, self.pending, self.subscriptions, self.write_sender
        )
    }
}

impl Clone for Ipc {
    fn clone(&self) -> Self {
        Ipc {
            inner: Inner::empty(),
            retry_interval: self.retry_interval.clone(),
            path: self.path.clone(),

            id: self.id.clone(),
            pending: self.pending.clone(),
            subscriptions: self.subscriptions.clone(),

            write_receiver: None,
            write_sender: self.write_sender.clone(),

            shutdown_receiver: None,
            shutdown_sender: self.shutdown_sender.clone(),
        }
    }
}

enum Inner {
    Empty,
    WaitForConnect {
        delay: Delay,
    },
    Connecting {
        connector: ConnectFuture,
    },

    Connected {
        client: Box<Future<Item = (), Error = ()> + Send>,
    },
}

impl Inner {
    fn empty() -> Inner {
        Inner::Empty
    }

    fn wait(delay: Duration) -> Inner {
        Inner::WaitForConnect {
            delay: Delay::new(Instant::now() + delay),
        }
    }

    fn connect<P>(path: P) -> Inner
    where
        P: AsRef<Path>,
    {
        Inner::Connecting {
            connector: UnixStream::connect(path),
        }
    }

    fn new_stream(
        stream: UnixStream,
        pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
        subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,
        write_receiver: Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
    ) -> Inner {
        let (read, write) = stream.split();

        let reader = ReadStream::new(read, pending.clone(), subscriptions.clone()).into_future();
        let writer =
            WriteStream::new(write, write_receiver, WriteState::WaitingForRequest).into_future();
        let client = Box::new(
            reader
                .join(writer)
                .map(|_| {})
                .map_err(|_| {})
                .into_future(),
        );

        Inner::Connected { client }
    }
}

#[derive(Debug)]
enum WriteState {
    WaitingForRequest,
    Writing { buffer: Vec<u8>, current_pos: usize },
}

/// Writing part of the IPC transport
/// Awaits new requests using `mpsc::UnboundedReceiver` and writes them to the socket.
#[cfg(unix)]
#[derive(Debug)]
struct WriteStream {
    write: WriteHalf<UnixStream>,
    incoming: Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
    state: WriteState,
}

impl WriteStream {
    fn new(
        write: WriteHalf<UnixStream>,
        incoming: Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
        state: WriteState,
    ) -> WriteStream {
        WriteStream {
            write,
            incoming,
            state,
        }
    }
}

#[cfg(unix)]
impl Future for WriteStream {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> futures::Poll<Self::Item, Self::Error> {
        loop {
            self.state = match self.state {
                WriteState::WaitingForRequest => {
                    // Ask for more to write
                    let to_send = try_ready!(self.incoming.lock().poll());
                    if let Some(to_send) = to_send {
                        trace!(
                            "Got new message to write: {:?}",
                            String::from_utf8_lossy(&to_send)
                        );
                        WriteState::Writing {
                            buffer: to_send,
                            current_pos: 0,
                        }
                    } else {
                        return Ok(futures::Async::NotReady);
                    }
                }
                WriteState::Writing {
                    ref buffer,
                    ref mut current_pos,
                } => {
                    // Write everything in the buffer
                    while *current_pos < buffer.len() {
                        let n = try_nb!(self.write.write(&buffer[*current_pos..]));
                        *current_pos += n;
                        if n == 0 {
                            warn!("IO Error: Zero write.");
                            return Err(()); // zero write?
                        }
                    }

                    WriteState::WaitingForRequest
                }
            };
        }
    }
}

/// Reading part of the IPC transport.
/// Reads data on the socket and tries to dispatch it to awaiting requests.
#[cfg(unix)]
#[derive(Debug)]
struct ReadStream {
    read: ReadHalf<UnixStream>,
    pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
    subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,
    buffer: Vec<u8>,
    current_pos: usize,
}

impl ReadStream {
    fn new(
        read: ReadHalf<UnixStream>,
        pending: Arc<Mutex<BTreeMap<RequestId, Pending>>>,
        subscriptions: Arc<Mutex<BTreeMap<SubscriptionId, Subscription>>>,
    ) -> ReadStream {
        ReadStream {
            read,
            pending,
            subscriptions,
            buffer: vec![],
            current_pos: 0,
        }
    }
}

#[cfg(unix)]
impl Future for ReadStream {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> futures::Poll<Self::Item, Self::Error> {
        const DEFAULT_BUF_SIZE: usize = 4096;
        let mut new_write_size = 128;
        loop {
            if self.current_pos == self.buffer.len() {
                if new_write_size < DEFAULT_BUF_SIZE {
                    new_write_size *= 2;
                }
                self.buffer.resize(self.current_pos + new_write_size, 0);
            }

            let read = try_nb!(self.read.read(&mut self.buffer[self.current_pos..]));
            if read == 0 {
                return Ok(futures::Async::NotReady);
            }

            let mut min = self.current_pos;
            self.current_pos += read;
            while let Some((response, len)) =
                Self::extract_response(&self.buffer[0..self.current_pos], min)
            {
                // Respond
                self.respond(response);

                // copy rest of buffer to the beginning
                for i in len..self.current_pos {
                    self.buffer.swap(i, i - len);
                }

                // truncate the buffer
                let new_len = self.current_pos - len;
                self.buffer.truncate(new_len + new_write_size);

                // Set new positions
                self.current_pos = new_len;
                min = 0;
            }
        }
    }
}

enum Message {
    Rpc(Vec<rpc::Output>),
    Notification(rpc::Notification),
}

#[cfg(unix)]
impl ReadStream {
    fn respond(&self, response: Message) {
        match response {
            Message::Rpc(outputs) => {
                let id = match outputs.get(0) {
                    Some(&rpc::Output::Success(ref success)) => success.id.clone(),
                    Some(&rpc::Output::Failure(ref failure)) => failure.id.clone(),
                    None => rpc::Id::Num(0),
                };

                if let rpc::Id::Num(num) = id {
                    if let Some(request) = self.pending.lock().remove(&(num as usize)) {
                        trace!("Responding to (id: {:?}) with {:?}", num, outputs);
                        if let Err(err) = request.send(helpers::to_results_from_outputs(outputs)) {
                            warn!("Sending a response to deallocated channel: {:?}", err);
                        }
                    } else {
                        warn!("Got response for unknown request (id: {:?})", num);
                    }
                } else {
                    warn!("Got unsupported response (id: {:?})", id);
                }
            }
            Message::Notification(notification) => {
                if let rpc::Params::Map(params) = notification.params {
                    let id = params.get("subscription");
                    let result = params.get("result");

                    if let (Some(&rpc::Value::String(ref id)), Some(result)) = (id, result) {
                        let id: SubscriptionId = id.clone().into();
                        if let Some(stream) = self.subscriptions.lock().get(&id) {
                            if let Err(e) = stream.unbounded_send(result.clone()) {
                                error!("Error sending notification (id: {:?}): {:?}", id, e);
                            }
                        } else {
                            warn!("Got notification for unknown subscription (id: {:?})", id);
                        }
                    } else {
                        error!("Got unsupported notification (id: {:?})", id);
                    }
                }
            }
        }
    }

    fn extract_response(buf: &[u8], min: usize) -> Option<(Message, usize)> {
        for pos in (min..buf.len()).rev() {
            // Look for end character
            if buf[pos] == b']' || buf[pos] == b'}' {
                // Try to deserialize
                let pos = pos + 1;
                match helpers::to_response_from_slice(&buf[0..pos]) {
                    Ok(rpc::Response::Single(output)) => {
                        return Some((Message::Rpc(vec![output]), pos));
                    }
                    Ok(rpc::Response::Batch(outputs)) => {
                        return Some((Message::Rpc(outputs), pos));
                    }
                    // just continue
                    _ => {}
                }
                match helpers::to_notification_from_slice(&buf[0..pos]) {
                    Ok(notification) => {
                        return Some((Message::Notification(notification), pos));
                    }
                    _ => {}
                }
            }
        }

        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    extern crate tokio;
    extern crate tokio_uds;

    use futures::{self, Future};
    use rpc;
    use std::io::{self, Read, Write};
    use std::time::{Duration, Instant};
    use tokio::timer::Delay;

    use super::{Ipc, Transport};

    #[test]
    fn should_send_a_request() {
        // given
        let mut runtime = tokio::runtime::Runtime::new().unwrap();
        let (server, client) = tokio_uds::UnixStream::pair().unwrap();
        let ipc = Ipc::with_stream(client, "").unwrap();

        runtime.spawn({
            struct Task {
                server: tokio_uds::UnixStream,
            }

            impl Future for Task {
                type Item = ();
                type Error = ();
                fn poll(&mut self) -> futures::Poll<(), ()> {
                    let mut data = [0; 2048];
                    // Read request
                    let read = try_nb!(self.server.read(&mut data));
                    let request = String::from_utf8(data[0..read].to_vec()).unwrap();
                    assert_eq!(
                        &request,
                        r#"{"jsonrpc":"2.0","method":"eth_accounts","params":["1"],"id":0}"#
                    );

                    // Write response
                    let response = r#"{"jsonrpc":"2.0","id":0,"result":"x"}"#;
                    self.server.write_all(response.as_bytes()).unwrap();
                    self.server.flush().unwrap();

                    Ok(futures::Async::Ready(()))
                }
            }

            Task { server: server }
        });

        runtime.spawn(
            ipc.execute("eth_accounts", vec![rpc::Value::String("1".into())])
                .then(|v| {
                    assert_eq!(v, Ok(rpc::Value::String("x".into())));
                    Ok(())
                }),
        );

        runtime.spawn({
            let ipc = ipc.clone();
            Delay::new(Instant::now() + Duration::from_millis(500)).then(|_| {
                ipc.close();
                Ok(())
            })
        });

        // then
        runtime.block_on(ipc).unwrap();
    }

    #[test]
    fn should_handle_double_response() {
        // given
        let mut runtime = tokio::runtime::Runtime::new().unwrap();
        let (server, client) = tokio_uds::UnixStream::pair().unwrap();
        let ipc = Ipc::with_stream(client, "").unwrap();

        runtime.spawn({
            struct Task {
                server: tokio_uds::UnixStream,
            }

            impl Future for Task {
                type Item = ();
                type Error = ();
                fn poll(&mut self) -> futures::Poll<(), ()> {
                    let mut data = [0; 2048];
                    // Read request
                    let read = try_nb!(self.server.read(&mut data));
                    let request = String::from_utf8(data[0..read].to_vec()).unwrap();
                    assert_eq!(&request, r#"{"jsonrpc":"2.0","method":"eth_accounts","params":["1"],"id":0}{"jsonrpc":"2.0","method":"eth_accounts","params":["1"],"id":1}"#);

                    // Write response
                    let response = r#"{"jsonrpc":"2.0","id":0,"result":"x"}{"jsonrpc":"2.0","id":1,"result":"x"}"#;
                    self.server.write_all(response.as_bytes()).unwrap();
                    self.server.flush().unwrap();

                    Ok(futures::Async::Ready(()))
                }
            }

            Task { server: server }
        });

        // when
        let res1 = ipc.execute("eth_accounts", vec![rpc::Value::String("1".into())]);
        let res2 = ipc.execute("eth_accounts", vec![rpc::Value::String("1".into())]);

        // then
        runtime.spawn(res1.join(res2).then(|res| {
            assert_eq!(
                res,
                Ok((
                    rpc::Value::String("x".into()),
                    rpc::Value::String("x".into()),
                ))
            );
            Ok(())
        }));

        runtime.spawn({
            let ipc = ipc.clone();
            Delay::new(Instant::now() + Duration::from_millis(100)).then(|_| {
                ipc.close();
                Ok(())
            })
        });

        // then
        runtime.block_on(ipc).unwrap();
    }
}
