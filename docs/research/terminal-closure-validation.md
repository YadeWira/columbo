<!-- SPDX-License-Identifier: MIT -->

# Terminal time and method closure in Max

11 September 2026. Baseline: `2e77f21`. **Accepted after matched validation.**
The implementation is in `src/deflate/optimize.rs`; current production gates
are catalogued in [routes and methods](../routes-and-methods.md).

## Why a single sweep misses reachable savings

The terminal methods do not commute. A later method can change the payload
lengths, transmitted code-length tree, symbol support, tokens or block
boundaries that an earlier method priced. In particular, a joint tree/RLE
improvement can change the next iteration's header costs, and an alphabet
split can create a new, profitable joint-tree problem. Finishing the ordered
R1–R9 sweep therefore does not establish a local fixed point.

Max now revisits terminal methods after a strict complete-stream improvement,
while the existing optional deadline permits another route. The first sweep
keeps the established order. Default still performs its single R1–R5 sweep,
and the independent Default comparison endpoint and historical Max search
seed retain their established construction.

Nine score slots record the input last presented to each method. Accepted
candidates strictly decrease `(physical bytes, meaningful bits)`, so an input
score identifies its generation within this particular descent. A suffix
already evaluated on the unchanged candidate is skipped. This is not a
cross-route cache: equal-score alternative encodings in other lineages are
not considered interchangeable.

Each method retains its original size, block, support, operation and price
bounds. Additional sweeps use optional time, even if the first Default sweep
was mandatory. No new grace, worker, encoded-parent cache or stream-planner
replay is introduced. Original-match restoration continues to consult the
original source certificates, and every accepted result passes complete
emission and identity validation.

Strict improvement prevents cycles. A raw stream occupying at most L bytes
has at most 8L positive meaningful-bit scores, giving a finite descent without
an arbitrary round limit. A completed, unexpired sweep with no change establishes a
fixed point only for these bounded operators. Exhausted operation budgets,
heuristic menus, excluded work classes and compatibility policy still limit
coverage; this is not a claim of globally optimal Deflate encoding.

## Giving terminal methods time

Repeating a late sweep is ineffective if earlier token and source searches
consume its entire allowance. Public-API trials of repetition alone confirmed
this: all 20 frozen-parent Max results were identical at ten seconds, with
18 timeouts in each build. The larger frozen-parent gains below are therefore
not evidence that late repetition alone improved normal CLI output.

For stream-owning `Complete` and `CompleteThenBounded` Max work, Columbo
reserves the final fifth of the original soft allowance for terminal methods
when source compressed and decoded sizes are both at most 128 KiB and the
wire stream has at most 128 blocks. These are the existing common work bounds
of R1–R5. The fraction reuses the existing primary/follow-up scheduling share;
it was not selected by testing several fractions against guard scores.

The primary phase includes elapsed parsing and Default-floor time and has no
separate grace. After the deferred compact source-split finalization, terminal
tree floors and R1–R9 use the original full deadline and grace. At a primary
phase yield, that split finalizer may use its existing cheap coarse rescue
only while the full soft deadline remains open; it does not start an exhaustive
split sweep in the reserved share. Phase expiry does not report a file timeout.
The mandatory Default floor still completes, even if it uses the allowance.

Each phase grows with configured time, preserving primary endpoints for longer
runs while giving compact header methods a chance under normal allowances.
This changes resource allocation and can lose a primary-search improvement;
complete-candidate comparison prevents larger replacements within a run, not
regression against a different schedule. The 22-file screening sample gained
73 bytes / 580 bits overall, with one loss of 1 byte / 6 bits, and measured
274.89 → 244.08 seconds. The hundred-file replay and independent generated
inputs below test whether that tradeoff extends beyond the screen. Shared container
children and larger work classes retain their earlier primary schedule.

## Portable regression witness

The existing generated coupled-swap fixture supplies literal frequencies,
source-certified matches and a stored history prefix. Its original stream
uses 37,782 bits. One terminal sweep stops at 36,729 bits. The resulting
alphabet splits expose another joint-tree improvement and another useful cut;
closure reaches at most 36,637 bits, at least 92 bits beyond one sweep.

The regression test validates the emitted stream, strict trees and actual
maximum distance, then checks that another closure invocation leaves the
bytes and meaningful bits unchanged. It also verifies that an expired Max
allowance retains precisely the ordinary terminal endpoint when Default work
is mandatory, and leaves the parent unchanged when all work is optional.
No private fixture bytes are embedded in the test.

## Controlled exploration

The private replay used 43 distinct completed Max parents from the previous
coupled-swap validation. The R2–R9 sequence first received one full sweep;
only subsequent gains are attributed to repetition. Repetition improved
27/43 parents by **189 raw bytes / 1,485 meaningful bits**, with no larger
result. These are frozen-parent gains, not additional whole-corpus benchmark
savings. This exploration omitted original-match restoration; the production
closure retains it against the original source.

