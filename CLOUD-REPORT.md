# CLOUD-REPORT: coverage-guided fuzzing of `mc-unpack`

## Result in one line

Ten fuzz targets, about 30 minutes each, **no panic, arithmetic overflow, out-of-memory, excessive
allocation or hang was found**. No library code was changed, so no regression test was added
(the rule says every bug fixed gets a test; there was no bug to fix).

## What was added (only under `fuzz/`, plus this file)

- `fuzz/`: a cargo-fuzz crate with its own empty `[workspace]`, so it is not part of the root
  package. The stable build, `cargo test` and CI never see it; `cargo package --list` does not list it
  (Cargo leaves out nested packages). It builds only with nightly. The fuzz profile keeps
  `overflow-checks` and `debug-assertions` on.
- Targets (one per public entry point that takes untrusted bytes):

  | Target | What it feeds |
  |---|---|
  | `nbt_parse` | `world::nbt::parse`, `Tag::render` |
  | `decompress` | `world::compress::decompress`: first byte picks gzip / zlib / none / LZ4, 1 MiB limit |
  | `region` | `Region::open` on a file with the input as content, every `read_chunk` (the same bytes also as external `c.0.0.mcc`), `nbt::parse`, `parse_chunk` |
  | `chunk` | `nbt::parse` then `parse_chunk` (with and without an expected place) then `grid::to_blocks` through a synthetic one-block jar; a second mode (first byte 0xFF) drives `chunk::unpack` with arbitrary bits/count/longs |
  | `level` | `parse_level` and `read_level_dat` (file read, gzip or plain) |
  | `jar` | `Jar::from_bytes`, then `read_limited`, `read_json`, `read_png`, `names_under`, `version::detect` on entries |
  | `png` | `decode_png`, `McMeta::parse`, `Texture::new`, `default_frame`, `shown_frames`, `all_frames` |
  | `json` | first byte selects: `BlockState::parse` + `select`, `RawModel::parse`, `Biome::parse`, `item::default_models`, `McMeta::parse`, `State::parse`, `normalize_location` + `AssetId::parse`, `parse_rgb`, `version::id_from_path` |
  | `assets` | a synthetic jar whose blockstate, two models, texture and `.mcmeta` come from the input, then `Assets::default_state`, `bake_block`, `bake_id`, `texture`, `classify_block` |
  | `world` | a save folder with a valid `level.dat` and the input as `r.0.0.mca`, through `World::open`, `regions` and `mesher::build` (loader, chunk reader, culling, mesher) |

- Seeds: `fuzz/corpus/<target>/`, 41 small synthetic inputs (147 KB in total), written by
  `cargo +nightly run --release --bin gen_seeds` (in `fuzz/`). They are built from scratch there
  (own NBT writer, own region writer, own LZ4-block-stream writer; the layouts follow what
  `src/world/compress.rs` and `region.rs` read). Files are named by content hash with no
  extension because `tools/check_public_tree.py` refuses `level.dat`, `*.mca`, `*.jar` by name.
  No crash inputs were added to the corpus because there were none.
- `fuzz/README.md`: how to run.

## Fuzzing time per target

All runs: libFuzzer in fork mode (`-fork=1 -ignore_crashes=1`, so a crash would not stop a run),
`-malloc_limit_mb=1024 -rss_limit_mb=3072 -timeout=20`, `-max_len` 32768 for `region` and `world`,
8192 for `jar` and `assets`, 4096 for the rest, started from the seeds above. Four cores, four
targets at a time.

| Target | Time |
|---|---|
| `world` | 1846 s |
| `region` | 1846 s |
| `assets` | 1849 s |
| `jar` | 1843 s |
| `chunk` | 1847 s (+ 600 s with `-malloc_limit_mb=96`, see below) |
| `png` | 1842 s |
| `json` | 1849 s |
| `nbt_parse` | 1846 s (+ 600 s with `-malloc_limit_mb=96`) |
| `level` | 1841 s |
| `decompress` | 1839 s |

The extra 600 s on `nbt_parse` and `chunk` used a 96 MB malloc limit with inputs of at most 4 KB, to look
for large allocations from tiny inputs; it found none. Short smoke runs (12 s and 40 s) before the real runs are not counted.
The corpora the runs grew (about 8,500 files, tens of MB) are not committed: they are reproducible
noise, and only the seeds belong in the repository.

## Bugs

None found. I did not invent any. Reading the code for the paths below agrees with the fuzzing:
lengths are checked against the bytes left before allocating (`nbt.rs`), the region reader checks
sector, length and file bounds before reading, decompressors read through a limit, `read_limited`
does not trust the zip header, `parse_chunk` bounds `Y`, palette sizes and index values.
The repository already had mutation tests in `src/world/fuzz.rs`; the coverage-guided runs found nothing beyond them.

## Left out, and why

- **Not fuzzed**: `install` (looks at folders on the machine, no untrusted bytes), `gltf` / `check` /
  `main` (write output, take parsed data), `bridge` and `world::scene` (need the optional
  `rewrite-bridge` dependency; fed by the same parsed data). The `bridge` feature is off in the fuzz crate.
- **Not exercised by structure**: a real 1.13 to 1.15 spanning-packed section appears only as what the
  mutator reaches from the seeds (the `chunk` raw mode does reach `unpack` for both packings).
  Pre-1.13 numeric-id chunks are refused by the reader before decoding, and so are not decoded.
- **No semantic oracle**: the targets look for crashes, overflow, hangs and big allocations. They do
  not check that the output is *right*; for that there is no reference in the repository, and I
  did not invent one.

## Uncertain / for the owner

1. **Branch name.** The task says `cloud/fuzz-mc-unpack`; the session's git setup pinned me to
   `claude/fuzz-mc-unpack-fny3xn` (already present on origin, and I was told not to push elsewhere).
   The work is on that branch. If you want the other name, it is one `git push origin <sha>:refs/heads/cloud/fuzz-mc-unpack`.
2. **CI lint on this toolchain.** `cargo clippy --all-targets -- -D warnings` fails *on `main` too*
   here (not caused by this branch): `src/lib.rs` allows `clippy::chunks_exact_to_as_chunks`, which
   the local stable (1.97) clippy reports as an unknown lint. `cargo fmt --check` and
   `cargo test` (default and `--no-default-features`) pass; I did not touch `src/`. CI's `stable`
   may differ from what I had.
3. **Allocation that is bounded but not input-proportional.** `texture::decode_png` allocates
   from the PNG header's width and height up to `MAX_PIXELS` (2048 x 2048 x 4 = 16 MiB) before it has
   read the image data, so a tiny PNG can ask for 16 MiB. A NBT list can use up to `MAX_TAGS` (2,000,000)
   tags at `size_of::<Tag>()` each, about 32 bytes per input byte at most. Both are fixed, documented
   constants and not unbounded; I left them alone since the fuzzer did not report them and a change
   would be a design decision.
4. The `png` target hand-splits its input (2-byte length, PNG, `.mcmeta`), and `assets` uses five
   length-prefixed parts; inputs that do not fit are cut short, not rejected.
5. Not run: `cargo +nightly fuzz coverage`; no coverage percentages are claimed.
