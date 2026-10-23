# sevenz-rust2 0.20.2, patched for Ferrum (FER-588)

This directory is a copy of [`sevenz-rust2`](https://github.com/hasenbanck/sevenz-rust2) **0.20.2**
as published on crates.io (`.cargo_vcs_info.json`: upstream commit
`424ebdb8fa98b78b8e1c18f73c9add6972fe5496`), licensed under Apache-2.0 (see `LICENSE`, unchanged).
`src-tauri/Cargo.toml` swaps it in with:

```toml
[patch.crates-io]
sevenz-rust2 = { path = "vendor/sevenz-rust2" }
```

Ferrum still declares `sevenz-rust2 = "=0.20.2"`; the patch only replaces where the source comes
from. The public API is unchanged.

## Why

Upstream sizes vectors straight from counts in the 7z header (`vec![..; num_files]`,
`vec![0; num_pack_streams]`, `reserve_exact(num_blocks)`, `resize(total_unpack_streams)`, ...).
A 42-byte archive whose plain `kHeader` declares 2^36 files makes `ArchiveReader::new` request
6.5 TB; the allocation fails and Rust **aborts the process** (`memory allocation of ... failed`).
An abort cannot be caught, so opening or previewing such a file closed Ferrum. The same happens
for an encoded header after it is decoded. Ferrum's own pre-check
(`src-tauri/src/archive_sevenz_header.rs`, FER-587) cannot see inside a plain or decoded `kHeader`,
so the fix belongs in the parser.

## What changed

Each modified file starts with a "Modified by the Ferrum project (FER-588)" line, and every changed
spot carries a `// FERRUM PATCH (FER-588):` comment. The structural checks below are the ones 7-Zip
itself applies, so archives 7-Zip accepts are not affected. The one exception is the absolute cap
on items: 7-Zip allows up to 2^31 - 1 files, this copy stops at 1,048,576 (about 100 MiB of entry
metadata). A count above the cap that the header can really describe fails with a message starting
`Too many entries`, which Ferrum reports as `LimitExceeded` ("too large"), not as a corrupt
archive; a count the header bytes cannot describe is a format error.

`src/reader.rs`

- New private helpers: `HeaderSource` (header parsing always reads from an in-memory buffer, so the
  unread byte count is known; the header parsers now take `R: HeaderSource` instead of `R: Read`),
  `bounded_count` (count <= unread header items, else a format error; <= absolute cap, else a
  `Too many entries` error) and
  `try_vec` (`vec![v; n]` via `try_reserve_exact`, failure becomes `Error::Other`).
- Caps: `MAX_HEADER_ITEMS = 1 << 20` (files, pack streams, blocks, sub-streams, digests),
  `MAX_CODERS_PER_BLOCK = 64`, `MAX_STREAMS_PER_BLOCK = 64` (per coder and per block, in and out),
  `MAX_HEADER_SIZE = 256 MiB` (raw and decoded header), `MAX_PREALLOC = 1 MiB` (capacity hint).
- `init_archive`: next header size capped; the header is read with `take().read_to_end()` so only
  bytes that exist are buffered (was `vec![0; next_header_size]` before reading); offset addition
  checked; the decoded-header buffer is reserved fallibly.
- `read_encoded_header`: decoded header size capped before any decoder is built.
- `read_header`: blocks without `kFilesInfo` are an error (the stream map stayed empty and
  decoding indexed into it; 7-Zip rejects streams without files as well).
- `read_files_info`: `num_files` bounded (unread header bits + sub-stream count); `kName` with size
  0 (underflow) and more names than files (index panic) are errors; more files with data than
  sub-streams (index panic) and a file/sub-stream count mismatch (later index panics; 7-Zip
  rejects it too) are errors.
- `calculate_stream_map`: blocks using more pack streams than declared, a missing pack stream and
  pack offset overflow are errors (were index panics / debug overflow panics during decoding).
- `read_pack_info`: `num_pack_streams` bounded; one size byte per stream must remain before the
  sizes vector is allocated; sizes and CRC vectors allocated fallibly.
- `read_unpack_info`: `num_blocks` bounded (one byte per block) and reserved fallibly.
- `read_sub_streams_info`: running total of `numStreams` bounded (no overflow); sizes, CRC and
  digest vectors allocated fallibly; the size sum is checked (overflow wrapped past the
  "sum exceeds total" check).
- `read_block`: `num_coders` bounded; coder in/out stream counts and their totals capped at 64
  (they size the bind pair and packed stream vectors); property size bounded by the unread header
  and allocated fallibly; a block without packed streams is an error (decoding indexed into an
  empty list; 7-Zip rejects it).
- `check_coder_graph` (new, called at the end of `read_block`), the checks of 7-Zip's
  `CBondsChecks`: every coder has exactly one out stream; bind pair indices are in range and not
  duplicated; packed streams are in range, unbound and not duplicated; exactly one main coder;
  walking from it reaches every coder exactly once. Without it a bind pair cycle in a BCJ2 block
  recursed in `get_in_stream` / `get_in_stream2` until the **stack overflowed** (process killed),
  and out-of-range bind pairs were index panics.
- `build_decode_stack2`, `get_in_stream`, `get_in_stream2` (decoding, defence in depth): a `depth`
  argument counts bind pairs followed (one per `get_in_stream` -> `get_in_stream2` step) and stops
  the recursion beyond `coders.len()`; coder, stream map and unpack
  size lookups use `get`; the BCJ2 main coder must have 4 in and 1 out streams before
  `Bcj2Reader::new` (it indexed four inputs and panicked).
- `pack_pos` additions (`read_encoded_header`, `build_decode_stack`, `build_decode_stack2`,
  `read_file`) are checked (debug overflow panics).
- `read_bits`: the bit vector must fit in the unread header before `BitSet::with_capacity`.
- `read_file`: the capacity hint taken from a header size is capped at `MAX_PREALLOC`; the vector
  still grows to the real size while reading.

`src/block.rs`

- `OrderedCoderIter` stops after `coders.len()` steps: a bind pair cycle in a hostile header looped
  forever, stacking decoders (also excluded up front by `check_coder_graph`).

`Cargo.toml`

- The `[[example]]` and `[[test]]` targets and the dev-dependencies were removed together with the
  `examples/` and `tests/` directories (test data not needed by Ferrum). Library targets, features
  and dependencies are unchanged.

## Not changed (known)

- Codec memory (LZMA/LZMA2 dictionaries, PPMd model) is still sized from coder properties. Ferrum
  bounds it before every open and decode (`archive_sevenz_header.rs` for encoded headers,
  `archive_contract::check_codec_memory` for content, FER-579/FER-587).
- AES `NumCyclesPower` is still trusted by `encryption/aes.rs`; Ferrum rejects values above 24
  before the crate runs (FER-587).
- Decode-time CPU (huge declared sizes in content) is bounded by Ferrum's read guard and cancel,
  not here.

## Tests

`src-tauri/tests/archive_sevenz_counts.rs`: byte-built hostile archives (valid CRCs) for every count
above and the BCJ2 coder-graph cases (cycle, out-of-range bind pair, BCJ2 with two inputs), through
list, read, extract and test, plus a deterministic CRC-fixed mutation fuzz (seeds include BCJ2
blocks) that runs Ferrum's pre-check, then this crate, and decodes every input it opens, under a
counting allocator (no panic, no allocation above the ceiling). `FER588_FUZZ_ROUNDS` and
`FER588_FUZZ_SEED` change the run locally.

## Upstream plan

Offer the same change upstream as one PR against `hasenbanck/sevenz-rust2` (bounded counts,
fallible allocation, the coder graph check, the panic fixes and the coder-iterator bound, with the 42-byte reproducer as
a regression test). When a release contains it, drop this directory and the `[patch.crates-io]`
entry and pin that version instead. Until then, re-apply these patches on every version bump.
