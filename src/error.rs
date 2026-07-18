//! Error types for columbo.

use std::fmt;

/// Errors that can occur during parsing or optimisation.
#[derive(Debug)]
pub enum Error {
    /// Input ended unexpectedly (truncated stream).
    UnexpectedEof,
    /// Invalid Deflate block type (BTYPE=3).
    InvalidBlockType(u8),
    /// Invalid GZIP header (bad magic or CM field).
    InvalidGzipHeader,
    /// A required Huffman symbol has zero code length.
    MissingCode(u16),
    /// Block size exceeds implementation limit.
    BlockTooLarge(usize),
    /// I/O error from the underlying reader/writer.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnexpectedEof => write!(f, "unexpected end of file"),
            Error::InvalidBlockType(b) => write!(f, "invalid block type: {}", b),
            Error::InvalidGzipHeader => write!(f, "invalid gzip header"),
            Error::MissingCode(s) => write!(f, "required symbol {} has no code in table", s),
            Error::BlockTooLarge(n) => write!(f, "block too large: {} bytes", n),
            Error::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
