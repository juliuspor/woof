use serde::{Deserialize, Serialize};

use crate::derive_vector_key;

/// Minimal source record accepted by a full vector rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotLikeRecord {
    pub namespace: String,
    pub stable_id: String,
    pub text: String,
    pub occurred_at_ms: i64,
}

impl SnapshotLikeRecord {
    pub fn snapshot(
        stable_id: impl Into<String>,
        text: impl Into<String>,
        occurred_at_ms: i64,
    ) -> Self {
        Self {
            namespace: "snapshot".into(),
            stable_id: stable_id.into(),
            text: text.into(),
            occurred_at_ms,
        }
    }
}

/// Non-content metadata persisted beside the native graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedRecord {
    pub key: u64,
    pub namespace: String,
    pub stable_id: String,
    pub occurred_at_ms: i64,
}

impl From<&SnapshotLikeRecord> for IndexedRecord {
    fn from(value: &SnapshotLikeRecord) -> Self {
        Self {
            key: derive_vector_key(&value.namespace, &value.stable_id),
            namespace: value.namespace.clone(),
            stable_id: value.stable_id.clone(),
            occurred_at_ms: value.occurred_at_ms,
        }
    }
}
