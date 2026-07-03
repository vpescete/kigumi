//! Content-addressed blob storage. Binary attachments live behind an interchangeable [`BlobStore`],
//! keyed by the sha256 of their content: identical bytes dedupe to one immutable object, and every
//! `put` re-verifies the hash so the content-address invariant is enforced, never trusted.
//!
//! Ships [`FsBlobStore`] (files at `<root>/ab/cd/<hash>`) and, behind the `s3` feature,
//! [`S3BlobStore`] (objects at `ab/cd/<hash>` in an S3-compatible bucket: AWS, MinIO, R2). The API is
//! buffered (a whole blob is held in memory); streaming for very large blobs (the backup/restore
//! path) is a later enhancement — attachments are single files of modest size.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Errors a blob store can return.
#[derive(Debug)]
pub enum BlobError {
    /// The bytes' actual sha256 did not match the claimed address — the content-address invariant.
    HashMismatch { expected: String, actual: String },
    /// No blob is stored under the given hash.
    NotFound(String),
    /// The key is not a 64-character lowercase-hex sha256.
    BadHash(String),
    /// An underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlobError::HashMismatch { expected, actual } => {
                write!(f, "blob hash mismatch: claimed {expected}, got {actual}")
            }
            BlobError::NotFound(h) => write!(f, "blob not found: {h}"),
            BlobError::BadHash(h) => write!(f, "invalid blob hash: {h}"),
            BlobError::Io(e) => write!(f, "blob io error: {e}"),
        }
    }
}
impl std::error::Error for BlobError {}
impl From<std::io::Error> for BlobError {
    fn from(e: std::io::Error) -> Self {
        BlobError::Io(e)
    }
}

/// The sha256 of `bytes` as lowercase hex — the content address used as a blob key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// A 64-char lowercase-hex sha256.
fn valid_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An interchangeable, content-addressed binary store.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Stores `bytes` under `sha256`. MUST reject if `sha256(bytes) != sha256` (verified, never
    /// trusted). Idempotent: storing an already-present blob is a no-op (content-addressed dedup).
    async fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), BlobError>;
    /// Reads a blob's bytes, or [`BlobError::NotFound`].
    async fn get(&self, sha256: &str) -> Result<Vec<u8>, BlobError>;
    /// Whether a blob is stored under `sha256`.
    async fn exists(&self, sha256: &str) -> Result<bool, BlobError>;
    /// Removes a blob (GC only — blobs are otherwise immutable). Idempotent.
    async fn delete(&self, sha256: &str) -> Result<(), BlobError>;
}

/// Filesystem blob store: immutable, deduplicated files at `<root>/ab/cd/<hash>`.
///
/// `root` must be an exclusive, trusted directory (only this store writes there): a dedup hit returns
/// early without re-hashing, and `get` returns the file bytes without re-verifying, so the safety of
/// serving an existing blob rests on `root`'s immutability + exclusivity. A `get` for a hash whose
/// `put` has not yet committed its rename returns `NotFound` — callers store a blob before serving it.
pub struct FsBlobStore {
    root: PathBuf,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsBlobStore { root: root.into() }
    }
    /// `<root>/<h[0:2]>/<h[2:4]>/<h>` — a two-level fan-out so no directory holds millions of files.
    fn path_for(&self, sha256: &str) -> PathBuf {
        self.root
            .join(&sha256[0..2])
            .join(&sha256[2..4])
            .join(sha256)
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), BlobError> {
        if !valid_hash(sha256) {
            return Err(BlobError::BadHash(sha256.to_string()));
        }
        // Enforce the content-address invariant: the bytes must actually hash to the claimed key.
        let actual = sha256_hex(bytes);
        if actual != sha256 {
            return Err(BlobError::HashMismatch {
                expected: sha256.to_string(),
                actual,
            });
        }
        let path = self.path_for(sha256);
        if tokio::fs::try_exists(&path).await? {
            return Ok(()); // dedup: already stored, and immutable, so the content is identical
        }
        let dir = path.parent().expect("path_for always has a parent");
        tokio::fs::create_dir_all(dir).await?;
        // Atomic publish: write a unique temp file in the SAME directory, then rename into place.
        let tmp = dir.join(format!(
            ".tmp.{}.{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // Remove the temp on ANY failure (write or rename) so a disk-full / interrupted put leaves no
        // orphan behind.
        if let Err(e) = tokio::fs::write(&tmp, bytes).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(BlobError::Io(e));
        }
        // rename is atomic on one filesystem. If another writer won the race, the destination holds
        // identical bytes (content-addressed), so the overwrite is harmless.
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(BlobError::Io(e));
        }
        Ok(())
    }

    async fn get(&self, sha256: &str) -> Result<Vec<u8>, BlobError> {
        if !valid_hash(sha256) {
            return Err(BlobError::BadHash(sha256.to_string()));
        }
        match tokio::fs::read(self.path_for(sha256)).await {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(BlobError::NotFound(sha256.to_string()))
            }
            Err(e) => Err(BlobError::Io(e)),
        }
    }

    async fn exists(&self, sha256: &str) -> Result<bool, BlobError> {
        if !valid_hash(sha256) {
            return Err(BlobError::BadHash(sha256.to_string()));
        }
        Ok(tokio::fs::try_exists(self.path_for(sha256)).await?)
    }

    async fn delete(&self, sha256: &str) -> Result<(), BlobError> {
        if !valid_hash(sha256) {
            return Err(BlobError::BadHash(sha256.to_string()));
        }
        match tokio::fs::remove_file(self.path_for(sha256)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // idempotent
            Err(e) => Err(BlobError::Io(e)),
        }
    }
}

