# mc-unpack

**NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.**

[![rewrite-bridge on crates.io](https://img.shields.io/crates/v/rewrite-bridge.svg?label=rewrite-bridge)](https://crates.io/crates/rewrite-bridge)

A small Rust reader, **for Minecraft: Java Edition**, of the block models, item models and textures inside the client jar
of a copy you already own:

- `assets/minecraft/blockstates/*.json` and `models/**.json`: parent inheritance, `#texture` variables, elements (box
  corners, rotation with `rescale`, per-face UV, face rotation, `cullface`, `tintindex`), variants and multipart, and a
  variant's `x`/`y` rotation with `uvlock`;
- textures: PNG, and the `.mcmeta` animation (every frame of the sheet is exposed; the default is frame 0);
- generated items (`builtin/generated`): the flat slab the game builds from a texture's alpha;
- item definitions (`assets/minecraft/items/*.json`, 1.21.4 and later) and the tints they carry;
- the biome colormaps and the lookup the game does in them;
- a [rewrite-bridge](https://github.com/patodois/rewrite-bridge) `Provider`, so other game rewrites can load Minecraft
  blocks and items next to their own assets, and a glTF 2.0 exporter.

It exists because the Rust crate for this, `minecraft-assets`, has not been touched since 2022 and stops at 1.18-era
formats.

This project contains no Mojang code and no Mojang assets. It is not affiliated with, endorsed by or sponsored by Mojang
Studios or Microsoft. "Minecraft" is their trademark; it is used here only to say which game the files belong to. It only
reads files from a client jar you already have. See [Legal](#legal).

The rule of this crate is **nothing invented, 1:1 with the data in the jar**: whatever lives in the game's code, not in
its data, is left out and listed under [What is left out](#what-is-left-out).

## What you need

- **A Minecraft: Java Edition client jar**, from a launcher you installed it with. Looked for, read only, in:

  | Where | Path |
  |-------|------|
  | Prism Launcher, macOS | `~/Library/Application Support/PrismLauncher/libraries/com/mojang/minecraft/<v>/minecraft-<v>-client.jar` |
  | Prism Launcher, Linux | `$XDG_DATA_HOME/PrismLauncher/...` (else `~/.local/share/PrismLauncher/...`), same layout |
  | Prism Launcher, Windows | `%APPDATA%\PrismLauncher\libraries\com\mojang\minecraft\<v>\minecraft-<v>-client.jar` |
  | Vanilla launcher, macOS | `~/Library/Application Support/minecraft/versions/<v>/<v>.jar` |
  | Vanilla launcher, Linux | `~/.minecraft/versions/<v>/<v>.jar` |
  | Vanilla launcher, Windows | `%APPDATA%\.minecraft\versions\<v>\<v>.jar` |
  | Anywhere | `--jar <path>` or `MC_UNPACK_JAR=<path>` |

  `MC_UNPACK_PRISM_DIR` and `MC_UNPACK_MINECRAFT_DIR` replace the two launcher folders. Only the macOS Prism layout has been
  tried by the author; the other paths are the launchers' documented defaults. A modded profile's jar (Fabric, Forge) holds no
  assets and is refused with that message.
- **Rust**, stable (edition 2021; written with 1.99). A release build takes about 10 seconds with `-j 4`, plus the download of
  the dependencies.
- **Disk and memory.** The jar is never extracted: the zip directory (5,000 to 15,000 entries, most of them classes) is read
  once and single entries are inflated on demand. `check` over eight jars peaks at about 90 MB.

## Words used here

- **blockstate**: `blockstates/<name>.json`, which model (and rotation) a block uses in a given state. **State**: a
  `property=value,property=value` string such as `facing=east,half=bottom`.
- **variants / multipart**: the two forms of a blockstate. Variants pick one model by state; multipart adds a model for each
  part whose `when` holds (fence arms, redstone wire).
- **element, face, UV**: a model is boxes; each face names a texture and the rectangle of it (`uv`, in 0 to 16) it shows.
- **uvlock**: the texture stays aligned to the world while a variant turns the model.
- **tintindex**: the face is multiplied by a colour the game picks at run time (grass, leaves, redstone dust).
- **block entity**: a block whose shape the game draws in code (chests, signs, banners, beds, skulls).
- **glTF**: the open 3D file format the exporter writes; any viewer or Blender opens it.
- **[rewrite-bridge](https://github.com/patodois/rewrite-bridge)** and **`Provider`**: see the
  [writing guide](https://github.com/patodois/rewrite-bridge/blob/main/docs/WRITING_A_PROVIDER.md). Other providers:
  [hytale-unpack](https://github.com/patodois/hytale-unpack), [nocturne-unpack](https://github.com/patodois/nocturne-unpack),
  [wf-unpack](https://github.com/patodois/wf-unpack), [wt-unpack](https://github.com/patodois/wt-unpack).
- **[bridge-testbed](https://github.com/patodois/bridge-testbed)**: a Bevy app that loads the assets of every provider side
  by side to check scale and orientation by eye.
- **[minecraft-link](https://github.com/patodois/minecraft-link)**: the mod that lets a running Minecraft talk to the same
  bridge's passthrough protocol; this crate uses its axis mapping.

## Versions

| Version | What the jar has | Support |
|---------|------------------|---------|
| 1.2.5 | art on atlas sheets (`terrain.png`), models in code | **unsupported**: no JSON models and no loose textures to read |
| 1.7.10 | loose PNGs in `textures/blocks` and `textures/items`, no JSON models (they arrived in 1.8) | **textures only** (941 PNGs, 13 `.mcmeta`, both colormaps) |
| 1.12.2 | JSON models, `textures/blocks`, blockstate `model` relative to `models/block/` | full |
| 1.19.2, 1.20.1, 1.21.1 | JSON models, `textures/block`, `models/item/<name>.json` | full |
| 26.2, 26.3 | as above plus `items/<name>.json` definitions, `force_translucent`, Euler element rotation, dry-foliage colormap | full |

The layout is decided from what the jar contains, so 1.8 to 1.12 (no `version.json`) and everything between the versions above
work by the same rules. The version id is read from `version.json` when there is one, else from the path. 1.8 to 1.12.1,
1.13 to 1.21.3 and 1.21.4 to 25.x have not been tried (no jar here); the formats are the same families as the ones that were.

## Build and use

```sh
cargo build --release                       # the `bridge` feature (Provider) is on by default
cargo build --release --no-default-features # readers, glTF export, `check`; no rewrite-bridge

mc-unpack versions                          # the jars found, and what is supported for each
mc-unpack list stairs                       # ids of blocks and items that draw something
mc-unpack list oak_stairs --blocks --states # and the states of each block
mc-unpack list --left-out                   # what the data has no geometry for, and why
mc-unpack model block/grass_block -o out/ --tint 0=grass@plains
mc-unpack model "block/oak_stairs?variant=facing=east,half=bottom,shape=straight" -o out/
mc-unpack model item/diamond_sword -o out/  # a flat item, as a one-pixel slab
mc-unpack texture block/water_still --frames -o out/   # every frame of an animated texture
mc-unpack tint grass --biome plains         # the colormap lookup: #91bd59
mc-unpack tint foliage --temperature 0.8 --downfall 0.4
mc-unpack inspect block/oak_stairs          # which state was used, what was ignored, every note, what was left out
mc-unpack check                             # parse and bake everything in every jar found
mc-unpack check-bridge                      # load everything through the Provider
mc-unpack check-conformance --filter stairs # the bridge's conformance checker
```

`--mc-version <v>` picks one installed version; without it, commands that read one jar use the newest fully supported
one. `--jar <path>` reads any jar. Output of `model` is `<name>.gltf`, `<name>.bin` and one PNG per distinct image
(`--name` sets the stem, `--frame N` picks a sheet frame of animated textures instead of the first one the animation
shows, `--corner-origin` keeps the game's origin).

**Ids.** `block/<name>[?variant=<state>]` and `item/<name>`, where `<name>` is the file name in the jar.

**Which state a block is.** The game's own default state is in code, so the data does not say. With no state, a block is the
blockstate file's `normal` (or empty) entry, else **its first entry**: `block/oak_stairs` is
`facing=east,half=bottom,shape=inner_left` (the first of 40 in 26.3), not a straight stair. For a multipart block that
needs properties (redstone wire), it is the first state its own conditions describe. A property the file does not mention
(`power`, `waterlogged`) is ignored and listed; a state made only of such properties is the default. A partial state takes
the first entry that fits. Where this is visible:
- `mc-unpack list <name> --states` marks the entry `block/<name>` builds, and `mc-unpack inspect <id>` prints the state
  used, what was ignored, every note and what was left out;
- the Provider has no channel for notes (rewrite-bridge 0.2 has only the model), so it puts the answer in two names that
  it does control: `Model::name` is the id that reproduces the model (`block/oak_stairs?variant=facing=east,half=bottom,shape=inner_left`
  when no state was given), and `AssetInfo::name` in `list` is `oak_stairs [facing=east,half=bottom,shape=inner_left]` for a
  block with several states (the `id` stays `block/oak_stairs`). `Minecraft::load_report` returns the full notes.

**Time and memory** (release build, 16 GB Apple silicon Mac, 23 to 24 October 2026): `check` over all eight jars takes 1.3 s
and peaks at 86 MB; `check-bridge` 0.1 s per jar; `check-conformance` on 26.3 (2,784 assets) a few seconds.

## Formats as decoded

The module docs are the reference: [`src/blockstate.rs`](src/blockstate.rs), [`src/model.rs`](src/model.rs),
[`src/bake.rs`](src/bake.rs), [`src/item.rs`](src/item.rs), [`src/texture.rs`](src/texture.rs), [`src/tint.rs`](src/tint.rs).
In short:

- **Space.** The game's: 16 units a block, +X east, +Y up, +Z south (north is -Z). Quads come out in blocks (0 to 1).
- **Face corners and UV.** Each face lists top-left, bottom-left, bottom-right, top-right as seen from outside
  (counter-clockwise), "up" being +Y on the sides, north on the up face, south on the down face. The UV rectangle
  `[u0 v0 u1 v1]` is laid on them as `(u0,v0) (u0,v1) (u1,v1) (u1,v0)`; a face `rotation` of r shifts that by r/90 corners.
  With no `uv`, the rectangle is the element's own extent projected on the face (a half-height slab's side shows the lower
  half of the texture).
- **Face textures are slot names.** A face's `texture` is the name of a slot in the model's `textures`, written with or
  without the `#` (`"all"` and `"#all"` are the same; Mojang's `heavy_core` model uses the first form, in 1.21.1, 26.2 and
  26.3, and is an ordinary block), never a path. A slot's value is a path, or `#another` slot, which is followed. A name no
  slot defines leaves that face out, with a note: the game draws its built-in missing-texture pattern there.
- **Element rotation.** Right-handed about the axis through `origin`. With `rescale`, each axis is first stretched by 1 / the
  largest component of where it ends up after the turn, which is 1 / cos(angle) up to 45 degrees and is defined beyond
  (that is the 26.x rule; 1.8 to 1.21 only accept -45 to 45, where the two agree). 26.x also writes `{x, y, z, origin}`; a missing
  angle is 0, and `rescale` works the same. The angles are applied **x first, then y, then z** (the matrix `Rz * Ry * Rx`, which the game's own
  code writes as one rotation; the two Euler rotations in the jars, `template_hanging_sign_rot_*` with x 180, y -67.5,
  z -180, are the only ones and come out as a turn about Y). A face with no area (the side faces of a flat element) is not
  emitted, and a note counts them.
- **Variant rotation.** `x` first, then `y`, then `z`, about the block centre: `y: 90` turns north to east (clockwise from
  above), `x: 90` turns up to north, `z: 90` turns east to down. 26.x blockstates may have `z` (none of the jars here do);
  the versions before it do not read it, so there it is ignored, with a note. **`uvlock`**: a corner's UV is re-derived by laying it on its face plane, rotating it with the
  block, and reading it back in the UV frame of the face it ended on. Checked against real stairs: with `uvlock` the same world
  position gets the same UV in all four facings (a real-data test).
- **Multipart `!`.** A condition value may be `a|b` (any of them), and may be negated. The two versions of the game that were
  read disagree on `!a|b`: 1.8 to 1.12 negate the whole value (neither `a` nor `b`), 26.x negate each term (not `a`, or `b`).
  This crate follows each by the jar's layout; for the jars between (1.13 to 1.21.3) the rule is not confirmed, so a `!`
  combined with `|` is refused there (the part does not apply, with a note). A single negated term, and plain alternatives, read the same everywhere.
  None of the jars here has a `!` in a blockstate.
- **Cull faces** are kept on the quads (rotated with the block) and not applied: a lone block shows every face. A cull face
  name the game does not know (Mojang's own `scaffolding_unstable` says `bottom`) means none, as in the game.
- **Textures.** A texture's `.mcmeta` `animation` says frame size (given, else the shorter side), the frame order and time
  per frame (`frames`, else every frame of the sheet in order), `interpolate`. UVs refer to **one frame**, so a model draws a
  frame, not the sheet. The default is **the first frame the animation lists** (`fire_0` lists 16 first, so that is not
  sheet frame 0); a texture without `frames` starts at sheet frame 0.
- **Invalid animation frames.** As in the game, a listed frame that is not on the sheet is dropped; with one frame or none left
  the texture is not animated at all (it is the sheet's first frame, whatever the one listed frame was).
- **What the format defaults, and what is required.** Read as the format defines them, not as this crate guesses: `weight` 1,
  `uvlock` false, `x`/`y`/`z` 0, element `shade` true, face `rotation` 0 (read modulo 360, so 360 and -90 are fine), no
  `cullface` (a name the game does not know is none), no `tintindex`, a rotation's `rescale` false, each missing Euler angle 0,
  `frametime` 1 and `interpolate` false in a `.mcmeta`. A model with an `elements` key whose list is empty replaces its parent's
  elements in 26.x and **inherits them** in 1.8 to 1.12. Required, as in the game (an error where they are missing): a rotation's `origin`, an
  element's `faces` (at least one), and, for this crate, the `temperature` and `downfall` of a `grass` item tint (the game's
  own default is in code: without them the tint is left uncoloured, with a note). `from` and `to` must lie in -16 to 32, every number must be
  finite, `light_emission` is 0 to 15; a face `rotation`, `tintindex` or a `.mcmeta` number written `90.0` is read as 90, anything that is not a
  whole number or is out of range is an error, never a silent 0 or a truncation (the game truncates a fraction; this crate
  refuses it).
- **Generated items.** Only `layer0` to `layer4`, in order, stopping at the first one that is missing or does not resolve
  (a gap ends the list; `layer5` is not read). Each layer: a front and a back face over the whole square at z 7.5 to 8.5, and
  one strip **per pixel edge** where an opaque pixel (alpha above 0 in some frame) meets a transparent one or the border,
  not merged into runs, each strip's UV its pixel shrunk by 0.1 texel on every side so that filtering does not bleed (the
  back face's u runs the other way, `[16 0 0 16]`, so that it samples the same texel as the front at the same x). **Left out:**
  the game names the left and right strips with the opposite direction (east for a strip on the west edge) and draws them
  without culling; this crate gives them the outward direction, which changes only their shading and makes them visible to
  a viewer that culls back faces. **Opacity is the union over the frames** of an animated item texture,
  and the mesh is built once from it; the game builds spans per frame, so for an animated item texture this can differ
  (none of the items in the eight jars animates by `.mcmeta`; the clock and compass are chosen by code). A layer over 128 x 128
  pixels, or needing more than 1,040 elements (a checkerboard alpha has one strip per pixel edge), is **refused**: left out,
  with the reason (the most any item in the jars needs is 260 quads). Layer N carries `tintindex` N. The one liberty: layers above the
  first are made 0.005 units thicker per layer so coplanar layers do not z-fight in a viewer (the game draws them in order).
- **Item definitions** (1.21.4 and later) are walked to their **default branch**: `model` is itself, `composite` is all of its
  models, `condition` takes `on_false`, `select` and `range_dispatch` take `fallback` (else the first case). A judgment call worth knowing:
  a `select` on `display_context` (26 items: bundles, spears, the spyglass ...) names the contexts where the item is a flat
  sprite (`gui`, `ground`, `fixed`, `on_shelf`) and keeps the model for **every other context** as its fallback, so
  `item/spyglass` is the in-hand model, not the flat icon; the note says which contexts the cases name. `special` is drawn by code and left out. What was chosen is in the
  model's notes, and so is every part left out of a composite (`dropped_parts`) or failed (`failed_parts`, which `check` counts). `tints` are read: `constant` is the colour, `grass`
  is a colormap lookup, a dynamic tint (dye, potion) uses its `default` when the data gives one, else nothing, with a note. An unreadable `constant` value is left uncoloured, with a note.
- **Generated textures.** The armor-trim layers of 1.20 and later items (`trims/items/boots_trim_amethyst`) have no PNG: the
  game makes them from `atlases/armor_trims.json` (`paletted_permutations`). They are produced the same way here: a base
  texel that equals a colour of the palette key is replaced by the same position in the other palette, alpha kept. What the
  game does in the awkward cases is not in the data, so palettes of different sizes, a colour twice in the key, and any
  transparent palette colour are **refused** (the item layer is left out, with the reason) instead of truncated or guessed.

## Tints

Every face keeps its `tintindex`. Which colour an index gets is **not** in the block data (grass blocks use the grass
colormap, oak leaves the foliage one, redstone dust a ramp by power, spruce and birch leaves fixed colours: all in the game's
code), so nothing is coloured unless you say so. The jar's own data helps in three places:

- the colormaps (`grass.png`, `foliage.png`, `dry_foliage.png`), with the game's lookup: clamp temperature and downfall to
  0..1, multiply downfall by temperature, take pixel `(1 - temperature) * 255`, `(1 - downfall) * 255`, truncated;
- biomes (`data/minecraft/worldgen/biome`, jars from 1.19): temperature, downfall and colour overrides, so `--tint
  0=grass@plains` needs no number from this crate. A biome's `grass_color_modifier` (swamp, dark forest) and
  `temperature_modifier` (frozen) are applied by the game's code and are reported, not applied;
- 26.x item definitions, which carry their own tints (oak leaves `#48b518`, grass block: the grass colormap at 0.5 / 1.0).

`--tint INDEX=COLOUR` takes `#rrggbb`, `grass:T,D`, `foliage:T,D`, `dry_foliage:T,D` or `grass@biome`. A tint is
**multiplied into the PNG** (per channel, in sRGB space, as the game does) rather than set as a glTF colour factor, which
would multiply in linear space and differ. Cross-check against a real game: the ground of a 1.7.10 superflat screenshot
averages `(83, 108, 51)`, which is plains grass `#91bd59` times 0.57, and the mean grey of `grass_top.png` is 0.578.

## Provider

`mc_unpack::bridge::Minecraft` implements `rewrite_bridge::Provider` (rewrite-bridge `0.2`, from
[crates.io](https://crates.io/crates/rewrite-bridge); every `0.2` requirement resolves to one copy, so `cargo tree -i
rewrite-bridge` should show one entry).

```toml
[dependencies]
mc-unpack = { git = "https://github.com/patodois/mc-unpack" }   # add rev = "<commit>" to pin it
rewrite-bridge = "0.2"
```

```rust
let mc = mc_unpack::bridge::Minecraft::open_default()?;          // the newest installed version it can read
let stairs = mc.load("block/oak_stairs?variant=facing=east,half=bottom,shape=straight")?;
let sword = mc.load("item/diamond_sword")?;
let all = mc.list("stairs")?;                                     // only what draws something
```

`with_tints(TintSet)` colours tint indices, `with_frame(n)` picks a frame of animated textures. `Minecraft::open` fails for a
jar without JSON models (1.7.10 and earlier); the readers in `Assets` still give its textures.

| | |
|---|---|
| ids | `block/<name>[?variant=<state>]`, `item/<name>`; kind `Prop` |
| units, up, winding | metres (1 block = 1 m), +Y up, counter-clockwise front faces, sRGB textures |
| axes | the game's, unchanged: +X east, +Y up, +Z south. This is the minecraft-link mapping, where a position maps to itself. A block has no front of its own: the `facing` of its state decides (`facing=south` puts a furnace face on +Z, the bridge's front) |
| origin | the bottom centre of the block's cell, like hytale-unpack's blocks: a full cube spans x and z from -0.5 to 0.5 and y from 0 to 1 |
| meshes | one per texture, tint and blend mode; normals from the face; a face is 4 vertices and 2 triangles |
| materials | the texture's frame (tinted if a tint is set), matte (`metallic 0`, `roughness 1`) |
| alpha | see below |
| filtering | **nearest** in the glTF files (sampler `NEAREST`, clamp). The bridge's `Material` has no sampler field, so a consumer must choose nearest itself: bridge-testbed filters linearly and blurs the pixel art |

**Alpha.** The game's render layer (solid, cutout, translucent) is in code in every version tested; 26.x names it for one
case only (`force_translucent` on a texture, used by redstone dust). The provider therefore uses what the pixels permit: all
texels opaque is `Opaque` (the same in any layer), only alpha 0 and 255 is `Mask(0.5)`, any partial alpha is `Blend`, and
`force_translucent` is `Blend`. That is not a statement about what the game does for a given block.

**Listing.** `list` gives every block whose blockstate reaches a model with geometry and every item whose default model has
some: 1,090 for 1.12.2, 2,465 for 1.20.1, 2,784 for 26.3. Block entities and the like are not listed; `mc-unpack list
--left-out` names them.

## Checking

`check` parses every blockstate, model, item definition, texture and `.mcmeta`, bakes every model each blockstate mentions
(not only the default state) and every item, and counts. Release build, this Mac, jars of the Prism libraries:

| Version | textures | `.mcmeta` | blockstates | block models | item models | item defs | blocks baked | items baked | failures |
|---------|---------:|----------:|------------:|-------------:|------------:|----------:|-------------:|------------:|---------:|
| 1.7.10 | 941 | 13 | - | - | - | - | - | - | 0 |
| 1.12.2 | 1,406 | 26 | 407 | 878 | 717 (3 templates) | - | 407 | 714 | 0 |
| 1.19.2 | 2,361 | 70 | 935 | 1,842 | 1,318 (4) | - | 935 | 1,314 | 0 |
| 1.20.1 | 2,431 | 73 | 1,005 | 2,016 | 1,675 (5) | - | 1,005 | 1,670 | 0 |
| 1.21.1 | 3,070 | 97 | 1,062 | 2,137 | 1,754 (6) | - | 1,062 | 1,748 | 0 |
| 26.2 | 3,855 | 176 | 1,198 | 2,657 | 1,271 | 1,537 | 1,198 | 1,537 | 0 |
| 26.3 | 3,964 | 218 | 1,288 | 2,883 | 1,309 | 1,658 | 1,288 | 1,658 | 0 |

Every number above is a file that parsed or baked. `check` also counts, apart from failures: faces and elements left out inside
models that otherwise bake (5 to 8 per version: the faces of flat elements that have no area, in `pitcher_plant` and the
mangrove propagules, written with a face for every side), items that lost a part to code or a refusal (0 in the jars here). A composite item with one part that fails to bake (a missing
texture) counts as **failed**, not ok. Things that needed a decision, each found by `check` failing first:

- `scaffolding_unstable` (1.19 to 1.21.1) names the cull face `bottom`: read as none, as the game does.
- 250 armor-trim items in 1.20.1 and 1.21.1 name textures with no PNG: generated from the atlas definition (above).
- "item templates" (`item/generated`, `item/handheld`, ...) are models other models inherit from, not items.

### Limits

A jar is read as untrusted input. The sizes below were measured on the eight jars on this machine, and each limit is a
multiple of what a real jar needs (`check` prints the biggest it met next to the limit):

| What | Limit | Biggest in the jars |
|------|-------|---------------------|
| a JSON or `.mcmeta` entry (parsed into a tree, several times its size) | 128 KiB | 24,324 bytes (`items/clock.json`, 26.3) |
| a PNG entry | 8 MiB | 125 KB |
| pixels of a PNG, from its header, before decoding | 2048 x 2048 | 512 x 512 (a GUI image; block and item textures: 16 x 16 to 16 x 1024) |
| frames in a texture sheet | 4,096 | 64 |
| steps in an animation's `frames` list | 4,096 | 64 |
| size of a generated item layer, and its elements | 128 x 128, 1,040 elements | 16 x 16 mostly, 259 elements for an item |
| one bake: models (parts) | 208 | 52 (a multipart block with every part at once) |
| one bake: quads | 1,040 | 260 (a diamond sword) |
| one mesh: bytes of first-frame images | 160 KiB | 40,960 bytes |
| decoded textures kept for reuse | 128 MiB (the cache empties) | a few MiB |
| parsed blockstates, models kept | 20,000 each (the cache empties) | about 4,300 |
| any other entry | 16 MiB | not read |

An entry is read through a limit, never sized from what its zip header declares (a 408 KB jar that declared 100 bytes and
inflated to 400 MiB now stops at the limit), and a 42 MB JSON that went to a 1.6 GB peak is refused from its header. The
sweep peaks at 86 MB over all eight jars. Names of entries with control or direction-override characters are not indexed, and
text that came from a jar is printed with them escaped, so a hostile jar cannot rewrite a terminal. `Quad::texture` is an
`Arc<str>` (one allocation per model, not one per face), and quads are grouped by material through a map.

`check-bridge` loads everything `list` gives through the Provider and validates each `Model`: 1,090 / 2,070 / 2,465 / 2,600 /
2,575 / 2,786 loaded of as many listed for 1.12.2, 1.19.2, 1.20.1, 1.21.1, 26.2 and 26.3, 0 failures. An item with a
part that failed to bake does not load (`Provider::load` returns the error); an item that only lost a part to code or a
refusal loads, and `check-bridge` counts those apart.

`check-conformance` runs the bridge's conformance checker over what the Provider returns (scale, orientation, winding,
materials; options are the checker's, `--help` lists them; put `--mc-version` before the subcommand). On every version above,
**no errors and no warnings** (exit status 0); the same assets in bridge-testbed give a 1.000 x 1.000 x 1.000 m box for
`block/stone`, identical to the Hytale block it is compared with (AABB `[-0.5, 0, -0.5]` to `[0.5, 1, 0.5]`).

Tests: `cargo test` runs 149 unit tests on tiny synthetic jars built in the tests (no game data anywhere; they include a zip that
lies about its size and PNGs over the limits) and 24 real-data tests
in `tests/game_data.rs` that read the installed jars and skip, saying so, where a version is not installed. The real-data
tests pin, among others: grass tint equals plains `#91bd59` from the jar's own colormap and biome; stairs facing east have
their tall side to the east in 1.12.2, 1.21.1 and 26.3; `uvlock` keeps planks aligned in all four facings; a wall torch
leans away from its wall (which fixes the sign of element rotation); pistons face where their state says; and `check` has
no failures on every installed version.

## Rendering checks

`render/` has a three.js + Playwright script that renders an exported glTF headless in Chromium (the approach of
hytale-unpack's `render/`). It needs [Node.js](https://nodejs.org) 20 or newer:

```sh
cd render
npm install
npx playwright-core install chromium     # once; or set CHROMIUM=/path/to/chrome
mc-unpack model block/grass_block -o /tmp/gl --tint 0=grass@plains
REFS=raw/top.png,raw/side.png node shot.cjs out.png /tmp/gl/block_grass_block.gltf "fit=1.55&dir=0.7,0.6,1&look=mc" 760x600
```

`look=mc` shades faces by direction the way the game does (top 1.0, bottom 0.5, north and south 0.8, east and west 0.6): viewer
styling, not data. `REFS` shows the raw textures of the jar, enlarged, beside the render. Used to check by eye: grass
block (untinted, tinted), oak stairs facing east, redstone wire, oak leaves, a chest (left out), a piston, a fence with arms,
a sapling (crossed planes with `rescale`), a wall torch, a diamond sword, in 1.12.2, 1.20.1 and 26.3. Renders are game
content and are not in this repository.

## What is left out

- **Block entities, mobs, the player.** Their models are built in code in every version here. The jars hold their textures
  (`textures/entity`, 743 PNGs in 26.3) but no geometry: no JSON model, no entity model file, nothing that is cuboids and
  UVs. 26.x has `equipment/*.json` (which texture layers an armor piece uses) and `special` item models (which entity renderer an
  item uses); both name code, they do not describe shape. **Nothing is reconstructed.** `list --left-out` names them with the
  reason. In 26.3 (blocks and items counted together): 32 banners, 22 chests, 34 shulker boxes, 12 heads, 16 copper golem
  statues, 2 player heads, a conduit, decorated pots, a shield, a trident, and 36 blocks with no geometry at all (air, barrier, portals, fluids, wall banners and wall heads, whose
  blockstates name an empty model); 1.8 to 1.12 give block entities no blockstate, only an item whose parent is `builtin/entity`
  (31 items).
- **1.2.5** (no JSON models, art on atlas sheets) and the models of **1.7.10** (none: textures only).
- **Render layers** (solid, cutout, translucent) and **mipmapping** (`mipmap_strategy` in a `.mcmeta`), which are the game's
  code: alpha is inferred from texels (see above). A glTF cannot make a viewer use nearest filtering through the bridge.
- **Tint colours** the game computes: redstone dust by power, spruce and birch leaves, anything per biome the data does not
  give, grass colour modifiers (swamp, dark forest), biome blending between neighbouring blocks. `tintindex` is exposed so a
  caller can supply them. Dynamic item tints (dye, potion, firework, map) use the definition's `default`, or nothing.
- **What it refuses to guess**, each with its reason where it is met: a generated item layer over the size limits, a
  palette swap whose palettes are ambiguous, an Euler rotation whose order matters, `rescale` past 45 degrees, an item tint
  whose data is missing. Nothing is defaulted where the data always speaks.
- **Which variant a weighted list picks** (the game uses the block position): the first is used, the rest are in
  `BlockState`. **The default state of a block** (in code): see Ids above.
- **Item model state** (`condition`, `select`, `range_dispatch`, `overrides` of 1.8 to 1.21.3): only the default branch;
  `display` transforms (GUI, hand, ground) are parsed and kept, not applied; `gui_light`, `ambientocclusion`, `shade`,
  `light_emission` and `shade_direction_override` are read where cheap and not used for shading.
- **Animation playing**: frames and timing are exposed, nothing plays; `interpolate` is exposed, not applied; the clock and
  compass of 1.12.2 (which the game animates in code) are their texture's frame 0.
- **Particles, fluids, sky, GUI textures, sounds, shaders, `post_effect`, fonts**: not read (the PNGs are decoded by `check`,
  nothing builds models from them).
- **Resource packs, modded namespaces**: only the `minecraft` namespace in the jar.
- **Blockstate properties are not enumerated**: the property list comes from the file (`list --states`), not from the game's
  block classes, so a state with a property the file never mentions (`power`, `waterlogged`) selects the same model.

## Legal

This repository publishes only code that reads files you already have: no Mojang code, no game files, textures, models or
renders are included, and the tests build their own tiny jars. `mc-unpack` reads resource files (JSON and PNG) only; it never
reads, decompiles or runs the `.class` files in the jar. What the official documents say, as read by this author for the
minecraft-link project on 2026-10-23 at minecraft.net (summarised, not legal advice; the pages could not be fetched again
while this was written, so read them yourself):

- [Minecraft End User License Agreement](https://www.minecraft.net/en-us/eula) and
  [Usage Guidelines](https://www.minecraft.net/en-us/usage-guidelines).
- A **mod** is something original that does not contain a substantial part of Mojang's copyrightable code or content; you own
  what you create and may share it as long as you do not sell it or try to make money from it, and **do not distribute a
  modded version of the game**. The guidelines add: a product that mentions Minecraft must say that it is not official (the
  line at the top of this page) and must not use the Minecraft name as its primary or dominant name, which is why this crate is
  called `mc-unpack` and says "for Minecraft" in its description. No Minecraft assets, models, textures or renders are
  distributed here, and the exported glTF and PNG files you make are Mojang's content: keep them to yourself or follow the
  guidelines before sharing them.
- Mojang may change the guidelines at any time and may take down projects that break them.

This tool only reads a jar on your own disk and writes files to a folder you name; it sends nothing anywhere. The author is
not affiliated with Mojang or Microsoft and does not claim the legality of reading your own install for any purpose you have
in mind: that is yours to check.

## Licence

MIT or Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
