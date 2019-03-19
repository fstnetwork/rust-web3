//! Contract call/query error.

#![allow(unknown_lints)]
#![allow(missing_docs)]

use super::types;
use super::{ApiError, ApiErrorKind};

error_chain! {
    links {
        Abi(ethabi::Error, ethabi::ErrorKind);
        Api(ApiError, ApiErrorKind);
    }

    errors {
        InvalidOutputType(e: String) {
            description("invalid output type requested by the caller"),
            display("Invalid output type: {}", e),
        }
    }
}

/// Contract deployment error.
pub mod deploy {
    use super::types::H256;

    error_chain! {
        links {
            Api(super::ApiError, super::ApiErrorKind);
        }

        errors {
            ContractDeploymentFailure(hash: H256) {
                description("Contract deployment failed")
                display("Failure during deployment. Tx hash: {:?}", hash),
            }
        }
    }
}
