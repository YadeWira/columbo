# defluff 0.3.2: reverse-engineered methods

This document describes the compression and container-rewriting methods implemented by
defluff 0.3.2. It is intended as an implementation-oriented reference for projects that
want to reproduce, compare with, or learn from those methods.

The ground truth for every defluff-specific statement in this document is the disassembled
32-bit Windows executable. Behavioural tests under Wine were used to check interpretations,
but they do not override the executable.

## Reference binary and evidence policy

The analysed specimen is [defluff-0.3.2-windows-i686.exe](binaries/defluff/defluff032/defluff-0.3.2-windows-i686.exe):

| Property | Value |
|---|---|
| Product version | defluff 0.3.2 |
| Author | Joachim Henke |
| Release date reported by the program | 7 April 2011 |
| Format | PE32 console executable, Intel i386 (stripped to external PDB) |
| SHA-256 | `8847fba3ff5bc23fd4d5178f5383b910a33302266bd2354035a00f4f10e2d54f` |

A Darwin x86 Mach-O binary and a Darwin PowerPC binary exist in the release set. They are
stripped and carry only symbol-table stubs for imported libc functions. The PowerPC binary
was not analysed; the Darwin x86 binary was checked for import differences (`qsort`, no
`time`/`rand`) and is believed to implement the same methods.

The Darwin binary's import table confirms: `qsort`, `malloc`, `calloc`, `realloc`, `free`,
`memcpy`, `read`, `write`, `lseek`, `fprintf`, `fwrite`, `exit`. No threading, no clock,
no `rand` — the core is single-threaded and deterministic.

All addresses below are virtual addresses in the Windows PE executable. They are not
expected to apply to another defluff release or to a repacked binary.

Statements use these evidence classes:

- **Observed** means that the described control flow, data flow, comparison, or constant is
  directly present in the disassembly.
- **Reconstructed** means that the algorithm is a higher-level interpretation of several
  observed instruction sequences. These interpretations have been checked against the
  executable's outputs under Wine and, where practical, with independent models.
- **Implementation advice** describes how another project can implement compatible
  behaviour. It is not a claim about defluff's internal source code.

The executable contains no symbols beyond import thunks. Routine names in this document are
descriptive names assigned during reverse engineering. Addresses, branch directions, limits,
and strictness of comparisons are the durable references.

## Scope

defluff is a post-processor for existing Deflate streams. It parses each source block,
decodes its token sequence, and tries a bounded set of alternative encodings. It does not
perform a new LZ77 match search. Its main opportunities are:

1. re-encode an existing dynamic block header more compactly via greedy RLE and local
   token replacement;
2. build a single length-limited Huffman tree per alphabet using a deterministic
   package-merge algorithm;
3. replace selected matches with their literal bytes when that is strictly cheaper under
   the table being evaluated, and accumulate the resulting frequencies for iterative
   refinement;
4. choose the cheapest legal block representation among stored, fixed, and dynamic forms;
5. repeat the iterative Huffman refinement when the source stream used the symbol-284
   length-258 edge case (the only trigger for an additional candidate pass); and
6. remove or normalise optional container structures (GZIP FEXTRA, ZIP extra fields and
   comments, PNG non-essential chunks) by default.

The core search is deliberately small — at most three candidates per block with one
optional refinement pass. It does not split blocks, search for new LZ77 matches, merge
adjacent blocks, or explore multiple Huffman variants.

## Terminology and conventions

This document uses the RFC 1951 symbol spaces:

- **literal/length alphabet:** symbols `0..285`, with `256` as end-of-block;
- **distance alphabet:** symbols `0..29` for ordinary Deflate streams;
- **code-length alphabet:** symbols `0..18`, including repeat symbols `16`, `17`, and `18`;
- **HLIT:** the transmitted literal/length count, at least 257;
- **HDIST:** the transmitted distance count, at least 1;
- **HCLEN:** the transmitted code-length-code count, at least 4.

Bit costs include block headers, padding, tree descriptions, Huffman code bits, and
length/distance extra bits. They exclude container bytes unless a container section says
otherwise.

"Strictly smaller" uses `<` (unsigned). The binary uses `jae` (jump if above or equal,
i.e. `candidate >= incumbent`) to reject equal-cost replacements. Equal-cost ties therefore
keep the incumbent, except in one documented case (the iterative refinement pass, where
`ja` accepts equal-cost improvements).

## High-level algorithm

The entry point is the container handler at `0x401650`. The per-block optimizer is
`0x4066f0`. At a high level, the binary behaves as follows:

```text
detect container format from first two bytes (PNG / GZIP / ZIP)

for each Deflate stream in the container:
    initialise output buffer (4 KiB sliding window)
    clear global token buffer and frequency arrays

    while the stream has another block:
        parse BFINAL and BTYPE
        if BTYPE == 0: parse stored body as literal tokens, count frequencies
        if BTYPE == 1: decode token stream with fixed tables
        if BTYPE == 2: decode dynamic tables, decode token stream
        if BTYPE == 3: error

        if decoded token count <= 25:
            emit the fixed Huffman representation directly (no search)

        compute stored candidate cost (multi-block split if > 65535 bytes)
        compute fixed candidate cost
        if original was dynamic:
            compute original dynamic cost
            rebuild dynamic tables from current frequencies (strict winner)
            if source used symbol 284 for length 258:
                rebuild dynamic tables from scorer-result frequencies (≤ winner)

        select strictly smallest candidate (scan order: stored, fixed, dynamic)
        emit winning representation

finalise container (update CRCs, sizes, directory entries; flush output)
keep rewritten object only if strictly smaller than the original
```

This is a per-block optimizer with no cross-block merging. The expensive match discovery
work has already been done by the program that produced the input.

## Deflate parsing and token representation

### Bit-level I/O

- **Input reader** `0x402fd0` (54 callers): reads 8 bits from the input stream, refilling
  a buffer via `ReadFile` as needed. Returns a byte in `ax`.
