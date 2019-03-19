use super::helpers::CallFuture;
use super::types::ParityNodeKind;
use super::{Namespace, Transport};

#[cfg(test)]
use super::{helpers, types};

#[derive(Debug, Clone)]
/// `Parity` Specific API
pub struct Parity<T> {
    transport: T,
}

impl<T: Transport> Namespace<T> for Parity<T> {
    fn new(transport: T) -> Self {
        Parity { transport }
    }

    fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Transport> Parity<T> {
    /// Get Parity enode URI
    pub fn enode(&self) -> CallFuture<String, T::Out> {
        CallFuture::new(self.transport().execute("parity_enode", vec![]))
    }

    /// Get the mode, returns one of: "active", "passive", "dark", "offline"
    pub fn mode(&self) -> CallFuture<String, T::Out> {
        CallFuture::new(self.transport().execute("parity_mode", vec![]))
    }

    /// Get the node type availability and capability
    pub fn node_kind(&self) -> CallFuture<ParityNodeKind, T::Out> {
        CallFuture::new(self.transport().execute("parity_nodeKind", vec![]))
    }

    /// Get node name, set when starting parity with --identity NAME.
    pub fn node_name(&self) -> CallFuture<String, T::Out> {
        CallFuture::new(self.transport().execute("parity_nodeName", vec![]))
    }

    /// Get the hostname and the port of WebSockets/Signer server
    pub fn websocket_url(&self) -> CallFuture<String, T::Out> {
        CallFuture::new(self.transport().execute("parity_wsUrl", vec![]))
    }
}

#[cfg(test)]
mod tests {
    use futures::Future;
    use rpc::Value;

    use super::types::{Availability, Capability, ParityNodeKind};
    use super::{Namespace, Parity};

    rpc_test! (
    Parity:enode => "parity_enode";
    Value::String("enode://050929adcfe47dbe0b002cb7ef2bf91ca74f77c4e0f68730e39e717f1ce38908542369ae017148bee4e0d968340885e2ad5adea4acd19c95055080a4b625df6a@172.17.0.1:30303".to_owned())
    => String::from("enode://050929adcfe47dbe0b002cb7ef2bf91ca74f77c4e0f68730e39e717f1ce38908542369ae017148bee4e0d968340885e2ad5adea4acd19c95055080a4b625df6a@172.17.0.1:30303")
    );

    rpc_test! (
    Parity:mode => "parity_mode";
    Value::String("offline".to_owned()) => String::from("offline")
    );

    rpc_test! (
    Parity:node_name => "parity_nodeName";
    Value::String("parity-ethereum".to_owned()) => String::from("parity-ethereum")
    );

    rpc_test! (
    Parity:node_kind => "parity_nodeKind";
    serde_json::from_str::<Value>(r#"{"availability": "personal","capability": "light"}"#).unwrap()
    => ParityNodeKind {
        availability: Availability::Personal,
        capability: Capability::Light,
    }
    );

    rpc_test! (
    Parity:websocket_url => "parity_wsUrl";
    Value::String("localhost:8546".to_owned()) => String::from("localhost:8546")
    );
}