The exploratory harness allowed eight sweeps and 20 seconds per parent.
Three parents were still improving on the eighth sweep, so its results are
lower bounds on further reachable savings, not fixed-point claims. Production
has no eight-sweep cutoff. Total measured time for all sweeps on these 43
parents was 48.654 seconds, including the first sweep; it is not the added
runtime of the CLI change.

## Matched hundred-file guard

The canonical hundred-file guard is unchanged. All trials use its recorded
mode and allowance, with the frozen baseline and candidate executables:

| Measure | Result |
| --- | ---: |
| Candidate executable SHA-256 | `644a72ce4fa85729c3dd07d6b3aaa5210c1b5250f36d30858c32b91528cdcd64` |
| Improvement / exact tie / loss against fresh baseline | 45 / 52 / 3 |
| Net change against fresh baseline | −315 bytes / −2,527 meaningful bits |
| Baseline → candidate trial time | 1,308.90 → 1,200.59 seconds (−8.3%) |
| Additional live Default comparison time | 60.13 → 60.41 seconds |
| Historical floor improvement / tie / loss | 89 / 9 / 2 |
| Net change against historical floors | −3,960 bytes / −31,711 meaningful bits |
| Live Max-over-Default comparisons | 53/53 pass |
| Validation errors | 0 |

These are serial trials on a protective cohort, not a fresh complete DeflOpt or
deft4j corpus. Historical floors have no comparable aggregate runtime. The
measured time change is a single matched observation, not a platform-wide
performance estimate. The screen overlaps this cohort and must not be added
to its savings.

The three losses against the fresh baseline are `motorcycle.png` (1 byte /
6 bits), `sample_38-fs8.png` (11 bytes / 86 bits), and `sample_53.png` (8 bytes /
64 bits), all under `css-ig-net`. They total 20 bytes / 156 bits, against gross
wins of 335 bytes / 2,683 bits. Only `sample_53.png` also misses its older guard
floor, by 1 byte / 3 bits. The inherited `medium/LevelLoading.png` residual
remains 6 bytes / 49 bits; it is outside the new reservation class.

All four smaller losses against the newer full journals are recovered:
`sample_04-fs8.png` improves that floor by 15 bytes / 121 bits, `sample_14.png`
by 20 bytes / 160 bits, `test-convertir-truecoloralpha-trns.png` by 7 bytes /
53 bits, and `small/psydk-Pink.png` by 4 bytes / 30 bits. These are the same
trials joined to different reference floors, not additional corpus savings.

Two additional serial baseline trials reproduce all three old endpoints
exactly. Two candidate confirmations reproduce `sample_38-fs8.png` at
+11 bytes / +86 bits and `sample_53.png` at +8 bytes / +64 bits. `motorcycle.png`
varies: one confirmation improves the baseline by 16 bytes / 127 bits, while
the other repeats +1 byte / +6 bits. Its 60-second trial also remains +1 / +6.
Thus the first cohort is kept intact, rather than replacing its loss with the
best repeat. Timed search order matters; monotone incumbent selection within a
run does not imply monotone output across different configured allowances.

At 60 seconds, `sample_38-fs8.png` improves on the fresh ten-second baseline
by 11 bytes / 92 bits (65.61 seconds measured), and `sample_53.png` by 18 bytes /
143 bits (65.66 seconds). Both formerly smaller endpoints remain recoverable;
the additional time is a quality/cost choice rather than a per-file default.
Relative to the candidate's ten-second results, each run spends another
53.90 seconds to recover 22 bytes / 178 bits and 26 bytes / 207 bits,
respectively. No filename-specific time allowance is built into the optimizer.
The 180-second `motorcycle.png` trial still reports +1 byte / +6 bits
(143.02 seconds measured). A better current-code result is nevertheless
witnessed by the ten-second repeat above, so it is not a structurally forbidden
endpoint. More allowance alone does not reproduce that search order. This tiny
residual is retained as a measured schedule tradeoff; no timer threshold or
filename exception is added to recover it.
The final build also recovers `medium/LevelLoading.png` at 180 seconds:
zero byte loss and five bits below its historical floor, in 195.92 seconds.
The fresh baseline needed the same allowance and took 195.87 seconds; its
60-second trial still lost 6 bytes / 49 bits. All hundred historical guard
floors are therefore witnessed in the current code: 98 at recorded allowances,
plus `sample_53.png` at 60 seconds and LevelLoading at 180 seconds. This does
not replace the matched-budget 98/100 result or its aggregate timing with
best-of-multiple-budget scores. The older APNG results are recorded below.

## Public raw Max control