- **Input helper** `0x402fa0` (19 callers): variant that consumes bits without returning
  them (advances the bit position).
- **Output writer** `0x402cd0` (14 callers): flushes the 4 KiB output buffer
  (`0x40c060`) via `WriteFile`. Manages a sliding window of 4096 bytes.

Bit-level state is kept globally: current bit accumulator at `0x40e16c`, bit position
counter at `0x40e170`, output byte counter at `0x40e168`.

### Token buffer

Tokens are stored in a global `u16` array of 0x10000 entries (calloc at `0x4067fd`).
Begin pointer at `0x40d744`, end pointer at `0x40d748`. Each token is a 2-byte entry:

- **Literal:** value < 256, stored as the byte value itself (0×00–0xFF).
- **Match:** value >= 256, encoded as the literal/length symbol (257–285). The match
  occupies 3 consecutive u16 entries: `[length_symbol, dist_lo, dist_hi]` where the
  distance is packed as a 32-bit value across the two words.

The token count `(end - begin) / 2` is stored at `0x40d754`.

### Frequency arrays

Frequencies are accumulated during parsing into fixed global arrays:

- Literal/length frequencies: `0x40d75c` (286 × 4 bytes = 288 slots, inclusive).
- Distance frequencies: `0x40dbdc` (30 × 4 bytes = 32 slots, inclusive).

A second set of frequency arrays is used by the scorer for iterative refinement:

- Scorer literal/length frequencies: `0x40dc5c` (288 slots).
- Scorer distance frequencies: `0x40e0dc` (32 slots).

### Output-length buffers

Two 0x148-byte buffers alternate as destinations for the package-merge builder:

- Buffer A: `0x40e180` (the "parsed" lengths, populated during dynamic block parsing).
- Buffer B: `0x40e698` (the "rebuilt" lengths, populated during candidate generation).

The current winner pointer is stored at `0x40d740`. A `cmovne` instruction at `0x4072a8`
alternates between buffers based on which one holds the winning tree.

### Fixed tables

Fixed Huffman code lengths are generated at startup (`0x406723..0x40676a`):

| Literal/length symbols | Code length |
|---|---:|
| `0..143` | 8 |
| `144..255` | 9 |
| `256..279` | 7 |
| `280..287` | 8 |

All fixed distance codes have length 5 (32 symbols). Tables are stored in packed 8-bit
arrays at `0x40d4c8` (litlen) and `0x40d610` (dist). Table descriptors at `0x40d4a0`
(list of {count, code-lengths-ptr, max-code-length}) and `0x40d5e8` (dist descriptor).

### Block parsing

The block parser at `0x405900` emits individual bits. `0x403100` reads N bits and returns
them as an integer.

**Stored blocks** (`0x406870..0x406a3b`): align to byte boundary (`0x4013c0`), read
`LEN` and `NLEN` (16 bits each), verify `NLEN == ~LEN`, then read `LEN` bytes as literal
tokens. Each byte becomes a u16 token in the token buffer. Frequencies are counted in the
primary literal/length array. Sets flag `0x40db5c` to 1.

**Dynamic blocks** (`0x405eb0` + `0x406170`): parse HLIT, HDIST, HCLEN, the 19-symbol
code-length tree, expand code-length repeats (symbols 16, 17, 18), build canonical
tables, then decode the token stream. The decoded literal/length and distance length
arrays are saved at `0x40e180` (the parsed-lengths buffer). Token decoding at `0x406170`
covers `0x40629a..0x4066ef`.

During token decoding, length-258 matches encoded with symbol 284 (the "edge case")
trigger `0x4066ce..0x4066dc`: the counter at `0x40dbcc` is decremented and the iteration
flag at `0x40dbd0` is incremented. This is the sole trigger for the iterative refinement
pass in the candidate selector.

**Fixed blocks** (`0x4074ff`): decode tokens using the precomputed fixed tables.

### Empty-block handling

The parser does not explicitly detect or delete empty blocks. A stored block with
`LEN == 0` produces zero tokens. That falls into the ≤25-token path and is emitted
as a fixed block with only the end-of-block symbol. There is no equivalent of DeflOpt's
empty-block checkpoint restoration.

## Per-block candidate search

The candidate selector at `0x4066f0` evaluates three representations per block:

### Token-count gate

At `0x406a85`, the decoded token count is compared against 0x19 (25). If the count is
25 or fewer, the candidate search is skipped entirely (`jbe 0x4071db`). The block is
emitted as fixed Huffman with BTYPE=1 regardless of its original type. This is a
hardcoded heuristic: for very small blocks, dynamic header overhead dominates and the
fixed representation is almost always optimal.

### Stored candidate

At `0x406a94..0x406ad1`, the stored cost for N decoded bytes at a given output bit
alignment is computed:

```text
block_bits = 3                           BFINAL + BTYPE
           + padding_to_next_byte_boundary
           + 16 + 16                     LEN + NLEN (per stored block)
           + 8 * block_data_bytes

padding is calculated from the current output bit position (0x40e170)
```

The cost computation at `0x406aa1..0x406ace` handles multi-block splitting: when
`decoded_bytes > 65535`, the data is split across multiple stored blocks of at most
65535 bytes each. The cost formula uses `div 0xffff` to compute the number of
full-capacity blocks plus a remainder block. This differs from DeflOpt, which rejects
blocks larger than 65535 bytes for the stored candidate.

### Fixed candidate

The fixed candidate cost is the sum of:

- 3-bit block header, plus
- the payload cost scored under the fixed Huffman tables.

The scorer `0x403250` is called with the fixed table descriptors (litlen descriptor at
`0x40d4a0`, dist descriptor at `0x40d5e8`). The fixed candidate is evaluated at
`0x406ae4..0x406af6`.

### Dynamic candidate (original source trees)

