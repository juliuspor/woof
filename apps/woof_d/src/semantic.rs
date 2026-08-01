use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use woof_search::{
    derive_vector_key, hybrid_rank_merge, Embedder, HybridWeights, LexicalHit, LocalEmbedder,
    RebuildReport, SearchError, SnapshotLikeRecord, VectorIndex,
};
use woof_storage::{SearchHit, SnapshotExport, Storage, StorageError};

const MAX_SEARCH_RESULTS: usize = 30;
const SEARCH_CANDIDATES: usize = 30;

pub type SharedSemanticSearch = Arc<Mutex<SemanticSearchService>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticInitialization {
    Loaded { indexed: usize },
    Rebuilt(RebuildReport),
}

#[derive(Debug, Error)]
pub enum SemanticServiceError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error("semantic search mutex is poisoned")]
    Poisoned,
    #[error("snapshot disappeared before semantic indexing")]
    MissingSnapshot,
}

pub struct SemanticSearchService {
    embedder: Arc<dyn Embedder>,
    index: VectorIndex,
    index_path: PathBuf,
}

impl SemanticSearchService {
    pub fn initialize_local(
        storage: &Storage,
        index_path: impl Into<PathBuf>,
    ) -> Result<(Self, SemanticInitialization), SemanticServiceError> {
        let embedder = Arc::new(LocalEmbedder::new());
        Self::initialize_with_embedder(storage, index_path, embedder)
    }

    pub fn initialize_with_embedder(
        storage: &Storage,
        index_path: impl Into<PathBuf>,
        embedder: Arc<dyn Embedder>,
    ) -> Result<(Self, SemanticInitialization), SemanticServiceError> {
        let index_path = index_path.into();
        let exports = storage.export_snapshots()?;
        let records = snapshot_records(&exports);
        if let Ok(index) = VectorIndex::load(&index_path) {
            if index_matches(&index, &records, embedder.as_ref()) {
                let indexed = index.len();
                return Ok((
                    Self {
                        embedder,
                        index,
                        index_path,
                    },
                    SemanticInitialization::Loaded { indexed },
                ));
            }
        }

        let (index, report) = VectorIndex::build(embedder.as_ref(), &records)?;
        index.save_atomic(&index_path)?;
        Ok((
            Self {
                embedder,
                index,
                index_path,
            },
            SemanticInitialization::Rebuilt(report),
        ))
    }

    pub fn shared(self) -> SharedSemanticSearch {
        Arc::new(Mutex::new(self))
    }

    pub fn indexed_len(&self) -> usize {
        self.index.len()
    }

    /// Unconditionally rebuilds the persisted vector graph from SQLite.
    pub fn rebuild(&mut self, storage: &Storage) -> Result<RebuildReport, SemanticServiceError> {
        let exports = storage.export_snapshots()?;
        let records = snapshot_records(&exports);
        let report = self.index.rebuild(self.embedder.as_ref(), &records)?;
        if let Err(error) = self.index.save_atomic(&self.index_path) {
            // A failed atomic replace must not leave a pre-retention or
            // pre-deletion graph on disk. The in-memory graph is already the
            // rebuilt one; fail closed by removing only woof's fixed index.
            match std::fs::remove_file(&self.index_path) {
                Ok(()) => {}
                Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {}
                Err(remove_error) => return Err(SearchError::Io(remove_error).into()),
            }
            return Err(error.into());
        }
        Ok(report)
    }

    /// Reads the committed row back from SQLite, then atomically persists the
    /// incrementally updated graph.
    pub fn upsert_persisted_snapshot(
        &mut self,
        storage: &Storage,
        snapshot_id: &str,
    ) -> Result<(), SemanticServiceError> {
        let export = storage
            .export_snapshot(snapshot_id)?
            .ok_or(SemanticServiceError::MissingSnapshot)?;
        self.index
            .upsert(self.embedder.as_ref(), &snapshot_record(&export))?;
        self.index.save_atomic(&self.index_path)?;
        Ok(())
    }

