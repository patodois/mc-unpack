# Fuzzing mc-unpack

Coverage-guided fuzz targets (cargo-fuzz / libFuzzer) for every public entry point that takes
untrusted bytes. This crate needs a **nightly** toolchain and is not part of the root package: the
stable build, the tests and CI never touch it.

```
cargo install cargo-fuzz
cd fuzz
cargo +nightly run --release --bin gen_seeds     # rewrites corpus/ (synthetic seeds, no game data)
cargo +nightly fuzz run region -- -max_len=32768 -malloc_limit_mb=1024 -timeout=20
```

| Target       | Entry points                                                                         |
|--------------|--------------------------------------------------------------------------------------|
| `nbt_parse`  | `world::nbt::parse`, `Tag::render`                                                   |
| `decompress` | `world::compress::decompress` (first byte: gzip, zlib, none, LZ4)                    |
| `region`     | `Region::open`, `read_chunk` (also as external `c.0.0.mcc`), `nbt::parse`, `parse_chunk` |
| `chunk`      | `parse_chunk`, `grid::to_blocks`, `chunk::unpack`                                    |
| `level`      | `parse_level`, `read_level_dat`                                                      |
| `jar`        | `Jar::from_bytes`, `read_limited`, `read_json`, `read_png`, `version::detect`        |
| `png`        | `decode_png`, `McMeta::parse`, `Texture::new` and its frames                         |
| `json`       | `BlockState`, `RawModel`, `Biome`, item definitions, `State`, locations, colours     |
| `assets`     | a synthetic jar built from the input, through `Assets` baking                        |
| `world`      | a save folder with the input as its region file, through `mesher::build`             |

The seeds are named by content hash with no extension, because `tools/check_public_tree.py`
refuses names such as `level.dat` and `*.mca` and `*.jar`. A crash input found by a run goes in
`corpus/<target>/` too, next to a regular test that fails without the fix.
