<!-- SPDX-License-Identifier: MIT -->

# Length-symbol exchange

13 September 2026. Baseline: `2a2df1f236b6d572ee757d6b41d72463ceea24b4`,
including header-directed match response and the existing terminal closure.
**Accepted and implemented in Max only.** This adds a search family to
Columbo without making a worldwide novelty or global-optimality claim. The unrelated DeflOpt and Defluff report
edits present at the start remain unchanged.

## What changes, and why it helps

The preceding R10 method changes positive code lengths while preserving symbol
support. This experiment removes a used length symbol's code and assigns the
same code length to a previously absent length symbol. It then finds the
cheapest certified spelling of every current match under the proposed tree.
The old token spelling cannot be emitted under that tree, so this is a joint
support/token move rather than an independent tree improvement.

The new code need not be used. Sometimes it enables a cheaper submatch. In
other cases it preserves the old tree's complete Kraft capacity while moving
an unused code to a position that is cheaper to describe. Removing a symbol,
rebuilding a frequency-optimal tree, and optimizing that rebuilt tree do not
enumerate this family of retained parent-tree shapes.

One frozen Max witness, `cthn0g04.deflate`, has these exact costs:

| Encoding | Payload including EOB | Complete header | Complete stream |
| --- | ---: | ---: | ---: |
| Current Max parent | 409 | 272 | 681 bits / 86 bytes |
| Exchange | 421 | 258 | 679 bits / 85 bytes |

Its single use of length symbol 265 is removed. That symbol's code length
moves to symbol 261, which remains unused. The payload pays twelve bits, the
header saves fourteen, and the complete stream saves two bits and one byte.
This is not a payload-optimal Huffman tree requirement: the complete emitted
header and payload determine acceptance.

Among seven frozen Max wins, two use their newly assigned length code and
five leave it unused. The former save six bits in total; the latter save
twelve. These are components of the same eighteen-bit total, not additional
corpus gains. An earlier Default probe of filler relocation alone found no
improvements; the new experiment couples relocation to removal of a used
length symbol through certified respelling.

The generated source regression fixture decodes to 296 bytes. R10 cannot
improve its 352-bit parent; the exchange emits 351 bits. The fixture was built
from deterministic tokens and bytes with seed 138, then settled under R10.
It contains no copied corpus bytes. This synthetic case verifies the support
barrier, while the frozen Max controls below establish real-input value.

## Search domain and bounds

The [implementation](../../src/deflate/header/response/exchange.rs) reuses the
[exact interval solver](../../src/deflate/header/response.rs).

Only symbols 257 through 285 participate. A donor has a positive code length
and a positive current payload frequency; a recipient has zero code length.
Every absent recipient is eligible, even when none of the current matches is
long enough to use it. Original literals and EOB retain their prices, all
distance lengths stay unchanged, and the literal/length code-length histogram
is preserved when implicit trailing zeros are included. No new distance or
history search is introduced.

For each pair, the method constructs complete headers for the minimum legal
literal span and the larger of that span and the source's advertised span.
Only the cheaper generated header needs to survive immediate block pricing:
both define the same payload code lengths. HDIST remains unchanged. Header
construction uses the existing full RLE/header repricer. This is not an
enumeration of every legal code-length Huffman tree or every advertised span.

Headers are priced with zero payload cost, then ranked by a lower bound on
their complete candidate cost. For a match of length n, each allowed token
edge has a price p and decoded width w. Its optimistic interval price is
`ceil(n * min(p / w))`. The minimum includes every supported canonical
submatch length, the exact source edge when available, and the cheapest
supported literal price in the interval. Ignoring literal positions and exact
coverage can only reduce the cost. The exact source edge is included even
for the existing relaxed length-258 spelling, so the bound does not wrongly
exclude that legal source choice. Fixed literals and EOB are added exactly.

The bound applies to a proposed tree with its chosen generated header.
It does not prove that a different header program for that tree cannot win.
Candidates whose bound cannot beat the current block are skipped. Remaining
candidates run the shared exact backward interval DP with absent codes
forbidden. Every generated match lies inside its current source match at the
same distance; original literals gain no certificates. Source matches may be
unavailable under the proposed tree and are then omitted from the DP. An
available source edge still wins a payload-cost tie. Generated length 258 uses
canonical symbol 285.

A block is admitted through 4,096 decoded bytes, 1,024 tokens, and 1–128
matches. There are at most 14×15 = 210 donor/recipient pairs and two span
prices per pair. One terminal invocation shares 2^25 bound/response work
units and 512 complete header prices across its stream. Generation, interval
setup, bound walks, and exact DP work are charged; header construction has its
separate price counter. An unfinished spelling is discarded. Completed
winners survive later work, price and callback stops.

Scratch consists of at most 210 complete header proposals, 128 interval
records, a 286-byte length array, the shared three 259-entry DP arrays, and
bounded candidate token buffers through 4,096 entries. The common terminal
pipeline additionally retains the parsed stream and completed block plans.
No cache or unbounded frontier is added. Explicit search allocations are
fallible; the existing header repricer remains unchanged.

R11 runs only in Max, after an unexpired R1–R10 sweep leaves the final
incumbent unchanged. It adds no earlier comparison-floor work. A winner
causes the existing methods to settle again before another exchange attempt.
R12 is terminal closure with eleven input-score slots. Common admission stays
at 1 MiB compressed and decoded data, 128 blocks, and no discarded wire
blocks. The common emitter validates decoded identity and actual distances,
regenerates later stored padding, and accepts only a complete byte-first,
meaningful-bit improvement. The existing Max deadline and route window remain
authoritative; Default retains its single R1–R5 sweep.

