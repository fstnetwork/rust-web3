//! HTTP Transport

extern crate hyper;
extern crate url;

#[cfg(feature = "tls")]
extern crate hyper_tls;
#[cfg(feature = "tls")]
extern crate native_tls;

use std::ops::Deref;
use std::sync::atomic::{self, AtomicUsize};
use std::sync::Arc;

use self::hyper::header::HeaderValue;
use self::url::Url;
use base64;
use futures::sync::{mpsc, oneshot};
use futures::{self, future, Async, Future, Poll, Stream};

use super::helpers;

use super::error::{Error, ErrorKind};
use super::transports::shared::Response;
use super::transports::Result;
use super::{BatchTransport, RequestId, Transport};

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

impl From<hyper::http::uri::InvalidUri> for Error {
    fn from(err: hyper::http::uri::InvalidUri) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

impl From<hyper::header::InvalidHeaderValue> for Error {
    fn from(err: hyper::header::InvalidHeaderValue) -> Self {
        ErrorKind::Transport(format!("{}", err)).into()
    }
}

#[cfg(all(feature = "http", not(feature = "ws")))]
impl From<self::url::ParseError> for Error {
    fn from(err: self::url::ParseError) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

#[cfg(feature = "tls")]
impl From<native_tls::Error> for Error {
    fn from(err: native_tls::Error) -> Self {
        ErrorKind::Transport(format!("{:?}", err)).into()
    }
}

// The max string length of a request without transfer-encoding: chunked.
const MAX_SINGLE_CHUNK: usize = 256;
const DEFAULT_MAX_PARALLEL: usize = 64;
type Pending = oneshot::Sender<Result<hyper::Chunk>>;

/// A future representing pending HTTP request, resolves to a response.
pub type FetchTask<F> = Response<F, hyper::Chunk>;

/// HTTP Transport (synchronous)
#[derive(Debug)]
pub struct Http {
    inner: Inner,
    id: Arc<AtomicUsize>,
    url: hyper::Uri,
    basic_auth: Option<HeaderValue>,
    write_sender: mpsc::UnboundedSender<(hyper::Request<hyper::Body>, Pending)>,
}

impl Clone for Http {
    fn clone(&self) -> Self {
        Http {
            inner: Inner::empty(),
            id: self.id.clone(),
            url: self.url.clone(),
            basic_auth: self.basic_auth.clone(),
            write_sender: self.write_sender.clone(),
        }
    }
}

impl Http {
    /// Create new HTTP transport with given URL
    pub fn new(url: &str) -> Result<Self> {
        Self::with_max_parallel(url, DEFAULT_MAX_PARALLEL)
    }

    /// Create new HTTP transport with given URL and existing event loop handle.
    pub fn with_max_parallel(url: &str, max_parallel: usize) -> Result<Self> {
        let basic_auth = {
            let url = Url::parse(url)?;
            let user = url.username();

            if user.len() > 0 {
                let auth = match url.password() {
                    Some(pass) => format!("{}:{}", user, pass),
                    None => format!("{}:", user),
                };
                Some(HeaderValue::from_str(&format!(
                    "Basic {}",
                    base64::encode(&auth)
                ))?)
            } else {
                None
            }
        };

        let (write_sender, write_receiver) = mpsc::unbounded();

        #[cfg(feature = "tls")]
        let client =
            hyper::Client::builder().build::<_, hyper::Body>(hyper_tls::HttpsConnector::new(4)?);

        #[cfg(not(feature = "tls"))]
        let client = hyper::Client::new();

        let inner = Inner::new_client(Box::new(
            write_receiver
                .map(move |(request, tx): (_, Pending)| {
                    client
                        .request(request)
                        .then(move |response| Ok((response, tx)))
                })
                .buffer_unordered(max_parallel)
                .for_each(|(response, tx)| {
                    use futures::future::Either::{A, B};
                    let future = match response {
                        Ok(ref res) if !res.status().is_success() => A(future::err(
                            ErrorKind::Transport(format!(
                                "Unexpected response status code: {}",
                                res.status()
                            ))
                            .into(),
                        )),
                        Ok(res) => B(res.into_body().concat2().map_err(Into::into)),
                        Err(err) => A(future::err(err.into())),
                    };
                    future.then(move |result| {
                        if let Err(err) = tx.send(result) {
                            warn!("Error resuming asynchronous request: {:?}", err);
                        }
                        Ok(())
                    })
                })
                .into_stream(),
        ));

        Ok(Http {
            inner,
            id: Default::default(),
            url: url.parse()?,
            basic_auth,
            write_sender,
        })
    }

