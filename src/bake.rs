//! From elements to quads: the geometry rules of the model format.
//!
//! Model space is the game's: 1 block = 16 units, +X east, +Y up, +Z south (north is -Z), the
//! block occupying 0..16 on each axis. [`Quad`]s come out in **blocks** (0..1), still in that frame;
//! the bridge provider moves the origin to the bottom centre.
//!
//! The rules, each with a unit test:
//!
//! - **Face corners.** Each face has four corners listed top-left, bottom-left, bottom-right,
//!   top-right as seen from outside (counter-clockwise), with "up" being +Y on the sides, north on
//!   the up face and south on the down face. The face's UV rectangle `[u0, v0, u1, v1]` is mapped
//!   to them as `(u0,v0) (u0,v1) (u1,v1) (u1,v0)`; face `rotation` r shifts the assignment by r/90
//!   corners.
//! - **Default UV** (no `uv` given): derived from the element's extent, see [`default_uv`].
//! - **Element rotation** about an axis through `origin`, turning counter-clockwise looking down the
//!   axis toward the origin (the right-hand rule), then, with `rescale`, a stretch of the other two
//!   axes by 1 / cos(angle).
//! - **Variant rotation** `x` then `y` about the block centre: `y: 90` turns north to east
//!   (clockwise seen from above), `x: 90` turns up to north.
//! - **`uvlock`**: the texture stays aligned to the world axes while the block turns: a corner's UV
//!   is re-derived by laying it on the face plane, rotating it with the block, and reading it back
//!   in the UV frame of the face it ended up on.

use anyhow::{bail, Result};

use crate::model::{Axis, Dir, Element, ElementRotation, Face, ResolvedModel};

/// A quad in blocks, counter-clockwise from outside
#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    pub pos: [[f32; 3]; 4],
    pub normal: [f32; 3],
    /// 0..1 over one frame of the texture, v down
    pub uv: [[f32; 2]; 4],
    /// Relative to `textures/`: `block/stone`
    pub texture: String,
    pub tint_index: Option<u32>,
    /// The side this face hides against a neighbour (in the frame after the block is rotated)
    pub cullface: Option<Dir>,
    /// The direction the face was written for, before any rotation
    pub face: Dir,
    pub shade: bool,
    /// The data says this texture is drawn translucent (`force_translucent`, 26.x)
    pub translucent: bool,
    pub light_emission: u8,
}

