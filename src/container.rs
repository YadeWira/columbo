//! Container format detection and reconstruction.
//!
//! v1 supports GZIP and raw Deflate. PNG and ZIP are deferred to v2
//! (see `research/design-v1.md`).

use crate::block;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

// GZIP FLG bits (RFC 1952 §2.3.1).
#[allow(dead_code)] // documents the bit; not queried in v1 (no text/binary distinction needed)
const FLG_FTEXT: u8 = 0x01;
const FLG_FHCRC: u8 = 0x02;
const FLG_FEXTRA: u8 = 0x04;
const FLG_FNAME: u8 = 0x08;
const FLG_FCOMMENT: u8 = 0x10;

/// Detect the container format of `input` and return an optimised copy if
/// one was produced, or `None` if the format is unrecognised.
///
/// Enforces the whole-file "never larger than input" policy internally
/// (`research/design-v1.md`'s Container layer responsibility): a `Some`
/// result is always `<= input.len()`, falling back to the unmodified
/// input bytes if optimisation didn't help. This matters because the
/// 25-token gate (`block::SMALL_BLOCK_GATE`) forces tiny blocks straight
/// to fixed Huffman without comparing candidates, which can occasionally
/// grow an individual block relative to a cheaper original encoding —
/// the whole-file fallback absorbs that rather than requiring every
/// block-level decision to be strictly monotonic.
pub fn detect_and_optimise(input: &[u8]) -> Option<Vec<u8>> {
    let candidate = if input.len() >= 2 && input[0..2] == GZIP_MAGIC {
        optimise_gzip(input).ok()
    } else {
        // Fallback: try treating the whole input as a raw Deflate stream.
        optimise_raw_deflate(input).ok()
    };
    candidate.map(|out| if out.len() < input.len() { out } else { input.to_vec() })
}

/// Re-encode a GZIP member, preserving its header fields and trailer
/// (CRC-32, ISIZE) untouched — only the Deflate payload is re-encoded.
///
/// v1 supports a single member only (matches DeflOpt and defluff, neither
/// of which handle GZIP multi-member streams).
fn optimise_gzip(input: &[u8]) -> Result<Vec<u8>, crate::error::Error> {
    use crate::error::Error;

    if input.len() < 18 {
        // 10-byte header + at least an empty deflate stream + 8-byte trailer.
        return Err(Error::InvalidGzipHeader);
    }
    if input[0..2] != GZIP_MAGIC {
        return Err(Error::InvalidGzipHeader);
    }
    let cm = input[2];
    if cm != 8 {
        return Err(Error::InvalidGzipHeader);
    }
    let flg = input[3];

    let mut pos = 10usize; // ID1 ID2 CM FLG MTIME(4) XFL OS

    if flg & FLG_FEXTRA != 0 {
        if pos + 2 > input.len() {
            return Err(Error::UnexpectedEof);
        }
        let xlen = u16::from_le_bytes([input[pos], input[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    if flg & FLG_FNAME != 0 {
        pos = skip_cstring(input, pos)?;
    }
    if flg & FLG_FCOMMENT != 0 {
        pos = skip_cstring(input, pos)?;
    }
    if flg & FLG_FHCRC != 0 {
        pos += 2;
    }
    if pos > input.len() {
        return Err(Error::UnexpectedEof);
    }

    let header = &input[0..pos];
    if input.len() < pos + 8 {
        return Err(Error::UnexpectedEof);
    }
    let trailer_start = input.len() - 8;
    let deflate_payload = &input[pos..trailer_start];
    let trailer = &input[trailer_start..]; // CRC-32 (4) + ISIZE (4), little-endian, unchanged.

    let optimised_payload = block::optimise_deflate_stream(deflate_payload)?;

    let mut out = Vec::with_capacity(header.len() + optimised_payload.len() + trailer.len());
    out.extend_from_slice(header);
    out.extend_from_slice(&optimised_payload);
    out.extend_from_slice(trailer);
    Ok(out)
}

/// Advance past a NUL-terminated string (FNAME or FCOMMENT), returning the
/// position just after the terminating NUL.
fn skip_cstring(input: &[u8], start: usize) -> Result<usize, crate::error::Error> {
    let mut pos = start;
    loop {
        if pos >= input.len() {
            return Err(crate::error::Error::UnexpectedEof);
        }
        if input[pos] == 0 {
            return Ok(pos + 1);
        }
        pos += 1;
    }
}

/// Re-encode a bare Deflate stream with no container framing.
fn optimise_raw_deflate(input: &[u8]) -> Result<Vec<u8>, crate::error::Error> {
    block::optimise_deflate_stream(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_input_as_gzip() {
        let input = [0x1fu8, 0x8b, 0x08];
        assert!(optimise_gzip(&input).is_err());
    }

    #[test]
    fn detects_gzip_magic() {
        let mut data = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0xff];
        // Empty stored Deflate block (BFINAL=1, BTYPE=00, padded, LEN=0, NLEN=0xFFFF) + trailer.
        data.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // CRC32 + ISIZE
        assert!(detect_and_optimise(&data).is_some());
    }

    /// End-to-end roundtrip against a real encoder: compress with flate2
    /// (independent of columbo's own bit-writer), re-encode with columbo,
    /// then decompress with flate2 again and check byte-exact equality
    /// against the original input. This is the structural-validity layer
    /// from `research/design-v1.md`'s testing strategy.
    ///
    /// 4 of 5 block candidates are wired now (stored, fixed, dynamic-rebuilt,
    /// dynamic-original-fallback — see `block.rs`), and the fallback
    /// candidate alone guarantees each block is never larger than its
    /// original encoding. So unlike the earlier version of this test, no
    /// CLI-level "use original if not smaller" fallback should be needed
    /// here — asserting `optimised.len() <= compressed.len()` directly.
    fn roundtrip_gzip(data: &[u8], level: u32) {
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::{Read, Write};

        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();

        let optimised = detect_and_optimise(&compressed)
            .unwrap_or_else(|| panic!("columbo failed to process a real gzip stream (level {level})"));

        assert!(
            optimised.len() <= compressed.len(),
            "optimised output ({} bytes) larger than input ({} bytes) at level {level}",
            optimised.len(),
            compressed.len()
        );

        let mut decoder = GzDecoder::new(&optimised[..]);
        let mut roundtripped = Vec::new();
        decoder
            .read_to_end(&mut roundtripped)
            .unwrap_or_else(|e| panic!("columbo output failed to decompress at level {level}: {e}"));

        assert_eq!(
            roundtripped, data,
            "roundtrip mismatch at level {level}: decompressed output does not match original"
        );
    }

    #[test]
    fn roundtrip_various_levels_and_data() {
        let corpus: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"hello, world!".to_vec(),
            b"a".repeat(1000),
            (0..=255u8).cycle().take(10_000).collect(),
            b"The quick brown fox jumps over the lazy dog. ".repeat(200),
            vec![0u8; 5000],
            vec![0xFFu8; 5000],
        ];

        for data in &corpus {
            for level in [1u32, 6, 9] {
                roundtrip_gzip(data, level);
            }
        }
    }
}
