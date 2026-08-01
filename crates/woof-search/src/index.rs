use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::{
    derive_vector_key,
    embed::{validate_embedding, Embedder},
    IndexedRecord, SearchError, SnapshotLikeRecord, INDEX_FORMAT_VERSION,
};

pub const DIMENSIONS: usize = 512;
pub const CONNECTIVITY: usize = 16;
/// USearch 2.25.1 derives M0 as `connectivity * 2`.
pub const CONNECTIVITY_BASE: usize = 32;
const USEARCH_VERSION: &str = "2.25.1";
const CONTAINER_MAGIC: &[u8; 8] = b"WOOFHNSW";
const CONTAINER_HEADER_BYTES: usize = 80;
const METRIC_INNER_PRODUCT: u8 = 1;
const SCALAR_F16: u8 = 4;
const MAX_METADATA_BYTES: usize = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 1024 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorHit {
    pub key: u64,
    /// USearch inner-product distance (`1 - dot(query, candidate)`).
    pub distance: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub indexed: usize,
    pub skipped_empty: usize,
}

pub struct VectorIndex {
    native: Index,
    records: BTreeMap<u64, IndexedRecord>,
    embedding_signature: [u8; 32],
}

impl VectorIndex {
    pub fn empty() -> Result<Self, SearchError> {
        Ok(Self {
            native: new_native_index()?,
            records: BTreeMap::new(),
            embedding_signature: [0; 32],
        })
    }

    pub fn rebuild<E: Embedder + ?Sized>(
        &mut self,
        embedder: &E,
        records: &[SnapshotLikeRecord],
    ) -> Result<RebuildReport, SearchError> {
        let (replacement, report) = Self::build(embedder, records)?;
        *self = replacement;
        Ok(report)
    }

