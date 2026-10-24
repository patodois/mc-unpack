//! Textures: PNG decoding, and the `.png.mcmeta` animation sidecar.
//!
//! A texture that has an `animation` section in its `.mcmeta` is a vertical (or tiled) sheet of
//! frames. The model UVs (0 to 16) always refer to **one frame**, so a model draws a frame, not the
//! sheet. [`Texture::frame`] cuts frame `i` of the sheet out; [`Texture::default_frame`] is the first
//! frame the animation shows.
//! The order and duration in which the game plays them is in [`Animation::frames`]; the frame a
//! model gets by default is the first of that list ([`Texture::default_frame`]).

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

/// Most pixels a PNG may have: 2048 x 2048. The largest PNG under `textures/` in the eight jars
/// the author has is 512 x 512 (a GUI image; block and item textures are 16 x 16 to 32 x 1024),
/// so this is 16 times that. It is checked from the header, before any pixel is decoded.
pub const MAX_PIXELS: u64 = 2048 * 2048;

/// Most frames a texture sheet may hold. The largest animated sheet in the jars has far fewer
/// (see `mc-unpack check`); this stops a 1 x 1 frame size on a big sheet from asking for millions.
pub const MAX_SHEET_FRAMES: u32 = 4096;

/// Most steps an animation's `frames` list may have
pub const MAX_FRAME_LIST: usize = 4096;

pub fn decode_png(bytes: &[u8]) -> Result<Rgba> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info()?;
    {
        let i = reader.info();
        let px = i.width as u64 * i.height as u64;
        if px == 0 || px > MAX_PIXELS {
            bail!(
                "PNG is {} x {} ({px} pixels); the limit is {MAX_PIXELS}",
                i.width,
                i.height
            );
        }
    }
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
    /// Listed frames that are not on the sheet and were dropped, as the game does
    pub skipped_frames: usize,
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

/// A whole number from JSON in `lo..=u32::MAX`; `90.0` counts as 90, a fraction does not
fn whole_u32(v: &Value, what: &str, lo: u32) -> Result<u32> {
    let n = match v.as_u64() {
        Some(n) => Some(n as f64),
        None => v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0),
    };
    match n {
        Some(n) if n >= lo as f64 && n <= u32::MAX as f64 => Ok(n as u32),
        _ => bail!("{what} must be a whole number from {lo} to {}", u32::MAX),
    }
}