    /// Advances metadata for a deduplicated capture without recomputing its
    /// unchanged embedding. If the record is unexpectedly absent, repair it
    /// with a normal content upsert before persisting the graph.
    pub fn refresh_persisted_snapshot_metadata(
        &mut self,
        storage: &Storage,
        snapshot_id: &str,
    ) -> Result<(), SemanticServiceError> {
        let export = storage
            .export_snapshot(snapshot_id)?
            .ok_or(SemanticServiceError::MissingSnapshot)?;
        let record = snapshot_record(&export);
        if !self.index.refresh_metadata(&record)? {
            self.index.upsert(self.embedder.as_ref(), &record)?;
        }
        self.index.save_atomic(&self.index_path)?;
        Ok(())
    }

    pub fn search(
        &self,
        storage: &Storage,
        query: &str,
        requested_limit: usize,
    ) -> Result<Vec<SearchHit>, SemanticServiceError> {
        let output_limit = requested_limit.clamp(1, MAX_SEARCH_RESULTS);
        let lexical_hits = storage.search_snapshots(query, SEARCH_CANDIDATES)?;
        let lexical = lexical_hits
            .iter()
            .map(|hit| LexicalHit {
                key: derive_vector_key("snapshot", &hit.snapshot_id),
                score: hit.score as f32,
            })
            .collect::<Vec<_>>();
        let vector = self
            .index
            .search_text(self.embedder.as_ref(), query, SEARCH_CANDIDATES)?;
        let fused = hybrid_rank_merge(&lexical, &vector, output_limit, HybridWeights::default());

        let mut identities = BTreeMap::<u64, String>::new();
        for hit in &lexical_hits {
            let key = derive_vector_key("snapshot", &hit.snapshot_id);
            if let Some(existing) = identities.insert(key, hit.snapshot_id.clone()) {
                if existing != hit.snapshot_id {
                    return Err(SearchError::KeyCollision {
                        first: format!("snapshot/{existing}"),
                        second: format!("snapshot/{}", hit.snapshot_id),
                    }
                    .into());
                }
            }
        }
        for hit in &vector {
            let Some(record) = self.index.record(hit.key) else {
                return Err(SearchError::InvalidIndexContainer.into());
            };
            if let Some(existing) = identities.insert(hit.key, record.stable_id.clone()) {
                if existing != record.stable_id {
                    return Err(SearchError::KeyCollision {
                        first: format!("snapshot/{existing}"),
                        second: format!("snapshot/{}", record.stable_id),
                    }
                    .into());
                }
            }
        }

        let mut ids = Vec::with_capacity(fused.len());
        let mut scores = BTreeMap::new();
        for hit in fused {
            if let Some(snapshot_id) = identities.get(&hit.key) {
                ids.push(snapshot_id.clone());
                scores.insert(snapshot_id.clone(), f64::from(hit.score));
            }
        }
        let mut results = storage.search_hits_by_ids(&ids)?;
        for hit in &mut results {
            if let Some(score) = scores.get(&hit.snapshot_id) {
                hit.score = *score;
            }
        }
        Ok(results)
    }
}

pub fn lock_semantic(
    semantic: &SharedSemanticSearch,
) -> Result<std::sync::MutexGuard<'_, SemanticSearchService>, SemanticServiceError> {
    semantic.lock().map_err(|_| SemanticServiceError::Poisoned)
}

fn snapshot_records(exports: &[SnapshotExport]) -> Vec<SnapshotLikeRecord> {
    exports.iter().map(snapshot_record).collect()
}

fn snapshot_record(export: &SnapshotExport) -> SnapshotLikeRecord {
    SnapshotLikeRecord::snapshot(
        export.snapshot_id.clone(),
        export.content.clone(),
        export.last_seen_at.saturating_mul(1_000),
    )
}

