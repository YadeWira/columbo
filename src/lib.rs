//! columbo — "Just One More Thing"
//!
//! Post-processor for existing Deflate streams.
//! Parses, re-encodes, and re-contains gzip/raw-deflate data
//! to save the last few bytes without changing decompressed output.
//!
//! # Architecture (v1)
//!
//! ```text
//! Container layer (gzip, raw deflate)
//!   → Block engine (5 candidates per block)
//!     → Huffman builder (package-merge, deterministic)
//!       → Header RLE encoder (greedy)
//! ```
//!
//! # References
//!
//! - `research/deflopt-methods.md`
//! - `research/deft4j-methods.md`
//! - `research/defluff-methods.md`
//! - `research/design-v1.md`

pub mod bit;
pub mod block;
pub mod container;
pub mod error;
pub mod huffman;
pub mod rle;