    pub fn close(&mut self) {
        self.inner = Inner::empty();
    }

    fn send_request<F, O>(&self, id: RequestId, request: rpc::Request, extract: F) -> FetchTask<F>
    where
        F: Fn(hyper::Chunk) -> O,
    {
        let request = helpers::to_string(&request);
        debug!("[{}] Sending: {} to {}", id, request, self.url);
        let len = request.len();
        let mut req = hyper::Request::new(hyper::Body::from(request));
        *req.method_mut() = hyper::Method::POST;
        *req.uri_mut() = self.url.clone();
        req.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        req.headers_mut().insert(
            hyper::header::USER_AGENT,
            HeaderValue::from_static("web3.rs"),
        );

        // Don't send chunked request
        if len < MAX_SINGLE_CHUNK {
            req.headers_mut()
                .insert(hyper::header::CONTENT_LENGTH, len.into());
        }
        // Send basic auth header
        if let Some(ref basic_auth) = self.basic_auth {
            req.headers_mut()
                .insert(hyper::header::AUTHORIZATION, basic_auth.clone());
        }
        let (tx, rx) = futures::oneshot();
        let result = self
            .write_sender
            .unbounded_send((req, tx))
            .map_err(|_| ErrorKind::Io(::std::io::ErrorKind::BrokenPipe.into()).into());

        Response::new(id, result, rx, extract)
    }
}

impl Future for Http {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match &mut self.inner {
            Inner::Empty => return Ok(Async::Ready(())),
            Inner::Connected { ref mut client } => loop {
                match client.poll() {
                    _ => return Ok(Async::NotReady),
                }
            },
        }
    }
}

impl Transport for Http {
    type Out = FetchTask<fn(hyper::Chunk) -> Result<rpc::Value>>;

    fn prepare(&self, method: &str, params: Vec<rpc::Value>) -> (RequestId, rpc::Call) {
        let id = self.id.fetch_add(1, atomic::Ordering::AcqRel);
        let request = helpers::build_request(id, method, params);

        (id, request)
    }

    fn send(&self, id: RequestId, request: rpc::Call) -> Self::Out {
        self.send_request(id, rpc::Request::Single(request), single_response)
    }
}

impl BatchTransport for Http {
    type Batch = FetchTask<fn(hyper::Chunk) -> Result<Vec<Result<rpc::Value>>>>;

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

        self.send_request(id, rpc::Request::Batch(requests), batch_response)
    }
}

/// Parse bytes RPC response into `Result`.
fn single_response<T: Deref<Target = [u8]>>(response: T) -> Result<rpc::Value> {
    let response = serde_json::from_slice(&*response)
        .map_err(|e| Error::from(ErrorKind::InvalidResponse(format!("{:?}", e))))?;

    match response {
        rpc::Response::Single(output) => helpers::to_result_from_output(output),
        _ => Err(ErrorKind::InvalidResponse("Expected single, got batch.".into()).into()),
    }
}

/// Parse bytes RPC batch response into `Result`.
fn batch_response<T: Deref<Target = [u8]>>(response: T) -> Result<Vec<Result<rpc::Value>>> {
    let response = serde_json::from_slice(&*response)
        .map_err(|e| Error::from(ErrorKind::InvalidResponse(format!("{:?}", e))))?;

    match response {
        rpc::Response::Batch(outputs) => Ok(outputs
            .into_iter()
            .map(helpers::to_result_from_output)
            .collect()),
        _ => Err(ErrorKind::InvalidResponse("Expected batch, got single.".into()).into()),
    }
}

enum Inner {
    Empty,
    Connected {
        client: Box<Stream<Item = (), Error = ()> + Send>,
    },
}

impl Inner {
    fn empty() -> Self {
        Inner::Empty
    }

    fn new_client(client: Box<Stream<Item = (), Error = ()> + Send>) -> Self {
        Inner::Connected { client: client }
    }
}

impl ::std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        // TODO
        write!(f, "")
        // write!(f, "{:?}", self.write_receiver)
    }
}
