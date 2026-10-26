//! A small glTF 2.0 writer for baked blocks and items.
//!
//! Output is `<name>.gltf`, `<name>.bin` and `<name>_<n>.png` (one PNG per distinct image). One node,
//! one mesh, one primitive per [`Group`]. Textures use nearest-neighbour sampling and clamp-to-edge
//! wrapping, because they are pixel art. Materials are matte (`metallic 0`, `roughness 1`); the
//! alpha mode comes from [`crate::texture::Rgba::alpha_class`]. Each primitive's `extras` carry the
//! `tintindex` of its faces and, when a tint was applied, the colour that is already multiplied into
//! the PNG.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::assets::BakedModel;
use crate::mesh::Group;
use crate::texture::{encode_png, AlphaClass, Rgba};

const NEAREST: u32 = 9728;
const CLAMP_TO_EDGE: u32 = 33071;

/// Percent-encodes a file name for a glTF `uri` (anything but unreserved characters)
fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Writes `model` (already grouped by [`crate::mesh::build`]) and returns the `.gltf` path
pub fn write(model: &BakedModel, groups: &[Group], name: &str, out_dir: &Path) -> Result<PathBuf> {
    if groups.is_empty() {
        bail!("{}: nothing to write (no geometry)", model.id);
    }
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut bin: Vec<u8> = Vec::new();
    let mut views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    let mut textures: Vec<Value> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    let mut primitives: Vec<Value> = Vec::new();
    // distinct images by content
    let mut image_ids: HashMap<(u32, u32, Vec<u8>), usize> = HashMap::new();

    for g in groups {
        let mut pos: Vec<f32> = Vec::new();
        let mut nor: Vec<f32> = Vec::new();
        let mut uvs: Vec<f32> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for q in &g.quads {
            let base = (pos.len() / 3) as u32;
            for i in 0..4 {
                pos.extend_from_slice(&q.pos[i]);
                nor.extend_from_slice(&q.normal);
                uvs.extend_from_slice(&q.uv[i]);
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in pos.chunks_exact(3) {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let mut add_view = |data: &[u8], target: u32| -> usize {
            pad4(&mut bin);
            let off = bin.len();
            bin.extend_from_slice(data);
            views.push(
                json!({"buffer": 0, "byteOffset": off, "byteLength": data.len(), "target": target}),
            );
            views.len() - 1
        };
        let f32s = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let pv = add_view(&f32s(&pos), 34962);
        let nv = add_view(&f32s(&nor), 34962);
        let uv = add_view(&f32s(&uvs), 34962);
        let iv = add_view(
            &idx.iter()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<u8>>(),
            34963,
        );
        let mut acc =
            |view: usize, count: usize, ty: &str, comp: u32, extra: Option<(Value, Value)>| {
                let mut a =
                    json!({"bufferView": view, "componentType": comp, "count": count, "type": ty});
                if let Some((mn, mx)) = extra {
                    a["min"] = mn;
                    a["max"] = mx;
                }
                accessors.push(a);
                accessors.len() - 1
            };
        let pa = acc(
            pv,
            pos.len() / 3,
            "VEC3",
            5126,
            Some((json!(min), json!(max))),
        );
        let na = acc(nv, nor.len() / 3, "VEC3", 5126, None);
        let ua = acc(uv, uvs.len() / 2, "VEC2", 5126, None);
        let ia = acc(iv, idx.len(), "SCALAR", 5125, None);

        let key = (g.image.width, g.image.height, g.image.data.clone());
        let image_index = match image_ids.get(&key) {
            Some(i) => *i,
            None => {
                let n = images.len();
                let file = format!("{name}_{n}.png");
                fs::write(
                    out_dir.join(&file),
                    encode_png(&Rgba {
                        width: g.image.width,
                        height: g.image.height,
                        data: g.image.data.clone(),
                    })?,
                )?;
                images.push(json!({"uri": uri_encode(&file)}));
                image_ids.insert(key, n);
                n
            }
        };
        textures.push(json!({"sampler": 0, "source": image_index}));
        let mut mat = json!({
            "name": match g.tint_index {
                Some(t) => format!("{}#tint{t}", &*g.texture),
                None => g.texture.to_string(),
            },
            "pbrMetallicRoughness": {
                "baseColorTexture": {"index": textures.len() - 1},
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0
            },
            "doubleSided": false
        });
        match g.alpha {
            AlphaClass::Opaque => {}
            AlphaClass::Mask => {
                mat["alphaMode"] = json!("MASK");
                mat["alphaCutoff"] = json!(0.5);
            }
            AlphaClass::Blend => mat["alphaMode"] = json!("BLEND"),
        }
        materials.push(mat);
        primitives.push(json!({
            "attributes": {"POSITION": pa, "NORMAL": na, "TEXCOORD_0": ua},
            "indices": ia,
            "material": materials.len() - 1,
            "extras": {
                "texture": &*g.texture,
                "tintindex": g.tint_index,
                "tint": g.tint.map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])),
            }
        }));
    }
    pad4(&mut bin);
    let bin_name = format!("{name}.bin");
    fs::write(out_dir.join(&bin_name), &bin)?;
    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "mc-unpack"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"name": name, "mesh": 0}],
        "meshes": [{"name": model.id, "primitives": primitives}],
        "buffers": [{"uri": uri_encode(&bin_name), "byteLength": bin.len()}],
        "bufferViews": views,
        "accessors": accessors,
        "samplers": [{"magFilter": NEAREST, "minFilter": NEAREST, "wrapS": CLAMP_TO_EDGE, "wrapT": CLAMP_TO_EDGE}],
    });
    if !materials.is_empty() {
        doc["materials"] = json!(materials);
        doc["textures"] = json!(textures);
        doc["images"] = json!(images);
    }
    let path = out_dir.join(format!("{name}.gltf"));
    fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
    Ok(path)
}