    pub fn build<E: Embedder + ?Sized>(
        embedder: &E,
        records: &[SnapshotLikeRecord],
    ) -> Result<(Self, RebuildReport), SearchError> {
        let embedding_signature = embedding_signature(embedder)?;
        let mut selected: Vec<&SnapshotLikeRecord> = records
            .iter()
            .filter(|record| !record.text.trim().is_empty())
            .collect();
        selected.sort_by(|left, right| {
            (&left.namespace, &left.stable_id).cmp(&(&right.namespace, &right.stable_id))
        });

        for pair in selected.windows(2) {
            if pair[0].namespace == pair[1].namespace && pair[0].stable_id == pair[1].stable_id {
                return Err(SearchError::DuplicateIdentity {
                    namespace: pair[0].namespace.clone(),
                    stable_id: pair[0].stable_id.clone(),
                });
            }
        }

        let texts: Vec<String> = selected.iter().map(|record| record.text.clone()).collect();
        let embeddings = embedder.embed_batch(&texts)?;
        if embeddings.len() != selected.len() {
            return Err(SearchError::EmbeddingCount);
        }

        let native = new_native_index()?;
        native
            .reserve(selected.len().max(1))
            .map_err(|_| SearchError::Index)?;
        let mut indexed_records = BTreeMap::new();
        for (record, embedding) in selected.into_iter().zip(embeddings) {
            validate_embedding(&embedding)?;
            let metadata = IndexedRecord::from(record);
            if let Some(existing) = indexed_records.get(&metadata.key) {
                let existing: &IndexedRecord = existing;
                return Err(SearchError::KeyCollision {
                    first: format!("{}/{}", existing.namespace, existing.stable_id),
                    second: format!("{}/{}", metadata.namespace, metadata.stable_id),
                });
            }
            native
                .add(metadata.key, &embedding)
                .map_err(|_| SearchError::Index)?;
            indexed_records.insert(metadata.key, metadata);
        }
        let report = RebuildReport {
            indexed: indexed_records.len(),
            skipped_empty: records.len().saturating_sub(indexed_records.len()),
        };
        Ok((
            Self {
                native,
                records: indexed_records,
                embedding_signature,
            },
            report,
        ))
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn dimensions(&self) -> usize {
        self.native.dimensions()
    }

    pub fn connectivity(&self) -> usize {
        self.native.connectivity()
    }

    pub fn records(&self) -> impl ExactSizeIterator<Item = &IndexedRecord> {
        self.records.values()
    }

    /// Confirms that the currently available embedding backend produces the
    /// same deterministic probe vectors as the backend that built this graph.
    pub fn embedding_compatible<E: Embedder + ?Sized>(
        &self,
        embedder: &E,
    ) -> Result<bool, SearchError> {
        Ok(self.embedding_signature == embedding_signature(embedder)?)
    }

    pub fn record(&self, key: u64) -> Option<&IndexedRecord> {
        self.records.get(&key)
    }

    pub fn search_vector(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<VectorHit>, SearchError> {
        validate_embedding(query)?;
        if limit == 0 || self.records.is_empty() {
            return Ok(Vec::new());
        }
        let matches = self
            .native
            .search(query, limit.min(self.records.len()))
            .map_err(|_| SearchError::Index)?;
        Ok(matches
            .keys
            .into_iter()
            .zip(matches.distances)
            .map(|(key, distance)| VectorHit { key, distance })
            .collect())
    }

    pub fn search_text<E: Embedder + ?Sized>(
        &self,
        embedder: &E,
        query: &str,
        limit: usize,
    ) -> Result<Vec<VectorHit>, SearchError> {
        let embedding = embedder.embed_one(query)?;
        self.search_vector(&embedding, limit)
    }

    /// Adds or replaces one stable record without rebuilding unrelated nodes.
    pub fn upsert<E: Embedder + ?Sized>(
        &mut self,
        embedder: &E,
        record: &SnapshotLikeRecord,
    ) -> Result<bool, SearchError> {
        if record.text.trim().is_empty() {
            return self.delete(&record.namespace, &record.stable_id);
        }

        let metadata = IndexedRecord::from(record);
        if let Some(existing) = self.records.get(&metadata.key) {
            if existing.namespace != metadata.namespace || existing.stable_id != metadata.stable_id
            {
                return Err(SearchError::KeyCollision {
                    first: format!("{}/{}", existing.namespace, existing.stable_id),
                    second: format!("{}/{}", metadata.namespace, metadata.stable_id),
                });
            }
        }

        let embedding = embedder.embed_one(&record.text)?;
        validate_embedding(&embedding)?;
        let replaced = self.records.contains_key(&metadata.key);
        let mut previous_embedding = Vec::new();
        if replaced {
            previous_embedding.resize(DIMENSIONS, 0.0_f32);
            let exported = self
                .native
                .get(metadata.key, &mut previous_embedding)
                .map_err(|_| SearchError::Index)?;
            if exported != 1 {
                return Err(SearchError::InvalidIndexContainer);
            }
            let removed = self
                .native
                .remove(metadata.key)
                .map_err(|_| SearchError::Index)?;
            if removed == 0 {
                return Err(SearchError::InvalidIndexContainer);
            }
        } else if self.native.capacity() < self.records.len() + 1 {
            self.native
                .reserve((self.records.len() + 1).next_power_of_two())
                .map_err(|_| SearchError::Index)?;
        }
        if self.native.add(metadata.key, &embedding).is_err() {
            if replaced && self.native.add(metadata.key, &previous_embedding).is_err() {
                return Err(SearchError::InvalidIndexContainer);
            }
            return Err(SearchError::Index);
        }
        self.records.insert(metadata.key, metadata);
        Ok(replaced)
    }

    /// Refreshes non-content metadata without recomputing or replacing the
    /// record's embedding. Returns `false` when the stable record is not in the
    /// graph so callers can self-heal with a full upsert.
    pub fn refresh_metadata(&mut self, record: &SnapshotLikeRecord) -> Result<bool, SearchError> {
        let metadata = IndexedRecord::from(record);
        let Some(existing) = self.records.get_mut(&metadata.key) else {
            return Ok(false);
        };
        if existing.namespace != metadata.namespace || existing.stable_id != metadata.stable_id {
            return Err(SearchError::KeyCollision {
                first: format!("{}/{}", existing.namespace, existing.stable_id),
                second: format!("{}/{}", metadata.namespace, metadata.stable_id),
            });
        }
        existing.occurred_at_ms = metadata.occurred_at_ms;
        Ok(true)
    }

    pub fn delete(&mut self, namespace: &str, stable_id: &str) -> Result<bool, SearchError> {
        let key = derive_vector_key(namespace, stable_id);
        let Some(record) = self.records.get(&key) else {
            return Ok(false);
        };
        if record.namespace != namespace || record.stable_id != stable_id {
            return Err(SearchError::KeyCollision {
                first: format!("{}/{}", record.namespace, record.stable_id),
                second: format!("{namespace}/{stable_id}"),
            });
        }
        let removed = self.native.remove(key).map_err(|_| SearchError::Index)?;
        if removed == 0 {
            return Err(SearchError::InvalidIndexContainer);
        }
        self.records.remove(&key);
        Ok(true)
    }

    /// Atomically writes a checksummed native container with exact configuration
    /// metadata, deterministic key metadata, and the native USearch graph.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), SearchError> {
        let path = path.as_ref();
        let metadata = PersistedMetadata {
            key_derivation: crate::KEY_DERIVATION_VERSION.to_owned(),
            embedding_signature: self.embedding_signature,
            records: self.records.values().cloned().collect(),
        };
        let metadata_bytes =
            serde_json::to_vec(&metadata).map_err(|_| SearchError::InvalidMetadata)?;
        let mut native_bytes = vec![0_u8; self.native.serialized_length()];
        self.native
            .save_to_buffer(&mut native_bytes)
            .map_err(|_| SearchError::Index)?;
        let container = encode_container(&metadata_bytes, &native_bytes, self.records.len())?;
        atomic_private_write(path, &container)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, SearchError> {
        let bytes = read_private_file(path.as_ref())?;
        let decoded = decode_container(&bytes)?;
        let metadata: PersistedMetadata =
            serde_json::from_slice(decoded.metadata).map_err(|_| SearchError::InvalidMetadata)?;
        if metadata.key_derivation != crate::KEY_DERIVATION_VERSION {
            return Err(SearchError::InvalidMetadata);
        }

        let mut records = BTreeMap::new();
        let mut identities = BTreeSet::new();
        for record in metadata.records {
            if record.key != derive_vector_key(&record.namespace, &record.stable_id)
                || !identities.insert((record.namespace.clone(), record.stable_id.clone()))
                || records.insert(record.key, record).is_some()
            {
                return Err(SearchError::InvalidMetadata);
            }
        }
        if records.len() != decoded.record_count {
            return Err(SearchError::InvalidMetadata);
        }

        let native = new_native_index()?;
        native
            .load_from_buffer(decoded.native)
            .map_err(|_| SearchError::InvalidIndexContainer)?;
        if native.dimensions() != DIMENSIONS
            || native.connectivity() != CONNECTIVITY
            || native.size() != records.len()
            || records.keys().any(|key| !native.contains(*key))
        {
            return Err(SearchError::InvalidIndexContainer);
        }
        Ok(Self {
            native,
            records,
            embedding_signature: metadata.embedding_signature,
        })
    }
}

fn new_native_index() -> Result<Index, SearchError> {
    if usearch::version() != USEARCH_VERSION || CONNECTIVITY_BASE != CONNECTIVITY * 2 {
        return Err(SearchError::Index);
    }
    let options = IndexOptions {
        dimensions: DIMENSIONS,
        metric: MetricKind::IP,
        quantization: ScalarKind::F16,
        connectivity: CONNECTIVITY,
        expansion_add: 0,
        expansion_search: 0,
        multi: false,
    };
    let index = Index::new(&options).map_err(|_| SearchError::Index)?;
    if index.dimensions() != DIMENSIONS || index.connectivity() != CONNECTIVITY {
        return Err(SearchError::Index);
    }
    Ok(index)
}

#[derive(Serialize, Deserialize)]
struct PersistedMetadata {
    key_derivation: String,
    embedding_signature: [u8; 32],
    records: Vec<IndexedRecord>,
}

fn embedding_signature<E: Embedder + ?Sized>(embedder: &E) -> Result<[u8; 32], SearchError> {
    const PROBES: [&str; 3] = [
        "woof local semantic probe dog canine bicycle",
        "quarterly planning notes and project roadmap",
        "private memory search language revision check",
    ];
    let probes = PROBES.map(str::to_owned);
    let vectors = embedder.embed_batch(&probes)?;
    if vectors.len() != PROBES.len() {
        return Err(SearchError::EmbeddingCount);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"woof.embedding-signature.v1\0");
    for vector in vectors {
        validate_embedding(&vector)?;
        for value in vector {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    Ok(hasher.finalize().into())
}

fn encode_container(
    metadata: &[u8],
    native: &[u8],
    record_count: usize,
) -> Result<Vec<u8>, SearchError> {
    let metadata_len =
        u64::try_from(metadata.len()).map_err(|_| SearchError::InvalidIndexContainer)?;
    let native_len = u64::try_from(native.len()).map_err(|_| SearchError::InvalidIndexContainer)?;
    let record_count =
        u64::try_from(record_count).map_err(|_| SearchError::InvalidIndexContainer)?;
    let mut payload_hasher = Sha256::new();
    payload_hasher.update(metadata);
    payload_hasher.update(native);
    let checksum = payload_hasher.finalize();

    let mut output = Vec::with_capacity(CONTAINER_HEADER_BYTES + metadata.len() + native.len());
    output.extend_from_slice(CONTAINER_MAGIC);
    output.extend_from_slice(&INDEX_FORMAT_VERSION.to_le_bytes());
    output.extend_from_slice(&(CONTAINER_HEADER_BYTES as u16).to_le_bytes());
    output.extend_from_slice(&(DIMENSIONS as u32).to_le_bytes());
    output.push(METRIC_INNER_PRODUCT);
    output.push(SCALAR_F16);
    output.extend_from_slice(&(CONNECTIVITY as u16).to_le_bytes());
    output.extend_from_slice(&(CONNECTIVITY_BASE as u16).to_le_bytes());
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&record_count.to_le_bytes());
    output.extend_from_slice(&metadata_len.to_le_bytes());
    output.extend_from_slice(&native_len.to_le_bytes());
    output.extend_from_slice(&checksum);
    debug_assert_eq!(output.len(), CONTAINER_HEADER_BYTES);
    output.extend_from_slice(metadata);
    output.extend_from_slice(native);
    Ok(output)
}

struct DecodedContainer<'a> {
    record_count: usize,
    metadata: &'a [u8],
    native: &'a [u8],
}

fn decode_container(bytes: &[u8]) -> Result<DecodedContainer<'_>, SearchError> {
    if bytes.len() < CONTAINER_HEADER_BYTES || &bytes[..8] != CONTAINER_MAGIC {
        return Err(SearchError::InvalidIndexContainer);
    }
    let version = read_u16(bytes, 8)?;
    let header_bytes = read_u16(bytes, 10)? as usize;
    let dimensions = read_u32(bytes, 12)? as usize;
    let metric = bytes[16];
    let scalar = bytes[17];
    let connectivity = read_u16(bytes, 18)? as usize;
    let connectivity_base = read_u16(bytes, 20)? as usize;
    let reserved = read_u16(bytes, 22)?;
    if version != INDEX_FORMAT_VERSION
        || header_bytes != CONTAINER_HEADER_BYTES
        || dimensions != DIMENSIONS
        || metric != METRIC_INNER_PRODUCT
        || scalar != SCALAR_F16
        || connectivity != CONNECTIVITY
        || connectivity_base != CONNECTIVITY_BASE
        || reserved != 0
    {
        return Err(SearchError::InvalidIndexContainer);
    }

    let record_count =
        usize::try_from(read_u64(bytes, 24)?).map_err(|_| SearchError::InvalidIndexContainer)?;
    let metadata_len =
        usize::try_from(read_u64(bytes, 32)?).map_err(|_| SearchError::InvalidIndexContainer)?;
    let native_len =
        usize::try_from(read_u64(bytes, 40)?).map_err(|_| SearchError::InvalidIndexContainer)?;
    if metadata_len > MAX_METADATA_BYTES || native_len == 0 {
        return Err(SearchError::InvalidIndexContainer);
    }
    let payload_len = metadata_len
        .checked_add(native_len)
        .ok_or(SearchError::InvalidIndexContainer)?;
    if bytes.len() != header_bytes + payload_len {
        return Err(SearchError::InvalidIndexContainer);
    }
    let metadata_end = header_bytes + metadata_len;
    let metadata = &bytes[header_bytes..metadata_end];
    let native = &bytes[metadata_end..];
    let mut hasher = Sha256::new();
    hasher.update(metadata);
    hasher.update(native);
    if hasher.finalize().as_slice() != &bytes[48..80] {
        return Err(SearchError::InvalidIndexContainer);
    }
    Ok(DecodedContainer {
        record_count,
        metadata,
        native,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SearchError> {
    let value: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(SearchError::InvalidIndexContainer)?
        .try_into()
        .map_err(|_| SearchError::InvalidIndexContainer)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SearchError> {
    let value: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(SearchError::InvalidIndexContainer)?
        .try_into()
        .map_err(|_| SearchError::InvalidIndexContainer)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SearchError> {
    let value: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or(SearchError::InvalidIndexContainer)?
        .try_into()
        .map_err(|_| SearchError::InvalidIndexContainer)?;
    Ok(u64::from_le_bytes(value))
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), SearchError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path);

    let result = (|| -> Result<(), SearchError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("woof.vector-index");
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ))
}

fn sync_directory(path: &Path) -> Result<(), SearchError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn read_private_file(path: &Path) -> Result<Vec<u8>, SearchError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SearchError::InvalidIndexContainer);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
        return Err(SearchError::InvalidIndexContainer);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(SearchError::InsecurePermissions);
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(SearchError::InvalidIndexContainer);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SyntheticEmbedder;

    impl Embedder for SyntheticEmbedder {
        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError> {
            Ok(texts.iter().map(|text| synthetic_vector(text)).collect())
        }
    }

    fn synthetic_vector(text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; DIMENSIONS];
        let lower = text.to_ascii_lowercase();
        if lower.contains("dog") || lower.contains("boxer") {
            vector[0] = 1.0;
        } else if lower.contains("cat") {
            vector[1] = 1.0;
        } else {
            vector[2] = 1.0;
        }
        vector
    }

