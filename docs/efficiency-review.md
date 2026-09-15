# Efficiency review

Initial review against `9825467` on 6 September 2026. The changes remove repeated
calculation and copying without changing the admitted search routes or their
tie-breaking rules.

## Changes

- **Dynamic-header RLE:** replace the repeated scan of 11–138-byte zero-run
  suffixes with a monotonic deque. Each endpoint enters and leaves the deque
  at most once. Equal prices retain the shortest repeat, and transitions keep
  their original order. A running count also replaces the allocated run-length
  array and its preliminary traversal.
- **Proven match splitting:** construct and price each legal submatch length
  once per solver call. For a 258-byte match this reduces those operations
  from 32,896 to 256. The shortest-path traversal, forbidden symbols, exact
  source-token preference and deadline-probe cadence remain unchanged. The
  lookup is a bounded stack array with no heap allocation.
- **ZIP processing:** borrow source payloads until a replacement exists; borrow
  the validated physical-order list; move terminal preflight metadata instead
  of cloning it. The terminal Store pass stages unchanged local records only
  when a replacement actually requires archive reconstruction.

## Focused measurements

Local macOS arm64, Rust 1.97.1, optimized builds with overflow checks enabled.
The baseline and changed functions used identical inputs and allocation/stop
helpers in a temporary harness. Values are medians of five batches; RLE batches
contained 20,000 calls and match-solver batches contained 2,000 calls. These
measure individual functions, not complete-file speedups.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| RLE, 318 zero lengths | 63.74 µs | 8.32 µs | 7.66× |
| RLE, 318 mixed run lengths | 13.41 µs | 6.36 µs | 2.11× |
| RLE, 318 varying nonzero lengths | 2.46 µs | 2.16 µs | 1.14× |
| Match splitting, 12 bytes | 0.46 µs | 0.40 µs | 1.17× |
| Match splitting, 64 bytes | 30.83 µs | 5.99 µs | 5.15× |
| Match splitting, 258 bytes | 791.06 µs | 91.82 µs | 8.62× |

Full-file checks covered 16 inputs in Default and eight in Max: static PNG,
APNG, GZIP, ZIP, zlib and raw Deflate, including a generated 32 MiB stored ZIP.
All 24 comparisons produced byte-identical baseline and candidate outputs.
Python's zlib, gzip and zipfile decoders independently confirmed payload
identity; PNG checks also validated chunk CRCs and every image/frame stream.

Default CLI medians over three runs improved from 0.326 to 0.253 seconds for
the generated raw stream (1.29×), from 0.804 to 0.728 seconds for
`checkers_src.zip` (1.10×), and from 0.064 to 0.055 seconds for the stored ZIP
(1.16×). Other sampled files were near neutral or improved. These are local
measurements that include process startup and I/O. Max used a ten-second search
timeout and often consumed its deadline and grace, so its elapsed times do not
establish an algorithm speedup.

## Validation

- `cargo test --release`: 499 passing tests, including new exhaustive
  match-spelling checks, long-run RLE comparisons against a scanning oracle,
  and mixed changed/unchanged ZIP reconstruction in physical order.
