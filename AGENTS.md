# Agent guide

mc-unpack reads Minecraft: Java Edition models, textures and world saves for asset-tool and game-rewrite developers.
It reads the user's own client jar and saves. The readers, glTF export and `check` are always built; the default
`bridge` feature adds the rewrite-bridge provider and the `check-bridge` and `check-conformance` commands.

## Toolchain, build and CI

- Rust 1.88 minimum (`Cargo.toml` and CI's MSRV job); normal CI uses stable Rust.
- CI checks Linux, macOS and Windows, with and without the default `bridge` feature.
- Exact normal CI commands:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
```

The Rust 1.88 CI job additionally runs:

```sh
cargo check --locked
cargo check --locked --no-default-features
```

## Running it

- `cargo build --release` builds `target/release/mc-unpack`; `cargo run --release -- <command>` runs it from the
  repository; `cargo install --path .` puts `mc-unpack` in Cargo's bin folder (`~/.cargo/bin`), which is on `PATH`
  after a normal Rust install.
- Without `--jar <path>` the CLI looks, read only, in the launcher folders of the machine it runs on (`versions`,
  `worlds` and every command that needs a jar). Ask the owner of the machine first; `--jar` reads that jar only.
- Environment: `MC_UNPACK_JAR` adds one jar wherever it is, and it is the jar read when no `--jar` or `--mc-version`
  names another (`world` reads a save with the jar of the save's version); `MC_UNPACK_SAVES` adds folders of saves; `MC_UNPACK_PRISM_DIR` and `MC_UNPACK_MINECRAFT_DIR` replace
  those two launchers' folders and `MC_UNPACK_LAUNCHER_DIR` adds launcher folders (lists are separated like `PATH`).

## Code map

- `src/lib.rs`: the modules, bottom to top; `src/main.rs`: the CLI.
- `src/jar.rs`, `install.rs`, `version.rs`: archive access, discovery and version rules.
- `src/assets.rs`: one jar with its caches; names to blockstates, models, textures and baked quads. Most work starts here.
- `src/model.rs`, `blockstate.rs`, `bake.rs`: the JSON files, model inheritance, elements to quads.
- `src/item.rs`: the geometry generated for flat items, and the `items/` definitions of 1.21.4 and later.
- `src/texture.rs`, `tint.rs`: PNG and animation sheets, colormaps and biome colours.
- `src/mesh.rs`: baked quads grouped by material, for both exporters; `gltf.rs`: glTF export.
- `src/check.rs`, `text.rs`: the sweep behind `check`, and the lines `list --states` and `inspect` print.
- `src/bridge.rs`: the rewrite-bridge provider (feature `bridge`).
- `src/world/nbt.rs`, `compress.rs`, `region.rs`, `level.rs`, `chunk.rs`: the save's file formats, bounded.
- `src/world/save.rs`: a save folder, its dimensions and regions, where saves are found.
- `src/world/blocks.rs`, `biomes.rs`, `grid.rs`: block states baked once each, biome colours, the loader.
- `src/world/mesher.rs`: a window of chunks to meshes and the report; `scene.rs`: the bridge scene (feature `bridge`).
- `src/world/testworld.rs`, `fuzz.rs`: synthetic saves for the tests, and the mutation tests.
- `tests/`: the opt-in tests on real installs and saves; `render/`: optional visual checks (Node, not CI).
- `tools/check_public_tree.py`: the public-tree gate (below).

## Project rules

- Follow the README's rule: nothing invented, 1:1 with the jar's data; leave code-only behavior out.
- Read individual jar entries without extracting the jar; enforce actual inflated-byte limits.
- Keep JSON entries within 128 KiB, PNG entries within 8 MiB and decoded textures within the documented caps.
- World defaults cap block data at 512 MiB and scene quads at 3,000,000; refuse over-limit windows explicitly.
- The limits are constants beside the code they guard (`src/jar.rs`, `texture.rs`, `item.rs`, `assets.rs`, and in
  `src/world/`: `nbt.rs`, `region.rs`, `chunk.rs`, `blocks.rs`, `biomes.rs`, `mesher.rs`); the README's two limits
  tables list them and must stay true. A new limit needs a measured number or a rule of the game behind it.
- `cargo test` reads no install and no save: the tests build synthetic jars and saves, in the system temp folder
  (the one place tests write outside the repository and the target folder).
- The tests on real data (`tests/game_data.rs`, `tests/world_data.rs`) are opt-in: `MC_UNPACK_REAL_DATA=1 cargo test`
  reads, never writes, the jars and saves in the user's launcher folders, and the fuzz test writes damaged copies of
  real chunks to the temp folder. Without the variable they skip, saying so, before anything is looked for. Ask the
  owner of the machine before setting it; reach installs and saves in tests through `tests/common` only.
- `MC_UNPACK_FUZZ=<rounds>` sets the rounds of the mutation tests (400 per target by default);
  `cargo test --release -- --ignored stress --nocapture` runs the one ignored test, a full synthetic region.
- No Mojang code/assets belong here; retain the README's unofficial-product disclaimer.

## Public files

- Never commit game files, extracted assets, renders, decompiled code or secrets.
- The public-tree gate needs Python 3.9+ and Git; run it from the repository root:
  `python3 tools/check_public_tree.py` (checks Git's index, including staged new files).
- CI checks committed changes with `python3 tools/check_public_tree.py --range BASE..HEAD`.

## Contributing, license and AI

- Open an issue to discuss a problem or send a pull request with the change.
- Before a pull request, run the applicable CI commands above and the public-tree gate.
- License: MIT OR Apache-2.0, at your option; see `LICENSE-MIT` and `LICENSE-APACHE`.

The code was written with AI coding agents (Anthropic's Claude and OpenAI's GPT models) under the maintainer's direction.
AI-assisted contributions are welcome when their use is disclosed and the changes are reviewed.
