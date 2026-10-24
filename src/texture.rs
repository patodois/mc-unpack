//! Textures: PNG decoding, and the `.png.mcmeta` animation sidecar.
//!
//! A texture that has an `animation` section in its `.mcmeta` is a vertical (or tiled) sheet of
//! frames. The model UVs (0 to 16) always refer to **one frame**, so a model draws a frame, not the
//! sheet. [`Texture::frame`] cuts frame `i` of the sheet out; [`Texture::default_frame`] is frame 0.
//! The order and duration in which the game plays them is in [`Animation::frames`].

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// 8-bit RGBA, rows top to bottom
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{})", self.width, self.height)
    }
}

impl Rgba {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    pub fn alpha(&self, x: u32, y: u32) -> u8 {
        self.data[((y * self.width + x) * 4 + 3) as usize]
    }

    /// Whether any texel is not fully opaque
    pub fn has_alpha(&self) -> bool {
        self.data.chunks_exact(4).any(|p| p[3] != 255)
    }

    /// Whether every texel is fully opaque or fully transparent
    pub fn alpha_is_binary(&self) -> bool {
        self.data.chunks_exact(4).all(|p| p[3] == 0 || p[3] == 255)
    }

    /// A rectangle of this image
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Rgba {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * self.width + x) * 4) as usize;
            data.extend_from_slice(&self.data[start..start + (w * 4) as usize]);
        }
        Rgba {
            width: w,
            height: h,
            data,
        }
    }

    /// Multiplies the colour channels by `rgb / 255`, as the game does with a tint (alpha is kept)
    pub fn tinted(&self, rgb: [u8; 3]) -> Rgba {
        let mut data = self.data.clone();
        for p in data.chunks_exact_mut(4) {
            for c in 0..3 {
                p[c] = ((p[c] as u32 * rgb[c] as u32 + 127) / 255) as u8;
            }
        }
        Rgba {
            width: self.width,
            height: self.height,
            data,
        }
    }
}

/// How a texture should be blended when drawn
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AlphaClass {
    /// Every texel is opaque: the same in any render layer
    Opaque,
    /// Texels are opaque or fully transparent: a cutout
    Mask,
    /// Some texel is partly transparent, or the data says the texture is drawn translucent
    Blend,
}

impl Rgba {
    /// The blend mode the *texels* call for. The game's render layer is not in the data (before
    /// 26.x not at all; in 26.x only as `force_translucent`), so this is what the pixels permit,
    /// not what the game does: an opaque texture is opaque in every layer, a binary-alpha texture
    /// is a cutout in the layers that have transparency, and a texture with partial alpha needs
    /// blending. `force_translucent` (from the data) wins.
    pub fn alpha_class(&self, force_translucent: bool) -> AlphaClass {
        if force_translucent {
            AlphaClass::Blend
        } else if !self.has_alpha() {
            AlphaClass::Opaque
        } else if self.alpha_is_binary() {
            AlphaClass::Mask
        } else {
            AlphaClass::Blend
        }
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<Rgba> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or_else(|| anyhow::anyhow!("PNG too large"))?
    ];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let px = (w as usize) * (h as usize);
    let data = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(px * 4);
            for p in buf.chunks_exact(3) {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(px * 4);
            for &g in &buf {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(px * 4);
            for p in buf.chunks_exact(2) {
                out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            out
        }
        png::ColorType::Indexed => bail!("palette PNG was not expanded"),
    };
    if data.len() != px * 4 {
        bail!("PNG decoded to {} bytes for {w}x{h}", data.len());
    }
    Ok(Rgba {
        width: w,
        height: h,
        data,
    })
}

pub fn encode_png(img: &Rgba) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&img.data)?;
    }
    Ok(out)
}

/// One step of an animation: which frame of the sheet, for how many game ticks (1/20 s)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimFrame {
    pub index: u32,
    pub ticks: u32,
}

/// The `animation` section of a `.mcmeta`
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub interpolate: bool,
    /// The default number of ticks per frame
    pub frametime: u32,
    /// Frame size as the file gives it (`None`: derived from the image)
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The order and timing of frames. When the file lists none, every frame of the sheet in order.
    pub frames: Vec<AnimFrame>,
    /// Whether the file listed its frames (false: `frames` was filled in from the sheet)
    pub explicit_frames: bool,
}

