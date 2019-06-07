use ethereum_types::H256;

use super::helpers::{self, CallFuture};
use super::types::BlockDetails;
use super::{Namespace, Transport};

#[cfg(test)]
use super::{helpers, types};

#[derive(Debug, Clone)]
/// `Internal` Specific API
pub struct Internal<T> {
    transport: T,
}

impl<T: Transport> Namespace<T> for Internal<T> {
    fn new(transport: T) -> Self {
        Internal { transport }
    }

    fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Transport> Internal<T> {
    /// Get BlockDetials by hash
    pub fn block_details_by_hash(&self, hash: H256) -> CallFuture<Option<BlockDetails>, T::Out> {
        let hash = helpers::serialize(&hash);
        CallFuture::new(
            self.transport
                .execute("internal_getBlockDetailsByHash", vec![hash]),
        )
    }
}