The final serial, interleaved API sample contains thirteen frozen raw parents
representing PNG/APNG, GZIP and ZIP families, three generated method witnesses,
and four guard endpoints. At ten seconds, **13/20 improve and seven tie**, with
no larger result: net **83 raw bytes / 664 meaningful bits**, and measured optimizer time
218.640 → 159.662 seconds. Eighteen baseline calls and two candidate calls
report the full deadline reached. A primary phase yield alone is not a file
timeout; a non-timeout result still does not prove global optimality.

These are already-optimized parents passed through the raw public API, not a
complete wrapper corpus, and some source identities overlap other controls.
They must not be added to the hundred-file savings. In contrast, repetition
without reserved time produced identical output on all twenty inputs.

Meaningful bits are parsed independently from the emitted streams. The public
API's `bits_saved` field uses eight bits per removed physical byte when a file
shrinks, and meaningful-bit savings only when its byte length ties. Subtracting
that display metric between different byte sizes is not a meaningful-bit
measurement; the independent check caught and corrected that harness assumption.

## Default control

A serial, interleaved public-API comparison covers 365 raw streams and 39
wrapper files. All **404 outputs are byte-for-byte identical**. Neither build
reports a timeout. Aggregate optimizer time is 148.677 seconds for baseline
and 149.513 seconds for the final candidate (+0.56%); this single observation does
not establish a speed difference.

## Generated input controls

The final build also runs on deterministic generated raw streams independent
of benchmark file identities:

| Control | Byte/meaningful-bit result | Measured optimizer time, baseline → candidate |
| --- | --- | --- |
| 64 literal-only fixed streams with varied alphabets and frequencies | All outputs identical; no timeouts | 74.096 → 74.479 seconds |
| 26 fixed streams with literal history and source-certified matches | Seven wins, 19 ties, no losses; −5 bytes / −44 bits | 260.534 → 188.938 seconds |

Literal controls are serial and interleaved. The match control uses the first
26 consecutive cases recorded before the final build; candidate trials are a
separate serial batch at the same ten-second allowance. Its timing is not an
interleaved performance estimate. Eighteen baseline match calls reach the full
deadline; no candidate call does. All generated sources and outputs are
independently decoded and all reported meaningful bits are independently parsed.
These raw controls are not added to wrapper-corpus totals.

## Older APNG residuals

At the original 20-second allowance, both candidate outputs are byte-identical
to the fresh baseline. `steam-stickers_apng/2313020_361766…` remains 4 bytes /
39 bits above its old floor; `657730_102978…` remains 2 bytes / 14 bits above.
At 60 seconds, the same differences remain, with measured times of 64.44 and
64.52 seconds. All four candidate outputs validate.

These shared-frame jobs do not receive the compact stream-owner reservation.
The earlier audit records the general tree-route tradeoff that left these six
bytes after much larger aggregate APNG gains. This change does not recover
them, and a 60-second non-recovery is not proof of structural unreachability.
No full current APNG corpus is claimed from this pair of residual checks.

## Acceptance and verification

Accept the general scheduling and closure changes for their net matched-guard
saving, reduced measured Max time, unchanged Default outputs, and supporting
raw/generated controls. The fixed fraction and work bounds were not adjusted
to recover individual losses. The canonical hundred-file guard is unchanged;
its two short-budget residuals and the three fresh-baseline losses remain in
the report even where longer or repeated runs recover better results.

- `cargo test --release --locked`: 561 regular Rust tests pass.
- Both opt-in private-corpus groups pass: six integration and four library
  tests, giving 571 Rust tests in total.
- Thirteen published-tool and 189 private benchmark-harness Python tests pass.
- Clippy with all targets/features and warnings denied passes; formatting and
  diff whitespace checks pass.
- Independent zlib/PNG/GZIP/ZIP validation passes for 77 frozen probe outputs
  and both arms of the 404 Default, 20 raw Max, 64 literal and 26 match controls.
  Raw meaningful-bit counts are checked independently of the public API's
  byte-first display metric. The benchmark runners also independently validate
  every retained guard, miss, Defluff, confirmation and longer-time output.
- All 66 Defluff pairs pass; all 17 published strict miss rows reproduce,
  sixteen recover under the relaxed policy and the signed PNG is preserved.
- Final source, executable and canonical-guard hashes match the frozen manifest.

Strict descent and positive time shares remove two barriers, not every search
limit. Method-specific budgets, heuristic menus, retained parent choices and
compatibility policy still constrain the reachable set. This work establishes
bounded method closure and measured corpus improvement, not global Deflate
optimality or monotonic quality across separate timeout settings.

Private inputs, outputs, executable hashes, exact source patch, timings,
independent bit counts, confirmations and checkpoints are retained under
`work/miss-regression-20260911/`. The complete DeflOpt, deft4j and APNG journals
retain their own earlier binary identities; they were not relabelled as full
runs of this candidate.
