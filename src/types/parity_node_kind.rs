/// Describes the kind of node. This information can provide a hint to
/// applications about how to utilize the RPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParityNodeKind {
    /// The capability of the node.
    pub capability: Capability,
    /// Who the node is available to.
    pub availability: Availability,
}

/// Who the node is available to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Availability {
    /// A personal node, not intended to be available to everyone.
    Personal,
    /// A public, open node.
    Public,
}

/// The capability of the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// A full node stores the full state and fully enacts incoming blocks.
    Full,
    /// A light node does a minimal header sync and fetches data as needed
    /// from the network.
    Light,
}
