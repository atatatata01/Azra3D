//! On-disk envelope for the wgpu pipeline cache (`pipeline_cache.bin`).
//!
//! wgpu 25's own header (`WGPUPLCH`) records a payload length but does **not**
//! checksum the driver blob (`hash_space` is a reserved constant — "the end
//! application" is responsible). A length-consistent but corrupt blob still
//! reaches `vkCreatePipelineCache` and, on NVIDIA, can hang the next launch.
//!
//! This envelope is what we persist. The bytes passed to
//! `Device::create_pipeline_cache` are the validated payload only.
//!
//! Layout, little-endian, no padding:
//! ```text
//!  0  magic        [u8; 4]  b"AZPC"
//!  4  version      u32      1
//!  8  payload_len  u64      byte length of the wgpu blob
//! 16  checksum     u64      FNV-1a-64 of the payload (length mixed in)
//! 24  payload      [u8; payload_len]
//! ```

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAGIC: [u8; 4] = *b"AZPC";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 24;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Why a cache file was refused. Callers log this and compile pipelines cold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    TooSmall {
        len: usize,
    },
    BadMagic,
    BadVersion {
        found: u32,
    },
    /// `payload_len` does not fit the bytes that follow the header.
    Truncated {
        claimed: u64,
        available: usize,
    },
    /// Bytes after the declared payload. A torn or concatenated write.
    Trailing {
        extra: usize,
    },
    BadChecksum,
    EmptyPayload,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reject::TooSmall { len } => {
                write!(f, "too small ({len} bytes, header is {HEADER_LEN})")
            }
            Reject::BadMagic => write!(f, "bad magic (not an AZPC cache)"),
            Reject::BadVersion { found } => {
                write!(f, "unsupported version {found} (want {VERSION})")
            }
            Reject::Truncated { claimed, available } => {
                write!(
                    f,
                    "truncated (header claims {claimed} payload bytes, {available} present)"
                )
            }
            Reject::Trailing { extra } => write!(f, "trailing {extra} bytes past payload"),
            Reject::BadChecksum => write!(f, "checksum mismatch"),
            Reject::EmptyPayload => write!(f, "empty payload"),
        }
    }
}

/// FNV-1a 64-bit over `data`, with the length mixed in so two buffers that
/// share a prefix and differ only in length do not collide.
pub fn digest(data: &[u8]) -> (usize, u64) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= data.len() as u64;
    h = h.wrapping_mul(0x0100_0000_01b3);
    (data.len(), h)
}

