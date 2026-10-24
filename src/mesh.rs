//! Quads grouped into drawable batches: one per texture, tint and blend mode.
//!
//! Both exporters (glTF and the rewrite-bridge provider) go through here, so they agree on the
//! texture frame, the tint, the alpha class and the origin.

use anyhow::{Context, Result};

use crate::assets::{Assets, BakedModel};
use crate::bake::Quad;
use crate::texture::{AlphaClass, Rgba};
use crate::tint::TintSet;

/// Where a model's origin sits
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The block's lowest corner, as the game's model space has it
    BlockCorner,
    /// The middle of the block's bottom face (the bridge's convention for props)
    BottomCentre,
}

#[derive(Clone, Debug)]
pub struct MeshOptions {
    /// Which frame of an animated texture to use (modulo the number of frames sheet); `None`: the
    /// first frame the texture's animation shows (see [`crate::texture::Texture::default_frame`])
    pub frame: Option<u32>,
    /// Tint colours by index; they replace any tint the data attached to the model
    pub tints: TintSet,
    pub origin: Origin,
}

impl Default for MeshOptions {
    fn default() -> Self {
        MeshOptions {
            frame: None,
            tints: TintSet::new(),
            origin: Origin::BottomCentre,
        }
    }
}

/// Quads that share a material
#[derive(Clone, Debug)]
pub struct Group {
    /// `block/stone`
    pub texture: String,
    pub tint_index: Option<u32>,
    /// The colour multiplied into the texture, if the index has one
    pub tint: Option<[u8; 3]>,
    pub alpha: AlphaClass,
    /// The data says the texture is drawn translucent (it is part of the group's key)
    pub force_translucent: bool,
    /// The frame, with the tint already multiplied in (the game multiplies in sRGB space, which a
    /// glTF colour factor cannot do)
    pub image: Rgba,
    /// Quads in blocks, origin applied
    pub quads: Vec<Quad>,
}

fn shift(q: &Quad, origin: Origin) -> Quad {
    let mut q = q.clone();
    if origin == Origin::BottomCentre {
        for p in q.pos.iter_mut() {
            p[0] -= 0.5;
            p[2] -= 0.5;
        }
    }
    q
}

pub fn build(assets: &Assets, model: &BakedModel, opts: &MeshOptions) -> Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for part in &model.parts {
        for q in &part.quads {
            let tint = match q.tint_index {
                Some(i) => match opts.tints.get(i).or_else(|| part.tints.get(i)) {
                    Some(t) => Some(assets.tint_rgb(t).with_context(|| format!("tint {i}"))?),
                    None => None,
                },
                None => None,
            };
            let found = groups.iter_mut().find(|g| {
                g.texture == q.texture
                    && g.tint_index == q.tint_index
                    && g.tint == tint
                    && g.force_translucent == q.translucent
            });
            match found {
                Some(g) => g.quads.push(shift(q, opts.origin)),
                None => {
                    let tex = assets
                        .texture(&q.texture)
                        .with_context(|| format!("{}: texture of a face", model.id))?;
                    let img = match opts.frame {
                        Some(f) => tex.frame(f % tex.sheet_frames())?,
                        None => tex.default_frame(),
                    };
                    let alpha = img.alpha_class(q.translucent);
                    let image = match tint {
                        Some(c) => img.tinted(c),
                        None => img,
                    };
                    groups.push(Group {
                        texture: q.texture.clone(),
                        tint_index: q.tint_index,
                        tint,
                        alpha,
                        force_translucent: q.translucent,
                        image,
                        quads: vec![shift(q, opts.origin)],
                    });
                }
            }
        }
    }
    Ok(groups)
}
