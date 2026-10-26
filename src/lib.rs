//! # mc-unpack
//!
//! A reader for the block models, item models and textures inside the client jar of your own
//! Minecraft: Java Edition install, with a [rewrite-bridge](https://crates.io/crates/rewrite-bridge)
//! provider behind the `bridge` feature. Unofficial: not affiliated with Mojang or Microsoft.
//! No game code or game data is in this crate.
//!
//! The rule is **nothing invented, 1:1 with the data in the jar**. What the game builds in code
//! (block entities, mobs, the player, render layers before 26.x, tint colours that are not in a
//! colormap) is left out and listed in the README.
//!
//! Layers, bottom to top:
//!
//! - [`jar`]: seek-to-entry reading of the zip; [`install`]: where the jars are; [`version`]: which
//!   format a jar has and what this crate supports for it;
//! - [`texture`]: PNG and `.mcmeta` animation; [`tint`]: colormaps and the lookup;
//! - [`blockstate`], [`model`]: the JSON files, parent inheritance, texture variables;
//! - [`bake`]: elements to quads (rotation, `uvlock`, UV rules); [`item`]: the generated-item mesh
//!   and the `items/` definitions of 1.21.4 and later;
//! - [`assets`]: the one object that ties them to a jar; [`mesh`]: quads grouped by material;
//!   [`gltf`]: export; [`check`]: the sweep;
//! - [`world`]: your own saves: NBT, region files, chunks, the block table and the face culling,
//!   biome tints, and (with the `bridge` feature) the scene.

// Pixel loops read better as chunks_exact(4) than as_chunks; is_multiple_of is newer than some toolchains
#![allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of)]

pub mod assets;
pub mod bake;
pub mod blockstate;
pub mod check;
pub mod gltf;
pub mod install;
pub mod item;
pub mod jar;
pub mod mesh;
pub mod model;
pub mod text;
pub mod texture;
pub mod tint;
pub mod version;
pub mod world;

#[cfg(feature = "bridge")]
pub mod bridge;

pub use assets::Assets;
