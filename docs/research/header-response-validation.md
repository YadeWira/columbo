<!-- SPDX-License-Identifier: MIT -->

# Header-directed certified match response

Date: 13 September 2026. Baseline: `bf3c20fc2af5b190551bac3ec29a080153c096c1`, including
terminal-method closure and its larger Max work class. The unrelated DeflOpt and Defluff
report edits present at the start were preserved byte for byte.

**Accepted and implemented in Max only**, with the existing deadline variation
documented below. The [route catalogue](../routes-and-methods.md#route-gate-reference)
records R10 and the [validation policy](future-deflate-methods.md#validation-policy)
governs acceptance. This adds search coverage in Columbo; it makes no worldwide novelty
or global-optimality claim.

## Why the joint move helps

A valid payload tree establishes prices for literals and match lengths. Changing that
tree can make the current tokens larger while making a different spelling of the same
certified matches cheaper. The new pass proposes a tree first, then finds its cheapest
certified payload and prices the complete header. Neither intermediate has to win.

This is a bounded tree-first experiment motivated by [proposal
4](novel-deflate-formulations.md#4-synthesize-short-headers-first-then-fit-certified-spellings-to-them).
It does not implement a serialized-header grammar, an unrestricted response envelope, or
new LZ77 matching. Its proposal family consists of the parent tree and one nonzero
code-length swap within either payload alphabet.

The checked-in synthetic fixture is generated, not copied from a private corpus. It
decodes to 276 bytes. Swapping literal/length positions 7 and 267 and adapting matches
gives:

| Encoding | Complete block bits |
| --- | ---: |
| Parent | 438 |
| Swap alone | 438 |
| Winning spelling under the parent tree | 440 |
| Cheapest certified response under the parent tree | 438 |
| Swap and response together | **437** |

All tree lengths are ordinary Deflate lengths. Swaps preserve support, each alphabet's
length histogram and Kraft capacity, and both advertised spans. A generated submatch
stays wholly inside its current source match and uses its distance. Literal output comes
from the already-decoded interval. Original literals acquire no match certificate; there
is no history search, new distance, or extension beyond the certificate. This pass does
not recover certificates lost by earlier literalization; R1 remains the separate
restoration method.

## Solver, admission and limits

[`header/response.rs`](../../src/deflate/header/response.rs) uses a backward
shortest-path DP for each existing match. Each edge emits one literal or a canonical
submatch of length 3 through the remaining interval length. Missing codes are forbidden,
rather than assigned estimated prices. A source edge is considered first, so a
payload-cost tie retains its exact spelling. Every candidate tree remains fixed during
this solve and during complete header pricing; a frequency-optimal replacement tree is
not required.

For a fixed proposed tree and these disjoint match intervals, solving each interval
independently gives the exact minimum payload cost. It does not prove an optimum over
other trees, certificates, token ties, layouts or header-search families. Header/RLE
construction uses Columbo's existing full repricer, not an unrestricted exact CL-tree
search.

The proposal menu starts with the unchanged tree. Unequal positive lengths may swap
within the literal/length alphabet or the 30 usable distance positions. Reserved
distance positions remain unchanged. Swaps with an original-token payload tax greater
than 128 bits or an increased adjacent transition count are skipped. A deterministic
256-entry menu ranks the remainder by `tax - 3 * transitions_removed`, then positions.
Those filters and ranks are heuristics: the response might repay a tax beyond the
admitted range.

A block must have at most 4,096 decoded bytes, 1,024 tokens and 128 source matches, with
at least one match. One stream invocation shares 2^25 work units and 512 full header
prices. Work includes menu visits, DP-array preparation, token walks and an upper bound
covering every interval edge, including unavailable symbols. An unfinished interval is
discarded. Previously completed block winners survive work, price and deadline stops.

Unchanged token spellings are skipped because existing fixed-token methods cover them.
Changed spellings are exactly priced only if their payload plus the 17 fixed
dynamic-header bits can beat the incumbent. The winning payload and its proposed tree
are retained together. The new pass adds no route cache or unbounded search frontier.
Its own scratch consists of a 257-slot proposal allocation, a 318-byte length list,
three 259-entry DP arrays, and at most two 4,096-token candidate buffers, while
searching one block. The common terminal pipeline also retains the parsed stream and
completed block plans within its 1 MiB/128-block limits, plus the existing header
repricer and plan metadata. Allocations are fallible.

R10 runs on the final Max incumbent only after an unexpired R1–R9 sweep makes no change.
It does not alter the earlier comparison floors. If R10 improves the stream, R1–R9
settle again before another response attempt. R11 is the existing terminal closure, now
with ten input-score slots. The existing common gate permits at most 1 MiB of compressed
and decoded data, 128 parsed blocks, and no discarded wire blocks. Strict mode rejects
incompatible parent trees; relaxed mode keeps its existing exceptions. The common
emitter checks decoded identity, actual distances and later stored padding, and retains
only a complete byte-first/meaningful-bit win. The pass consumes the owner's existing
Max time or route window. Default gains no new mandatory search work, grace period or
route.

## Frozen-parent controls

Fresh baseline Default outputs were generated for 365 distinct raw streams. The
exploratory 256-profile solver, before stream work caps and unchanged-spelling
exclusion, improves 23 streams by 47 meaningful bits and four bytes. The production pass
improves **19/365, by 41 bits and four bytes**. Four tree-only wins are deliberately
excluded; the shared work cap loses one additional bit on a medium ZIP member.

Seven targeted source streams then received fresh ten-second baseline Max runs. All
seven completed without reported timeout. Running the existing R1–R9 terminal closure
again, without a deadline and with the true original input certificates retained,
changes none of them; a second invocation verifies stability. The bounded response pass
improves **five of these seven frozen Max outputs by 21 bits and four bytes**. These
overlap the Default sample and are not additive corpus totals.

An additional control enumerates every unequal positive single swap in both alphabets on
the five winning blocks, without the new menu, tax band or transition filter. Only the
admissible 17-bit fixed-header lower bound prunes a price. It performs **7,804 full
header prices** and finds no improvement. The new production candidate beats the exact
unchanged-tree response on two blocks:

| Frozen Max block | Parent / best single swap | Unchanged-tree response | Joint response |
| --- | ---: | ---: | ---: |
| `sample-project.zip` member `351f0b74d9a2036d` | 2,681 | 2,681 | **2,679** |
| `013-Weedle-1.png`, block 1 | 1,497 | 1,495 | **1,494** |

For the ZIP witness, exchanging lengths at positions 112 and 257 alone costs 2,684 bits;
its selected new spelling under the old tree costs 2,682. The combination costs 2,679.
Broader fixed-token enumeration uses 933 header prices on this block, versus 55 complete
response prices in the production pass on its stream. Repeating existing terminal
methods or merely evaluating more single-swap trees does not recover this win.

The other three frozen Max improvements come from an exact response under the unchanged
tree. They validate that component of the method, not a joint-tree interaction. The
five-stream 21-bit total must not be attributed entirely to the combined move; the two
interaction witnesses retain three bits beyond the unchanged-tree response.

Every emitted isolated result is reparsed and independently decoded with Python zlib.
The production probe also checks each generated match's original interval, distance and
canonical length encoding. The generated synthetic fixture is independently zlib-decoded
as well.

Instrumentation records 1,677,881,582 charged work units and 2,760 full prices across
the 365 Default parents. Of 153 streams that enter generation, 17 exhaust the shared
work limit; none exhausts the price limit. The largest price count is 256. The isolated
pass takes 3.704 seconds in total, with a 230.201 ms maximum, including parsing,
emission and verification. On the seven Max parents it takes 0.397 seconds, maximum
142.252 ms; two exhaust work, none exhausts prices. These are observations, not paired
public-API overhead estimates. The conservative caps preserve all five observed frozen
Max wins.

## Placement and timing control

The initial integration also ran on early Max-owned comparison floors and before R1–R9
had settled. The retained placement follows the final settled endpoint so a new spelling
cannot redirect an unfinished existing search. This preserves the established endpoint
before accepting a new response; all five frozen Max witnesses remain in scope.

The first 26-case paired strict-Max comparison had nine improvements and one one-byte
regression on `briefcase.png` (2,342 versus 2,343 bytes). A repeat reversed the outcome:
the unchanged baseline emitted 2,343 bytes and the initial candidate 2,342. Both
reported no timeout. Verbose traces show that the new pass found no candidate on that
PNG: its one 65,664-byte, 1,365-token block exceeds the local gates. The variation comes
from the existing timed search schedule, whose primary phase can yield before the file
deadline without reporting a file timeout. It is not evidence of a response-pass
compression gain or loss. The initial result and repeats are retained in the evidence
directory. The final build reproduces the same one-byte timed difference. Its larger
output is byte-for-byte identical to an observed unchanged-baseline output, not just
equal in size. An additional matched 30-second-allowance run produces identical
2,343-byte files from both binaries (24.9 seconds each, 17,937 Deflate bits). The new
method finds no candidate in the verbose traces. This is explicitly retained as a timed
comparison exception; it is not silently counted as a non-regressing row.

## Public-API, wrapper and regression validation

The final source and release binary were frozen before a fresh paired run. The runner
alternates baseline/candidate order for each input and gives both the same options and
allowance. No investigation build or probe runs alongside these paired timings.

| Comparison | Cases | Final result |
| --- | ---: | --- |
| Default raw | 365 | All output bytes identical; 94.041 → 94.789 s total (+0.80%) |
| Default wrappers | 39 | All output bytes identical; 61.455 → 61.858 s total (+0.65%) |
| Strict Max, 10 s | 26 | Seven improvements, 18 ties, one reproduced baseline deadline-variation row; eight timeouts in each arm |
| Relaxed Max, 10 s | 4 | All four tie; three timeouts in each arm |
| Defluff compatibility | 66 | 61 better, five ties, no missing cases, errors or misses |

The strict Max improvements include five target raw streams, `s35i3p04.png` and
`XYB.icc.zlib`. All other strict rows except the explicitly documented `briefcase.png`
comparison are non-regressing in byte/meaningful-bit order. The raw ZIP member
`351f0b74d9a2036d` reaches 2,670 bits after subsequent existing-method refinement,
versus 2,681 at baseline. The direct isolated response alone saves two bits on its
frozen parent; the larger timed result must not be credited entirely to that one move.
Container and extracted-raw cases overlap and must not be added as independent corpus
totals.

Python zlib independently verifies all raw identities. The wrapper verifier checks
PNG/APNG structure, CRCs, stream identity and zlib window bounds, GZIP members and
checksums, ZIP entries and metadata, and zlib payloads. Strict and relaxed runs use
separate manifests. All emitted benchmark outputs decode to their source contents. The
generic comparison verifier deliberately flags the one timed size loss; the separate
timing audit checks its exact match to an observed baseline output and the identical
longer-allowance results.

Defluff's totals remain **109 bytes and 932 meaningful bits smaller** than its
references. This is Default compatibility evidence, not a new R10 gain: the new search
is Max only. Results carry the final binary SHA-256
`f1ab6e09aa790df75382daf5cbe0e055d05169e37c8a0d9fd320748a7e47faad`.

The release executable grows from 1,761,232 to 1,777,760 bytes: **16,528 bytes**. The
measured Default runtime differences remain under one percent; this is not a claim of
exact zero overhead on every machine.

 The Rust suite passes 577 tests with ignored tests included; all 13 Python tests pass.
Focused tests compare the interval DP against exhaustive tiny spellings, including
absent symbols; verify the synthetic interaction barrier, source ties and canonical
length 258; check provenance, strict trees, support and histogram preservation; exercise
work/price limits, shared budgets and callback stops; and validate stored-block
alignment at all eight incoming bit residues. Clippy with warnings denied, formatting,
packaging and whitespace checks pass.

Private fixture inputs were not modified. Reproduction scripts, CSVs, baseline/candidate
binaries and hashes are under `work/header-response-validation/`; they are excluded from
source packages. The source tests include a generated fixture and do not require private
corpora.
