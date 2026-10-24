//! Which version a jar is, which of the formats it has, and what this crate does with it.
//!
//! The layout is decided from what the jar contains, not from a version number, so a jar whose
//! `version.json` is missing (everything before 18w47a, such as 1.12.2) still works. The version id
//! is read from `version.json` when there is one, else from the path (the launcher libraries name
//! it `minecraft-<id>-client.jar`, the vanilla launcher `versions/<id>/<id>.jar`).

use std::path::Path;

use crate::jar::Jar;

/// How the jar stores block and item art
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layout {
    /// Everything on a few atlas sheets (`terrain.png`); no loose textures, no JSON models.
    /// 1.2.5 is this. Unsupported.
    Atlas,
    /// Loose PNGs under `textures/blocks` and `textures/items`, but no JSON models (they arrived in
    /// 1.8). 1.7.10 is this. Textures only.
    LooseTextures,
    /// JSON blockstates and models (1.8 to 1.12.2). Textures live in `textures/blocks` and
    /// `textures/items`, a blockstate's `model` is relative to `models/block/`, and an item's model
    /// is `models/item/<name>.json`.
    LegacyModels,
    /// 1.13 to 1.21.3: `textures/block` and `textures/item`, a blockstate's `model` is relative to
    /// `models/`, an item's model is `models/item/<name>.json`.
    Models,
    /// 1.21.4 and later: as `Models`, and `items/<name>.json` says which model an item uses.
    ModelsWithItemDefs,
}

impl Layout {
    pub fn has_json_models(self) -> bool {
        self >= Layout::LegacyModels
    }
    pub fn has_loose_textures(self) -> bool {
        self >= Layout::LooseTextures
    }
    /// Folder under `assets/minecraft/textures/` that holds block textures
    pub fn block_texture_dir(self) -> &'static str {
        match self {
            Layout::LooseTextures | Layout::LegacyModels => "blocks",
            _ => "block",
        }
    }
    pub fn item_texture_dir(self) -> &'static str {
        match self {
            Layout::LooseTextures | Layout::LegacyModels => "items",
            _ => "item",
        }
    }
    /// Whether a blockstate's `model` is relative to `models/block/` rather than `models/`
    pub fn blockstate_models_are_block_relative(self) -> bool {
        self == Layout::LegacyModels
    }
    pub fn name(self) -> &'static str {
        match self {
            Layout::Atlas => "atlas sheets, no models",
            Layout::LooseTextures => "loose textures, no JSON models",
            Layout::LegacyModels => "JSON models, legacy names (1.8 to 1.12)",
            Layout::Models => "JSON models (1.13 to 1.21.3)",
            Layout::ModelsWithItemDefs => "JSON models and item definitions (1.21.4 and later)",
        }
    }
}

/// What a jar's `version.json` said, when it has one
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionJson {
    pub id: Option<String>,
    pub world_version: Option<i64>,
    /// The resource pack format, as written: a number, or `major.minor` in 26.x
    pub resource_pack: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VersionInfo {
    pub id: String,
    pub id_from: IdSource,
    pub layout: Layout,
    pub json: Option<VersionJson>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdSource {
    VersionJson,
    Path,
    Unknown,
}

/// What this crate does for a jar of this layout
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Full,
    TexturesOnly,
    Unsupported,
}

impl VersionInfo {
    pub fn support(&self) -> Support {
        match self.layout {
            Layout::Atlas => Support::Unsupported,
            Layout::LooseTextures => Support::TexturesOnly,
            _ => Support::Full,
        }
    }

    /// One line saying why a layout is limited
    pub fn support_note(&self) -> &'static str {
        match self.layout {
            Layout::Atlas => {
                "no JSON models and no loose textures: the models are in code and the art is on atlas sheets"
            }
            Layout::LooseTextures => "no JSON block models (they arrived in 1.8): textures only",
            Layout::LegacyModels => "blockstates, block models, item models (render layers are in code)",
            Layout::Models => "blockstates, block models, item models (render layers are in code)",
            Layout::ModelsWithItemDefs => {
                "blockstates, block models, item definitions and models (only `force_translucent` says a render layer)"
            }
        }
    }
}

pub fn detect_layout(jar: &Jar) -> Layout {
    let has = |prefix: &str| jar.names_under(prefix).next().is_some();
    if has("assets/minecraft/blockstates/") {
        if has("assets/minecraft/textures/blocks/") {
            Layout::LegacyModels
        } else if has("assets/minecraft/items/") {
            Layout::ModelsWithItemDefs
        } else {
            Layout::Models
        }
    } else if has("assets/minecraft/textures/blocks/") {
        Layout::LooseTextures
    } else {
        Layout::Atlas
    }
}

