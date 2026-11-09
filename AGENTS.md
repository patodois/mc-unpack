# Agent guide

mc-unpack reads Minecraft: Java Edition models, textures and world saves for asset-tool and game-rewrite developers.
Its default `bridge` feature supplies a rewrite-bridge provider and glTF export; it reads the user's own client jar and saves.

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

The README builds the executable with `cargo build --release`.

## Code map

- `src/jar.rs`, `install.rs`, `version.rs`: archive access, discovery and version rules.
- `src/model.rs`, `blockstate.rs`, `bake.rs`, `item.rs`: model inheritance and baking.
- `src/texture.rs`, `tint.rs`, `mesh.rs`: textures, colours and generated geometry.
- `src/world/`: saves, regions, chunks, NBT and world-scene construction.
- `src/bridge.rs`, `gltf.rs`, `main.rs`, `check.rs`: provider, exports and CLI checks.
- `tests/`, `render/`: synthetic/install checks and optional visual checks.

## Project rules

- Follow the README's rule: nothing invented, 1:1 with the jar's data; leave code-only behavior out.
- Read individual jar entries without extracting the jar; enforce actual inflated-byte limits.
- Keep JSON entries within 128 KiB, PNG entries within 8 MiB and decoded textures within the documented caps.
- World defaults cap block data at 512 MiB and scene quads at 3,000,000; refuse over-limit windows explicitly.
- Tests use synthetic data or the user's own install (`MC_UNPACK_JAR`); install-dependent tests skip if absent.
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