/// What a `.mcmeta` says that this crate uses
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McMeta {
    pub animation: Option<RawAnimation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawAnimation {
    pub interpolate: bool,
    pub frametime: u32,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frames: Option<Vec<(u32, Option<u32>)>>,
}

impl McMeta {
    pub fn parse(text: &str) -> Result<McMeta> {
        let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .context("the .mcmeta is not JSON")?;
        let Some(a) = v.get("animation") else {
            return Ok(McMeta { animation: None });
        };
        let num = |k: &str| a.get(k).and_then(|n| n.as_u64()).map(|n| n as u32);
        let frametime = num("frametime").unwrap_or(1);
        if frametime == 0 {
            bail!("animation.frametime is 0");
        }
        let frames = match a.get("frames") {
            None => None,
            Some(Value::Array(list)) => {
                let mut out = Vec::with_capacity(list.len());
                for f in list {
                    match f {
                        Value::Number(n) => {
                            out.push((n.as_u64().context("frame index")? as u32, None))
                        }
                        Value::Object(o) => {
                            let index = o
                                .get("index")
                                .and_then(|n| n.as_u64())
                                .context("frame without an index")?
                                as u32;
                            let time = o.get("time").and_then(|n| n.as_u64()).map(|n| n as u32);
                            out.push((index, time));
                        }
                        _ => bail!("a frame is neither a number nor an object"),
                    }
                }
                Some(out)
            }
            Some(_) => bail!("animation.frames is not a list"),
        };
        Ok(McMeta {
            animation: Some(RawAnimation {
                interpolate: a
                    .get("interpolate")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false),
                frametime,
                width: num("width"),
                height: num("height"),
                frames,
            }),
        })
    }
}

/// A decoded texture and, if it has one, its animation
#[derive(Clone, Debug)]
pub struct Texture {
    /// The whole PNG: for an animated texture, the sheet of frames
    pub sheet: Rgba,
    pub frame_width: u32,
    pub frame_height: u32,
    pub animation: Option<Animation>,
}