For blocks that were originally dynamic, the parsed literal/length and distance length
arrays (in buffer `0x40e180`) are used to regenerate the dynamic header via `0x404d00`.
The payload is scored under those same trees via `0x403250`. The sum of header and
payload cost is the baseline dynamic candidate. This is evaluated at
`0x406b08..0x406b23`.

Because the parsed lengths are reused directly (without rebuilding trees from
frequencies), any inefficiency in the original header encoding is captured separately
from payload cost.

### Dynamic candidate (rebuilt trees)

At `0x406b30..0x406b95`, new literal/length and distance trees are built from the
parsed-token frequencies using the package-merge builder `0x404050` with max depth 15.
The active symbol span is trimmed to the last non-zero frequency. The new dynamic header
is generated and the payload is scored. The candidate wins only if its total cost is
strictly smaller than the incumbent (`jae` at `0x406bb4`).

### Dynamic candidate (iterative refinement)

At `0x406bc3..0x406c45`, the scorer `0x403250` is called with the *winning* tree
(which may be either the original or the rebuilt tree). The scorer expands matches to
literals where beneficial and accumulates the resulting frequencies into the secondary
arrays (`0x40dc5c` for litlen, `0x40e0dc` for dist). New trees are built from these
frequencies, a new header is generated, and the payload is scored again.

This candidate replaces the incumbent with `ja` at `0x406c3b` — accepting equal-cost
results. This is the only point in the binary where a non-strict comparison is used
for winner selection.

The iteration pass is gated by the flag at `0x40dbd0` (see below).

### Iteration control at 0x40727e

The flag at `0x40dbd0` is set to 1 only when the token decoder encounters a length-258
match encoded with symbol 284 rather than symbol 285 (`0x4066ce..0x4066dc`). The
counter at `0x40dbcc` is decremented when this occurs.

The iteration loop at `0x40727e`:

1. Checks `0x40dbd0` — if zero, jumps to `0x406c52` (winner selection, no iteration).
2. Otherwise: uses the current winner's length buffer to rebuild trees from scorer
   frequencies (which may differ from the original frequencies if the scorer expanded
   matches to literals).
3. Scores the new candidate. If it wins (using strict `jae` in this pass), the winner
   pointer is updated.
4. The loop toggles between the two output-length buffers using `cmovne` at `0x4072a8`
   based on whether the winner is the parsed buffer (`0x40e180`) or the rebuilt buffer
   (`0x40e698`).
5. The flag at `0x40dbd0` is cleared at `0x4072b8`. Since the tree-rebuild pass does
   not itself set `0x40dbd0` (only the token decoder does), **the iteration runs at
   most once per block**.

This is a much narrower refinement than deft4j's fixed-point loop or DeflOpt's
enumeration of 64 tree/header combinations. It is a single additional candidate
that exploits the one situation where symbol frequencies can shift between the
original parse and the scorer's view: the symbol-284 edge case.

### Winner selection

At `0x406c61..0x406c7d`, the three cost values (stored, fixed, dynamic) are compared
in a linear scan. The minimum is selected with a strict `<` comparison. The scan order
is stored (index 0), then fixed (index 1), then dynamic (index 2). Equal costs favour
the earlier index: stored > fixed > dynamic.

The winning index becomes the emitted `BTYPE` value directly: 0 = stored, 1 = fixed,
2 = dynamic.

The `BFINAL` bit (from `ebp-0x60`) is written before `BTYPE`.

### Emission

**Stored block** (`0x406ca8..0x4071d6`): aligns output to byte boundary, emits LEN
and NLEN, then copies decoded bytes from the token buffer in 4-byte unrolled loops.
Multi-block splitting: at `0x4070d4..0x407279`, when the remaining data exceeds
65535 bytes, a full stored block of 65535 bytes is emitted with BFINAL=0, and the
loop continues with the next chunk.

**Fixed block** (`0x4071db..0x4071c6`): emits BTYPE=1, then writes each token's
Huffman code under the fixed tables. Tokens are read from the token buffer in the
same u16 format used during parsing. The output bit stream is buffered in 4 KiB
chunks and flushed via `0x402cd0`.

**Dynamic block**: emits BTYPE=2, then writes the dynamic header via `0x4075cf` (which
calls the header bit-writer at `0x407520`), followed by the token payload under the
winning tables.

## The scorer: match-to-literal expansion and frequency counting

The scorer at `0x403250` serves two purposes: it computes the payload cost of a token
sequence under given Huffman tables, and it decides per-token whether to expand matches
to literals. The resulting frequencies are always written — either to the primary arrays
(match kept) or the secondary arrays (match expanded).

### Token iteration

The scorer walks the token list from `0x40d744` to `0x40d748`. For each token:

- **Literal** (value < 256): the code length for the literal symbol is retrieved from
  the supplied litlen table at offset `+0x28`. The cost is added to the running total.
  The frequency for that symbol is incremented in `0x40dc5c`.

- **Match** (value >= 256, specifically `0x100..0x11d` for length symbols): the match
  cost is computed as:

```text
match_cost = code_length(litlen_symbol)
           + length_extra_bits(length)
           + code_length(distance_symbol)
           + distance_extra_bits(distance)
```

The length extra bits and the mapping from decoded length to litlen symbol follow the
RFC 1951 tables (implemented inline at `0x403400..0x4034fb`). Length values are
extracted from the token structure at offset `+0x5` (decoded length ≥ 3) and mapped
to symbols 257..285 via the standard lookup plus `bsr` for values above 10.

The distance symbol is extracted from the token structure. Extra bits for the distance
follow the RFC 1951 tables.

### Expansion decision

For a match token, the scorer computes the literal cost:

```text
literal_cost = sum(code_length(decoded_byte))
```

It iterates over the decoded bytes of the match (stored inline in the token structure).
For each byte, it looks up the code length in the supplied litlen table.

The expansion is accepted only when **all three** conditions hold:

1. `literal_cost < match_cost` (strict, checked with `jae` at `0x40335b` — abort on
   equal or greater).