/// The rotation a blockstate variant asks for
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VariantRotation {
    pub x: i32,
    pub y: i32,
    pub uvlock: bool,
}

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, c) in row.iter_mut().enumerate() {
            *c = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

fn apply(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Right-handed rotation by `deg` about an axis, with exact entries for multiples of 90
fn rot_axis(axis: Axis, deg: f64) -> M3 {
    let r = deg.rem_euclid(360.0);
    let (s, c) = if r == 0.0 {
        (0.0, 1.0)
    } else if r == 90.0 {
        (1.0, 0.0)
    } else if r == 180.0 {
        (0.0, -1.0)
    } else if r == 270.0 {
        (-1.0, 0.0)
    } else {
        let t = deg.to_radians();
        (t.sin(), t.cos())
    };
    match axis {
        Axis::X => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        Axis::Y => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        Axis::Z => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// The block rotation of a variant: `x` first, then `y`; positive `y` is clockwise seen from above
/// and positive `x` tips the top toward north, which are right-handed turns by `-y` and `-x`
fn variant_matrix(x: i32, y: i32) -> M3 {
    mul(&rot_axis(Axis::Y, -(y as f64)), &rot_axis(Axis::X, -(x as f64)))
}

/// Per direction, the four corners (top-left, bottom-left, bottom-right, top-right) as
/// `[x, y, z]` picks: `false` = the element's `from` on that axis, `true` = its `to`
const CORNERS: [(Dir, [[bool; 3]; 4]); 6] = [
    (
        Dir::Down,
        [
            [false, false, true],
            [false, false, false],
            [true, false, false],
            [true, false, true],
        ],
    ),
    (
        Dir::Up,
        [
            [false, true, false],
            [false, true, true],
            [true, true, true],
            [true, true, false],
        ],
    ),
    (
        Dir::North,
        [
            [true, true, false],
            [true, false, false],
            [false, false, false],
            [false, true, false],
        ],
    ),
    (
        Dir::South,
        [
            [false, true, true],
            [false, false, true],
            [true, false, true],
            [true, true, true],
        ],
    ),
    (
        Dir::West,
        [
            [false, true, false],
            [false, false, false],
            [false, false, true],
            [false, true, true],
        ],
    ),
    (
        Dir::East,
        [
            [true, true, true],
            [true, false, true],
            [true, false, false],
            [true, true, false],
        ],
    ),
];

fn corners_of(d: Dir) -> &'static [[bool; 3]; 4] {
    &CORNERS.iter().find(|(x, _)| *x == d).unwrap().1
}

/// The UV rectangle a face gets when the file gives none, from the element's `from` and `to`:
/// the projection of the element onto the face plane in the face's own frame
pub fn default_uv(d: Dir, from: V3, to: V3) -> [f64; 4] {
    match d {
        Dir::Down => [from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        Dir::Up => [from[0], from[2], to[0], to[2]],
        Dir::North => [16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        Dir::South => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        Dir::West => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        Dir::East => [16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
    }
}

/// The UV of a point (in 0..16 block units) on the plane of face `d`, in the face's standard frame
fn plane_uv(d: Dir, p: V3) -> [f64; 2] {
    match d {
        Dir::Down => [p[0], 16.0 - p[2]],
        Dir::Up => [p[0], p[2]],
        Dir::North => [16.0 - p[0], 16.0 - p[1]],
        Dir::South => [p[0], 16.0 - p[1]],
        Dir::West => [p[2], 16.0 - p[1]],
        Dir::East => [16.0 - p[2], 16.0 - p[1]],
    }
}

/// The inverse of [`plane_uv`]: the point of the block-boundary plane of face `d` with that UV
fn unplane_uv(d: Dir, uv: [f64; 2]) -> V3 {
    let (u, v) = (uv[0], uv[1]);
    match d {
        Dir::Down => [u, 0.0, 16.0 - v],
        Dir::Up => [u, 16.0, v],
        Dir::North => [16.0 - u, 16.0 - v, 0.0],
        Dir::South => [u, 16.0 - v, 16.0],
        Dir::West => [0.0, 16.0 - v, u],
        Dir::East => [16.0, 16.0 - v, 16.0 - u],
    }
}

fn element_matrix(r: &ElementRotation) -> (V3, M3, V3) {
    match r {
        ElementRotation::Axis {
            origin,
            axis,
            angle,
            rescale,
        } => {
            let m = rot_axis(*axis, *angle);
            let k = if *rescale {
                1.0 / angle.to_radians().cos().abs().max(1e-9)
            } else {
                1.0
            };
            let scale = match axis {
                Axis::X => [1.0, k, k],
                Axis::Y => [k, 1.0, k],
                Axis::Z => [k, k, 1.0],
            };
            (*origin, m, scale)
        }
        ElementRotation::Euler { origin, x, y, z } => {
            let m = mul(
                &mul(&rot_axis(Axis::X, *x), &rot_axis(Axis::Y, *y)),
                &rot_axis(Axis::Z, *z),
            );
            (*origin, m, [1.0; 3])
        }
    }
}

fn snap(v: f64) -> f32 {
    ((v * 1e7).round() / 1e7) as f32
}

/// Bakes one element into up to six quads. `resolve_tex` maps a face's texture reference to its
/// path and translucency.
pub fn bake_element(
    el: &Element,
    rot: VariantRotation,
    resolve_tex: &mut dyn FnMut(&str) -> Result<(String, bool)>,
    out: &mut Vec<Quad>,
) -> Result<()> {
    let vm = variant_matrix(rot.x, rot.y);
    let identity_variant = rot.x == 0 && rot.y == 0;
    let erot = el.rotation.as_ref().map(element_matrix);
    for (dir, face) in &el.faces {
        let (tex, translucent) = resolve_tex(&face.texture)?;
        let corner_pos = corners_of(*dir).map(|c| {
            [
                if c[0] { el.to[0] } else { el.from[0] },
                if c[1] { el.to[1] } else { el.from[1] },
                if c[2] { el.to[2] } else { el.from[2] },
            ]
        });
        // Positions: element rotation, then variant rotation about the block centre
        let mut pos = [[0.0; 3]; 4];
        for (i, p) in corner_pos.iter().enumerate() {
            let mut q = *p;
            if let Some((origin, m, scale)) = &erot {
                let r = apply(m, [q[0] - origin[0], q[1] - origin[1], q[2] - origin[2]]);
                q = [
                    origin[0] + r[0] * scale[0],
                    origin[1] + r[1] * scale[1],
                    origin[2] + r[2] * scale[2],
                ];
            }
            if !identity_variant {
                let r = apply(&vm, [q[0] - 8.0, q[1] - 8.0, q[2] - 8.0]);
                q = [r[0] + 8.0, r[1] + 8.0, r[2] + 8.0];
            }
            pos[i] = q;
        }
        // The face's direction after both rotations, for the normal and for uvlock
        let mut n = apply_i(*dir);
        if let Some((_, m, _)) = &erot {
            n = apply(m, n);
        }
        if !identity_variant {
            n = apply(&vm, n);
        }
        // UV per corner
        let rect = face.uv.unwrap_or_else(|| default_uv(*dir, el.from, el.to));
        let shift = (face.rotation / 90) as usize;
        let rc = [
            [rect[0], rect[1]],
            [rect[0], rect[3]],
            [rect[2], rect[3]],
            [rect[2], rect[1]],
        ];
        let mut uv = [[0.0; 2]; 4];
        for (i, u) in uv.iter_mut().enumerate() {
            *u = rc[(i + shift) % 4];
        }
        if rot.uvlock && !identity_variant {
            // Where did this face's direction end up (rounded to a whole axis)?
            let nd = variant_dir(&vm, *dir);
            for u in uv.iter_mut() {
                let on_plane = unplane_uv(*dir, *u);
                let r = apply(&vm, [on_plane[0] - 8.0, on_plane[1] - 8.0, on_plane[2] - 8.0]);
                *u = plane_uv(nd, [r[0] + 8.0, r[1] + 8.0, r[2] + 8.0]);
            }
        }
        // The normal from the corners where the quad has area, else from the rotated direction
        let e1 = sub(pos[1], pos[0]);
        let e2 = sub(pos[2], pos[1]);
        let c = cross(e1, e2);
        let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        let normal = if len > 1e-9 {
            [c[0] / len, c[1] / len, c[2] / len]
        } else {
            n
        };
        let cull = face.cullface.map(|d| variant_dir(&vm, d));
        out.push(Quad {
            pos: pos.map(|p| [snap(p[0] / 16.0), snap(p[1] / 16.0), snap(p[2] / 16.0)]),
            normal: [snap(normal[0]), snap(normal[1]), snap(normal[2])],
            uv: uv.map(|u| [snap(u[0] / 16.0), snap(u[1] / 16.0)]),
            texture: tex.clone(),
            tint_index: face.tintindex,
            cullface: if identity_variant { face.cullface } else { cull },
            face: *dir,
            shade: el.shade,
            translucent,
            light_emission: el.light_emission,
        });
    }
    Ok(())
}

fn apply_i(d: Dir) -> V3 {
    let n = d.normal();
    [n[0] as f64, n[1] as f64, n[2] as f64]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A direction after a block rotation (whole axes only)
fn variant_dir(vm: &M3, d: Dir) -> Dir {
    let r = apply(vm, apply_i(d));
    Dir::from_normal([r[0].round() as i32, r[1].round() as i32, r[2].round() as i32])
        .expect("a quarter turn keeps axes")
}

/// Bakes every element of a resolved model
pub fn bake_model(model: &ResolvedModel, rot: VariantRotation) -> Result<Vec<Quad>> {
    let mut out = Vec::new();
    for (i, el) in model.elements.iter().enumerate() {
        bake_element(
            el,
            rot,
            &mut |r| {
                let t = model.texture(r)?;
                Ok((t.path, t.translucent))
            },
            &mut out,
        )
        .map_err(|e| e.context(format!("{} element {i}", model.id)))?;
    }
    Ok(out)
}

/// Checks a face has a texture reference that can be followed (used by the sweep)
pub fn check_face(model: &ResolvedModel, face: &Face) -> Result<()> {
    if face.texture.is_empty() {
        bail!("empty texture reference");
    }
    model.texture(&face.texture).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(from: V3, to: V3, rotation: Option<ElementRotation>, faces: &[(Dir, Option<[f64; 4]>, i32)]) -> Element {
        Element {
            from,
            to,
            rotation,
            shade: true,
            light_emission: 0,
            faces: faces
                .iter()
                .map(|(d, uv, r)| {
                    (
                        *d,
                        Face {
                            uv: *uv,
                            texture: "t".into(),
                            cullface: None,
                            rotation: *r,
                            tintindex: None,
                        },
                    )
                })
                .collect(),
        }
    }

    fn bake(e: &Element, rot: VariantRotation) -> Vec<Quad> {
        let mut out = Vec::new();
        bake_element(e, rot, &mut |_| Ok(("t".into(), false)), &mut out).unwrap();
        out
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    const NONE: VariantRotation = VariantRotation { x: 0, y: 0, uvlock: false };

    #[test]
    fn every_face_of_a_cube_is_counter_clockwise_from_outside() {
        let faces: Vec<_> = Dir::ALL.iter().map(|d| (*d, None, 0)).collect();
        let q = bake(&el([0.0; 3], [16.0; 3], None, &faces), NONE);
        assert_eq!(q.len(), 6);
        for quad in &q {
            let n = quad.face.normal();
            assert!(close(quad.normal, [n[0] as f32, n[1] as f32, n[2] as f32]), "{:?}", quad.face);
            // the corners go round the face centre counter-clockwise about the outward normal
            let e1 = sub(quad.pos[1].map(|x| x as f64), quad.pos[0].map(|x| x as f64));
            let e2 = sub(quad.pos[2].map(|x| x as f64), quad.pos[1].map(|x| x as f64));
            let c = cross(e1, e2);
            let dot = c[0] * n[0] as f64 + c[1] * n[1] as f64 + c[2] * n[2] as f64;
            assert!(dot > 0.0, "{:?} is wound the wrong way", quad.face);
            // all four corners lie on the face's plane
            let k = |p: [f32; 3]| p[0] * n[0] as f32 + p[1] * n[1] as f32 + p[2] * n[2] as f32;
            let plane = k(quad.pos[0]);
            assert!(quad.pos.iter().all(|p| (k(*p) - plane).abs() < 1e-6));
            assert!(plane == 0.0 || plane == 1.0);
        }
    }

    #[test]
    fn default_uvs_follow_the_extent() {
        // a half-height slab: the side faces show the lower half of the texture
        let u = default_uv(Dir::North, [0.0, 0.0, 0.0], [16.0, 8.0, 16.0]);
        assert_eq!(u, [0.0, 8.0, 16.0, 16.0]);
        let u = default_uv(Dir::Up, [4.0, 0.0, 2.0], [12.0, 8.0, 10.0]);
        assert_eq!(u, [4.0, 2.0, 12.0, 10.0]);
        let u = default_uv(Dir::Down, [0.0, 0.0, 0.0], [16.0, 16.0, 8.0]);
        assert_eq!(u, [0.0, 8.0, 16.0, 16.0]);
    }

    #[test]
    fn texture_orientation_on_the_side_faces() {
        // on a south face, u grows with +X and v grows downward: the top-left corner is (0,16)
        let q = bake(&el([0.0; 3], [16.0; 3], None, &[(Dir::South, Some([0.0, 0.0, 16.0, 16.0]), 0)]), NONE);
        assert!(close(q[0].pos[0], [0.0, 1.0, 1.0]));
        assert_eq!(q[0].uv[0], [0.0, 0.0]);
        assert!(close(q[0].pos[2], [1.0, 0.0, 1.0]));
        assert_eq!(q[0].uv[2], [1.0, 1.0]);
        // on a north face u grows toward -X: its top-left corner is at x = 1
        let q = bake(&el([0.0; 3], [16.0; 3], None, &[(Dir::North, Some([0.0, 0.0, 16.0, 16.0]), 0)]), NONE);
        assert!(close(q[0].pos[0], [1.0, 1.0, 0.0]));
        assert_eq!(q[0].uv[0], [0.0, 0.0]);
        // on the up face u grows with +X and v with +Z
        let q = bake(&el([0.0; 3], [16.0; 3], None, &[(Dir::Up, Some([0.0, 0.0, 16.0, 16.0]), 0)]), NONE);
        assert!(close(q[0].pos[0], [0.0, 1.0, 0.0]));
        assert!(close(q[0].pos[2], [1.0, 1.0, 1.0]));
    }

    #[test]
    fn face_rotation_shifts_the_corner_assignment() {
        let f = |r| bake(&el([0.0; 3], [16.0; 3], None, &[(Dir::South, Some([0.0, 0.0, 16.0, 16.0]), r)]), NONE);
        // rotation 90: the texture's bottom-left lands on the top-left corner (clockwise quarter turn)
        assert_eq!(f(90)[0].uv[0], [0.0, 1.0]);
        assert_eq!(f(180)[0].uv[0], [1.0, 1.0]);
        assert_eq!(f(270)[0].uv[0], [1.0, 0.0]);
    }

    #[test]
    fn a_reversed_uv_rectangle_mirrors() {
        let q = bake(&el([0.0; 3], [16.0; 3], None, &[(Dir::South, Some([16.0, 0.0, 0.0, 16.0]), 0)]), NONE);
        assert_eq!(q[0].uv[0], [1.0, 0.0]);
        assert_eq!(q[0].uv[2], [0.0, 1.0]);
    }

    #[test]
    fn variant_y_turns_north_to_east() {
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::North, None, 0)]);
        let q = bake(&e, VariantRotation { x: 0, y: 90, uvlock: false });
        assert!(close(q[0].normal, [1.0, 0.0, 0.0]), "{:?}", q[0].normal);
        let q = bake(&e, VariantRotation { x: 0, y: 270, uvlock: false });
        assert!(close(q[0].normal, [-1.0, 0.0, 0.0]));
    }

    #[test]
    fn variant_x_turns_up_to_north_and_x_comes_first() {
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::Up, None, 0)]);
        let q = bake(&e, VariantRotation { x: 90, y: 0, uvlock: false });
        assert!(close(q[0].normal, [0.0, 0.0, -1.0]), "{:?}", q[0].normal);
        // a piston facing east: x 90 then y 90 takes up to north, then north to east
        let q = bake(&e, VariantRotation { x: 90, y: 90, uvlock: false });
        assert!(close(q[0].normal, [1.0, 0.0, 0.0]), "{:?}", q[0].normal);
        let q = bake(&e, VariantRotation { x: 180, y: 0, uvlock: false });
        assert!(close(q[0].normal, [0.0, -1.0, 0.0]));
    }

    #[test]
    fn variant_rotation_keeps_the_block_in_its_cell() {
        let faces: Vec<_> = Dir::ALL.iter().map(|d| (*d, None, 0)).collect();
        let e = el([0.0; 3], [16.0; 3], None, &faces);
        for x in [0, 90, 180, 270] {
            for y in [0, 90, 180, 270] {
                for q in bake(&e, VariantRotation { x, y, uvlock: false }) {
                    assert!(q.pos.iter().flatten().all(|c| *c == 0.0 || *c == 1.0), "x{x} y{y}: {:?}", q.pos);
                }
            }
        }
    }

    #[test]
    fn element_rotation_is_right_handed_about_its_axis() {
        // a point at +X of the origin, turned 90 degrees about Y, goes to -Z (counter-clockwise from above)
        let e = el(
            [12.0, 0.0, 8.0],
            [16.0, 4.0, 8.0],
            Some(ElementRotation::Axis { origin: [8.0, 8.0, 8.0], axis: Axis::Y, angle: 90.0, rescale: false }),
            &[(Dir::South, None, 0)],
        );
        let q = bake(&e, NONE);
        // the south face's bottom-left corner was (12, 0, 8): offset (4, -8, 0) -> (0, -8, -4) + origin
        assert!(close(q[0].pos[1], [0.5, 0.0, 0.25]), "{:?}", q[0].pos[1]);
    }

    #[test]
    fn rescale_keeps_a_diagonal_quad_corner_to_corner() {
        // the cross-model plane: x 0.8..15.2 at z = 8, turned 45 degrees about Y with rescale
        let e = el(
            [0.8, 0.0, 8.0],
            [15.2, 16.0, 8.0],
            Some(ElementRotation::Axis { origin: [8.0, 8.0, 8.0], axis: Axis::Y, angle: 45.0, rescale: true }),
            &[(Dir::North, None, 0), (Dir::South, None, 0)],
        );
        let q = bake(&e, NONE);
        for quad in &q {
            let xs: Vec<f32> = quad.pos.iter().map(|p| p[0]).collect();
            let zs: Vec<f32> = quad.pos.iter().map(|p| p[2]).collect();
            let (xmin, xmax) = (xs.iter().cloned().fold(9.0, f32::min), xs.iter().cloned().fold(-9.0, f32::max));
            let (zmin, zmax) = (zs.iter().cloned().fold(9.0, f32::min), zs.iter().cloned().fold(-9.0, f32::max));
            assert!((xmin - 0.05).abs() < 1e-4 && (xmax - 0.95).abs() < 1e-4, "{xmin} {xmax}");
            assert!((zmin - 0.05).abs() < 1e-4 && (zmax - 0.95).abs() < 1e-4, "{zmin} {zmax}");
        }
    }

    #[test]
    fn euler_rotation_composes_x_y_z() {
        // 180 about X then 180 about Z is a half turn about Y: (1,0,0) -> (-1,0,0)
        let e = el(
            [16.0, 0.0, 0.0],
            [16.0, 0.0, 0.0],
            Some(ElementRotation::Euler { origin: [0.0; 3], x: 180.0, y: 0.0, z: 180.0 }),
            &[(Dir::East, None, 0)],
        );
        let q = bake(&e, NONE);
        assert!(close(q[0].pos[0], [-1.0, 0.0, 0.0]), "{:?}", q[0].pos);
    }

    #[test]
    fn uvlock_keeps_the_texture_on_the_world_axes() {
        // a full top face with a full uv: with uvlock, a turn about Y leaves each corner's UV equal
        // to its world position's own (x, z) projection
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::Up, Some([0.0, 0.0, 16.0, 16.0]), 0)]);
        for y in [90, 180, 270] {
            let q = bake(&e, VariantRotation { x: 0, y, uvlock: true });
            for i in 0..4 {
                let p = q[0].pos[i];
                assert!(
                    (q[0].uv[i][0] - p[0]).abs() < 1e-5 && (q[0].uv[i][1] - p[2]).abs() < 1e-5,
                    "y{y} corner {i}: pos {:?} uv {:?}",
                    p,
                    q[0].uv[i]
                );
            }
            // without uvlock the texture turns with the block
            let q = bake(&e, VariantRotation { x: 0, y, uvlock: false });
            assert!((0..4).any(|i| (q[0].uv[i][0] - q[0].pos[i][0]).abs() > 1e-5 || (q[0].uv[i][1] - q[0].pos[i][2]).abs() > 1e-5));
        }
    }

    #[test]
    fn uvlock_on_a_side_face_that_becomes_another_side() {
        // a south face turned to the west by y = 90: locked UV equals the west plane's projection
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::South, Some([0.0, 0.0, 16.0, 16.0]), 0)]);
        let q = bake(&e, VariantRotation { x: 0, y: 90, uvlock: true });
        assert!(close(q[0].normal, [-1.0, 0.0, 0.0]));
        for i in 0..4 {
            let p = q[0].pos[i];
            // west face: u = z, v = 1 - y
            assert!((q[0].uv[i][0] - p[2]).abs() < 1e-5 && (q[0].uv[i][1] - (1.0 - p[1])).abs() < 1e-5);
        }
    }

    #[test]
    fn uvlock_with_the_identity_is_a_no_op() {
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::East, Some([2.0, 3.0, 9.0, 11.0]), 90)]);
        let a = bake(&e, NONE);
        let b = bake(&e, VariantRotation { x: 0, y: 0, uvlock: true });
        assert_eq!(a, b);
    }

    #[test]
    fn cullface_follows_the_rotation() {
        let mut e = el([0.0; 3], [16.0; 3], None, &[(Dir::North, None, 0)]);
        e.faces[0].1.cullface = Some(Dir::North);
        let q = bake(&e, VariantRotation { x: 0, y: 90, uvlock: false });
        assert_eq!(q[0].cullface, Some(Dir::East));
        assert_eq!(q[0].face, Dir::North);
    }

    #[test]
    fn flat_elements_keep_a_direction_normal() {
        // the redstone-dust plane: zero thickness, up and down faces only
        let e = el([0.0, 0.25, 0.0], [16.0, 0.25, 16.0], None, &[(Dir::Up, Some([0.0, 0.0, 16.0, 16.0]), 0), (Dir::Down, Some([0.0, 16.0, 16.0, 0.0]), 0)]);
        let q = bake(&e, NONE);
        assert!(close(q[0].normal, [0.0, 1.0, 0.0]));
        assert!(close(q[1].normal, [0.0, -1.0, 0.0]));
        assert!(q[0].pos.iter().all(|p| (p[1] - 0.015625).abs() < 1e-6));
    }

    #[test]
    fn unresolved_texture_is_an_error() {
        let e = el([0.0; 3], [16.0; 3], None, &[(Dir::Up, None, 0)]);
        let mut out = Vec::new();
        let r = bake_element(&e, NONE, &mut |_| bail!("no such texture"), &mut out);
        assert!(r.is_err());
    }
}