#[cfg(feature = "s3")]
pub use s3_store::S3BlobStore;

#[cfg(feature = "s3")]
mod s3_store {
    use super::{sha256_hex, valid_hash, BlobError, BlobStore};
    use async_trait::async_trait;
    use s3::{creds::Credentials, region::Region, Bucket};

    /// The two-level fan-out key for a blob: `<h[0:2]>/<h[2:4]>/<h>`, mirroring the fs store's layout.
    fn blob_key(sha256: &str) -> String {
        format!("{}/{}/{}", &sha256[0..2], &sha256[2..4], sha256)
    }

    /// Any S3-SDK error (config, credentials, transport) is an I/O failure to a blob caller — mapped
    /// to [`BlobError::Io`] so the trait's error surface stays backend-agnostic (no new variant).
    fn s3_io<E: Into<Box<dyn std::error::Error + Send + Sync>>>(e: E) -> BlobError {
        BlobError::Io(std::io::Error::other(e))
    }

    /// Content-addressed blob store over an S3-compatible bucket (AWS S3, MinIO, Cloudflare R2).
    ///
    /// Keys mirror [`FsBlobStore`](super::FsBlobStore): `ab/cd/<hash>`. Credentials come from the
    /// standard AWS chain (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`, then
    /// profile, then IAM) — never from config. A non-AWS `endpoint` (MinIO/R2) implies path-style
    /// addressing. Built with `fail-on-err` off, so this store inspects HTTP status codes directly
    /// (via `head_status`) — an ambiguous status (403/5xx) is an error, never a false "exists", so a
    /// transient HEAD failure can never make `put` silently skip an upload.
    ///
    /// Unlike the fs store, a bucket is NOT assumed exclusive: another principal (console, lifecycle,
    /// a co-tenant app) could replace an object out-of-band, so [`get`](Self::get) re-verifies the
    /// content address on read — the invariant is verified, never trusted, on both write and read.
    pub struct S3BlobStore {
        bucket: Box<Bucket>,
    }

    impl S3BlobStore {
        /// `bucket` and `region` come from `[storage]` config; `endpoint` (for MinIO/R2/custom) and
        /// credentials come from the environment. `endpoint = None` targets real AWS S3.
        pub fn new(bucket: &str, region: &str, endpoint: Option<&str>) -> Result<Self, BlobError> {
            // Validate the region BEFORE touching the credential chain, so an obvious config typo
            // fails fast and independently of the environment.
            let region = match endpoint {
                Some(ep) => Region::Custom { region: region.to_string(), endpoint: ep.to_string() },
                None => {
                    // aws-region's FromStr never rejects: an unknown string degrades to a Custom
                    // region whose ENDPOINT is that string, which would silently send credentialed,
                    // SigV4-signed requests to a stray host. Reaching a custom host must require the
                    // explicit endpoint, so a region that does not resolve to a real AWS region is an
                    // error here, not a mystery connection failure later.
                    match region.parse::<Region>().map_err(s3_io)? {
                        Region::Custom { .. } => {
                            return Err(s3_io(format!(
                                "unknown S3 region '{region}'; set KIGUMI_S3_ENDPOINT to target a custom S3-compatible host"
                            )))
                        }
                        r => r,
                    }
                }
            };
            let creds = Credentials::default().map_err(s3_io)?;
            let mut b = Bucket::new(bucket, region, creds).map_err(s3_io)?;
            // A custom endpoint (MinIO, R2, LocalStack) needs path-style URLs; virtual-host style is
            // AWS-only. On real AWS this stays the default virtual-host style.
            if endpoint.is_some() {
                b = b.with_path_style();
            }
            Ok(S3BlobStore { bucket: b })
        }