pub fn read_version_json(jar: &Jar) -> Option<VersionJson> {
    let v = crate::jar::parse_json(&jar.read_json("version.json").ok()?).ok()?;
    let id = v
        .get("id")
        .or_else(|| v.get("name"))
        .and_then(|s| s.as_str())
        .map(String::from);
    let world_version = v.get("world_version").and_then(|n| n.as_i64());
    let resource_pack = match v.get("pack_version") {
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        Some(serde_json::Value::Object(o)) => {
            let major = o.get("resource_major").or_else(|| o.get("resource"));
            let minor = o.get("resource_minor");
            match (major, minor) {
                (Some(a), Some(b)) => Some(format!("{a}.{b}")),
                (Some(a), None) => Some(a.to_string()),
                _ => None,
            }
        }
        _ => None,
    };
    Some(VersionJson {
        id,
        world_version,
        resource_pack,
    })
}

/// The version id a path suggests: `.../1.12.2/minecraft-1.12.2-client.jar` (launcher libraries) or
/// `.../versions/1.12.2/1.12.2.jar` (vanilla launcher). `None` when the name says nothing.
pub fn id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if let Some(rest) = stem.strip_prefix("minecraft-") {
        let id = rest.strip_suffix("-client").unwrap_or(rest);
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let dir = path.parent()?.file_name()?.to_str()?;
    if dir == stem {
        return Some(stem.to_string());
    }
    // `client.jar` inside a folder named after the version, and similar
    if looks_like_version(dir) {
        return Some(dir.to_string());
    }
    if looks_like_version(stem) {
        return Some(stem.to_string());
    }
    None
}

fn looks_like_version(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

pub fn detect(jar: &Jar, path: Option<&Path>) -> VersionInfo {
    let layout = detect_layout(jar);
    let json = read_version_json(jar);
    let (id, id_from) = match json.as_ref().and_then(|j| j.id.clone()) {
        Some(id) => (crate::jar::escape_control(&id), IdSource::VersionJson),
        None => match path.and_then(id_from_path) {
            Some(id) => (id, IdSource::Path),
            None => ("unknown".to_string(), IdSource::Unknown),
        },
    };
    VersionInfo {
        id,
        id_from,
        layout,
        json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jar::testjar::JarBuilder;

    #[test]
    fn layouts() {
        let atlas = JarBuilder::new().add("terrain.png", b"x").build();
        assert_eq!(detect_layout(&atlas), Layout::Atlas);
        let loose = JarBuilder::new()
            .add("assets/minecraft/textures/blocks/stone.png", b"x")
            .build();
        assert_eq!(detect_layout(&loose), Layout::LooseTextures);
        let legacy = JarBuilder::new()
            .add("assets/minecraft/blockstates/stone.json", b"{}")
            .add("assets/minecraft/textures/blocks/stone.png", b"x")
            .build();
        assert_eq!(detect_layout(&legacy), Layout::LegacyModels);
        let modern = JarBuilder::new()
            .add("assets/minecraft/blockstates/stone.json", b"{}")
            .add("assets/minecraft/textures/block/stone.png", b"x")
            .build();
        assert_eq!(detect_layout(&modern), Layout::Models);
        let defs = JarBuilder::new()
            .add("assets/minecraft/blockstates/stone.json", b"{}")
            .add("assets/minecraft/items/stone.json", b"{}")
            .build();
        assert_eq!(detect_layout(&defs), Layout::ModelsWithItemDefs);
    }

    #[test]
    fn ids() {
        let p = Path::new("/x/libraries/com/mojang/minecraft/1.20.1/minecraft-1.20.1-client.jar");
        assert_eq!(id_from_path(p).as_deref(), Some("1.20.1"));
        let p = Path::new("/x/versions/1.12.2/1.12.2.jar");
        assert_eq!(id_from_path(p).as_deref(), Some("1.12.2"));
        let p = Path::new("/x/downloads/client.jar");
        assert_eq!(id_from_path(p), None);
    }

    #[test]
    fn version_json_wins() {
        let jar = JarBuilder::new()
            .add(
                "version.json",
                br#"{"id":"26.3","world_version":5023,"pack_version":{"resource_major":97,"resource_minor":1}}"#,
            )
            .add("assets/minecraft/blockstates/a.json", b"{}")
            .build();
        let v = detect(&jar, Some(Path::new("/x/1.0/minecraft-1.0-client.jar")));
        assert_eq!(v.id, "26.3");
        assert_eq!(v.id_from, IdSource::VersionJson);
        assert_eq!(v.json.unwrap().resource_pack.as_deref(), Some("97.1"));
    }
}