fn index_matches(
    index: &VectorIndex,
    expected: &[SnapshotLikeRecord],
    embedder: &dyn Embedder,
) -> bool {
    let expected = expected
        .iter()
        .filter(|record| !record.text.trim().is_empty())
        .collect::<Vec<_>>();
    index.embedding_compatible(embedder).unwrap_or(false)
        && index.len() == expected.len()
        && expected.iter().all(|record| {
            index
                .record(derive_vector_key(&record.namespace, &record.stable_id))
                .is_some_and(|indexed| {
                    indexed.namespace == record.namespace
                        && indexed.stable_id == record.stable_id
                        && indexed.occurred_at_ms == record.occurred_at_ms
                })
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use woof_search::{SearchError, DIMENSIONS};
    use woof_storage::CaptureRecord;

    use super::*;

    struct SyntheticEmbedder;

    impl Embedder for SyntheticEmbedder {
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError> {
            Ok(texts.iter().map(|text| vector(text)).collect())
        }
    }

    fn vector(text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; DIMENSIONS];
        let lower = text.to_ascii_lowercase();
        if lower.contains("canine") || lower.contains("dog") {
            vector[0] = 1.0;
        } else if lower.contains("feline") || lower.contains("cat") {
            vector[1] = 1.0;
        } else {
            vector[2] = 1.0;
        }
        vector
    }

    fn temp_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "woof-semantic-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn capture(id: &str, content: &str, observed_at: i64) -> CaptureRecord {
        CaptureRecord {
            snapshot_id: Some(id.to_string()),
            content: content.to_string(),
            app: "TextEdit".to_string(),
            window_title: "Fixture".to_string(),
            url: None,
            domain: None,
            captured_at: observed_at,
            last_seen_at: observed_at,
            duration_s: 1.0,
            focused_name: None,
            focused_role: None,
            focused_path: None,
        }
    }

    #[test]
    fn rebuild_load_incremental_upsert_and_hybrid_search_are_local() {
        let directory = temp_directory("lifecycle");
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        storage
            .record_capture(&capture("semantic-dog", "canine project notes", 1), 20)
            .unwrap();
        storage
            .record_capture(&capture("lexical-cat", "feline invoice details", 2), 20)
            .unwrap();
        let index_path = directory.join("woof.vector-index");
        let (mut service, initialized) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();
        assert!(matches!(
            initialized,
            SemanticInitialization::Rebuilt(RebuildReport { indexed: 2, .. })
        ));
        assert_eq!(service.indexed_len(), 2);

        let semantic = service.search(&storage, "dog", 10).unwrap();
        assert_eq!(semantic[0].snapshot_id, "semantic-dog");
        assert_eq!(
            serde_json::to_value(&semantic[0])
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            7
        );

        storage
            .record_capture(&capture("new-cat", "cat-only semantic result", 3), 20)
            .unwrap();
        service
            .upsert_persisted_snapshot(&storage, "new-cat")
            .unwrap();
        assert_eq!(service.indexed_len(), 3);
        assert!(service
            .search(&storage, "feline", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.snapshot_id == "new-cat"));

        let (_, loaded) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();
        assert_eq!(loaded, SemanticInitialization::Loaded { indexed: 3 });
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn stale_or_corrupt_index_is_rebuilt_atomically() {
        let directory = temp_directory("stale");
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let index_path = directory.join("woof.vector-index");
        let (service, _) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();
        drop(service);
        fs::write(&index_path, b"corrupt").unwrap();

        storage
            .record_capture(&capture("dog", "canine fixture", 4), 20)
            .unwrap();
        let (service, initialized) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();
        assert!(matches!(
            initialized,
            SemanticInitialization::Rebuilt(RebuildReport { indexed: 1, .. })
        ));
        assert_eq!(service.indexed_len(), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn deduplicated_metadata_refresh_keeps_the_index_loadable() {
        let directory = temp_directory("deduplicated-metadata");
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let index_path = directory.join("woof.vector-index");
        storage
            .record_capture(&capture("dog", "canine fixture", 4), 20)
            .unwrap();
        let (mut service, _) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();

        storage
            .record_capture(&capture("dog", "canine fixture", 40), 20)
            .unwrap();
        service
            .refresh_persisted_snapshot_metadata(&storage, "dog")
            .unwrap();
        drop(service);

        let (_, initialized) = SemanticSearchService::initialize_with_embedder(
            &storage,
            &index_path,
            Arc::new(SyntheticEmbedder),
        )
        .unwrap();
        assert_eq!(initialized, SemanticInitialization::Loaded { indexed: 1 });
        let _ = fs::remove_dir_all(directory);
    }
}