- An additional 8,192 comparisons against the original match solver retained
  identical token spellings across all legal lengths, missing-code prices,
  forbidden symbols and the relaxed length-258 alias.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check` and
  `git diff --check` passed.
- The release executable remains 1,628,864 bytes on this host.

Temporary harnesses and detailed results are retained under the ignored
`work/efficiency-review/` directory; the original executable is retained under
`target/efficiency-review-baseline/`.

## Follow-up against `996c30e`, 9 September 2026

The earlier changes are committed in `8165633`. The follow-up inspected the
reorganized source and the new joint-tree and symbol-set searches, then
implemented two further improvements in shared code:

- **Greedy source-block merging:** retire absorbed slots in place rather than
  removing them from a vector and moving every later block. Two forward
  cursors retain the same left-to-right greedy choices; the emission pass
  skips retired slots. List maintenance is now linear. Payload and split-array
  copying during a merge are unchanged. Retired blocks are dropped immediately
  and their budget charges released at the same point as before.
- **Canonical match lengths:** replace a scan of up to 28 length families with
  a 256-byte lookup table built at compile time from the existing base lengths.
  The table preserves symbol 285 for length 258 and rejects every out-of-range
  input without allocating.

The source-list benchmark called the original and revised route directly on
identical arrays of one-byte stored blocks. It isolates list maintenance: the
public optimizer can bypass this route for wholly stored input, so these are
not whole-file speedups. Medians of five runs on the same macOS arm64 host:

| Source blocks | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| 128 | 0.556 ms | 0.060 ms | 9.27× |
| 1,024 | 30.132 ms | 0.515 ms | 58.51× |
| 4,096 | 495.570 ms | 3.101 ms | 159.81× |

The original and optional working-block slots are both 3,072 bytes on this
host. Memory accounting uses the actual optional-slot size for portability.
Length encoding averaged 15.353 ns before and 1.055 ns after (14.55×), taking
the median of seven batches of 2,560,000 calls over all legal lengths.

Validation for the follow-up:

- `cargo test --release -- --include-ignored`: all 516 Rust tests passed,
  including the ten private-corpus regressions.
- All six Python utility tests passed; Clippy with warnings denied, formatting,
  and whitespace checks passed.
- 576 original/revised route comparisons retained identical complete plans
  and stop-check counts across strictness modes, alignments and interruptions.
- All 65,536 possible `u16` length inputs matched the original encoder.
- 32 API comparisons covered 16 PNG/APNG, GZIP, ZIP, zlib and raw inputs in
  Default and zero-budget Max. Output bytes and reported savings matched
  exactly, and independent decoders confirmed payload identity. Sampled
  whole-file timings were largely unchanged; these checks do not measure
  completed Max searches.
- The release executable remains 1,694,976 bytes on this host.

The follow-up harnesses and results are in `work/efficiency-review-sep9/`, with
the committed baseline executable in `target/efficiency-review-sep9-baseline/`.

## Follow-up against `625fb30`, 9 September 2026

The review of the newer joint-tree, symbol-set and paired-alphabet searches
found two further opportunities to avoid repeated work:

- **Joint-tree payload bounds:** update each suffix-table row one code length
  at a time. Its payload price is calculated once, and a shifted pair of slices
  visits only capacities where the length fits. This replaces the inner
  capacity check, repeated multiplication and indexed row lookups. Zero-length
  assignments copy the next row. The exact minima, table size, search order,
  conservative work charge and per-row stop checks are unchanged.
- **Symbol-set admission:** return as soon as the running affected-match count
  or decoded-byte total exceeds its existing limit. Both totals only increase,
  so scanning the remaining tokens cannot change rejection. The full initial
  scan charge is still reserved, preserving subsequent candidates' budgets.
  Candidates exactly at either limit remain eligible.

The paired-alphabet route already caches range estimates, prepared edges and
header kernels, and shares the established eight-alignment boundary graph.
The established coarse-to-fine split scorer also caches sampled positions.
Those caches and route policies are retained. Earlier RLE, match-length,
source-block-list and ZIP borrowing improvements remain present in this
baseline and are covered by the current test suite.

Focused measurements used extracted original/revised functions, identical
inputs and the same `SearchStop` implementation, with optimized builds,
overflow checks, fat LTO and one codegen unit on the same macOS arm64 host.
Values are medians of seven batches with alternating baseline/candidate order.
Payload-table batches used 100 calls (20,000 for the short case); complete
joint-solver batches used ten calls. Symbol admission used 20,000 calls on
8,192-token blocks. These measurements isolate the named operations.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Payload bounds, eight symbols, depth three | 0.466 µs | 0.266 µs | 1.75× |
| Payload bounds, 286 dense symbols, depth nine | 1,613.375 µs | 669.081 µs | 2.41× |
| Payload bounds, 286 sparse symbols, depth nine | 1,646.482 µs | 637.376 µs | 2.58× |
| Payload bounds, 32 distance positions, depth nine | 169.730 µs | 65.510 µs | 2.59× |
| Joint solver, depth three | 294.912 µs | 288.321 µs | 1.02× |
| Joint solver, depth six | 600.237 µs | 499.225 µs | 1.20× |
| Joint solver, depth nine | 67.378 ms | 66.327 ms | 1.02× |
| Symbol admission, accepted at match limit | 2.709 µs | 2.711 µs | 1.00× |
| Symbol admission, exceeds match limit | 2.705 µs | 0.122 µs | 22.21× |
| Symbol admission, exceeds byte limit | 2.653 µs | 0.056 µs | 47.50× |

The table-building improvement is larger than the complete joint-solver gain
because the state/edge search remains unchanged. Early rejection helps only
over-limit symbol candidates; the accepted-case scan was neutral in this
sample. Neither result is a whole-file speedup claim.

Validation for this follow-up:

- `cargo test --release -- --include-ignored`: all 522 Rust tests passed,
  including private-corpus regressions and both new tests.
- The new payload test checks 5,184 suffix/capacity minima against exhaustive
  assignment enumeration, including unavailable lengths, reserved symbols,
  zero-length eligibility, impossible capacities and `u32::MAX` frequencies.
- The new symbol-limit test exercises exact match and byte limits, rejection
  just beyond each limit, and an unaffected block. It checks emitted literals,
  source certificates, budget charges and stop-check counts.
- 1,800 original/revised payload-table comparisons, 5,120 joint-solver
  comparisons and 1,920 symbol-admission comparisons matched exactly. These
  include interruptions and limited budgets; complete solutions retain their
  lengths, RLE instructions and tie choices.
- All six Python utility tests, Clippy with warnings denied, formatting and
  whitespace checks passed.
- All 36 API comparisons retained exact output bytes and reported savings,
  with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding. They
  cover 16 inputs in Default and zero-budget Max, plus four ten-second Max
  runs. One of those four Max runs completed; three reached their deadline,
  so they do not establish complete-search timing equivalence.
- Whole-file timings were largely unchanged. An initially slower Default
  `Load.png` sample (0.847 ms versus 1.273 ms) was followed by 30 alternating
  pairs with exact output checks: medians were 0.838 ms versus 0.807 ms, and
  the median paired candidate/baseline ratio was 0.993. The noisy initial
  sample remains in the recorded results.
- The baseline and candidate release executables are both 1,728,048 bytes.

Harness sources, logs and baseline/candidate API drivers are retained under
`work/efficiency-review-joint/`. The committed baseline build is retained under
`target/efficiency-review-joint-baseline/`.

## Follow-up against `4cc332f`, 9 September 2026

The committed joint-tree and symbol-set changes match their validated source
hashes. A further review of boundary scoring found repeated token counting in
the shared range-histogram helper. Even for a one-token interval near the end
of a checkpoint, it reconstructed two overlapping cumulative prefixes and
subtracted their full frequency arrays.

The helper now compares the range length with the number of tokens needed to
reconstruct those two checkpoint tails. When a direct scan visits no more
tokens, it counts the range directly using the existing fallback loop.
Long ranges retain the checkpoint index; indexed calls still visit at most
510 tokens. No cache, allocation, search route or budget change is introduced.
The literal/length counts, distance counts, payload extra bits and single
end-of-block frequency remain exact.

Focused measurements used extracted original/revised helpers and checkpoint
construction code with identical mixed literal/match input. Rust 1.97.1,
optimized builds, overflow checks, fat LTO and one codegen unit on macOS arm64;
medians of seven batches of 100,000 calls, alternating variant order:

| Token range | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Empty, 255..255 | 0.696 µs | 0.054 µs | 12.99× |
| One token, 254..255 | 0.699 µs | 0.052 µs | 13.43× |
| Within a checkpoint, 224..248 | 0.661 µs | 0.069 µs | 9.63× |
| Across a checkpoint, 250..270 | 0.483 µs | 0.064 µs | 7.51× |
| Aligned, 256..512 | 0.272 µs | 0.271 µs | 1.00× |
| Long, 17..32760 | 0.483 µs | 0.483 µs | 1.00× |

These are histogram-call measurements, not whole-file speedups. The two
indexed control cases were neutral in this sample.

Validation for this follow-up:

- All 522 Rust tests passed with `cargo test --release -- --include-ignored`.
  The existing direct-recount regression now includes more empty ranges,
  ranges on both sides of checkpoints and invalid bounds; its duplicate
  aligned-range case was replaced.
- 592,138 original/revised comparisons matched, covering every interval of a
  768-token mixed stream with and without the index, plus invalid bounds.
  An additional 34,832 checks used an independent direct counter.
- All 36 API comparisons retained exact output bytes and reported savings,
  with independent decoding across PNG/APNG, GZIP, ZIP, zlib and raw Deflate.
  These cover 16 inputs in Default and zero-budget Max, plus four ten-second
  Max runs. One Max run completed and three reached their deadlines.
  Whole-file timings were largely unchanged; the focused speedups above
  should not be applied to complete optimization runs.
- Clippy with warnings denied, formatting and whitespace checks passed.
- The baseline and candidate release executables are both 1,728,048 bytes.

The baseline executable/API driver, extracted helpers, source hashes and logs
are retained under `work/efficiency-review-ranges/`.

## Follow-up against `f31050b`, 10 September 2026

The newer Max header-tree search repeated the same positive-run dynamic
program for every emitted code-length symbol. A run's optimal price depends
only on its size, literal-code price and repeat-16 price, so those calculations
can be shared across symbol histograms.

The dynamic program also has a direct solution. A nonempty positive run needs
one explicit literal. If repeats are beneficial, consider the maximum number
of six-value repeats that fit after it, or one additional repeat with values
redistributed among groups of three to six. Comparing those two costs with
the all-literal cost gives the exact minimum. Runs shorter than four and trees
without repeat 16 use literals directly. Prices are computed once per run
length and code-price pair, then weighted by each symbol's run histogram.

The preparation keeps the original budget charges and their order. Kraft
assignment, zero-run pricing, candidate order and final RLE reconstruction
retain their existing behavior. No heap allocation or additional table is
introduced: a run-price array replaces the old DP array.

The baseline build came from an immutable Git archive of `f31050b`. Focused
measurements used extracted original/revised price preparation and full search
functions with identical stopping policies. Rust 1.97.1, macOS arm64, optimized
builds with overflow checks, fat LTO and one codegen unit. Values are medians of
seven alternating batches: 20,000 preparation calls or 100 full-search calls
per batch. Preparation measurements use a three-bit repeat-16 code.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Prepare prices, distinct short runs | 0.684 µs | 0.591 µs | 1.16× |
| Prepare prices, 15 symbols with 20-value runs | 12.188 µs | 2.035 µs | 5.99× |
| Prepare prices, mixed run histograms | 4.051 µs | 1.257 µs | 3.22× |
| Prepare prices, one 317-value positive run | 14.727 µs | 5.910 µs | 2.49× |
| Full header search, distinct short runs | 7.336 µs | 7.321 µs | 1.00× |
| Full header search, shared run lengths | 131.500 µs | 58.516 µs | 2.25× |
| Full header search, mixed run histograms | 71.976 µs | 51.942 µs | 1.39× |
| Full header search, one long positive run | 107.325 µs | 41.773 µs | 2.57× |

These measure the named header-search operations, not whole-file speedups.

Validation for this follow-up:

- All 541 Rust tests passed with `cargo test --release -- --include-ignored`.
- The new formula test compares all 17,808 combinations of run lengths 1–318,
  literal prices 1–7 and repeat prices 0–7 against the general shortest-RLE
  solver, including repeat absence and short tails.
- A second test independently prices run histograms spanning all 15 positive
  symbols and long runs, and checks exact budget exhaustion behavior.
- 7,680 original/revised price-table comparisons and 1,920 header-search calls
  matched exactly, including selected trees, costs, remaining budgets and
  stop-check counts under interruptions.
- All 36 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 16 inputs in Default and zero-budget Max plus four ten-second
  Max runs. One of those Max runs completed; three reached their deadlines.
  Whole-file timings were largely unchanged.
- All ten Python utility tests, Clippy with warnings denied, formatting and
  whitespace checks passed.
- The contributor-guide checks also passed: locked build, all-feature Clippy,
  all-target debug tests including the private corpus, and documentation
  tests. The package list contains 94 files and excludes private fixtures and
  scratch artifacts.
- The baseline and candidate release executables are both 1,744,592 bytes.

The immutable baseline source, extracted helpers, source hashes, executables,
API drivers and logs are retained under `work/efficiency-review-header-runs/`
and `target/efficiency-review-header-runs-snapshot/`.

## Follow-up against `a60ce63`, 12 September 2026

Three header searches maintained separate counters for the change in adjacent
code-length transitions: pair swaps, three-symbol rotations and coupled
literal/length and distance swaps. Each repeatedly searched replacement
positions or previously visited edges to resolve adjacency.

All callers already supply strictly increasing replacement positions. The
shared counter uses that order to visit each affected edge once. Only the
immediately preceding or following replacement can share an edge; no repeated
position lookup is needed. Adjacent changes and the literal/length-to-distance
seam retain their exact prices. The implementation removes 31 production
lines overall and introduces no allocation. Candidate ranking, work budgets,
stop checks and final header pricing retain their existing behavior.

The baseline came from an immutable Git archive of `a60ce63`. Focused
measurements used extracted original/revised counters and candidate-generation
functions with identical stopping policies. Rust 1.97.1, macOS arm64, optimized
builds with overflow checks, fat LTO and one codegen unit. Values are medians
of seven alternating batches: one million counter calls, 100 swap/coupled
menu calls, five rotation-menu calls at 32 and 120 symbols, and one at 286.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Pair-swap counter | 0.017 µs | 0.003 µs | 5.46× |
| Coupled-swap counter across the seam | 0.022 µs | 0.006 µs | 3.61× |
| Rotation counter with adjacent positions | 0.022 µs | 0.006 µs | 3.43× |
| Pair-swap menu, 286 symbols | 323.592 µs | 131.490 µs | 2.46× |
| Coupled menus, 286 + 30 symbols | 523.295 µs | 353.775 µs | 1.48× |
| Rotation menu, 32 symbols | 115.508 µs | 39.400 µs | 2.93× |
| Rotation menu, 120 symbols | 8,582.633 µs | 3,505.008 µs | 2.45× |
| Rotation menu, 286 symbols | 100,226.541 µs | 40,153.708 µs | 2.50× |

These measure transition counting and candidate generation, not complete
header searches or whole-file optimization.

Validation for this follow-up:

- All 571 Rust tests passed with `cargo test --release -- --include-ignored`.
  Existing full-sequence and candidate-menu oracle tests cover all three
  callers, including adjacent changes, endpoints and the alphabet seam.
- 106,700 counter comparisons matched both the original implementations and
  independently rewritten full sequences.
- 2,640 candidate-menu comparisons matched exactly, including remaining work
  budgets and stop-check counts under interruptions.
- All 36 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 16 inputs in Default and zero-budget Max plus four Max runs with
  ten-second limits. None of those four runs reported a timeout in either
  build. Whole-file timings were largely unchanged in this sample.
- All 13 Python utility tests and the contributor-guide checks passed:
  locked build, formatting, all-feature Clippy with warnings denied,
  all-target debug tests including the private corpus, and documentation
  tests. The package list excludes private fixtures and scratch artifacts.
- Whitespace checks passed, and source hashes still match the tested build.
- The baseline and candidate release executables are both 1,761,232 bytes.

The immutable baseline source, extracted helpers, source hashes, executables,
API drivers and logs are retained under `work/efficiency-review-transitions/`
and `target/efficiency-review-transitions-baseline/`.

## Follow-up against `fae577c`, 13 September 2026

The current review checked the revised route schedule, shared stream helpers,
Huffman construction, header searches, bitstream I/O and checksums. The route
changes deliberately reserve time for terminal methods; their admission,
ordering and stopping policies remain intact. Two further repeated-work
patterns were removed:

- **Fixed-block coalescing:** borrowing the last output plan replaces cloning
  both inputs and then restoring them if a join fails. Those temporary Arc
  clones forced the shared-vector helper to copy the accumulated tokens and
  decoded bytes on every join. The joined vectors now stay uniquely owned and
  grow amortized across subsequent appends. A shared source is copied on its
  first mutation. Both payload buffers are prepared before either is appended,
  retaining separate valid blocks if allocation fails. Stored-block alignment
  safeguards, source provenance and the exact ten-bit saving are unchanged.
- **Package-merge leaf ordering:** collect leaf indices and sort once by
  weight and original index. This replaces quadratic insertion into the
  growing list with an O(n log n) sort. Original indices preserve ascending
  symbol order even across inactive symbols, retaining both leaf-first and
  package-first tie policies. Package construction and reconstruction are
  unchanged, and the replacement removes five implementation lines.

The baseline came from an immutable Git archive of `fae577c`. Focused
measurements used extracted original/revised functions and the same block
model. Rust 1.97.1, macOS arm64, optimized builds with overflow checks, fat LTO
and one codegen unit. Values are medians of seven alternating batches:
1,000 complete package-merge calls, ten fixed-join runs at 128 and 1,024
blocks, and three at 4,096 blocks. Every fixed block contains 16 literal bytes;
the timing includes cloning the shared input plans and dropping the result.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Package merge, 19 positions with tied weights | 1.261 µs | 1.308 µs | 0.96× |
| Package merge, 286 descending weights | 53.375 µs | 29.491 µs | 1.81× |
| Package merge, 286 ascending weights | 28.748 µs | 29.458 µs | 0.98× |
| Package merge, 286 mixed weights | 42.234 µs | 31.452 µs | 1.34× |
| Join 128 fixed blocks | 65.108 µs | 7.158 µs | 9.10× |
| Join 1,024 fixed blocks | 4,352.983 µs | 49.925 µs | 87.19× |
| Join 4,096 fixed blocks | 53,671.875 µs | 203.555 µs | 263.67× |

The large fixed-join gains isolate accumulated payload copying; they do not
include per-block planning or represent whole-file speedups. Package merge
improves on larger unsorted alphabets; small and already ordered inputs were
slightly slower in this sample, by up to 4%.

Validation for this follow-up:

- All 572 Rust tests passed in release and debug modes, including the private
  corpus. The new regression verifies buffer reuse through 128 joins, shared
  source preservation, exact bit costs, provenance and emitted payloads.
  The existing stored-suffix test checks alignment-sensitive emission.
- 83,328 original/revised package-merge comparisons retained exact lengths
  under both tie policies. Cases include all six-position frequency vectors
  with counts 0–3, empty/singleton alphabets, impossible depth limits, sparse
  and dense alphabets up to 320 positions, and counts up to `u32::MAX`.
- 21,600 original/revised append comparisons matched complete plans and bit
  totals across mixed fixed, original-fixed and alignment-sensitive stored
  representations, empty payloads and different batch boundaries.
- All 38 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 17 inputs in Default and zero-budget Max, including generated
  stored and fixed-block streams, plus four Max runs with ten-second limits.
  Neither build reported a timeout in those four runs. Default timings were
  largely unchanged, with baseline/candidate ratios between 0.96 and 1.02.
- All 13 Python utility tests and the contributor-guide checks passed:
  locked build, formatting, all-feature Clippy with warnings denied, all-target
  debug tests, and documentation tests. The package list excludes private
  fixtures and scratch artifacts.
- The baseline and candidate release executables are both 1,761,232 bytes.

Whitespace checks passed, and the final source hashes match the tested build.
Pre-existing edits to the DeflOpt and Defluff benchmark reports were preserved.

The immutable baseline source, extracted helpers, source hashes, executables,
API drivers and logs are retained under `work/efficiency-review-sep13/` and
`target/efficiency-review-sep13-baseline/`.

## Follow-up against `042d430`, 13 September 2026

The newly added header-response and length-exchange methods repeatedly price
canonical match widths under fixed proposed trees. All widths in a Deflate
length family share a symbol and extra-bit count, hence the same price.
The revised code shares that family pricing between both methods:

- **Exact match spelling:** query the cheapest solved suffix in each available
  family instead of walking every possible width at every decoded position.
  An incremental table of six power-of-two minima covers the maximum 32-width
  family. Each query takes two lookups. The table stores suffix positions so
  equal prices choose the shortest canonical edge, while the original token
  and literal retain their earlier tie priority. When all available families
  contain at most four widths, direct scans avoid building the table. The
  solver uses bounded stack storage and introduces no heap allocation.
- **Exchange lower bounds:** examine only each family's greatest fitting
  width, where its fixed price has the lowest bits/byte ratio. The exact source
  edge is still considered separately, including relaxed length-258 spelling.
  Canonical family 284 ends at 257; canonical 258 belongs to symbol 285.

Both changes retain the original conservative work charges, price budgets,
stop-probe cadence, candidate order and match-distance proofs. The exact solver
now does at most 29 family queries and six index updates per position instead
of a quadratic edge walk. The lower bound examines at most 29 families rather
than up to 256 canonical widths.

The baseline came from an immutable Git archive of `042d430`. Focused
measurements used extracted original/revised solvers with the same model and
stopping implementation. Rust 1.97.1, macOS arm64, optimized builds with
overflow checks, fat LTO and one codegen unit. Values are medians of seven
alternating batches: 1,000 spelling calls or 100,000 bound calls. Dense cases
use eight-bit literal/length prices and a five-bit distance code with thirteen
extra bits. Sparse cases keep only length symbol 285 available; their literal
prices are unchanged.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Spell 3 bytes, dense prices | 0.136 µs | 0.100 µs | 1.36× |
| Spell 12 bytes, dense prices | 0.280 µs | 0.229 µs | 1.22× |
| Spell 64 bytes, dense prices | 4.833 µs | 2.846 µs | 1.70× |
| Spell 128 bytes, dense prices | 18.783 µs | 7.247 µs | 2.59× |
| Spell 258 bytes, dense prices | 74.694 µs | 17.766 µs | 4.20× |
| Spell 258 bytes, mixed/absent length prices | 77.584 µs | 15.828 µs | 4.90× |
| Spell 258 bytes, only length 258 available | 68.144 µs | 0.659 µs | 103.38× |
| Bound 3 bytes | 0.011 µs | 0.019 µs | 0.61× |
| Bound 12 bytes | 0.030 µs | 0.044 µs | 0.68× |
| Bound 64 bytes | 0.167 µs | 0.103 µs | 1.61× |
| Bound 258 bytes | 0.628 µs | 0.125 µs | 5.02× |

The sparse gain removes scans over unavailable edges. Short lower-bound calls
are slightly slower in absolute terms, by 8–14 ns in this sample. These are
individual solver measurements, not complete-search or whole-file speedups.

Validation for this follow-up:

- All 584 Rust tests passed in release and debug modes, including the private
  corpus. Existing generated witnesses still cross the intended tree/spelling
  and symbol-support barriers and pass exact emission and source-proof checks.
- The new regression makes 9,216 comparisons against an independent quadratic
  solver across every legal match length, four generated price profiles,
  missing distance codes, budget boundaries and callback interruptions. It
  compares exact tokens, return values, remaining budgets and stop counts,
  including relaxed length-258 source tokens and pre-existing output prefixes.
- The lower-bound regression now also compares exact values against a full
  width scan, in addition to checking admissibility against exact spelling.
- The standalone harness made 32,768 matching spelling/budget/stop comparisons
  and 12,288 matching lower-bound comparisons with additional price profiles,
  literal bounds and distance extra bits.
- All 44 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 19 inputs in Default and zero-budget Max plus six Max runs with
  ten-second limits, including both generated method witnesses. Neither build
  reported a timeout in those six runs. Whole-file timings were broadly
  unchanged. An initially noisy ZIP result was rechecked over five alternating
  pairs: median baseline/candidate speed was 1.02×; a PNG recheck was 1.00×.
- All 13 Python tests and contributor-guide checks passed: locked build,
  formatting, all-feature Clippy with warnings denied, all-target debug tests
  and documentation tests. The package excludes private fixtures and scratch
  artifacts.
- The release executable grew by 16 bytes, from 1,794,288 to 1,794,304 bytes.

Whitespace checks passed, and final source hashes match the tested build.
Pre-existing edits to the DeflOpt and Defluff benchmark reports were preserved.

The immutable baseline source, extracted helpers, source hashes, executables,
API drivers and logs are retained under `work/efficiency-review-response/`
and `target/efficiency-review-response-baseline/`.

## Follow-up against `931f8a9`, 15 September 2026

The review checked the larger terminal-header admission class, shared planning
caches, partitioning kernels and Huffman construction. The new admission rule
and existing search/stop policies remain intact. This follow-up simplifies
three repeated storage or implementation patterns in `huffman.rs`:

- **Package nodes:** a tagged leaf-or-pair representation replaces three
  optional indices. A leaf carries one symbol; a pair carries its two child
  indices. This removes invalid combinations and optional-child checks while
  reducing each node from 56 to 32 bytes on this host.
- **Package lists:** alternate two reusable buffers between depths instead
  of allocating a fresh list at every level. Leaf ordering, both package tie
  policies, selected nodes and length reconstruction are unchanged.
- **Protected frequency runs:** share the equal-count run marker used by the
  Zopfli and Brotli smoothers. Their distinct moving averages, stride rules
  and treatment of protected boundaries remain in their own functions.

The production change removes eight lines overall. Existing regression tests
cover package tie behavior, depth repair, inactive symbols, protected runs,
trailing zeros and extreme frequency counts.

The baseline came from an immutable Git archive of `931f8a9`. Focused timings
used extracted original/revised functions with Rust 1.97.1 on macOS arm64,
optimized builds, overflow checks, fat LTO and one codegen unit. Values are
medians of seven alternating batches: 2,000 package-merge calls or 20,000
smoothing calls. Smoothing uses 286 positions arranged in seven-value runs.

| Workload | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Package merge, 19 positions with tied weights | 1.441 µs | 1.206 µs | 1.19× |
| Package merge, 286 descending weights | 33.565 µs | 30.461 µs | 1.10× |
| Package merge, 286 ascending weights | 30.786 µs | 29.119 µs | 1.06× |
| Package merge, 286 mixed weights | 33.806 µs | 30.963 µs | 1.09× |
| Zopfli smoothing | 1.130 µs | 1.151 µs | 0.98× |
| Brotli smoothing | 0.991 µs | 1.000 µs | 0.99× |

A separate allocator-instrumented harness measured complete package-merge
calls on descending weights. Requests include allocations and reallocations.
Peak bytes count live requested allocations, not process RSS or allocator
overhead; allocator instrumentation was absent from the timing executable.

| Symbols / maximum depth | Requests before → after | Peak bytes before → after |
| --- | ---: | ---: |
| 2 / 1 | 5 → 5 | 260 → 164 |
| 19 / 7 | 16 → 11 | 7,915 → 4,862 |
| 286 / 15 | 29 → 16 | 241,070 → 143,052 |
| 320 / 15 | 30 → 17 | 471,840 → 275,552 |

For 286 symbols this removes 13 allocation requests and about 41% of peak
requested memory. These are package-builder savings, not whole-program memory
or speedup claims. The smoother refactor is a readability improvement; its
focused timings were within 2% of the original implementation.

Validation for this follow-up:

- All 585 Rust tests passed in release and debug modes, including the private
  corpus and the new larger-stream terminal-header regression in the baseline.
- 83,328 package-merge comparisons retained exact length vectors and fallback
  behavior under both tie policies, including inactive symbols, impossible
  depth limits, dense/sparse alphabets and `u32::MAX` frequencies.
- 139,074 smoother comparisons retained exact histograms. Cases include all
  eight-position vectors with counts 0–3, empty arrays, protected zero and
  positive runs, trailing zeros and larger arrays with extreme counts.
- All 44 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 19 inputs in Default and zero-budget Max plus six Max runs with
  ten-second limits, including the generated header-response and length-exchange
  witnesses. Neither build reported a timeout in those six runs.
- Whole-file times varied in both directions. Five alternating Default pairs
  for the ZIP and PNG timing outliers gave baseline/candidate median ratios
  of 0.95 and 0.96 respectively. The ZIP times ranged from 4.28–6.12 seconds
  before and 4.19–5.56 seconds after; these samples do not establish a reliable
  whole-file speedup. The package-builder measurements above are the supported
  performance and memory claims.
- All 13 Python utility tests and contributor-guide checks passed: locked
  build, formatting, all-feature Clippy with warnings denied, all-target debug
  tests and documentation tests. The package list excludes private fixtures
  and scratch artifacts.
- Both release executables are 1,794,304 bytes.

Whitespace checks passed, and final source hashes match the tested build.
The pre-existing untracked terminal-header validation document was preserved.

The immutable baseline source, extracted helpers, allocator instrumentation,
source hashes, executables, API drivers and logs are retained under
`work/efficiency-review-huffman-storage/` and
`target/efficiency-review-huffman-storage-baseline/`.

## Follow-up against `9860910`, 15 September 2026

This pass reviewed same-distance partitioning, header-plan ownership and the
remaining match-restoration recurrence after the Huffman storage changes. The
restoration solver has different source-proof and per-edge budget rules, so
its recurrence remains separate. Two repeated operations were removed:

- **Same-distance partitions:** replace the scan of up to 256 individual
  deficits at every DP state with one range-minimum query per canonical
  length family. Each family's lengths share a symbol and extra-bit count.
  The existing six-level suffix-minimum helper now lives in `minima.rs` and
  serves both this solver and header-response spelling. Reversing the deficit
  axis preserves the preference for the smallest token deficit; family order
  preserves ties between families. Choice-table allocation, backtracking,
  fallback admission and every cooperative stop probe remain unchanged.
- **Saturated header caches:** return the newly built owned plan to the caller
  and clone its zero-payload kernel only after the cache has room to retain it.
  This avoids three vector clones on each successful saturated miss. Payload
  overflow still rejects the plan before cache insertion; optional cache
  allocation failure still leaves a usable caller-owned plan.

For `A` active matches and maximum deficit `D`, partitioning changes from
`O(A D²)` to `O(A D (F + log 32))`, with at most 29 length families. Family
prices are calculated 29 times instead of 256 times per construction. The
shared lookup adds no heap allocation. The declared fixed scratch arrays grow
by 4,860 bytes on this 64-bit host, including wider cost rows; this is bounded
local storage, not an overall program-memory reduction. Legal callers have
at most 257 active matches, so even maximum `u8` prices remain far below the
old `u32` sentinel.

The immutable baseline is a Git archive of `9860910`. Focused measurements
use extracted original/revised functions, Rust 1.97.1 on macOS arm64, optimized
builds with overflow checks, fat LTO and one codegen unit. Values are medians
of seven alternating batches under fixed Huffman prices. Batches contain
20,000, 20,000, 1,000, 500, 200 and 20 calls respectively.

| Active matches / maximum deficit | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| 2 / 1 | 0.336 µs | 0.278 µs | 1.21× |
| 2 / 6 | 0.415 µs | 0.383 µs | 1.08× |
| 16 / 31 | 12.743 µs | 5.871 µs | 2.17× |
| 16 / 127 | 180.741 µs | 34.867 µs | 5.18× |
| 16 / 257 | 709.645 µs | 128.429 µs | 5.53× |
| 257 / 257 | 11,596.800 µs | 2,041.412 µs | 5.68× |

These are complete partition-table construction timings, not whole-file
speedups. The cache change removes copying by construction; no separate
cache or whole-program memory saving is claimed.

Validation for this follow-up:

- All 588 Rust tests passed in both debug and release modes, including private
  corpus regressions. New tests compare complete partition choices with the
  original direct recurrence across missing, truncated, fixed, tied and
  extreme price profiles, and check every stop boundary in a three-row DP.
  Cache tests cover saturation, hits, independent policy keys, payload costs
  and arithmetic overflow.
- The separate harness passed 2,639 exact partition-table and stop-count
  comparisons. It covers every maximum deficit from 0 through 257, full
  257-row tables, zero active matches and 256 generated price profiles.
- All 50 file-level API comparisons retained exact output bytes and reported
  savings, with independent PNG/APNG, GZIP, ZIP, zlib and raw Deflate decoding.
  They cover 21 inputs in Default and zero-budget Max plus eight Max runs with
  ten-second limits. Neither build reported a timeout in those eight runs.
  Two new generated fixed-block inputs exercise 16- and 257-active-match
  partitions without embedding corpus data.
- Five additional alternating pairs confirmed a complete-file improvement
  on each generated partition input: short Default median time fell from
  26.778 to 19.339 ms (1.38×), and deep Max from 682.771 to 578.480 ms (1.18×).
  Outputs and metrics remained identical and decoded independently in every
  repeat. Other sampled files were broadly unchanged; these generated-case
  results do not establish a corpus-wide speedup.
- All 13 Python tests and contributor-guide checks passed: locked build,
  formatting, all-feature Clippy with warnings denied, all-target debug tests,
  documentation tests and package inspection. The package includes the new
  shared helper and excludes private fixtures and scratch artifacts.
- Both release executables are 1,794,304 bytes.

The final diff and source hashes were checked against the tested build. The
pre-existing untracked terminal-header validation document was preserved.
The architecture guide records the shared helper's responsibility.

The immutable baseline, extracted helpers, source hashes, generated inputs,
executables, API drivers and logs are retained under
`work/efficiency-review-partition/` and
`target/efficiency-review-partition-baseline/`.