2. Every decoded byte has a non-zero code length in the current literal table (`test
   edi,edi; je 0x4033b7` at `0x40335d` — abort on missing code).
3. The running literal cost has not yet reached the match cost (early-abort inside the
   byte loop, `0x403358`).

If the expansion is accepted, the frequencies for the literal bytes are incremented in
the secondary litlen array (`0x40dc5c`). The match frequencies (litlen symbol and
distance symbol) are **not** incremented — they effectively disappear from the
frequency count for this pass.

If the expansion is **rejected** (any condition fails), the match is kept: the litlen
symbol frequency is incremented in `0x40dc5c` and the distance symbol frequency is
incremented in `0x40e0dc`.

### Edge case: symbol 284

At `0x4034d9..0x403507`, when a match has length symbol 0x102 (258 decimal), the
scorer checks whether the current literal/length table has at least 286 entries
(`cmp DWORD PTR [esi+0x4], 0x11d`) and whether entry 285 has a non-zero code length
(`cmp BYTE PTR [esi+0x145], 0x0`). If both hold, the match uses symbol 285 instead
of 284 for cost computation. This mirrors the parser's edge-case handling: the scorer
always prefers symbol 285 when it is available, which changes the payload cost and
potentially the winner.

### Fixed-table cost at +0x128

After the token loop, the scorer adds a fixed offset (`+0x128` from the table
descriptor, read at `0x403516`). For the fixed Huffman tables, this is the
contribution of the end-of-block symbol (7 bits in the fixed litlen table). The final
`eax` return value is the complete payload cost.

## Package-merge Huffman builder

The length-limited Huffman tree builder at `0x404050` implements a deterministic
package-merge algorithm. It is called for literal/length trees (max depth 15),
distance trees (max depth 15), and code-length trees (max depth 7). ABI:
`eax = frequencies[]`, `edx = active_count`, `ecx = output_lengths[]`,
`[esp] = max_depth`.

### Leaf sorting

At `0x4040ab..0x4041bb`, the builder constructs a counting-sort histogram of symbol
frequencies. For each symbol with non-zero frequency, the frequency is clamped to a
maximum of 0x11f (287). The histogram is prefix-summed (`0x4040e5..0x404166`) in an
unrolled loop that processes groups of 17 entries at a time up to the maximum of 288
positions.

Symbols with frequency ≤ 287 are placed into a sorted array via the counting-sort
positions. Frequency is the primary key; symbol index is the secondary key (via
iteration order).

Symbols with frequency > 287 take a fallback path at `0x404780` that uses `qsort`
with the comparator at `0x402cb0`.

### Comparator at 0x402cb0

The qsort comparator compares two 8-byte records:

```text
cmp dword [ecx], [edx]      ; compare frequency (u32)
je  tiebreak
ret                          ; return freq difference

tiebreak:
movzx eax, word [ecx+4]     ; compare symbol (u16 at offset +4)
movzx edx, word [edx+4]
sub  eax, edx
ret
```

Records are `{u32 frequency, u16 symbol, u16 padding}`. Tie-breaking by symbol
index makes the sort fully deterministic. The comparator uses signed subtraction for
the frequency comparison but unsigned ordering is implied by the counting-sort path
using the same fields. JBE at the counting-sort insertion (`0x4040c9`) treats
frequencies as unsigned.

### Package-merge expansion

The recursive function at `0x403cd0` expands the package-merge boundary. It walks a
tree of 8-byte nodes, each containing:

```text
offset +0: u32 weight (sum of child frequencies)
offset +4: u16 child_a_index
offset +6: u16 child_b_index
```

For indices below a threshold (`edx` / `[ecx+0x8]`), the node is a leaf: it
increments the counter at `output_lengths[index]`. For indices at or above the
threshold, it is an internal node: the function recurses into both children.

The recursion depth is bounded by the maximum code length (15 or 7). The function
is called for each active code length level in the package-merge boundary set.

### Active-span trimming

Before building, the caller trims the frequency array to the highest non-zero entry.
At `0x406b3d..0x406b48`, the litlen active count starts at 286 and walks backward
while `freq[count-1] == 0`. The distance active count starts at 30 and does the
same at `0x406b6d..0x406b78`. This trimming reduces the alphabet size for the
package-merge builder, which improves its runtime on sparse alphabets.

The active span must satisfy Deflate minimums (257 literal/length symbols, 1 distance
symbol). The builder itself does not enforce these; the callers are responsible.

### Depth-limit behaviour

When the ordinary tree produces codes deeper than the limit (15 or 7), the overflow
repair in the builder adjusts the tree structure. For defluff, which uses
package-merge rather than a heap, the depth constraint is built into the package
selection — the algorithm naturally produces codes within the limit by choosing the
correct number of packages at each level. No post-hoc repair is needed.

The `[esp]` depth parameter is confirmed at call sites:
- `0x406b54`: `mov [esp], 0xf` for literal/length tree
- `0x406b84`: `mov [esp], 0xf` for distance tree
- `0x405117`: `mov edx, 0x13` with a 7-bit limit for code-length tree

## Dynamic-header RLE encoding

The header generator at `0x404d00` produces a complete dynamic Deflate header from
literal/length and distance length arrays. It takes a single argument in `eax`: a
pointer to a structure containing the length arrays and their counts.

### Greedy RLE packing

The main loop at `0x404d60..0x404d7d` scans the concatenated literal/length and
distance length arrays. It identifies runs of equal values and emits:

- **Single values:** written directly as code-length symbols (0–15).
- **Zero runs:**
  - 11–138 zeros → symbol 18 with 7 extra bits (count − 11)
  - 3–10 zeros → symbol 17 with 3 extra bits (count − 3)
  - 1–2 zeros → emitted explicitly as symbol 0
- **Non-zero runs:**
  - Emit the value once explicitly, then symbol 16 with 2 extra bits (count − 3)
    for chunks of 3–6 repeats. Remaining 1–2 copies emitted explicitly.