/// Writes a world's meshes as one glTF: a node per chunk column (translated to its place in the
/// world, metres = blocks, the game's axes), one mesh per column with a primitive per material.
/// Output is `<name>.gltf`, `<name>.bin` and `<name>_<n>.png` (one per distinct image); the binary is
/// written as it is produced, so the file is never held in memory next to the meshes.
pub fn write_world(
    mesh: &crate::world::mesher::WorldMesh,
    name: &str,
    out_dir: &Path,
) -> Result<PathBuf> {
    use std::io::Write;
    if mesh.chunks.is_empty() {
        bail!("{}: nothing to write (no geometry)", mesh.name);
    }
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let bin_name = format!("{name}.bin");
    let mut bin = std::io::BufWriter::new(
        fs::File::create(out_dir.join(&bin_name))
            .with_context(|| format!("creating {bin_name}"))?,
    );
    let mut offset: usize = 0;
    let mut views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut put =
        |bin: &mut std::io::BufWriter<fs::File>, data: &[u8], target: u32| -> Result<usize> {
            // every array here is of 4-byte elements, so views stay aligned
            bin.write_all(data)?;
            views.push(
            json!({"buffer": 0, "byteOffset": offset, "byteLength": data.len(), "target": target}),
        );
            offset += data.len();
            Ok(views.len() - 1)
        };

    // materials, textures, images: shared by the whole world
    let mut images: Vec<Value> = Vec::new();
    let mut image_of: HashMap<*const Rgba, usize> = HashMap::new();
    let mut textures: Vec<Value> = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    for m in &mesh.materials {
        let key = std::sync::Arc::as_ptr(&m.image);
        let img = match image_of.get(&key) {
            Some(&i) => i,
            None => {
                let n = images.len();
                let file = format!("{name}_{n}.png");
                fs::write(out_dir.join(&file), encode_png(&m.image)?)?;
                images.push(json!({"uri": uri_encode(&file)}));
                image_of.insert(key, n);
                n
            }
        };
        textures.push(json!({"sampler": 0, "source": img}));
        let mut mat = json!({
            "name": m.name(),
            "pbrMetallicRoughness": {
                "baseColorTexture": {"index": textures.len() - 1},
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0
            },
            "doubleSided": false
        });
        match m.alpha {
            AlphaClass::Opaque => {}
            AlphaClass::Mask => {
                mat["alphaMode"] = json!("MASK");
                mat["alphaCutoff"] = json!(0.5);
            }
            AlphaClass::Blend => mat["alphaMode"] = json!("BLEND"),
        }
        materials.push(mat);
    }

    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::new();
    for c in &mesh.chunks {
        let mut prims: Vec<Value> = Vec::new();
        for g in &c.groups {
            let n = g.quads();
            let mut pos = Vec::with_capacity(n * 4 * 12);
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            for p in &g.positions {
                for k in 0..3 {
                    min[k] = min[k].min(p[k]);
                    max[k] = max[k].max(p[k]);
                }
                for v in p {
                    pos.extend_from_slice(&v.to_le_bytes());
                }
            }
            let mut nor = Vec::with_capacity(n * 4 * 12);
            for q in &g.normals {
                for _ in 0..4 {
                    for v in q {
                        nor.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
            let mut uv = Vec::with_capacity(n * 4 * 8);
            for t in &g.uvs {
                for v in t {
                    uv.extend_from_slice(&v.to_le_bytes());
                }
            }
            let mut idx = Vec::with_capacity(n * 24);
            for i in 0..n as u32 {
                let b = i * 4;
                for k in [0, 1, 2, 0, 2, 3] {
                    idx.extend_from_slice(&(b + k).to_le_bytes());
                }
            }
            let pv = put(&mut bin, &pos, 34962)?;
            let nv = put(&mut bin, &nor, 34962)?;
            let uvv = put(&mut bin, &uv, 34962)?;
            let iv = put(&mut bin, &idx, 34963)?;
            let mut acc = |view: usize,
                           count: usize,
                           ty: &str,
                           comp: u32,
                           mm: Option<([f32; 3], [f32; 3])>| {
                let mut a =
                    json!({"bufferView": view, "componentType": comp, "count": count, "type": ty});
                if let Some((lo, hi)) = mm {
                    a["min"] = json!(lo);
                    a["max"] = json!(hi);
                }
                accessors.push(a);
                accessors.len() - 1
            };
            let pa = acc(pv, g.positions.len(), "VEC3", 5126, Some((min, max)));
            let na = acc(nv, g.positions.len(), "VEC3", 5126, None);
            let ua = acc(uvv, g.uvs.len(), "VEC2", 5126, None);
            let ia = acc(iv, n * 6, "SCALAR", 5125, None);
            prims.push(json!({
                "attributes": {"POSITION": pa, "NORMAL": na, "TEXCOORD_0": ua},
                "indices": ia,
                "material": g.material,
            }));
        }
        meshes.push(json!({"name": format!("chunk {},{}", c.cx, c.cz), "primitives": prims}));
        nodes.push(json!({
            "name": format!("chunk {},{}", c.cx, c.cz),
            "mesh": meshes.len() - 1,
            "translation": [c.cx as i64 * 16, 0, c.cz as i64 * 16],
        }));
    }
    bin.flush()?;
    drop(bin);
    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "mc-unpack"},
        "scene": 0,
        "scenes": [{"name": mesh.name, "nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes,
        "meshes": meshes,
        "buffers": [{"uri": uri_encode(&bin_name), "byteLength": offset}],
        "bufferViews": views,
        "accessors": accessors,
        "samplers": [{"magFilter": NEAREST, "minFilter": NEAREST, "wrapS": CLAMP_TO_EDGE, "wrapT": CLAMP_TO_EDGE}],
        "materials": materials,
        "textures": textures,
        "images": images,
    });
    if let Some(s) = mesh.spawn {
        doc["scenes"][0]["extras"] = json!({"spawn": s});
    }
    let path = out_dir.join(format!("{name}.gltf"));
    fs::write(&path, serde_json::to_vec(&doc)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::tiny;
    use crate::blockstate::State;
    use crate::mesh::{build, MeshOptions};

    #[test]
    fn writes_a_valid_looking_gltf() {
        let a = tiny();
        let b = a.bake_block("cube", &State::default()).unwrap();
        let groups = build(&a, &b, &MeshOptions::default()).unwrap();
        assert_eq!(groups.len(), 1);
        let dir = std::env::temp_dir().join(format!("mc-unpack-gltf-{}", std::process::id()));
        let p = write(&b, &groups, "cube", &dir).unwrap();
        let doc: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(doc["asset"]["version"], "2.0");
        assert_eq!(doc["samplers"][0]["magFilter"], NEAREST);
        assert_eq!(
            doc["meshes"][0]["primitives"][0]["extras"]["texture"],
            "block/t"
        );
        // the box is a unit cube on its bottom centre
        let acc = &doc["accessors"][0];
        assert_eq!(acc["count"], 24);
        assert_eq!(acc["min"], json!([-0.5, 0.0, -0.5]));
        assert_eq!(acc["max"], json!([0.5, 1.0, 0.5]));
        let bin_len = fs::metadata(dir.join("cube.bin")).unwrap().len();
        assert_eq!(doc["buffers"][0]["byteLength"], bin_len);
        assert!(dir.join("cube_0.png").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zero_groups_is_an_error_not_an_invalid_file() {
        let a = tiny();
        let b = a.bake_block("chest", &State::default()).unwrap();
        let dir = std::env::temp_dir().join(format!("mc-unpack-gltf0-{}", std::process::id()));
        assert!(write(&b, &[], "chest", &dir).is_err());
        assert!(!dir.join("chest.gltf").exists());
    }

    #[test]
    fn uris_are_percent_encoded() {
        assert_eq!(uri_encode("a b#c%d_é.png"), "a%20b%23c%25d_%C3%A9.png");
        let a = tiny();
        let b = a.bake_block("cube", &State::default()).unwrap();
        let groups = build(&a, &b, &MeshOptions::default()).unwrap();
        let dir = std::env::temp_dir().join(format!("mc-unpack-gltfu-{}", std::process::id()));
        let p = write(&b, &groups, "my model #1", &dir).unwrap();
        let doc: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(doc["buffers"][0]["uri"], "my%20model%20%231.bin");
        assert_eq!(doc["images"][0]["uri"], "my%20model%20%231_0.png");
        assert!(dir.join("my model #1.bin").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cutout_texture_gets_mask_and_tint_is_baked() {
        use crate::tint::{Tint, TintSet};
        let a = tiny();
        let b = a.bake_block("leaves", &State::default()).unwrap();
        let opts = MeshOptions {
            tints: TintSet::new().with(0, Tint::Rgb([128, 255, 0])),
            ..MeshOptions::default()
        };
        let groups = build(&a, &b, &opts).unwrap();
        assert_eq!(groups[0].alpha, AlphaClass::Mask);
        assert_eq!(groups[0].tint, Some([128, 255, 0]));
        // the green texel (0,200,0) times (128,255,0)/255
        assert_eq!(groups[0].image.pixel(0, 0), [0, 200, 0, 255]);
        let untinted = build(&a, &b, &MeshOptions::default()).unwrap();
        assert_eq!(untinted[0].tint, None);
        assert_eq!(untinted[0].tint_index, Some(0));
    }
}