/// Build the on-disk bytes for `payload` (the raw `PipelineCache::get_data()` blob).
pub fn encode(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&digest(payload).1.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Validate `bytes` and return the wgpu payload on success.
///
/// Does not allocate. A truncated or corrupt file is rejected here, before
/// any driver call.
pub fn validate(bytes: &[u8]) -> Result<&[u8], Reject> {
    if bytes.len() < HEADER_LEN {
        return Err(Reject::TooSmall { len: bytes.len() });
    }
    if bytes[..4] != MAGIC {
        return Err(Reject::BadMagic);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != VERSION {
        return Err(Reject::BadVersion { found: version });
    }
    let claimed = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let stored_sum = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let rest = &bytes[HEADER_LEN..];
    let Ok(claimed_usize) = usize::try_from(claimed) else {
        return Err(Reject::Truncated {
            claimed,
            available: rest.len(),
        });
    };
    if rest.len() < claimed_usize {
        return Err(Reject::Truncated {
            claimed,
            available: rest.len(),
        });
    }
    if rest.len() > claimed_usize {
        return Err(Reject::Trailing {
            extra: rest.len() - claimed_usize,
        });
    }
    let payload = &rest[..claimed_usize];
    if payload.is_empty() {
        return Err(Reject::EmptyPayload);
    }
    if digest(payload).1 != stored_sum {
        return Err(Reject::BadChecksum);
    }
    Ok(payload)
}

/// Write `payload` as a complete envelope: unique temp file in the same
/// directory, flush + `sync_all`, then rename over `path`.
///
/// The destination is either the previous complete file or the new complete
/// file. A crash mid-write leaves the temp file and does not truncate `path`.
/// Two callers never share a temp name (`pid` + counter).
pub fn write_atomic(path: &Path, payload: &[u8]) -> io::Result<()> {
    if payload.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pipeline cache payload is empty",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = tmp_path(path);
    let encoded = encode(payload);
    let write_result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(&encoded)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Best-effort: make the directory entry durable. Opening a directory
    // fails on Windows; the file `sync_all` above is the required flush.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "pipeline_cache.bin".into());
    name.push(format!(".{}.{seq}.tmp", std::process::id()));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_known_vectors() {
        assert_eq!(digest(b"azra").1, 0x641e_9174_1f5f_d1a5);
        assert_eq!(digest(b"").1, 0xaf63_bd4c_8601_b7df);
        assert_eq!(
            digest(&(0u8..32).collect::<Vec<_>>()).1,
            0x4013_a150_50e8_031f
        );
    }

    #[test]
    fn roundtrip_header_version_size_checksum() {
        let payload = b"wgpu-cache-blob-not-empty";
        let bytes = encode(payload);
        assert_eq!(&bytes[..4], b"AZPC");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), VERSION);
        assert_eq!(
            u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            payload.len() as u64
        );
        assert_eq!(validate(&bytes).unwrap(), payload);
    }

    #[test]
    fn rejects_truncated_file() {
        let bytes = encode(b"abcdefghijklmnopqrstuvwxyz");
        let cut = &bytes[..bytes.len() / 2];
        assert!(matches!(
            validate(cut),
            Err(Reject::TooSmall { .. } | Reject::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_truncated_payload_with_intact_header() {
        let bytes = encode(&[0xAB; 64]);
        // Keep the header (which claims 64 bytes) but drop the tail.
        let cut = &bytes[..HEADER_LEN + 10];
        match validate(cut) {
            Err(Reject::Truncated { claimed, available }) => {
                assert_eq!(claimed, 64);
                assert_eq!(available, 10);
            }
            other => panic!("expected truncated, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode(b"payload");
        bytes[0] = b'X';
        assert_eq!(validate(&bytes), Err(Reject::BadMagic));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = encode(b"payload");
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(validate(&bytes), Err(Reject::BadVersion { found: 99 }));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut bytes = encode(b"payload-bytes");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(validate(&bytes), Err(Reject::BadChecksum));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode(b"payload-bytes");
        bytes.push(0);
        assert_eq!(validate(&bytes), Err(Reject::Trailing { extra: 1 }));
    }

    #[test]
    fn rejects_empty_payload() {
        let bytes = encode(b"");
        assert_eq!(validate(&bytes), Err(Reject::EmptyPayload));
    }

    #[test]
    fn rejects_size_field_that_does_not_match() {
        let mut bytes = encode(b"0123456789");
        // Claim a shorter payload while leaving the original bytes in place.
        bytes[8..16].copy_from_slice(&4u64.to_le_bytes());
        assert_eq!(validate(&bytes), Err(Reject::Trailing { extra: 6 }));
    }

    #[test]
    fn atomic_write_roundtrip_and_no_temp_left_behind() {
        let dir = std::env::temp_dir().join(format!(
            "azra-pipeline-cache-test-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pipeline_cache.bin");
        let payload = b"cold-cache-payload-v1";
        write_atomic(&path, payload).unwrap();
        let read = std::fs::read(&path).unwrap();
        assert_eq!(validate(&read).unwrap(), payload);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_replace_does_not_publish_a_truncated_file() {
        let dir = std::env::temp_dir().join(format!(
            "azra-pipeline-cache-trunc-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pipeline_cache.bin");
        write_atomic(&path, b"first-complete-cache").unwrap();
        // Simulate a crash that wrote a short prefix of a new envelope directly
        // onto a temp name and never renamed. The published file stays valid.
        let tmp = tmp_path(&path);
        std::fs::write(&tmp, &encode(b"second-complete-cache")[..8]).unwrap();
        let still = std::fs::read(&path).unwrap();
        assert_eq!(validate(&still).unwrap(), b"first-complete-cache");
        // And a truncated destination (the pre-fix failure mode) is rejected.
        std::fs::write(&path, &encode(b"second-complete-cache")[..10]).unwrap();
        assert!(validate(&std::fs::read(&path).unwrap()).is_err());
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
