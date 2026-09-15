// SPDX-License-Identifier: MIT

//! Range minima shared by searches over canonical match-length families.

/// Incremental suffix minima for the at-most-32-width length families.
/// Six power-of-two ranges answer each family query with two lookups. Store
/// positions rather than costs so equal prices retain the lowest position.
pub(crate) struct SuffixMinima {
    positions: [[u16; 259]; 6],
}

impl SuffixMinima {
    pub(crate) fn new(end: usize) -> Self {
        Self {
            positions: [[end as u16; 259]; 6],
        }
    }

    pub(crate) fn insert(&mut self, start: usize, end: usize, costs: &[u64; 259]) {
        self.positions[0][start] = start as u16;
        for level in 1..self.positions.len() {
            let half = 1 << (level - 1);
            if start + 2 * half > end + 1 {
                break;
            }
            let a = self.positions[level - 1][start];
            let b = self.positions[level - 1][start + half];
            self.positions[level][start] = if costs[usize::from(a)] <= costs[usize::from(b)] {
                a
            } else {
                b
            };
        }
    }

    pub(crate) fn minimum(&self, start: usize, end: usize, costs: &[u64; 259]) -> usize {
        let width = end - start + 1;
        let level = (usize::BITS - 1 - width.leading_zeros()) as usize;
        let a = usize::from(self.positions[level][start]);
        let b = usize::from(self.positions[level][end + 1 - (1 << level)]);
        if (costs[a], a) <= (costs[b], b) {
            a
        } else {
            b
        }
    }
}
