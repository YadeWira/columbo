<!-- SPDX-License-Identifier: MIT -->

# Coupled literal/length and distance swaps

Date: 10 September 2026. Baseline: `b931212`, including R8 and the exact
positive-run pricing optimization. **Accepted and implemented in Max only** as R9 in the
[route catalogue](../routes-and-methods.md#route-gate-reference), following
the [validation policy](future-deflate-methods.md#validation-policy).

This implements the paired literal/distance extension proposed in
[new byte-saving methods](new-byte-saving-methods.md#1-spend-payload-bits-on-better-code-length-permutations).
It adds search coverage within Columbo; it makes no worldwide novelty or
global-optimality claim.

## Why coupling can save bits

Literal/length and distance codes have separate payload alphabets, but their
length lists are described together by one code-length Huffman tree and one
RLE sequence. A swap in each alphabet can make a different shared description
profitable. Repricing either intermediate separately need not find a saving.
The joint price also accounts for repeat runs across the alphabet seam.

A deterministic synthetic frequency search produced the checked-in witness.
Swapping literal/length symbols 266 and 268 adds two payload bits; swapping
distance symbols 14 and 18 adds six. Together they remove ten header bits,
saving **two meaningful bits: 4,974 → 4,972**. Every single nonzero,
unequal-length swap in either alphabet ties or loses under full existing
header repricing. The unchanged parent's exact CL-tree/RLE solver also ties.
A generated stored prefix supplies the long-distance history. No private
fixture bytes are embedded in the regression test.

Both swaps preserve each alphabet's support, code-length histogram, Kraft
capacity, maximum depth and advertised count. Tokens, match distances,
decoded bytes and block boundaries remain fixed. The output uses ordinary
Deflate trees and repeat codes; it does not search history or discover matches.

## Candidate generation and bounds

[`header/coupled.rs`](../../src/deflate/header/coupled.rs) first builds a menu
for the small distance alphabet. Empty, singleton and uniform distance trees
cannot supply a swap, so they avoid the literal-alphabet scan. Reserved
distance symbols 30 and 31 are excluded. Only two distinct positive lengths
may exchange positions.

For positions `a` and `b`, the exact payload delta is:

`(frequency[a] - frequency[b]) × (length[b] - length[a])`.

The parsed frequencies supply this price without recounting tokens. Each
alphabet admits deltas from −32 through +32 bits and swaps that do not
increase adjacent length transitions. It retains **32 proposals per
alphabet**, ranked by `payload_delta − 3 × transitions_removed`, then by
delta and source positions. Neutral transition counts remain eligible
because the shared CL tree can still change.

The cross-product contains at most 1,024 pairs. Their combined payload delta
must also lie in −32…+32. At most eight affected edges give the exact joint
transition delta, counting the LL/DD seam once. A deterministic **256-pair
menu** uses the same heuristic rank. These admission rules and rankings are
heuristics, not lower bounds or an exhaustive search claim.

Every retained pair starts from the unchanged parent and receives full
existing header/RLE repricing. Neither single-swap intermediate is adopted.
Only a complete payload-plus-header improvement is proposed to the emitter.
Checked signed-cost updates, complete-tree checks and final stream validation
remain authoritative.

One stream invocation shares **2^20 generation-work units** and **512 complete
header prices**. Generation charges the symbol scans, outer-loop visits,
pair visits within each alphabet and cross-product visits. One visit has
fixed local-edge work and a bounded sorted-menu insertion. These are admitted
work units, not CPU instructions. A loose full-block generation bound is
42,846 units for 286 literal/length and 30 usable distance symbols. No route
cache or growing search frontier is introduced.

On the measured 64-bit platform, both 33-element swap allocations, the
257-element pair allocation and the 318-byte length array total **10,654
bytes of element storage**. This excludes compiler temporaries, vector
metadata, allocator overhead, the existing header pricer and retained plans.
Allocation is fallible. A work, price or deadline stop retains every
fully priced improvement already found.

R9 follows R8 on the completed ordinary/APNG Default comparison endpoint
used by Max and on the final eligible Max incumbent. It uses the owner's
existing deadline or route window, adds no grace period, and preserves the
historical Max seed independently. The common gate permits at most 1 MiB each
of compressed and decoded data, 128 parsed blocks and no discarded wire
blocks. Strict mode refuses non-strict parent trees; relaxed exceptions are
preserved. Default performs no new search or mandatory comparison-floor work.
The common emitter recalculates later stored padding, checks identity and
actual distances, and accepts only a whole-stream byte/meaningful-bit win.

## Controls and isolated evidence

Fresh baseline Default runs reproduce all 365 previous raw outputs exactly.
Each input has a distinct completed encoding and passes independent Python
zlib decoding. Before testing the new method, each parent receives R7, R8
and another R7 pass. This excludes those existing operations' own gains.

An exploratory 32×32 cross-product without the production stream price cap
improves **43/365 streams by 336 bits and 39 bytes**. Exhaustive single-swap
controls on its 49 winning blocks perform **933,145 full header prices**.
They have no tax band or transition filter; a pair is skipped only if its
payload plus the 17 fixed header bits cannot beat the incumbent even with a
free tree description. Exact CL-tree/RLE repricing of the unchanged parent
finds no gain on any of those blocks.

The bounded production method retains **43 wins, 293 bits and 34 bytes**.
It beats the best exhaustively priced single swap on **21 blocks in 20
streams**, retaining 76 bits beyond that stronger control. This is a control
on the same outputs, not an additional additive corpus saving.

A real frame from the Steam sticker APNG is a single-swap local optimum:

| Choice | Complete block bits |
| --- | ---: |
| Parent, including exact CL-tree/RLE optimization | 2,023 |
| Best fully repriced single swap in either alphabet | 2,023 |
| Selected LL swap alone, with exact CL-tree/RLE optimization | 2,023 |
| Selected DD swap alone, with exact CL-tree/RLE optimization | 2,024 |
| Both swaps together | **2,020** |

Here LL symbols 266/272 and DD symbols 8/24 exchange lengths. Their payload
taxes are three and one bits. The same paired header is also optimal under
the exact fixed-data-tree CL solver. This demonstrates a barrier to two
strictly improving single-swap steps. It does not rule out longer paths
through other assignments or a different tree/token search.

Fresh ten-second baseline Max runs on the 43 exploratory witnesses all reach
their timeouts. Applying the existing R7/R8/R7 control afterward strengthens
23 of those frozen outputs by 140 bits; those bits are not attributed to R9.
The bounded new method then improves **42/43 distinct controlled Max parents,
by 302 bits and 37 bytes**. This overlaps the Default-parent sample and must
not be added to it. Frozen-byte comparisons isolate the operation from timed
route scheduling. Every emitted result passes the common identity/token
checks and independent Python zlib decoding.

Isolated production runs take 11.654 seconds for the 365 Default-derived
parents and 2.531 seconds for the 43 Max-derived parents, including parsing,
emission and validation. These exploratory timing observations were made
while other local work could run; they are not public-API overhead estimates.

### Budget coverage and limits

Instrumentation reproduces the uninstrumented production bytes on all 408
parents. It records **5,753,474 generation units and 61,437 full prices**.
The largest per-stream generation charge is 115,760 units; no generation
allowance is exhausted. Eleven Default-derived and six Max-derived streams
consume all 512 prices. No route cache exists, so cache hits do not apply.

A control retains the same 32/256 candidate menus but removes stream work
and price caps. It recovers 43 further bits/five bytes on the Default-derived
`kzipmix` stream. On the Max-derived parents it recovers 46 further bits/five
bytes on `kzipmix` and one bit on `pengbrew_160x160`. It produces no other
output differences. The control improves the same 43 and 42 streams,
respectively; the retained implementation deliberately bounds pricing rather
than claiming to exhaust all opportunities in multi-block streams.

## Public API and acceptance

The method is accepted in Max only. Frozen-parent controls establish additional
coverage; the public API retains whole-file gains under the existing allowance.

All **404 Default outputs are byte-identical** to the current baseline. Independent
checks verify decoded bytes, PNG CRCs, APNG frame/control structure, ZIP contents
and metadata, GZIP integrity and advertised zlib windows.

| Default workload | Baseline | With R9 | Difference |
| --- | ---: | ---: | ---: |
| 365 raw streams | 88.723 s | 88.963 s | +0.27% |
| 39 containers | 58.995 s | 58.931 s | -0.11% |

These are alternating paired observations from persistent API runners, excluding
compilation, startup and file I/O. They ran after this investigation’s other
benchmarks and builds finished. They are observations, not general speed guarantees.

Strict Max improves **5/13 selected cases**, with no regressions.
Both arms receive ten seconds per case; timeout counts are 13
for the baseline and 13 for R9.

| Input | Baseline bytes | With R9 | Meaningful bits saved |
| --- | ---: | ---: | ---: |
| basi6a16, raw | 4,072 | 4,070 | 16 |
| Steam sticker frame, raw | 253 | 253 | 3 |
| kzipmix GZIP member, raw | 52,661 | 52,661 | 0 |
| happy frame, raw | 6,541 | 6,541 | 0 |
| basi6a16.png | 4,151 | 4,149 | 16 |
| f04n0g08.png | 263 | 263 | 7 |
| briefcase.png | 2,348 | 2,348 | 0 |
| grayscale_alpha_8_reduce_alpha.png | 21,901 | 21,901 | 0 |
| happy.png | 125,168 | 125,164 | 34 |
| Steam sticker APNG | 1,841 | 1,841 | 0 |
| kzipmix-20200115-bsd.tar.gz | 52,679 | 52,679 | 0 |
| checkers_src.zip | 14,014 | 14,014 | 0 |
| XYB.icc.zlib | 347 | 347 | 0 |

Relaxed Max improves **2/4 selected cases**, with no regressions.
Both arms receive ten seconds per case; timeout counts are 4
for the baseline and 4 for R9.

| Input | Baseline bytes | With R9 | Meaningful bits saved |
| --- | ---: | ---: | ---: |
| Steam sticker frame, raw | 253 | 253 | 3 |
| happy.png | 125,167 | 125,164 | 34 |
| kzipmix-20200115-bsd.tar.gz | 52,679 | 52,679 | 0 |
| checkers_src.zip | 14,014 | 14,014 | 0 |

The raw inputs and wrappers overlap, as do the strict and relaxed samples; their
savings must not be added as independent workload coverage. Timed differences can
include subsequent search decisions after the improved comparison floor. The
frozen-parent experiments isolate the new operation separately. Meaningful-bit
totals come from parsing every emitted Deflate stream, including wrapped streams.

The Defluff relaxed Default benchmark passes **66/66 comparisons**: 61 better,
5 ties, no errors or parity misses. Its 109-byte / 932-bit lead is unchanged.
This verifies Default compatibility; it does not exercise R9 or attribute new
Defluff savings to the Max-only search. The fresh report is
`work/coupled-swap-validation/public/defluff.md`.

## Tests and reproduction

All **548 Rust tests** pass: 484 library, 50 CLI, six private-corpus and eight
public-API tests. The new focused tests independently enumerate both menus,
verify transition deltas including the alphabet seam, establish the synthetic
single-swap barrier, check exact payload/header accounting, and exercise
generation, price and callback stops. Empty, singleton and uniform distance
trees skip the large menu. An emitted-stream test covers all eight incoming
alignments, later stored padding, cross-block history, unchanged tokens,
support, length histograms, advertised spans and actual maximum distances.
Zero-budget Max remains identical to Default in both strictness modes.

All-target/all-feature Clippy is warning-free. Formatting and diff checks
pass, as do all ten Python distribution-tooling tests.

```sh
cargo test --release --locked -- --include-ignored
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
python3 -m unittest discover -s tests -p 'test_*.py'
```

Local evidence is in `work/coupled-swap-validation/`. The baseline source and
pre-existing unrelated document are recorded in `baseline-state.json`.
Exploratory, exhaustive-control and instrumented code live in a separate
source copy. Production invokes the integrated terminal method.
`work/run-coupled-validation.py` runs alternating public-API comparisons;
`work/verify-coupled-public.py` independently decodes raw streams and checks
wrapper identities, metadata, checksums and zlib windows. These helpers need
the private corpus; the checked-in synthetic tests are standalone.

Comparable stripped release executables grow from 1,744,592 to 1,761,104
bytes, an increase of 16,512 bytes. Builds use Rust 1.97.1 on macOS Apple
Silicon, with the repository's release profile, fat LTO and overflow checks.
Candidate SHA-256:
`a31ab444fbc6a27ed408371b690e7dbb1a6bce9b8179d15010a85e067f0d8045`.
`public/source-hashes.json` records the benchmarked source; the completion
audit checks it against the final worktree and executable.