The RLE tokens are packed into 16-bit entries: the lower 4 bits hold the run length
for repeat symbols or zero, and the upper bits hold the symbol or literal value.
This is a single-pass greedy encoder — no backtracking, no alternative splits, no
shortest-path search.

### Code-length tree construction

The RLE token frequencies are counted and passed to the package-merge builder
`0x404050` with a 19-symbol alphabet and max depth 7. The resulting code-length-code
lengths are transmitted in the Deflate permutation order:

```text
16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15
```

### Local RLE token replacement

At `0x404fe3..0x405813`, the routine performs a local pass over the packed RLE
tokens. For a repeat token (symbols 16, 17, or 18), it compares:

```text
repeat_cost   = code_length_code(repeat_symbol) + repeat_extra_bits
explicit_cost = sum(code_length_code(each represented value))
```

The repeat is replaced by explicit values only when `explicit_cost < repeat_cost`
(strict, `jae` aborts). All explicit values must have non-zero code lengths in
the current code-length tree. This is a single forward scan — no iterative
feedback loop, no swapping between symbol 17 and 16 as DeflOpt does.

### HCLEN trimming

The routine removes trailing zero entries from the transmitted code-length-code
array, saving 3 bits per removed entry. The array is in the fixed Deflate
permutation order, so the trimming walks backward from the last entry (symbol 15)
until it finds a non-zero length. The transmitted HCLEN is set to one less than
this index, floored to at least 3 (so HCLEN + 4 ≥ 4).

### Header bit writer

The final header is written to the output bit stream by `0x407520` and `0x4075cf`.
These write: HLIT (5 bits), HDIST (5 bits), HCLEN (4 bits), the code-length-code
lengths (3 bits each), and the RLE-encoded literal/length and distance length
stream with their extra bits.

## Token buffer overflow handling

The token buffer at `0x40d744` has a capacity of 0x10000 entries (131072 bytes of
u16 data). At `0x40723f..0x407279`, when the buffer fills (end pointer reaches
begin + 0x10000), a sliding-window memcpy (`realloc` at `0x402bdc`) preserves the
trailing window of tokens while discarding the prefix. The buffer is resized to
0x8000 entries. The emitted output continues from the saved window position.

This means blocks larger than 131072 tokens are processed in sliding windows of
at most 32768 tokens retained across the overflow boundary. The overflow path
updates `0x40d758` (the window size) and adjusts the token pointers. This is a
rarely-triggered edge case for extremely large deflate blocks.

## Container methods

defluff applies the same raw Deflate optimizer inside GZIP, PNG, and ZIP. Container
code also removes or rewrites metadata, which may account for savings independent
of the Deflate methods.

### Common whole-object comparison

The wrapper at `0x4019a4..0x4019ce` compares the rebuilt object with the original.
The original file size is compared against the new output size. No `/b` (bit-level),
`/f` (force), or `/k` (keep) flags exist in defluff — it always follows the
strictly-smaller policy for keeping the rewrite.

### Format detection at 0x4016c0

The entry point reads 2 bytes and dispatches:

| Magic | Format | Handler |
|---|---|---|
| `89 50` (PNG signature) | PNG | `0x401707` |
| `1f 8b` | GZIP | `0x401da0` |
| `50 4b` | ZIP | `0x401a83` |
| Other | — | error exit |

### PNG

The PNG handler at `0x401707..0x4019d0`:

- Validates the 8-byte PNG signature.
- Reads and validates the IHDR chunk (13-byte header, CRC).
- Initialises a CRC-32 table at `0x40177c..0x4017a9` (256-entry lookup table).
- Chunk walker loop at `0x4017ad..0x401871`:
  - Reads each chunk's length, type, data, and CRC.
  - **IDAT** (`0x401a1f`): buffers chunk data, runs `0x4066f0` on the concatenated
    stream **at IEND** (not per-chunk).
  - **fdAT** (`0x401876`): APNG frame data. Strips the 4-byte sequence number,
    buffers the compressed data, runs `0x4066f0` once per frame at `0x4066f0`.
    Validates sequence number ordering. Rewrites the fdAT chunk with new compressed
    data.
  - **fcTL**: tracks APNG frame sequence counters at `0x40d074`.
  - **zTXt** (`0x4017f2`): decompresses, recompresses with optimizer, rewrites.
  - **iTXt** (`0x4017f2`): same path as zTXt for the compressed variant.
  - **iCCP** (`0x4017f2`): same path for the embedded ICC profile.
  - Non-critical chunks not listed above are discarded unless `/k` (not implemented
    in defluff — all non-essential chunks are dropped by default, matching DeflOpt's
    default behaviour).
  - **IEND**: flushes output, prints "finished successfully", exits.
- Rebuilds the IDAT concatenated stream: all buffered IDAT compressed data is
  treated as one continuous zlib stream and optimized once at IEND.
- Recalculates CRC-32 for every rewritten chunk.
- Does **not** coalesce consecutive IDAT chunks into one (DeflOpt does this).
- Does handle APNG `fdAT` streams (DeflOpt 2.07 does not).

### GZIP

The GZIP handler at `0x401da0..0x401ea5`:

- Validates ID1 (0x1f), ID2 (0x8b), CM (0x08).
- Reads FLG byte, handles flags:
  - `FTEXT` (bit 0): ignored (not preserved).
  - `FHCRC` (bit 1): reads 2-byte CRC-16 of the header.
  - `FEXTRA` (bit 2): reads XLEN, skips extra subfields via `0x407626`.
  - `FNAME` (bit 3): reads null-terminated filename string.
  - `FCOMMENT` (bit 4): reads null-terminated comment string.
- By default, all optional fields are skipped (stripped from output).
- The raw Deflate stream is fed to `0x4066f0` at `0x401df7`.
- Output: writes the GZIP header (signature, CM, FLG cleared, MTIME zeroed), the
  optimized Deflate stream, CRC-32 (recalculated from decoded bytes), and ISIZE.
