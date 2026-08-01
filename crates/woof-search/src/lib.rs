//! Private local vector and hybrid search for woof.

mod embed;
mod error;
mod hybrid;
mod index;
mod key;
mod record;

pub use embed::{Embedder, LocalEmbedder};
pub use error::SearchError;
pub use hybrid::{hybrid_rank_merge, HybridHit, HybridWeights, LexicalHit, RankedVectorHit};
pub use index::{
    RebuildReport, VectorHit, VectorIndex, CONNECTIVITY, CONNECTIVITY_BASE, DIMENSIONS,
};
pub use key::{derive_vector_key, KEY_DERIVATION_VERSION};
pub use record::{IndexedRecord, SnapshotLikeRecord};

pub const INDEX_FORMAT_VERSION: u16 = 2;
