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

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::assets::BakedModel;
use crate::mesh::Group;
use crate::texture::{encode_png, AlphaClass, Rgba};

const NEAREST: u32 = 9728;
const CLAMP_TO_EDGE: u32 = 33071;

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Writes `model` (already grouped by [`crate::mesh::build`]) and returns the `.gltf` path
pub fn write(model: &BakedModel, groups: &[Group], name: &str, out_dir: &Path) -> Result<PathBuf> {
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
            views.push(json!({"buffer": 0, "byteOffset": off, "byteLength": data.len(), "target": target}));
            views.len() - 1
        };
        let f32s = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let pv = add_view(&f32s(&pos), 34962);
        let nv = add_view(&f32s(&nor), 34962);
        let uv = add_view(&f32s(&uvs), 34962);
        let iv = add_view(
            &idx.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>(),
            34963,
        );
        let mut acc = |view: usize, count: usize, ty: &str, comp: u32, extra: Option<(Value, Value)>| {
            let mut a = json!({"bufferView": view, "componentType": comp, "count": count, "type": ty});
            if let Some((mn, mx)) = extra {
                a["min"] = mn;
                a["max"] = mx;
            }
            accessors.push(a);
            accessors.len() - 1
        };
        let pa = acc(pv, pos.len() / 3, "VEC3", 5126, Some((json!(min), json!(max))));
        let na = acc(nv, nor.len() / 3, "VEC3", 5126, None);
        let ua = acc(uv, uvs.len() / 2, "VEC2", 5126, None);
        let ia = acc(iv, idx.len(), "SCALAR", 5125, None);

        let key = (g.image.width, g.image.height, g.image.data.clone());
        let image_index = match image_ids.get(&key) {
            Some(i) => *i,
            None => {
                let n = images.len();
                let file = format!("{name}_{n}.png");
                fs::write(out_dir.join(&file), encode_png(&Rgba {
                    width: g.image.width,
                    height: g.image.height,
                    data: g.image.data.clone(),
                })?)?;
                images.push(json!({"uri": file}));
                image_ids.insert(key, n);
                n
            }
        };
        textures.push(json!({"sampler": 0, "source": image_index}));
        let mut mat = json!({
            "name": match g.tint_index {
                Some(t) => format!("{}#tint{t}", g.texture),
                None => g.texture.clone(),
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
                "texture": g.texture,
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
        "buffers": [{"uri": bin_name, "byteLength": bin.len()}],
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
        assert_eq!(doc["meshes"][0]["primitives"][0]["extras"]["texture"], "block/t");
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