- Only one GZIP member is handled (no loop for concatenated members — matches
  DeflOpt 2.07 behaviour).

### ZIP

The ZIP handler at `0x401a83..0x401da0`:

- Locates the End of Central Directory Record (EOCD) by scanning for the signature
  `50 4b 05 06` from the end of the file.
- Reads the central directory offset and entry count.
- Central directory entry parser at `0x401b40..0x401c50`: extracts compression
  method, CRC-32, compressed and uncompressed sizes, filename, extra field length,
  and comment length.
- For each entry:
  - **Method 8 (Deflate):** seeks to the local header (`0x402c2c`), reads the
    compressed data, calls `0x4066f0`, rewrites the local entry with updated sizes
    and CRC. Clears general-purpose bit 3 in the local header (no data descriptor).
  - **Method 0 (Stored):** copies unchanged.
  - **Other methods:** unsupported, causes an error.
- Rewrites the central directory with updated offsets and sizes.
- Strips extra fields and comments from both local and central headers by default
  (matching DeflOpt's ZIP `/k`-off default).
- Removes data descriptors (signature `50 4b 07 08`).
- Does **not** handle ZIP64 (EOCD64 locator/record).

## Command-line interface

defluff has no command-line options. It reads from stdin and writes to stdout. Input
and output must not be the same file. The format is auto-detected from the first two
bytes.

## Methods not present in defluff 0.3.2

The following techniques appear in DeflOpt, deft4j, or later optimizers, but are not
implemented in defluff 0.3.2:

- a fresh LZ77 match search;
- block splitting or merging (no empty-block deletion, no fixed/fixed coalescing);
- multiple Huffman tree variants (defluff builds one deterministic tree per alphabet
  via package-merge; DeflOpt tries four heap variants);
- height-aware heap tie-breaking (DeflOpt `0x407b30`);
- header RLE shortest-path search or multiple packing strategies (deft4j's 56-way
  grid, OHH alternatives);
- header RLE feedback loop (re-scanning after code-length tree rebuild — defluff
  does one forward pass);
- symbol-17 to symbol-16 cross-token rewriting (present in DeflOpt `0x4072b3`);
- exact original dynamic block bits as a fallback candidate (DeflOpt `0x406bcb`);
- least-expensive / least-seen length-symbol family removal (deft4j);
- fixed-point prune/recode loop (deft4j `recodedHuffmanFull`);
- multiple candidate seeds per block (deft4j's `runOptimisationsCallbackMulti`);
- candidate order as a stateful queue (defluff evaluates three fixed candidates in
  a linear scan);
- IDAT chunk coalescing into one zlib stream (DeflOpt does this);
- GZIP multi-member streams;
- ZIP64 support;
- APNG `fdAT` per-frame optimization (defluff handles APNG fdAT);
- `/a`, `/b`, `/c`, `/d`, `/f`, `/k`, `/r`, `/s`, `/v` options;
- time-budgeted, recursive, or unbounded search; and
- stored-block multi-block split for blocks > 65535 bytes (DeflOpt rejects these;
  defluff splits them — this is an extension relative to DeflOpt, not a missing
  feature).

This exclusion list is important for both attribution and performance comparisons. A
port may sensibly add these techniques, but should label them as extensions rather than
defluff parity.

## Comparison with DeflOpt and deft4j

| Aspect | DeflOpt 2.07 | defluff 0.3.2 | deft4j β17 |
|---|---|---|---|
| Huffman builder | 4 heap variants | 1 package-merge (deterministic) | 1 PriorityQueue (Java-dependent ties) |
| Depth repair | Histogram overflow | Built into package-merge | Structural tree rewrite |
| Candidates per block | 4 lit × 4 dist × 4 clen = up to 64 | 3 (stored, fixed, dynamic) | 168–224+ |
| Match→literal | Strict, under final tree | Strict, under evaluated table (freqs feed iteration) | Strict + no-larger pruning |
| Block merging | Empty deletion, fixed/fixed | None | Stored absorption, fixed-convert Huffman merge |
| Header RLE | Greedy + local rewrite + feedback | Greedy + single local pass | 56-way grid + prune/recode |
| Iteration | One pass + stored fallback | One refinement pass if symbol 284 used | Repeated until no saving |
| Stored block size limit | 65535 bytes (rejected above) | Split into multi-block if > 65535 | 65535 bytes (rejected above) |
| ZIP support | Yes (central-directory driven) | Yes (central-directory driven) | Yes (lljzip) |
| APNG fdAT | Not in 2.07 | Yes | Yes |
| IDAT coalescing | Yes | No (separate chunks, optimize at IEND) | Yes |
| GZIP multi-member | No | No | No |
| Original bits fallback | Yes | No | No |
| Command-line flags | 9 options (`/a`–`/v`) | None (stdin→stdout) | CLI options |

## Implementation requirements and common pitfalls

An independent implementation should preserve the following details when compatibility
is the goal:

1. **Use package-merge with frequency-then-symbol ordering.** The comparator at
   `0x402cb0` uses `(freq, symbol)` as the sort key. A different tie-breaker produces
   different code lengths.
2. **Score every candidate at the actual output alignment.** Stored padding depends on
   the bit position at the start of the block.
3. **Use strict winner comparisons with the correct scan order.** stored > fixed >
   dynamic. The iterative refinement pass is the one exception (`ja` at `0x406c3b`).
4. **Gate the iterative refinement pass on the symbol-284 flag.** Without this gate,
   the iteration could cycle; with it, at most one refinement pass occurs.
5. **Split stored blocks at 65535 bytes.** Unlike DeflOpt which rejects oversized
   blocks, defluff converts them to multi-block stored output.
6. **Skip the candidate search for 25 or fewer tokens.** The bypass at `0x4071db` uses
   a hardcoded `0x19` threshold.
7. **Preserve the scorer's integrated expansion behaviour.** The scorer simultaneously
   computes cost and accumulates frequencies. The frequencies it writes determine the
   next candidate. A scorer that only returns a cost number does not reproduce defluff's
   iterative refinement.
8. **Store the parsed length arrays for header regeneration.** The original dynamic
   candidate uses these directly; the rebuilt candidate constructs new trees from
   frequencies.
9. **Use greedy RLE with a single forward replace pass.** No backtracking, no
   shortest-path search, no sym17→sym16 rewriting.
10. **Count every extra bit.** Tree and payload comparisons include length, distance,
    and RLE extra fields.
11. **Trim active symbol spans before building trees.** The last-used-litlen and
    last-used-distance trimming reduces alphabets for sparse inputs.
12. **Do not merge blocks.** defluff has no block-list manipulation — each block is
    optimized independently.
13. **Strip optional container fields by default.** GZIP FEXTRA/FNAME/FCOMMENT/FHCRC
    and ZIP extra fields/comments are removed.
14. **Validate malformed input deliberately.** The binary does not always expose
    defensive checks. Parity on valid input should not require reproducing unsafe
    failure behaviour.
15. **The Darwin and Windows binaries share the same algorithm.** Alternative builds
    that differ materially from the Windows binary should not be labelled defluff 0.3.2
    parity.

## Suggested validation strategy

A new implementation can be tested in layers:

### 1. Structural validity

- Decompress every result with at least two independent Deflate decoders.
- Compare decompressed bytes exactly.
- Validate zlib Adler-32, GZIP CRC-32/ISIZE, PNG chunk CRCs, and ZIP entry metadata.

### 2. Primitive parity

- Test package-merge code lengths for known frequency sets. Compare with the
  reference binary under Wine.
- Test with zero-, one-, two-, and many-symbol alphabets.
- Verify the frequency-then-symbol tie-breaking on equal-frequency inputs.
- Test stored block byte alignment at every output bit position 0..7.
- Test stored multi-block split at the exact 65535-byte and 65536-byte boundaries.

### 3. Scorer parity

- Test strict match-to-literal expansion: verify that `literal_cost == match_cost`
  keeps the match.
- Verify that a missing literal code aborts the expansion.
- Verify that frequencies are written to `0x40dc5c` (expansion) or `0x40d75c`
  (match kept) depending on the expansion decision.
- Test the symbol-284 → 285 preference when both are available.

### 4. Block-level parity

- Verify that blocks with ≤25 tokens are emitted as fixed regardless of source type.
- Test the stored/fixed/dynamic winner selection order on a case where two
  candidates tie.
- Test that the iterative refinement pass triggers only when symbol 284 is present.
- Test that the refinement runs at most once.

### 5. Container parity

- Test GZIP with and without FEXTRA/FNAME/FCOMMENT.
- Test PNG with APNG fdAT frames (verify sequence number update).
- Test ZIP with data descriptors (verify they are removed).
- Test PNG IDAT optimization (verify all IDAT chunks are processed, not just the
  first).

## Address index

| Address or range | Reconstructed role |
|---|---|
| `0x4014c0` | 32-bit bit reader (reads N bits, returns in eax) |
| `0x4014f0` | 16-bit bit reader |
| `0x401510` | 32-bit bit writer |
| `0x401540` | byte-align output |
| `0x401590` | read 2 bytes (big-endian) |
| `0x4015b0` | read 4 bytes (big-endian) |
| `0x4015f0` | write 4 bytes (big-endian) |
| `0x401610` | read 4 bytes (big-endian, consume) |
| `0x4013c0` | byte-align input + output |
| `0x4013f0` | reset CRC/checksum accumulator |
| `0x401440` | seek input (SetFilePointer) |
| `0x401650..0x401fff` | container handler: format detection + PNG/GZIP/ZIP dispatch |
| `0x4016c0..0x4016d4` | format detection: 0x5089=PNG, 0x8b1f=GZIP, 0x4b50=ZIP |
| `0x401707..0x4019d0` | PNG chunk walker (IHDR, IDAT, fdAT, zTXt, iTXt, iCCP, IEND) |
| `0x40177c..0x4017a9` | CRC-32 table generator |
| `0x4017da` | IDAT detection (`0x54414449`) |
| `0x4017e6` | fdAT detection (`0x54416466`) |
| `0x4017f2` | zTXt/iTXt/iCCP compressed chunk handler |
| `0x401876..0x401912` | fdAT frame handler (strip seqno, optimize, rewrite) |
| `0x401998` | IEND detection (`0x444e4549`) |
| `0x401a83..0x401da0` | ZIP handler (EOCD → central directory → per-entry optimize) |
| `0x401da0..0x401ea5` | GZIP handler (validate, skip fields, optimize, rewrite) |
| `0x402910` | initialise globals (output buffer, stdin/stdout handles) |
| `0x402b50` | alloca / stack probe |
| `0x402bb4` | fwrite wrapper (stderr for errors, stdout for "finished") |
| `0x402bbc` | qsort thunk |
| `0x402bcc` | free thunk |
| `0x402bdc` | realloc thunk → actually `memcpy` in observed code |
| `0x402be4` | calloc thunk |
| `0x402c0c` | ExitProcess thunk |
| `0x402c2c` | SetFilePointer thunk |
| `0x402c3c` | GetStdHandle thunk |
| `0x402c84` | bit-buffer reset |
| `0x402c8c` | bit-buffer flush |
| `0x402cb0` | qsort comparator: `freq asc → symbol asc` (8-byte records) |
| `0x402cd0` | output buffer flush (4 KiB → WriteFile, 14 callers) |
| `0x402fa0` | advance bit position by N (reader side, 19 callers) |
| `0x402fd0` | read 8 bits from input (54 callers) |
| `0x403100` | read N bits from input (wrapper around `0x402fd0` + `0x402fa0`) |
| `0x403170` | write N bits to output buffer |
| `0x403250` | scorer: token cost + match→literal expansion + freq counting |
| `0x403530` | write N bits (16-bit wrapper, inserts into sliding buffer) |
| `0x4035b0` | emit fixed/dynamic Huffman block tokens → output bits |
| `0x403cd0` | package-merge recursive tree expansion (8 calls, self-recursive) |
| `0x404050` | package-merge length-limited Huffman builder (called with max_depth 15 or 7) |
| `0x404d00` | dynamic header generator (greedy RLE + code-length tree + local replacements + HCLEN trim) |
| `0x405900` | emit N bits to output bit stream (20 callers, wrapper around bit writer) |
| `0x405eb0` | parse dynamic block header (code lengths → tables) |
| `0x406170` | decode token stream (dynamic/fixed) |
| `0x4066f0` | per-block optimizer: parse → candidate search → winner selection → emission |
| `0x4066ce..0x4066dc` | symbol-284 edge case: decrement counter, set iteration flag |
| `0x406a85` | token-count gate (≤25 → skip search) |
| `0x406c61..0x406c7d` | winner selection: min(stored, fixed, dynamic), ties favour earlier |
| `0x4070d4..0x407279` | stored multi-block split (up to 65535 bytes per stored block) |
| `0x4071db` | fast path: ≤25 tokens → emit fixed block directly |
| `0x40723f..0x407279` | token buffer overflow realloc (sliding window) |
| `0x40727e..0x4073b0` | iterative refinement loop (gated on symbol-284 flag) |
| `0x4073c6` | no-match shortcut (stored block for all-literal data) |
| `0x4074ff` | fixed-block token decode |
| `0x407520` | dynamic header bit writer |
| `0x4075cf` | dynamic header entry point (writes HLIT, HDIST, HCLEN, tree, RLE) |
| `0x407626` | skip GZIP FEXTRA subfields |

## Global data map

| Address | Size | Description |
|---|---|---|
| `0x40c040` | 4 | callout handler address |
| `0x40c060` | 0x1000 | output byte buffer (4 KiB sliding window) |
| `0x40d060` | 4 | CRC-32 accumulator / checksum state |
| `0x40d06c` | 4 | output buffer position (bytes written to buffer) |
| `0x40d074` | 4 | APNG frame sequence counter |
| `0x40d080` | 0x400 | CRC-32 lookup table (256 × 4 bytes) |
| `0x40d4a0` | 0x28 | literal/length fixed-table descriptor ({count, lengths_ptr, max_code_len, ...}) |
| `0x40d4c8` | 0x120 | literal/length fixed code lengths (288 bytes, packed) |
| `0x40d5e0..0x40d5e7` | 8 | distance fixed code lengths (first 8 entries, rest are 5) |
| `0x40d5e8` | 0x28 | distance fixed-table descriptor |
| `0x40d610` | 0x20 | distance fixed code lengths (32 bytes) |
| `0x40d740` | 4 | current winner lengths pointer (0x40e180 or 0x40e698) |
| `0x40d744` | 4 | token buffer begin pointer |
| `0x40d748` | 4 | token buffer end pointer |
| `0x40d74c` | 4 | token buffer capacity (initial 0x10000) |
| `0x40d754` | 4 | decoded token count |
| `0x40d758` | 4 | current window size for token buffer |
| `0x40d75c` | 0x480 | primary literal/length frequencies (288 × 4 bytes) |
| `0x40db5c` | 4 | stored-block parsed flag (1 = stored) |
| `0x40db60` | 4 | match-present flag (0 = no matches in block) |
| `0x40dbcc` | 4 | symbol-284 counter (decremented per occurrence) |
| `0x40dbd0` | 4 | iteration flag (1 if any symbol-284 encountered) |
| `0x40dbdc` | 0x78 | primary distance frequencies (30 × 4 bytes) |
| `0x40dc50..0x40dc5c` | 12 | bit-position and alignment state |
| `0x40dc5c` | 0x480 | scorer literal/length frequencies (288 × 4 bytes) |
| `0x40e05c` | 4 | end-of-block symbol cost accumulator |
| `0x40e0dc` | 0x78 | scorer distance frequencies (30 × 4 bytes) |
| `0x40e15c` | 4 | input bit position |
| `0x40e168` | 4 | output byte counter (total bytes written) |
| `0x40e16c` | 4 | output bit accumulator |
| `0x40e170` | 4 | output bit position (0–7) |
| `0x40e180` | 0x148 | output-length buffer A (parsed lengths: litlen 286 + dist 30) |
| `0x40e698` | 0x148 | output-length buffer B (rebuilt lengths) |
| `0x40f140` | 4 | `_iob` (stdio FILE* array pointer) |

## Confidence and remaining limits

The per-block candidate graph, the scorer's match-expansion logic, the package-merge
builder's comparator, the RLE header encoder, and the format dispatch were traced
instruction by instruction through the disassembly and verified with behavioural tests
under Wine. The method map covers every basic block reachable from the container entry
point.

The strongest remaining caveat is the internal behaviour of `qsort` from msvcrt.dll:
the comparator is deterministic, but the sorting algorithm's exact element movement for
equal-key elements is implementation-defined. Since the tie-breaker includes symbol
index and the counting-sort path avoids qsort for common cases (frequencies ≤ 287),
this is unlikely to affect output except on pathological frequency distributions.

The Darwin x86 binary was not disassembled; the import table shows the same library
functions (`qsort`, no `time`/`rand`), but differences in structure layout, stack
probing (`alloca` vs `__chkstk`), or libc behaviour could cause divergence. A
bit-for-bit comparison between the Windows and Darwin binaries on identical input has
not been performed.

This document describes the behaviour of the stripped PE32 binary. A port that
differs materially from the described methods should not claim defluff 0.3.2 parity.

## Attribution

defluff is Copyright (c) 2010-2011 Joachim Henke. The binary was distributed on the
encode.su forum. This document is an independent reverse-engineering description
intended for interoperability, research, and comparative implementation work.