        /// HEAD → present/absent, mapping the raw HTTP status: `Some(true)` = 2xx (present),
        /// `Some(false)` = 404 (absent), any other status (403/5xx) = `Err`. This is the safe
        /// primitive `object_exists` is NOT: with `fail-on-err` off it collapses every non-404 to
        /// "exists", so a 403/5xx would read as present and let `put` skip the upload.
        async fn head_present(&self, key: &str) -> Result<bool, BlobError> {
            let (_, status) = self.bucket.head_object(key).await.map_err(s3_io)?;
            match status {
                200..=299 => Ok(true),
                404 => Ok(false),
                code => Err(s3_io(format!("s3 head {key} returned status {code}"))),
            }
        }
    }

    #[async_trait]
    impl BlobStore for S3BlobStore {
        async fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), BlobError> {
            if !valid_hash(sha256) {
                return Err(BlobError::BadHash(sha256.to_string()));
            }
            // Same content-address invariant as the fs store: the bytes must hash to the claimed key.
            let actual = sha256_hex(bytes);
            if actual != sha256 {
                return Err(BlobError::HashMismatch { expected: sha256.to_string(), actual });
            }
            let key = blob_key(sha256);
            // Dedup: an object under this key already holds identical bytes (content-addressed), so a
            // re-upload is wasted bandwidth. A cheap HEAD beats a redundant PUT of the whole blob.
            // head_present errors (not "exists") on an ambiguous status, so a transient/403 HEAD
            // surfaces instead of silently skipping the upload.
            if self.head_present(&key).await? {
                return Ok(());
            }
            let resp = self.bucket.put_object(&key, bytes).await.map_err(s3_io)?;
            match resp.status_code() {
                200..=299 => Ok(()),
                code => Err(s3_io(format!("s3 put {key} returned status {code}"))),
            }
        }

        async fn get(&self, sha256: &str) -> Result<Vec<u8>, BlobError> {
            if !valid_hash(sha256) {
                return Err(BlobError::BadHash(sha256.to_string()));
            }
            let key = blob_key(sha256);
            let resp = self.bucket.get_object(&key).await.map_err(s3_io)?;
            match resp.status_code() {
                200..=299 => {
                    let bytes = resp.bytes().to_vec();
                    // Re-verify on read: a shared/remote bucket is not the fs store's exclusive dir,
                    // so an object could have been replaced out-of-band. Refuse to serve bytes that do
                    // not hash to the requested key rather than trust the store's contents.
                    let actual = sha256_hex(&bytes);
                    if actual != sha256 {
                        return Err(BlobError::HashMismatch { expected: sha256.to_string(), actual });
                    }
                    Ok(bytes)
                }
                404 => Err(BlobError::NotFound(sha256.to_string())),
                code => Err(s3_io(format!("s3 get {key} returned status {code}"))),
            }
        }

        async fn exists(&self, sha256: &str) -> Result<bool, BlobError> {
            if !valid_hash(sha256) {
                return Err(BlobError::BadHash(sha256.to_string()));
            }
            self.head_present(&blob_key(sha256)).await
        }