impl Texture {
    pub fn new(sheet: Rgba, meta: Option<&McMeta>) -> Result<Texture> {
        let raw = meta.and_then(|m| m.animation.as_ref());
        let Some(a) = raw else {
            return Ok(Texture {
                frame_width: sheet.width,
                frame_height: sheet.height,
                sheet,
                animation: None,
            });
        };
        // The game's rule: a given width or height is used; the missing one is the image's own
        // size on that axis if the other was given, else both are the shorter side
        let (fw, fh) = match (a.width, a.height) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, sheet.height),
            (None, Some(h)) => (sheet.width, h),
            (None, None) => {
                let s = sheet.width.min(sheet.height);
                (s, s)
            }
        };
        if fw == 0 || fh == 0 || sheet.width % fw != 0 || sheet.height % fh != 0 {
            bail!(
                "frame size {fw}x{fh} does not divide the {}x{} sheet",
                sheet.width,
                sheet.height
            );
        }
        let count = (sheet.width / fw) * (sheet.height / fh);
        let (frames, explicit) = match &a.frames {
            Some(list) => {
                let mut out = Vec::with_capacity(list.len());
                for &(index, time) in list {
                    if index >= count {
                        bail!("frame {index} is past the {count} frames of the sheet");
                    }
                    out.push(AnimFrame {
                        index,
                        ticks: time.unwrap_or(a.frametime),
                    });
                }
                (out, true)
            }
            None => (
                (0..count)
                    .map(|index| AnimFrame {
                        index,
                        ticks: a.frametime,
                    })
                    .collect(),
                false,
            ),
        };
        Ok(Texture {
            frame_width: fw,
            frame_height: fh,
            sheet,
            animation: Some(Animation {
                interpolate: a.interpolate,
                frametime: a.frametime,
                width: a.width,
                height: a.height,
                frames,
                explicit_frames: explicit,
            }),
        })
    }

    /// How many frames the sheet holds (1 for a plain texture)
    pub fn sheet_frames(&self) -> u32 {
        (self.sheet.width / self.frame_width) * (self.sheet.height / self.frame_height)
    }

    /// Frame `i` of the sheet, counted left to right, top to bottom
    pub fn frame(&self, i: u32) -> Result<Rgba> {
        if i >= self.sheet_frames() {
            bail!("frame {i}: the sheet has {}", self.sheet_frames());
        }
        let cols = self.sheet.width / self.frame_width;
        let (cx, cy) = (i % cols, i / cols);
        Ok(self.sheet.crop(
            cx * self.frame_width,
            cy * self.frame_height,
            self.frame_width,
            self.frame_height,
        ))
    }

    /// Frame 0 of the sheet, which is what a model gets unless asked for another
    pub fn default_frame(&self) -> Rgba {
        self.frame(0).expect("a texture has frame 0")
    }

    /// Every frame, in sheet order
    pub fn all_frames(&self) -> Vec<Rgba> {
        (0..self.sheet_frames())
            .map(|i| self.frame(i).unwrap())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jar::testjar::png;

    #[test]
    fn plain_texture_is_one_frame() {
        let img = decode_png(&png(16, 16, |x, _| [x as u8, 0, 0, 255])).unwrap();
        let t = Texture::new(img, None).unwrap();
        assert_eq!(t.sheet_frames(), 1);
        assert_eq!(t.default_frame().width, 16);
        assert!(t.animation.is_none());
    }

    #[test]
    fn animated_sheet_is_cut_into_frames() {
        // 4 x 12: three 4 x 4 frames, red, green, blue
        let img = decode_png(&png(4, 12, |_, y| match y / 4 {
            0 => [255, 0, 0, 255],
            1 => [0, 255, 0, 255],
            _ => [0, 0, 255, 255],
        }))
        .unwrap();
        let meta = McMeta::parse(r#"{"animation":{"frametime":3}}"#).unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        assert_eq!(t.frame_height, 4);
        assert_eq!(t.sheet_frames(), 3);
        assert_eq!(t.default_frame().pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(t.frame(2).unwrap().pixel(3, 3), [0, 0, 255, 255]);
        let a = t.animation.as_ref().unwrap();
        assert!(!a.explicit_frames);
        assert_eq!(a.frames.len(), 3);
        assert_eq!(a.frames[1], AnimFrame { index: 1, ticks: 3 });
        assert!(t.frame(3).is_err());
    }

    #[test]
    fn explicit_frames_with_times() {
        let img = decode_png(&png(2, 4, |_, y| [y as u8 * 50, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(
            r#"{"animation":{"interpolate":true,"frametime":2,"frames":[1,{"index":0,"time":7},1]}}"#,
        )
        .unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        let a = t.animation.unwrap();
        assert!(a.interpolate && a.explicit_frames);
        assert_eq!(
            a.frames,
            vec![
                AnimFrame { index: 1, ticks: 2 },
                AnimFrame { index: 0, ticks: 7 },
                AnimFrame { index: 1, ticks: 2 }
            ]
        );
    }

    #[test]
    fn bad_frame_is_an_error() {
        let img = decode_png(&png(2, 4, |_, _| [0, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(r#"{"animation":{"frames":[5]}}"#).unwrap();
        assert!(Texture::new(img, Some(&meta)).is_err());
    }

    #[test]
    fn meta_without_animation() {
        let m = McMeta::parse(r#"{"texture":{"mipmap_strategy":"mean"}}"#).unwrap();
        assert!(m.animation.is_none());
    }

    #[test]
    fn explicit_width_and_height() {
        let img = decode_png(&png(8, 4, |_, _| [0, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(r#"{"animation":{"width":4,"height":4}}"#).unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        assert_eq!(t.sheet_frames(), 2);
    }

    #[test]
    fn tint_multiplies_like_the_game() {
        let img = Rgba {
            width: 1,
            height: 1,
            data: vec![200, 100, 50, 128],
        };
        let t = img.tinted([255, 128, 0]);
        assert_eq!(t.data, vec![200, 50, 0, 128]);
    }

    #[test]
    fn png_round_trip() {
        let img = decode_png(&png(3, 2, |x, y| [x as u8, y as u8, 9, 200])).unwrap();
        let again = decode_png(&encode_png(&img).unwrap()).unwrap();
        assert_eq!(img, again);
    }
}
