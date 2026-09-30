// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Andrew C.E. Dent <https://github.com/ace-dent>

//! Checksum helpers shared by unit-test fixtures.

use super::adler32_update;

pub(crate) fn adler32(bytes: &[u8]) -> u32 {
    adler32_update(1, bytes)
}
