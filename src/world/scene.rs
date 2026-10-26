//! Worlds as rewrite-bridge scenes (the `bridge` feature).
//!
//! # Ids
//!
//! `world/<save>/<x>,<z>` names a region of a save: `<save>` is the save's folder name (when two
//! saves share one, `<launcher instance>/<folder>`; [`crate::world::save::discover_saves`] makes
//! the labels; a `?` or `%` in a label is written `%3F` or `%25`), `<x>,<z>` the region file's coordinates, `r.<x>.<z>.mca`. After a `?`, options
//! joined by `&` narrow or widen it:
//!
//! | option | meaning |
//! |--------|---------|
//! | `radius=<n>` | add `n` regions on every side |
//! | `chunks=<x0>,<z0>,<x1>,<z1>` | keep only these chunks (chunk coordinates, inclusive) |
//! | `y=<min>,<max>` | keep only these block heights (inclusive) |
//! | `dimension=<name>` | `overworld` (the default), `the_nether`, `the_end`, `namespace:name` |
//! | `unfinished=1` | also read chunks the game had not finished generating |
//!
//! # The scene
//!
//! One inline model per chunk column (16 x 16 blocks, the save's full height), one instance of it
//! at `(16 cx, 0, 16 cz)`. Units are blocks = metres, the axes are the game's (+X east, +Y up, +Z
//! south) and a block's position is the game's own block coordinate: the block at `(bx, by, bz)`
//! spans `bx..bx+1`, `by..by+1`, `bz..bz+1` in the scene. (This is the mapping minecraft-link uses
//! for a running game; the lone blocks the provider also serves sit on their bottom centre.) Models
//! have no lights and the scene no sky: the game's sky and light are not in the data this reads.

use anyhow::{anyhow, bail, Context, Result};
use rewrite_bridge::{
    AlphaMode, Image, Instance, Material, Mesh, Model, Node, Scene, SceneModel, Transform,
};

use super::mesher::{Window, WorldMesh};
use crate::texture::AlphaClass;

/// A parsed world id
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldId {
    pub save: String,
    pub window: Window,
    pub dimension: String,
    pub include_unfinished: bool,
}

impl WorldId {
    pub fn parse(id: &str) -> Result<WorldId> {
        let rest = id
            .strip_prefix("world/")
            .ok_or_else(|| anyhow!("{id:?} is not a world id (`world/<save>/<x>,<z>`)"))?;
        // the save label may hold `/`; the last `/` before the options starts the coordinates
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let (save, coords) = path
            .rsplit_once('/')
            .ok_or_else(|| anyhow!("{id:?}: expected `world/<save>/<x>,<z>`"))?;
        if save.is_empty() {
            bail!("{id:?}: the save has no name");
        }
        let (x, z) = coords.split_once(',').ok_or_else(|| {
            anyhow!("{id:?}: expected region coordinates `<x>,<z>`, got {coords:?}")
        })?;
        let x: i32 = x
            .parse()
            .with_context(|| format!("{id:?}: region x {x:?}"))?;
        let z: i32 = z
            .parse()
            .with_context(|| format!("{id:?}: region z {z:?}"))?;
        let mut w = Window::region(x, z);
        let mut dimension = "overworld".to_string();
        let mut include_unfinished = false;
        if let Some(q) = query {
            for opt in q.split('&').filter(|o| !o.is_empty()) {
                let (k, v) = opt
                    .split_once('=')
                    .ok_or_else(|| anyhow!("{id:?}: option {opt:?} is not key=value"))?;
                let nums = |n: usize| -> Result<Vec<i32>> {
                    let v: Vec<i32> = v
                        .split(',')
                        .map(|p| p.parse::<i32>())
                        .collect::<std::result::Result<_, _>>()
                        .map_err(|_| {
                            anyhow!("{id:?}: {k} wants {n} whole numbers separated by commas")
                        })?;
                    if v.len() != n {
                        bail!("{id:?}: {k} wants {n} whole numbers separated by commas");
                    }
                    Ok(v)
                };
                match k {
                    "radius" => {
                        w.radius = v.parse().with_context(|| format!("{id:?}: radius {v:?}"))?
                    }
                    "chunks" => {
                        let n = nums(4)?;
                        w.chunks = Some([n[0], n[1], n[2], n[3]]);
                    }
                    "y" => {
                        let n = nums(2)?;
                        w.y = Some((n[0], n[1]));
                    }
                    "dimension" => dimension = v.to_string(),
                    "unfinished" => include_unfinished = matches!(v, "1" | "true"),
                    other => bail!("{id:?}: unknown option {other:?} (radius, chunks, y, dimension, unfinished)"),
                }
            }
        }
        Ok(WorldId {
            save: unescape_label(save),
            window: w,
            dimension,
            include_unfinished,
        })
    }

