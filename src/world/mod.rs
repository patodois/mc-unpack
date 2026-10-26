//! Worlds: your own Minecraft: Java Edition saves, read from disk and turned into scenes of baked
//! blocks. Read only: nothing here writes to a save.
//!
//! The rule is the crate's: **nothing invented, 1:1 with the data**. What a save and the jar hold is
//! read; what lives in the game's code (the numeric block ids and data values before 1.13, fluids,
//! block entities beyond their block's model, the game's own occlusion and tint rules) is left out
//! and reported, or, where this crate has to decide something the data does not (which faces a block
//! hides, which blocks take which biome colormap), the rule is written down in one place and called
//! this crate's own: [`blocks`].
//!
//! Layers, bottom to top:
//!
//! - [`nbt`], [`compress`], [`region`], [`level`]: the file formats, bounded (every length is checked
//!   against the bytes that are there before anything is allocated from it);
//! - [`chunk`]: a chunk's NBT to palettes and indices per 16 x 16 x 16 section (1.13 and later);
//! - [`save`]: a save folder, its dimensions and regions, and where saves live on a machine;
//! - [`blocks`], [`biomes`]: block states baked once each (through [`crate::assets::Assets`]), the
//!   cover rule and the tint table, biome colours from the jar's data;
//! - [`grid`]: the blocks of a window of chunks in compact form, and the loader;
//! - [`mesher`]: the window to meshes, with face culling, tints and the [`mesher::WorldReport`] of
//!   what was left out; [`crate::gltf::write_world`] writes it, [`scene`] (feature `bridge`) turns it
//!   into a rewrite-bridge scene.
pub mod biomes;
pub mod blocks;
pub mod chunk;
pub mod compress;
#[cfg(test)]
mod fuzz;
pub mod grid;
pub mod level;
pub mod mesher;
pub mod nbt;
pub mod region;
pub mod save;
#[cfg(feature = "bridge")]
pub mod scene;
#[cfg(test)]
pub(crate) mod testworld;