impl McMeta {
    pub fn parse(text: impl AsRef<[u8]>) -> Result<McMeta> {
        let v: Value = crate::jar::parse_json(text.as_ref()).context("the .mcmeta is not JSON")?;
        let Some(a) = v.get("animation") else {
            return Ok(McMeta { animation: None });
        };
        let opt = |k: &str, lo: u32| -> Result<Option<u32>> {
            a.get(k)
                .map(|n| whole_u32(n, &format!("animation.{k}"), lo))
                .transpose()
        };
        // frametime, width and height are positive in the game; a section that breaks that is
        // not read at all
        let frametime = opt("frametime", 1)?.unwrap_or(1);
        let frames = match a.get("frames") {
            None => None,
            Some(Value::Array(list)) => {
                if list.len() > MAX_FRAME_LIST {
                    bail!(
                        "the animation lists {} frames; the limit is {MAX_FRAME_LIST}",
                        list.len()
                    );
                }
                let mut out = Vec::with_capacity(list.len());
                for f in list {
                    match f {
                        Value::Number(_) => out.push((whole_u32(f, "a frame index", 0)?, None)),
                        Value::Object(o) => {
                            let index = whole_u32(
                                o.get("index").context("a frame without an index")?,
                                "a frame index",
                                0,
                            )?;
                            let time = o
                                .get("time")
                                .map(|t| whole_u32(t, "a frame time", 1))
                                .transpose()?;
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
                interpolate: match a.get("interpolate") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => bail!("animation.interpolate is not true or false"),
                },
                frametime,
                width: opt("width", 1)?,
                height: opt("height", 1)?,
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
        if count > MAX_SHEET_FRAMES {
            bail!("the sheet holds {count} frames; the limit is {MAX_SHEET_FRAMES}");
        }
        let mut skipped = 0usize;
        let (frames, explicit) = match &a.frames {
            Some(list) => {
                if list.len() > MAX_FRAME_LIST {
                    bail!(
                        "the animation lists {} frames; the limit is {MAX_FRAME_LIST}",
                        list.len()
                    );
                }
                // The game drops a listed frame that is not on the sheet (and says so in its log)
                let mut out = Vec::with_capacity(list.len());
                for &(index, time) in list {
                    if index >= count {
                        skipped += 1;
                        continue;
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
        // With one frame or none left the game does not animate the sprite at all: it is the
        // sheet's first frame, whatever the one listed frame was
        let animation = (frames.len() > 1).then_some(Animation {
            interpolate: a.interpolate,
            frametime: a.frametime,
            width: a.width,
            height: a.height,
            frames,
            explicit_frames: explicit,
            skipped_frames: skipped,
        });
        Ok(Texture {
            frame_width: fw,
            frame_height: fh,
            sheet,
            animation,
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

    /// The sheet frame the animation shows first: the first entry of the `.mcmeta` `frames` list
    /// (which is not always frame 0: `fire_0` lists 16 first), else frame 0. This is what a model
    /// gets unless asked for another frame.
    pub fn default_frame_index(&self) -> u32 {
        self.animation
            .as_ref()
            .and_then(|a| a.frames.first())
            .map(|f| f.index)
            .unwrap_or(0)
    }

    pub fn default_frame(&self) -> Rgba {
        // A frame in the list was checked against the sheet when the texture was built
        self.frame(self.default_frame_index())
            .unwrap_or_else(|_| self.sheet.crop(0, 0, self.frame_width, self.frame_height))
    }

    /// The sheet frames the game shows, in sheet order: the distinct ones the animation lists, or
    /// just frame 0 for a texture it treats as static (no animation, or one frame left of it)
    pub fn shown_frames(&self) -> Vec<u32> {
        match &self.animation {
            Some(a) => {
                let mut v: Vec<u32> = a.frames.iter().map(|f| f.index).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            None => vec![0],
        }
    }

    /// The frames [`Texture::shown_frames`] names, cut out
    pub fn all_frames(&self) -> Vec<Rgba> {
        self.shown_frames()
            .into_iter()
            .filter_map(|i| self.frame(i).ok())
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
    fn frames_that_are_not_on_the_sheet_are_dropped_as_the_game_does() {
        // 2 frames; the list names 0, 7, 1: frame 7 is dropped and the other two animate
        let img = decode_png(&png(2, 4, |_, y| [y as u8, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(r#"{"animation":{"frames":[0,7,1]}}"#).unwrap();
        let t = Texture::new(img.clone(), Some(&meta)).unwrap();
        let a = t.animation.unwrap();
        assert_eq!(a.frames.iter().map(|f| f.index).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(a.skipped_frames, 1);
        // with one frame left the game does not animate at all: the texture is its first frame
        let meta = McMeta::parse(r#"{"animation":{"frames":[1,9]}}"#).unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        assert!(t.animation.is_none());
        assert_eq!(t.default_frame_index(), 0);
        assert_eq!(t.frame_height, 2);
    }

    #[test]
    fn numbers_in_a_mcmeta_may_be_written_16_0_but_not_out_of_range() {
        let m =
            McMeta::parse(r#"{"animation":{"width":16.0,"height":16,"frametime":2.0}}"#).unwrap();
        let a = m.animation.unwrap();
        assert_eq!((a.width, a.height, a.frametime), (Some(16), Some(16), 2));
        for bad in [
            r#"{"animation":{"width":16.5}}"#,
            r#"{"animation":{"width":0}}"#,
            r#"{"animation":{"frametime":-1}}"#,
            r#"{"animation":{"width":4294967296}}"#,
            r#"{"animation":{"frames":[-1]}}"#,
            r#"{"animation":{"frames":[{"index":0,"time":0}]}}"#,
            r#"{"animation":{"frames":[{"index":99999999999}]}}"#,
        ] {
            assert!(McMeta::parse(bad).is_err(), "{bad}");
        }
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

    #[test]
    fn a_png_over_the_pixel_limit_is_refused_from_its_header() {
        // 2100 x 2100 is 4.41 million pixels; it is refused before the pixels are inflated
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2100, 2100);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&vec![0u8; 2100 * 2100]).unwrap();
        }
        let e = decode_png(&out).unwrap_err().to_string();
        assert!(e.contains("limit"), "{e}");
        assert!(
            out.len() < 100_000,
            "the file is small ({} bytes) and still refused",
            out.len()
        );
    }

    #[test]
    fn the_default_frame_is_the_first_one_listed() {
        let img = decode_png(&png(2, 4, |_, y| [y as u8, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(r#"{"animation":{"frames":[1,0]}}"#).unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        assert_eq!(t.default_frame_index(), 1);
        assert_eq!(t.default_frame().pixel(0, 0), [2, 0, 0, 255]);
        let plain =
            Texture::new(decode_png(&png(2, 2, |_, _| [1, 1, 1, 255])).unwrap(), None).unwrap();
        assert_eq!(plain.default_frame_index(), 0);
    }

    #[test]
    fn frame_counts_are_bounded() {
        // a 1 x 1 frame size on a 128 x 128 sheet asks for 16,384 frames
        let img = decode_png(&png(128, 128, |_, _| [0, 0, 0, 255])).unwrap();
        let meta = McMeta::parse(r#"{"animation":{"width":1,"height":1}}"#).unwrap();
        assert!(Texture::new(img, Some(&meta))
            .unwrap_err()
            .to_string()
            .contains("limit"));
        let img = decode_png(&png(2, 2, |_, _| [0, 0, 0, 255])).unwrap();
        let list = format!(
            r#"{{"animation":{{"frames":[{}]}}}}"#,
            vec!["0"; MAX_FRAME_LIST + 1].join(",")
        );
        assert!(McMeta::parse(list)
            .unwrap_err()
            .to_string()
            .contains("limit"));
        let _ = img;
    }
}
