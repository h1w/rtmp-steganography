//! Reed–Solomon erasure code wrapper.
//! One block = 120 data bytes + 52 parity bytes = 172 bytes total.
//! Corrects up to 52 erasures per block (bytes whose position is marked unreliable).

use anyhow::{anyhow, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;

pub const RS_BLOCK_N: usize = 172;
pub const RS_BLOCK_K: usize = 120;
pub const RS_BLOCK_PARITY: usize = RS_BLOCK_N - RS_BLOCK_K; // 52

/// Encode `k` data bytes into `n` shard bytes (data + parity).
/// Reed-Solomon codec instance. Building one costs ~1-5 ms (Cauchy GF(256)
/// matrix init) — prohibitive to do per block on large grids where we encode
/// 20+ blocks per frame × 24 fps × two peers. Build it once and reuse.
static RS_CODEC: std::sync::OnceLock<ReedSolomon> = std::sync::OnceLock::new();
fn rs() -> &'static ReedSolomon {
    RS_CODEC.get_or_init(|| {
        ReedSolomon::new(RS_BLOCK_K, RS_BLOCK_PARITY)
            .expect("ReedSolomon::new(120, 52) must succeed — constants")
    })
}

/// Input must be exactly RS_BLOCK_K bytes; output is RS_BLOCK_N bytes.
pub fn encode_block(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() != RS_BLOCK_K {
        return Err(anyhow!("encode_block expects {} bytes, got {}", RS_BLOCK_K, data.len()));
    }
    // reed-solomon-erasure operates on shards-of-shards; we use byte-per-shard (shard size = 1).
    let mut shards: Vec<Vec<u8>> = data.iter().map(|b| vec![*b]).collect();
    for _ in 0..RS_BLOCK_PARITY {
        shards.push(vec![0u8]);
    }
    rs().encode(&mut shards).map_err(|e| anyhow!("RS encode: {e}"))?;
    Ok(shards.into_iter().map(|s| s[0]).collect())
}

/// Decode `n` bytes where some may be marked as erasures (None).
/// Returns the original `k` data bytes, or Err if too many erasures.
pub fn decode_block(shards: &[Option<u8>]) -> Result<Vec<u8>> {
    if shards.len() != RS_BLOCK_N {
        return Err(anyhow!("decode_block expects {} shards, got {}", RS_BLOCK_N, shards.len()));
    }
    let mut mutable: Vec<Option<Vec<u8>>> = shards.iter().map(|o| o.map(|b| vec![b])).collect();
    rs().reconstruct(&mut mutable).map_err(|e| anyhow!("RS decode: {e}"))?;
    let out: Vec<u8> = mutable.iter().take(RS_BLOCK_K).map(|o| o.as_ref().unwrap()[0]).collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_without_errors() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        assert_eq!(encoded.len(), RS_BLOCK_N);
        let shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        let decoded = decode_block(&shards).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn recovers_up_to_52_erasures() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        let mut shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        // Erase 52 positions scattered.
        for i in (0..RS_BLOCK_N).step_by(3).take(52) {
            shards[i] = None;
        }
        let decoded = decode_block(&shards).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn fails_with_more_than_52_erasures() {
        let data: Vec<u8> = (0..RS_BLOCK_K as u8).collect();
        let encoded = encode_block(&data).unwrap();
        let mut shards: Vec<Option<u8>> = encoded.iter().map(|b| Some(*b)).collect();
        for i in 0..53 {
            shards[i] = None;
        }
        assert!(decode_block(&shards).is_err());
    }
}