    /// The id text, options only where they differ from the default
    pub fn to_id(&self) -> String {
        let mut o = Vec::new();
        if self.window.radius != 0 {
            o.push(format!("radius={}", self.window.radius));
        }
        if let Some([a, b, c, d]) = self.window.chunks {
            o.push(format!("chunks={a},{b},{c},{d}"));
        }
        if let Some((a, b)) = self.window.y {
            o.push(format!("y={a},{b}"));
        }
        if self.dimension != "overworld" {
            o.push(format!("dimension={}", self.dimension));
        }
        if self.include_unfinished {
            o.push("unfinished=1".into());
        }
        let q = if o.is_empty() {
            String::new()
        } else {
            format!("?{}", o.join("&"))
        };
        format!(
            "world/{}/{},{}{q}",
            escape_label(&self.save),
            self.window.region.0,
            self.window.region.1
        )
    }
}

/// A save label in an id: `?` starts the options and `%` is the escape, so those two are written
/// `%3F` and `%25` (a folder may be called anything the file system allows)
pub fn escape_label(s: &str) -> String {
    s.replace('%', "%25").replace('?', "%3F")
}

fn unescape_label(s: &str) -> String {
    s.replace("%3F", "?").replace("%25", "%")
}

fn alpha(a: AlphaClass) -> AlphaMode {
    match a {
        AlphaClass::Opaque => AlphaMode::Opaque,
        AlphaClass::Mask => AlphaMode::Mask(0.5),
        AlphaClass::Blend => AlphaMode::Blend,
    }
}

/// The mesh as a scene: a model per chunk column, an instance of each at its place
pub fn scene_of(mesh: &WorldMesh, name: &str) -> Result<Scene> {
    let mut scene = Scene {
        name: name.to_string(),
        ..Scene::default()
    };
    for c in &mesh.chunks {
        let mname = format!("chunk {},{}", c.cx, c.cz);
        let mut m = Model {
            name: mname.clone(),
            nodes: vec![Node::new(mname.as_str(), None)],
            ..Model::default()
        };
        for g in &c.groups {
            let wm = &mesh.materials[g.material as usize];
            m.materials.push(Material {
                name: wm.name(),
                base_color: Some(Image {
                    width: wm.image.width,
                    height: wm.image.height,
                    rgba: wm.image.data.clone(),
                }),
                base_color_factor: [1.0; 4],
                metallic: 0.0,
                roughness: 1.0,
                alpha: alpha(wm.alpha),
                double_sided: false,
                ..Material::default()
            });
            let n = g.quads();
            let mut normals = Vec::with_capacity(n * 4);
            let mut indices = Vec::with_capacity(n * 6);
            for (i, q) in g.normals.iter().enumerate() {
                for _ in 0..4 {
                    normals.push(*q);
                }
                let b = (i * 4) as u32;
                indices.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
            }
            m.meshes.push(Mesh {
                name: wm.name(),
                node: 0,
                material: m.materials.len() - 1,
                positions: g.positions.clone(),
                normals,
                uvs: g.uvs.clone(),
                indices,
                ..Mesh::default()
            });
        }
        scene.models.push(SceneModel::Inline(m));
        scene.instances.push(Instance::new(
            scene.models.len() - 1,
            Transform::from_translation([c.cx as f32 * 16.0, 0.0, c.cz as f32 * 16.0]),
        ));
    }
    scene
        .validate()
        .map_err(|e| anyhow!("the scene built is invalid: {e}"))?;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for id in [
            "world/New World/0,0",
            "world/26.3(1)/New World/-1,2?radius=1",
            "world/a/0,0?chunks=0,0,3,3&y=-64,20&dimension=the_nether&unfinished=1",
            "world/a/5,-7?dimension=pack:sky",
            "world/what%3F/0,0?radius=2",
            "world/100%25 sure/0,0",
        ] {
            let p = WorldId::parse(id).unwrap();
            let again = WorldId::parse(&p.to_id()).unwrap();
            assert_eq!(p, again, "{id}");
        }
        let p = WorldId::parse("world/26.3(1)/New World/-1,2?radius=1").unwrap();
        assert_eq!(p.save, "26.3(1)/New World");
        assert_eq!(p.window.region, (-1, 2));
        assert_eq!(p.window.radius, 1);
        assert_eq!(p.to_id(), "world/26.3(1)/New World/-1,2?radius=1");
        // a label with `?` or `%` in it
        let q = WorldId::parse("world/what%3F 100%25/0,0?radius=1").unwrap();
        assert_eq!(q.save, "what? 100%");
        assert_eq!(q.to_id(), "world/what%3F 100%25/0,0?radius=1");
        assert_eq!(
            WorldId::parse(
                &WorldId {
                    save: "a?b%c".into(),
                    ..q.clone()
                }
                .to_id()
            )
            .unwrap()
            .save,
            "a?b%c"
        );
    }

    #[test]
    fn bad_ids_are_errors() {
        for id in [
            "block/stone",
            "world/",
            "world/x",
            "world//0,0",
            "world/x/0",
            "world/x/a,b",
            "world/x/0,0?radius=-1",
            "world/x/0,0?radius",
            "world/x/0,0?chunks=1,2,3",
            "world/x/0,0?y=a,b",
            "world/x/0,0?zoom=2",
        ] {
            assert!(WorldId::parse(id).is_err(), "{id}");
        }
    }
}