## Isolated and frozen-parent controls

Across 365 completed Default streams, the full bounded exchange search
improves eight, saving 29 meaningful bits and two bytes. Allowing absent
literal symbols in the initial probe added no wins and is not implemented.
Expanding length recipients to include unusable spare codes changes none of
these observed scores; the retained family nonetheless includes all absent
length symbols.

Eight discovered cases received fresh ten-second baseline Max runs. Every
run completed without a reported timeout. Repeating R1–R10 with the true
original input certificates and no deadline changes none of them; a second
closure invocation confirms stability. Seven improve under the new bounded
pass, saving eighteen meaningful bits and one byte:

| Frozen Max source | Parent bits | Exchange bits | Recipient used? |
| --- | ---: | ---: | --- |
| `cthn0g04` | 681 | 679 | No |
| `ctjn0g04` | 720 | 718 | Yes |
| `filmreel.png`, first block | 1,885 | 1,881 | Yes |
| `107-Hitmonchan-0.png`, second block | 1,616 | 1,613 | No |
| `s32n3p04` | 872 | 869 | No |
| `samples.zip` / `samples/pg.exe` | 2,863 | 2,861 | No |
| `samples.zip` / `samples/rnd_arr1.exe` | 3,691 | 3,689 | No |

The table's two multi-block entries give the changed block's cost. Their
stream savings are respectively four and three bits. These Max cases overlap
the Default sample and must not be added to it as independent corpus totals.

A broader control enumerates all absent length recipients and both admitted
spans, with unlimited work and price budgets. It omits the new interval bound;
only the exact payload plus the seventeen fixed header bits may prune a full
header price. It reaches exactly the same best scores on all eight frozen Max
parents, using 1,300 full header prices.
The header-first bounded pass uses 1,306 prices and 38,050,096 charged work
units in aggregate. One stream exhausts work; none exhausts prices. It retains
all seven improvements. Independent Python zlib decoding verifies every
isolated stream, and the Rust probe checks match provenance before emission.

On the broader Default parents, the pass charges 594,906,077 work units and
23,944 full header prices; three streams exhaust work, none exhaust prices.
The largest price count is 371. Observed isolated time is 6.996 seconds in
total, maximum 176.372 ms, and 0.387 seconds over the Max parents, maximum
159.829 ms. These observations include parsing, emission and verification and
are not public-API overhead estimates.

## Public validation

The frozen source and binary were compared with the committed baseline using
alternating baseline/candidate order, identical options and identical time
allowances. No probe or compilation ran alongside these paired timings.

| Public comparison | Cases | Result |
| --- | ---: | --- |
| Default raw | 365 | All output bytes identical; 103.869 → 101.103 s total (-2.66%) |
| Default wrappers | 39 | All output bytes identical; 68.616 → 65.332 s total (-4.79%) |
| Strict Max, 10 s | 24 | 11 improvements, 13 ties, no regressions; timeouts 6 baseline / 8 candidate |
| Relaxed Max, 10 s | 4 | 0 improvements, 4 ties, no regressions; timeouts 3 baseline / 3 candidate |
| Defluff compatibility | 66 | 61 better, 5 ties, no errors, missing cases or misses |

Strict Max takes 222.612 → 224.608 seconds in total, about 0.90% more.
Two ZIP-member cases newly report timeout: `864b0ea02721ef82` takes
9.858 → 10.025 seconds and saves two bits, while `c1eea5d0f9da6649` takes
9.942 → 10.066 seconds and ties. Both retain complete valid results under the
unchanged ten-second allowance and existing grace policy. Default's measured
timing differences are compatibility observations, not an attributed speedup
from this Max-only search.

Every raw output is independently decoded with Python zlib. The wrapper
verifier checks PNG/APNG structure, CRCs and payload identities; GZIP members
and checksums; ZIP entry contents and metadata; zlib payloads; and emitted
distances against wrapper window bounds. Strict and relaxed runs use separate
manifests. Public Max gains can include later refinements by existing methods,
and raw streams overlap their container cases; these totals are not additive
to the direct frozen-parent gains.

Defluff totals remain 109 bytes and
932 meaningful bits smaller than its
references. This validates Default compatibility rather than a new R11 gain.
All rows carry the final candidate binary hash recorded below.

The current source passes 583 Rust tests with ignored tests included and all
13 Python tests. Focused checks cover exhaustive tiny spelling oracles with
missing source symbols, interval-bound admissibility, relaxed length 258,
complete unpruned exchange enumeration, support and histogram preservation,
strict compatibility, proof containment, shared resource limits, callback
stops, and actual stored-block alignment at all eight incoming bit offsets.
Clippy with warnings denied, formatting, package inventory and whitespace
checks pass.

The release executable grows from 1,777,760 to 1,794,288 bytes, an increase of
16,528 bytes. Candidate SHA-256:
`11590cb41ebf0ea2cf321077fe2d0c9909f5fbf4863bd8b921eb51e5513906fc`.

Reproduction scripts, probes, original-input manifests, frozen binaries,
source hashes, CSVs and outputs are retained under
`work/support-exchange-validation/`. Fixtures are read-only and all outputs
stay in `work`; none of this private corpus evidence enters source packages.