    fn records() -> Vec<SnapshotLikeRecord> {
        vec![
            SnapshotLikeRecord::snapshot("dog", "boxer dog at the park", 30),
            SnapshotLikeRecord::snapshot("cat", "cat sleeping on a chair", 20),
            SnapshotLikeRecord::snapshot("code", "Rust ownership notes", 10),
        ]
    }

    #[test]
    fn canonical_configuration_and_rebuild_search() {
        let (index, report) = VectorIndex::build(&SyntheticEmbedder, &records()).unwrap();
        assert_eq!(report.indexed, 3);
        assert_eq!(index.dimensions(), DIMENSIONS);
        assert_eq!(index.connectivity(), CONNECTIVITY);
        assert_eq!(CONNECTIVITY_BASE, CONNECTIVITY * 2);
        let hits = index
            .search_text(&SyntheticEmbedder, "my boxer dog", 3)
            .unwrap();
        assert_eq!(
            index
                .record(hits[0].key)
                .map(|record| record.stable_id.as_str()),
            Some("dog")
        );
    }

    #[test]
    fn delete_and_full_rebuild_are_consistent() {
        let (mut index, _) = VectorIndex::build(&SyntheticEmbedder, &records()).unwrap();
        assert!(index.delete("snapshot", "dog").unwrap());
        assert!(!index.delete("snapshot", "dog").unwrap());
        assert_eq!(index.len(), 2);
        let report = index.rebuild(&SyntheticEmbedder, &records()).unwrap();
        assert_eq!(report.indexed, 3);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn incremental_upsert_adds_replaces_and_removes_empty_records() {
        let (mut index, _) = VectorIndex::build(&SyntheticEmbedder, &records()).unwrap();
        assert!(!index
            .upsert(
                &SyntheticEmbedder,
                &SnapshotLikeRecord::snapshot("new", "cat project", 40),
            )
            .unwrap());
        let hits = index.search_text(&SyntheticEmbedder, "cat", 4).unwrap();
        assert!(hits.iter().any(|hit| {
            index
                .record(hit.key)
                .is_some_and(|record| record.stable_id == "new")
        }));

        assert!(index
            .upsert(
                &SyntheticEmbedder,
                &SnapshotLikeRecord::snapshot("new", "boxer project", 50),
            )
            .unwrap());
        assert_eq!(
            index
                .record(derive_vector_key("snapshot", "new"))
                .map(|record| record.occurred_at_ms),
            Some(50)
        );
        assert!(index
            .upsert(
                &SyntheticEmbedder,
                &SnapshotLikeRecord::snapshot("new", " ", 60),
            )
            .unwrap());
        assert!(index.record(derive_vector_key("snapshot", "new")).is_none());
    }

    #[test]
    fn metadata_refresh_preserves_the_existing_embedding() {
        let (mut index, _) = VectorIndex::build(&SyntheticEmbedder, &records()).unwrap();
        let before = index
            .search_text(&SyntheticEmbedder, "dog", 3)
            .unwrap()
            .into_iter()
            .map(|hit| (hit.key, hit.distance))
            .collect::<Vec<_>>();

        assert!(index
            .refresh_metadata(&SnapshotLikeRecord::snapshot(
                "dog",
                "content is deliberately ignored",
                9_999,
            ))
            .unwrap());
        assert_eq!(
            index
                .record(derive_vector_key("snapshot", "dog"))
                .map(|record| record.occurred_at_ms),
            Some(9_999)
        );
        let after = index
            .search_text(&SyntheticEmbedder, "dog", 3)
            .unwrap()
            .into_iter()
            .map(|hit| (hit.key, hit.distance))
            .collect::<Vec<_>>();
        assert_eq!(after, before);

        assert!(!index
            .refresh_metadata(&SnapshotLikeRecord::snapshot("missing", "dog", 1))
            .unwrap());
    }

    #[test]
    fn empty_text_is_skipped_and_duplicate_identity_is_rejected() {
        let mut values = records();
        values.push(SnapshotLikeRecord::snapshot("empty", "  ", 0));
        let (_, report) = VectorIndex::build(&SyntheticEmbedder, &values).unwrap();
        assert_eq!(report.skipped_empty, 1);

        let duplicate = vec![
            SnapshotLikeRecord::snapshot("same", "dog", 1),
            SnapshotLikeRecord::snapshot("same", "cat", 2),
        ];
        assert!(matches!(
            VectorIndex::build(&SyntheticEmbedder, &duplicate),
            Err(SearchError::DuplicateIdentity { .. })
        ));
    }

    #[test]
    fn save_load_is_atomic_private_and_checksummed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("woof.vector-index");
        let (index, _) = VectorIndex::build(&SyntheticEmbedder, &records()).unwrap();
        index.save_atomic(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp-")));

        let loaded = VectorIndex::load(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        let hits = loaded.search_text(&SyntheticEmbedder, "cat", 1).unwrap();
        assert_eq!(loaded.record(hits[0].key).unwrap().stable_id, "cat");

        let mut corrupt = fs::read(&path).unwrap();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        atomic_private_write(&path, &corrupt).unwrap();
        assert!(matches!(
            VectorIndex::load(&path),
            Err(SearchError::InvalidIndexContainer)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_nonprivate_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("woof.vector-index");
        let index = VectorIndex::empty().unwrap();
        index.save_atomic(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            VectorIndex::load(&path),
            Err(SearchError::InsecurePermissions)
        ));
    }
}
