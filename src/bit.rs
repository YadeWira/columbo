//! Bit-level I/O for reading and writing Deflate streams.
//!
//! Deflate transmits bits least-significant-bit first within bytes.
//! This module provides buffered readers and writers that handle
//! the bit-level protocol.

use crate::error::Error;

/// Bit reader wrapping a byte slice with LSB-first bit order.
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Next byte position in `bytes`.
    pos: usize,
    /// Current bit accumulator.
    buf: u32,
    /// Number of valid bits in `buf` (0..32).
    bits: u32,
}

impl<'a> BitReader<'a> {
    /// Create a new bit reader from a byte slice.
    pub fn new(bytes: &'a [u8]) -> Self {
        BitReader {
            bytes,
            pos: 0,
            buf: 0,
            bits: 0,
        }
    }

    /// Ensure there are at least `n` bits in the buffer (n ≤ 32).
    /// Returns the number of bits actually available.
    fn ensure(&mut self, n: u32) -> Result<u32, Error> {
        debug_assert!(n <= 32);
        while self.bits < n {
            if self.pos >= self.bytes.len() {
                return Err(Error::UnexpectedEof);
            }
            self.buf |= (self.bytes[self.pos] as u32) << self.bits;
            self.bits += 8;
            self.pos += 1;
        }
        Ok(self.bits.min(n))
    }

    /// Read `n` bits (1..32) from the stream. LSB-first: the first bit
    /// read is the least significant bit of the first byte.
    pub fn read_bits(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        debug_assert!(n <= 32);
        self.ensure(n)?;
        let mask = (1u64 << n).wrapping_sub(1) as u32;
        let value = self.buf & mask;
        self.buf >>= n;
        self.bits -= n;
        Ok(value)
    }

    /// Read a single bit.
    pub fn read_bit(&mut self) -> Result<bool, Error> {
        Ok(self.read_bits(1)? != 0)
    }

    /// Align to the next byte boundary (discard remaining bits in buffer).
    pub fn align_to_byte(&mut self) {
        let drop = self.bits & 7;
        self.buf >>= drop;
        self.bits -= drop;
    }

    /// Return the current byte position (bits consumed / 8).
    pub fn byte_position(&self) -> usize {
        self.pos - (self.bits as usize / 8)
    }

    /// Total number of bits consumed so far.
    pub fn bits_consumed(&self) -> u64 {
        (self.pos as u64 * 8) - self.bits as u64
    }
}

/// Bit writer building a byte vector with LSB-first bit order.
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Current bit accumulator.
    buf: u32,
    /// Number of bits in `buf` (0..32).
    bits: u32,
}

impl BitWriter {
    /// Create a new, empty bit writer.
    pub fn new() -> Self {
        BitWriter {
            bytes: Vec::new(),
            buf: 0,
            bits: 0,
        }
    }

    /// Write `n` bits of `value` to the stream (n ≤ 32).
    /// Bits beyond `n` in `value` are ignored.
    pub fn write_bits(&mut self, n: u32, value: u32) {
        if n == 0 {
            return;
        }
        debug_assert!(n <= 32);
        let mask = (1u64 << n).wrapping_sub(1) as u32;
        self.buf |= (value & mask) << self.bits;
        self.bits += n;
        self.drain();
    }

    /// Align to the next byte boundary by writing zero bits.
    pub fn align_to_byte(&mut self) {
        let pad = (8 - (self.bits & 7)) & 7;
        if pad > 0 {
            self.write_bits(pad, 0);
        }
    }

    /// Flush remaining bits to the byte vector.
    fn drain(&mut self) {
        while self.bits >= 8 {
            self.bytes.push((self.buf & 0xFF) as u8);
            self.buf >>= 8;
            self.bits -= 8;
        }
    }

    /// Consume the writer and return the byte vector, flushing any
    /// remaining bits.
    pub fn into_bytes(mut self) -> Vec<u8> {
        self.drain();
        if self.bits > 0 {
            // Partial byte — pad with zeros
            self.bytes.push((self.buf & 0xFF) as u8);
        }
        self.bytes
    }

    /// Return the current bit position (0 = byte-aligned).
    pub fn bit_position(&self) -> u32 {
        self.bits
    }

    /// Current byte length of the output.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Total bits written so far.
    pub fn bits_written(&self) -> u64 {
        (self.bytes.len() as u64 * 8) + self.bits as u64
    }
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read_roundtrip() {
        let mut w = BitWriter::new();
        w.write_bits(1, 1);   // BFINAL=1
        w.write_bits(2, 1);   // BTYPE=1 (fixed)
        w.write_bits(7, 0);   // EOB symbol 256
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(1).unwrap(), 1);
        assert_eq!(r.read_bits(2).unwrap(), 1);
        assert_eq!(r.read_bits(7).unwrap(), 0);
    }

    #[test]
    fn test_byte_alignment() {
        let mut w = BitWriter::new();
        w.write_bits(3, 5);     // 3 arbitrary bits
        w.align_to_byte();       // pad to byte
        w.write_bits(8, 0xAB); // full byte
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 5);
        r.align_to_byte();
        assert_eq!(r.read_bits(8).unwrap(), 0xAB);
    }

    #[test]
    fn test_multi_byte_sequence() {
        let mut w = BitWriter::new();
        // Write 16 bits, spanning byte boundaries
        w.write_bits(16, 0b1010101100111101);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(16).unwrap(), 0b1010101100111101);
    }

    #[test]
    fn test_eof_detection() {
        let r = BitReader::new(&[0x42]);
        let mut r2 = r;
        assert!(r2.read_bits(9).is_err()); // can't read 9 bits from 1 byte
    }

    #[test]
    fn test_write_zero_bits() {
        let mut w = BitWriter::new();
        w.write_bits(0, 0xFFFF);
        w.write_bits(3, 7);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 7);
    }
}
