use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("embedding returned {actual} dimensions; expected {expected}")]
    Dimensions { expected: usize, actual: usize },
    #[error("embedding returned the wrong number of vectors")]
    EmbeddingCount,
    #[error("embedding contains non-finite values")]
    NonFiniteEmbedding,
    #[error("embedding is a zero vector")]
    ZeroEmbedding,
    #[error("duplicate stable record identity: {namespace}/{stable_id}")]
    DuplicateIdentity {
        namespace: String,
        stable_id: String,
    },
    #[error("provisional vector key collision between {first} and {second}")]
    KeyCollision { first: String, second: String },
    #[error("vector index operation failed")]
    Index,
    #[error("vector index path is not valid UTF-8")]
    InvalidIndexPath,
    #[error("vector index I/O failed")]
    Io(#[from] std::io::Error),
    #[error("vector index container is corrupt or uses an unsupported format")]
    InvalidIndexContainer,
    #[error("vector index permissions are not private (expected 0600)")]
    InsecurePermissions,
    #[error("vector index metadata is invalid")]
    InvalidMetadata,
}
