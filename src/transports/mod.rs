//! Supported Ethereum JSON-RPC transports.

use super::api;
use super::error;
use super::helpers;
use super::transports;
use super::{BatchTransport, DuplexTransport, RequestId, Transport};

/// RPC Result.
pub type Result<T> = ::std::result::Result<T, error::Error>;

pub mod batch;
pub use self::batch::Batch;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "http")]
pub use self::http::Http;

#[cfg(feature = "ipc")]
pub mod ipc;
#[cfg(feature = "ipc")]
pub use self::ipc::Ipc;

#[cfg(feature = "ws")]
pub mod ws;
#[cfg(feature = "ws")]
pub use self::ws::WebSocket;

#[cfg(any(feature = "ipc", feature = "http", feature = "ws"))]
mod shared;