        async fn delete(&self, sha256: &str) -> Result<(), BlobError> {
            if !valid_hash(sha256) {
                return Err(BlobError::BadHash(sha256.to_string()));
            }
            let key = blob_key(sha256);
            let resp = self.bucket.delete_object(&key).await.map_err(s3_io)?;
            match resp.status_code() {
                // 204 = deleted, 404 = already gone: both satisfy the idempotent contract.
                200..=299 | 404 => Ok(()),
                code => Err(s3_io(format!("s3 delete {key} returned status {code}"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (FsBlobStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (FsBlobStore::new(dir.path()), dir)
    }

    #[tokio::test]
    async fn put_get_roundtrip_and_dedup() {
        let (s, _d) = store();
        let bytes = b"hello blob";
        let h = sha256_hex(bytes);
        assert!(!s.exists(&h).await.unwrap());
        s.put(&h, bytes).await.unwrap();
        assert!(s.exists(&h).await.unwrap());
        assert_eq!(s.get(&h).await.unwrap(), bytes);
        // Idempotent: putting the same content again is a no-op.
        s.put(&h, bytes).await.unwrap();
        assert_eq!(s.get(&h).await.unwrap(), bytes);
        // Content-addressed layout: <root>/ab/cd/<hash>.
        assert!(s
            .path_for(&h)
            .ends_with(format!("{}/{}/{}", &h[0..2], &h[2..4], h)));
    }

    #[tokio::test]
    async fn put_rejects_hash_mismatch() {
        let (s, _d) = store();
        let real = sha256_hex(b"correct");
        // Claim the hash of "correct" but supply different bytes → rejected, nothing stored.
        let err = s.put(&real, b"tampered").await;
        assert!(matches!(err, Err(BlobError::HashMismatch { .. })));
        assert!(
            !s.exists(&real).await.unwrap(),
            "rejected blob was not stored"
        );
    }

    #[tokio::test]
    async fn delete_is_idempotent_and_get_missing_is_not_found() {
        let (s, _d) = store();
        let bytes = b"to delete";
        let h = sha256_hex(bytes);
        s.put(&h, bytes).await.unwrap();
        s.delete(&h).await.unwrap();
        assert!(!s.exists(&h).await.unwrap());
        assert!(matches!(s.get(&h).await, Err(BlobError::NotFound(_))));
        s.delete(&h).await.unwrap(); // deleting an absent blob is fine
    }

    #[tokio::test]
    async fn rejects_invalid_hash_keys() {
        let (s, _d) = store();
        assert!(matches!(
            s.get("not-a-hash").await,
            Err(BlobError::BadHash(_))
        ));
        assert!(matches!(
            s.put("xyz", b"x").await,
            Err(BlobError::BadHash(_))
        ));
        // An uppercase hash is rejected (keys are canonical lowercase hex).
        let upper = sha256_hex(b"x").to_uppercase();
        assert!(matches!(s.exists(&upper).await, Err(BlobError::BadHash(_))));
    }
}

// Live S3 round-trip, gated on both the `s3` feature and a configured bucket. Skips (does not fail)
// when the env is absent, like the DB-backed tests — CI without a MinIO/S3 target just skips it.
// Run against a local MinIO with:
//   AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//   KIGUMI_S3_TEST_BUCKET=kigumi-test KIGUMI_S3_TEST_ENDPOINT=http://127.0.0.1:9000 \
//   cargo test -p kigumi-storage --features s3 -- --ignored
#[cfg(all(test, feature = "s3"))]
mod s3_tests {
    use super::*;

    // Network-free: the region is validated before the credential chain, so an unknown region with no
    // explicit endpoint is rejected rather than silently used as a host (credential-exfil guard).
    #[test]
    fn rejects_unknown_region_without_endpoint() {
        assert!(S3BlobStore::new("b", "not-a-real-region", None).is_err());
    }

    #[tokio::test]
    #[ignore = "requires a live S3-compatible bucket (KIGUMI_S3_TEST_BUCKET/ENDPOINT)"]
    async fn s3_roundtrip_dedup_and_delete() {
        let (Ok(bucket), endpoint) =
            (std::env::var("KIGUMI_S3_TEST_BUCKET"), std::env::var("KIGUMI_S3_TEST_ENDPOINT").ok())
        else {
            eprintln!("skipping: KIGUMI_S3_TEST_BUCKET unset");
            return;
        };
        let region = std::env::var("KIGUMI_S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".into());
        let s = S3BlobStore::new(&bucket, &region, endpoint.as_deref()).unwrap();

        // Unique bytes per run so a shared test bucket does not cross-contaminate.
        let bytes = format!("kigumi s3 blob {}", std::process::id()).into_bytes();
        let h = sha256_hex(&bytes);

        s.delete(&h).await.unwrap(); // clean slate; idempotent
        assert!(!s.exists(&h).await.unwrap());
        s.put(&h, &bytes).await.unwrap();
        assert!(s.exists(&h).await.unwrap());
        assert_eq!(s.get(&h).await.unwrap(), bytes);
        s.put(&h, &bytes).await.unwrap(); // dedup: no-op
        assert_eq!(s.get(&h).await.unwrap(), bytes);

        // Content-address invariant holds over S3 too.
        assert!(matches!(s.put(&h, b"tampered").await, Err(BlobError::HashMismatch { .. })));

        s.delete(&h).await.unwrap();
        assert!(!s.exists(&h).await.unwrap());
        assert!(matches!(s.get(&h).await, Err(BlobError::NotFound(_))));
        s.delete(&h).await.unwrap(); // idempotent
    }
}
