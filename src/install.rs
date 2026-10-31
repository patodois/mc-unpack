//! Where client jars and saves live on a machine.
//!
//! Launcher folders are searched, read only, for the jars of the game and for `saves` folders:
//!
//! | Launcher | Folder (macOS / Linux / Windows) |
//! |----------|----------------------------------|
//! | the vanilla launcher | `~/Library/Application Support/minecraft` / `~/.minecraft` (and Flatpak) / `%APPDATA%\.minecraft` |
//! | Prism Launcher | `~/Library/Application Support/PrismLauncher` / `$XDG_DATA_HOME/PrismLauncher` (and Flatpak) / `%APPDATA%\PrismLauncher` |
//! | MultiMC | `~/Library/Application Support/MultiMC` / `$XDG_DATA_HOME/multimc` / none (it lives where it was unzipped) |
//! | Modrinth App | `.../ModrinthApp` (and `com.modrinth.theseus`, its name before 0.8) in the same three places |
//! | ATLauncher | `~/Library/Application Support/ATLauncher` or inside the app / `$XDG_DATA_HOME/atlauncher` (and Flatpak) / `%APPDATA%\ATLauncher` |
//! | GDLauncher | `gdlauncher_carbon` (and `gdlauncher_next`) in the same three places |
//! | CurseForge app | `~/Documents/curseforge/minecraft` / the same / `%USERPROFILE%\curseforge\minecraft` |
//!
//! Inside a launcher folder the jar is wherever that launcher keeps it; all of these are tried in
//! every folder, because the launchers share layouts:
//!
//! - `versions/<v>/<v>.jar`: the vanilla launcher, the CurseForge app (its `Install` folder), the
//!   Modrinth App (inside its `meta` folder), and a mod loader's profile, `versions/<v>-<loader>/`;
//! - `libraries/com/mojang/minecraft/<v>/minecraft-<v>-client.jar`: Prism Launcher and MultiMC;
//! - `libraries/net/minecraft/client/<v>/client-<v>.jar`: ATLauncher and GDLauncher;
//! - the path `meta/net.minecraft/<v>.json` names as its `mainJar` (Prism Launcher and MultiMC).
//!
//! Worlds are in `saves` folders: the vanilla launcher's own, and one per instance
//! (`instances/<name>/minecraft/saves` in Prism and MultiMC, `profiles/<name>/saves` in the Modrinth
//! App, `instances/<name>/saves` in ATLauncher, `instances/<name>/instance/saves` in GDLauncher,
//! `Instances/<name>/saves` in the CurseForge app).
//!
//! Environment: `MC_UNPACK_PRISM_DIR` and `MC_UNPACK_MINECRAFT_DIR` replace the folders of those two
//! launchers; `MC_UNPACK_LAUNCHER_DIR` adds folders (separated like `PATH`) of any launcher, for one
//! kept somewhere else (MultiMC on macOS and Windows has no default). A folder inside a launcher's
//! `instances` stands for the launcher folder above it, where the shared libraries are.
//! `MC_UNPACK_JAR` adds a jar. Only the macOS Prism layout has been tried by the author, and nothing
//! here was tried on Linux or Windows; the other layouts are tested on folders made for the purpose,
//! and each path says below how it is known.
//!
//! Layout facts collected with help from Horizon's launcher (<https://github.com/horizon-rust/horizon>,
//! MIT, Copyright (c) 2026 Ludovic Marie and the Horizon contributors), which keeps a list of where
//! launchers put their files. The code is this crate's own. Each launcher's own public documentation
//! was checked where it says something (cited at the folder, in `known_folders`); a path marked
//! "Horizon only" is asserted by Horizon and by nothing else this crate could find.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Prism,
    Vanilla,
    Explicit,
    MultiMc,
    Modrinth,
    AtLauncher,
    GdLauncher,
    CurseForge,
    /// A folder named in `MC_UNPACK_LAUNCHER_DIR`: any launcher's, its layout found by looking
    Folder,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Prism => "Prism Launcher",
            Source::Vanilla => "vanilla launcher",
            Source::Explicit => "given path",
            Source::MultiMc => "MultiMC",
            Source::Modrinth => "Modrinth App",
            Source::AtLauncher => "ATLauncher",
            Source::GdLauncher => "GDLauncher",
            Source::CurseForge => "CurseForge app",
            Source::Folder => "launcher folder",
        }
    }
}

#[derive(Clone, Debug)]
pub struct JarFile {
    /// The version the folder name gives; the jar's own `version.json` can say better
    pub version: String,
    pub path: PathBuf,
    pub source: Source,
}

/// A launcher's folder (it may not exist) and which launcher it is for
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherDir {
    pub source: Source,
    pub dir: PathBuf,
}

/// Where in a launcher folder to look. The launchers share enough that every folder is searched
/// with the layout of its launcher, and `Folder` with all of them together.
struct Layout {
    /// Folders, relative to the launcher folder, that hold `versions` and `libraries`
    jar_bases: &'static [&'static str],
    /// Folders that hold one folder per instance
    instance_dirs: &'static [&'static str],
    /// Inside an instance, the folders that hold `saves` ("" is the instance itself)
    game_dirs: &'static [&'static str],
}

fn layout(source: Source) -> Layout {
    match source {
        // Prism's game folder is `minecraft`, MultiMC's `.minecraft` in older versions
        Source::Prism | Source::MultiMc => Layout {
            jar_bases: &[""],
            instance_dirs: &["instances"],
            game_dirs: &["minecraft", ".minecraft"],
        },
        // `meta/versions` and `meta/libraries`; the profiles are the game folders
        Source::Modrinth => Layout {
            jar_bases: &["meta"],
            instance_dirs: &["profiles"],
            game_dirs: &[""],
        },
        // the instance folder is the game folder
        Source::AtLauncher => Layout {
            jar_bases: &[""],
            instance_dirs: &["instances"],
            game_dirs: &[""],
        },
        // the runtime path is `data` inside the app data folder; Carbon's game folder is `instance`
        Source::GdLauncher => Layout {
            jar_bases: &["", "data"],
            instance_dirs: &["instances", "data/instances"],
            game_dirs: &["instance", ""],
        },
        // the modding folder holds `Install` (the official launcher's folder) and `Instances`
        Source::CurseForge => Layout {
            jar_bases: &["Install"],
            instance_dirs: &["Instances"],
            game_dirs: &[""],
        },
        Source::Vanilla | Source::Explicit => Layout {
            jar_bases: &[""],
            instance_dirs: &[],
            game_dirs: &[],
        },
        Source::Folder => Layout {
            jar_bases: &["", "meta", "data", "Install"],
            instance_dirs: &["instances", "profiles", "Instances", "data/instances"],
            game_dirs: &["minecraft", ".minecraft", "instance", ""],
        },
    }
}

/// The folders the environment names the user's own data in
struct Places {
    home: Option<PathBuf>,
    /// `%APPDATA%`
    appdata: Option<PathBuf>,
    /// `$XDG_DATA_HOME`, else `~/.local/share`
    data: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`, else `~/.config`
    config: Option<PathBuf>,
}

impl Places {
    fn from_env() -> Places {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = var("HOME").or_else(|| var("USERPROFILE"));
        let under_home =
            |k: &str, rest: &str| var(k).or_else(|| home.as_ref().map(|h| h.join(rest)));
        Places {
            appdata: var("APPDATA"),
            data: under_home("XDG_DATA_HOME", ".local/share"),
            config: under_home("XDG_CONFIG_HOME", ".config"),
            home,
        }
    }
}

/// Every launcher's default folders, whether they exist or not. Paths of every system are listed
/// (those of another system do not exist, so nothing is found there).
fn known_folders(p: &Places) -> Vec<LauncherDir> {
    let mut out: Vec<LauncherDir> = Vec::new();
    let mac = p
        .home
        .as_ref()
        .map(|h| h.join("Library/Application Support"));
    let flatpak = |app: &str| p.home.as_ref().map(|h| h.join(".var/app").join(app));
    // a folder, once
    fn add(out: &mut Vec<LauncherDir>, source: Source, dir: PathBuf) {
        let l = LauncherDir { source, dir };
        if !out.contains(&l) {
            out.push(l);
        }
    }
    // the same name under each place a launcher may keep its data
    fn under(out: &mut Vec<LauncherDir>, source: Source, name: &str, places: &[&Option<PathBuf>]) {
        for place in places.iter().copied().flatten() {
            add(out, source, place.join(name));
        }
    }

    // The official launcher. macOS and Windows: minecraft.wiki "Minecraft Launcher" (`.minecraft`
    // in %APPDATA%, `minecraft` in Application Support), Linux `~/.minecraft`; the Flatpak one is
    // Horizon only.
    under(&mut out, Source::Vanilla, "minecraft", &[&mac]);
    under(
        &mut out,
        Source::Vanilla,
        ".minecraft",
        &[&p.home, &p.appdata],
    );
    if let Some(f) = flatpak("com.mojang.Minecraft") {
        add(&mut out, Source::Vanilla, f.join(".minecraft"));
    }

    // Prism Launcher: its documentation, "Data location" (prismlauncher.org/wiki), lists these, the
    // Flatpak one included.
    under(
        &mut out,
        Source::Prism,
        "PrismLauncher",
        &[&mac, &p.data, &p.appdata],
    );
    if let Some(f) = flatpak("org.prismlauncher.PrismLauncher") {
        add(&mut out, Source::Prism, f.join("data/PrismLauncher"));
    }

    // MultiMC has no fixed folder on macOS and Windows (it lives where it was unzipped; its wiki,
    // "Folder Structure", says so and names `~/.config/local/multimc` for Linux and macOS, which
    // looks like a typo). `multimc` in the data folder and `MultiMC` in Application Support are
    // Horizon only.
    under(&mut out, Source::MultiMc, "MultiMC", &[&mac]);
    under(&mut out, Source::MultiMc, "multimc", &[&p.data]);
    if let Some(h) = &p.home {
        add(&mut out, Source::MultiMc, h.join(".config/local/multimc"));
    }

    // Modrinth App: its support article "Modrinth App storage folder location" gives `%AppData%`,
    // Application Support and `$XDG_DATA_HOME` (usually `~/.local/share`) and says the folder was
    // `com.modrinth.theseus` before 0.8. A GitHub issue and the support article's older text put
    // Linux in `$XDG_CONFIG_HOME`, so that is tried too. The Flatpak one is Horizon only.
    for name in ["ModrinthApp", "com.modrinth.theseus"] {
        under(
            &mut out,
            Source::Modrinth,
            name,
            &[&mac, &p.data, &p.config, &p.appdata],
        );
    }
    if let Some(f) = flatpak("com.modrinth.ModrinthApp") {
        add(&mut out, Source::Modrinth, f.join("data/ModrinthApp"));
    }

    // ATLauncher keeps its files in its working folder (the launcher's FileSystem.java takes
    // `user.dir`, the folder holding the jar on Linux): `%APPDATA%\ATLauncher` from its Windows
    // installer, inside the macOS app, and the folders its Linux packages and Flatpak start it in.
    // Which folder each of those is comes from Horizon only.
    under(
        &mut out,
        Source::AtLauncher,
        "ATLauncher",
        &[&mac, &p.appdata],
    );
    under(&mut out, Source::AtLauncher, "atlauncher", &[&p.data]);
    let app = Path::new("Applications/ATLauncher.app/Contents/Java");
    add(&mut out, Source::AtLauncher, Path::new("/").join(app));
    if let Some(h) = &p.home {
        add(&mut out, Source::AtLauncher, h.join(app));
    }
    if let Some(f) = flatpak("com.atlauncher.ATLauncher") {
        add(&mut out, Source::AtLauncher, f.join("data/atlauncher"));
        add(&mut out, Source::AtLauncher, f.join("data"));
    }

    // GDLauncher: its documentation, "Runtime path vs app data", gives `gdlauncher_carbon` in
    // %APPDATA%, Application Support and `$XDG_DATA_HOME`. `gdlauncher_next` (the version before
    // Carbon) is Horizon only.
    under(
        &mut out,
        Source::GdLauncher,
        "gdlauncher_carbon",
        &[&mac, &p.data, &p.appdata],
    );
    under(
        &mut out,
        Source::GdLauncher,
        "gdlauncher_next",
        &[&mac, &p.config, &p.appdata],
    );

    // The CurseForge app: its support article "Getting Started" names `C:\Users\<user>\curseforge\minecraft`
    // and `~/Documents/curseforge/minecraft`; its `Install` folder is Horizon only.
    if let Some(h) = &p.home {
        add(
            &mut out,
            Source::CurseForge,
            h.join("Documents/curseforge/minecraft"),
        );
        add(&mut out, Source::CurseForge, h.join("curseforge/minecraft"));
    }
    out
}

/// The launcher folders to search: the defaults, those the environment replaces or adds
/// (`MC_UNPACK_PRISM_DIR`, `MC_UNPACK_MINECRAFT_DIR`, `MC_UNPACK_LAUNCHER_DIR`), and for each the
/// launcher folder above it when it is inside one's `instances`
pub fn launcher_dirs() -> Vec<LauncherDir> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let mut v = known_folders(&Places::from_env());
    let mut replace = |source: Source, with: Option<std::ffi::OsString>| {
        if let Some(w) = with {
            v.retain(|l| l.source != source);
            v.push(LauncherDir {
                source,
                dir: PathBuf::from(w),
            });
        }
    };
    replace(Source::Prism, var("MC_UNPACK_PRISM_DIR"));
    replace(Source::Vanilla, var("MC_UNPACK_MINECRAFT_DIR"));
    if let Some(extra) = var("MC_UNPACK_LAUNCHER_DIR") {
        for d in std::env::split_paths(&extra).filter(|d| !d.as_os_str().is_empty()) {
            v.push(LauncherDir {
                source: Source::Folder,
                dir: d,
            });
        }
    }
    with_roots(v)
}

/// Each folder inside a launcher's instances is preceded by the launcher folder above it
fn with_roots(dirs: Vec<LauncherDir>) -> Vec<LauncherDir> {
    let mut out: Vec<LauncherDir> = Vec::new();
    for l in dirs {
        // the launcher folder first: it finds the instance under its name
        let up = launcher_root(&l.dir)
            .filter(|r| *r != l.dir)
            .map(|dir| LauncherDir {
                source: l.source,
                dir,
            });
        for x in up.into_iter().chain([l]) {
            if !out.contains(&x) {
                out.push(x);
            }
        }
    }
    out
}

/// The launcher folder above `dir` when `dir` is an instance folder (or the folder of the instances,
/// or inside one): the closest `instances`, `Instances` or `profiles` above it whose parent holds
/// what a launcher folder holds (`libraries`, `meta`, `Install`). The shared libraries are there,
/// two or three folders above what a person picks.
pub fn launcher_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .filter(|a| {
            matches!(
                a.file_name().and_then(|n| n.to_str()),
                Some("instances" | "Instances" | "profiles")
            )
        })
        .filter_map(Path::parent)
        .find(|root| {
            ["libraries", "meta", "Install", "data/libraries"]
                .iter()
                .any(|d| root.join(d).is_dir())
        })
        .map(Path::to_path_buf)
}

/// Candidate Prism data folders, most specific first
pub fn prism_dirs() -> Vec<PathBuf> {
    dirs_of(Source::Prism)
}

/// Candidate vanilla launcher folders
pub fn minecraft_dirs() -> Vec<PathBuf> {
    dirs_of(Source::Vanilla)
}

fn dirs_of(source: Source) -> Vec<PathBuf> {
    launcher_dirs()
        .into_iter()
        .filter(|l| l.source == source)
        .map(|l| l.dir)
        .collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn dir_name(p: &Path) -> Option<String> {
    p.file_name().and_then(|s| s.to_str()).map(String::from)
}

/// `<root>/<v>/<file(v)>.jar` for each version folder under `root`
fn jars_named(root: &Path, source: Source, file: impl Fn(&str) -> String) -> Vec<JarFile> {
    subdirs(root)
        .into_iter()
        .filter_map(|d| {
            let v = dir_name(&d)?;
            let jar = d.join(file(&v));
            jar.is_file().then_some(JarFile {
                version: v,
                path: jar,
                source,
            })
        })
        .collect()
}

/// Jars under one Prism data folder
pub fn prism_jars(data: &Path) -> Vec<JarFile> {
    jars_named(
        &data.join("libraries/com/mojang/minecraft"),
        Source::Prism,
        |v| format!("minecraft-{v}-client.jar"),
    )
}

/// Jars under one vanilla launcher folder. Modded profiles (Fabric, Forge) have a version folder
/// whose jar is not the game's; those are skipped when the jar is opened and has no assets.
pub fn vanilla_jars(dir: &Path) -> Vec<JarFile> {
    jars_named(&dir.join("versions"), Source::Vanilla, |v| {
        format!("{v}.jar")
    })
}

/// The path of a Maven name in a repository: `com.mojang:minecraft:26.3:client` is
/// `com/mojang/minecraft/26.3/minecraft-26.3-client.jar`. Names with a path in them are refused.
fn maven_path(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split(':').collect();
    let (group, artifact, version, classifier) = match parts[..] {
        [g, a, v] => (g, a, v, None),
        [g, a, v, c] => (g, a, v, Some(c)),
        _ => return None,
    };
    if parts
        .iter()
        .any(|p| p.is_empty() || p.contains(['/', '\\', '@']) || p.contains(".."))
    {
        return None;
    }
    let classifier = classifier.map(|c| format!("-{c}")).unwrap_or_default();
    Some(format!(
        "{}/{artifact}/{version}/{artifact}-{version}{classifier}.jar",
        group.replace('.', "/")
    ))
}

/// The jars Prism's and MultiMC's metadata name: `meta/net.minecraft/<v>.json` has a `mainJar`
/// that is a Maven name in `libraries`
fn meta_jars(base: &Path, source: Source) -> Vec<JarFile> {
    let Ok(rd) = fs::read_dir(base.join("meta/net.minecraft")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        let (Some(version), true) = (
            path.file_stem().and_then(|s| s.to_str()).map(String::from),
            path.extension().is_some_and(|x| x == "json"),
        ) else {
            continue;
        };
        // a metadata file is a few kilobytes at most (the biggest is about 100 KB)
        if !e.metadata().is_ok_and(|m| m.len() <= 4 << 20) {
            continue;
        }
        let Some(meta) = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        else {
            continue;
        };
        let Some(rel) = meta["mainJar"]["name"].as_str().and_then(maven_path) else {
            continue;
        };
        let jar = base.join("libraries").join(rel);
        if jar.is_file() {
            out.push(JarFile {
                version,
                path: jar,
                source,
            });
        }
    }
    out
}

/// Every client jar in one launcher folder, in every layout the launchers use
pub fn jars_in(l: &LauncherDir) -> Vec<JarFile> {
    let mut out = Vec::new();
    for base in layout(l.source).jar_bases {
        let b = l.dir.join(base);
        out.extend(vanilla_jars_as(&b, l.source));
        out.extend(jars_named(
            &b.join("libraries/com/mojang/minecraft"),
            l.source,
            |v| format!("minecraft-{v}-client.jar"),
        ));
        out.extend(jars_named(
            &b.join("libraries/net/minecraft/client"),
            l.source,
            |v| format!("client-{v}.jar"),
        ));
        out.extend(meta_jars(&b, l.source));
    }
    out
}

fn vanilla_jars_as(dir: &Path, source: Source) -> Vec<JarFile> {
    vanilla_jars(dir)
        .into_iter()
        .map(|j| JarFile { source, ..j })
        .collect()
}

/// The `saves` folders (not the saves in them) of one launcher folder, each with the instance it
/// belongs to when there is one. The instances come first.
pub fn save_folders_in(l: &LauncherDir) -> Vec<(Option<String>, PathBuf)> {
    let lay = layout(l.source);
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for inst_dir in lay.instance_dirs {
        // `instances` and `Instances` are one folder on a system that ignores case
        let lower = inst_dir.to_ascii_lowercase();
        if seen.contains(&lower) {
            continue;
        }
        seen.push(lower);
        for inst in subdirs(&l.dir.join(inst_dir)) {
            let Some(name) = dir_name(&inst) else {
                continue;
            };
            for game in lay.game_dirs {
                out.push((Some(name.clone()), inst.join(game).join("saves")));
            }
        }
    }
    // the folder itself, as the vanilla launcher has it, or an instance picked by hand
    out.push((None, l.dir.join("saves")));
    for game in ["minecraft", ".minecraft", "instance"] {
        out.push((None, l.dir.join(game).join("saves")));
    }
    out.retain(|(_, p)| p.is_dir());
    // a folder is listed once, with the first name it was found under
    let mut unique: Vec<(Option<String>, PathBuf)> = Vec::new();
    for f in out {
        if !unique.iter().any(|u| u.1 == f.1) {
            unique.push(f);
        }
    }
    unique
}

/// Every jar found in the usual places, sorted by version name then source
pub fn find_jars() -> Vec<JarFile> {
    let mut out = Vec::new();
    for l in launcher_dirs() {
        out.extend(jars_in(&l));
    }
    if let Some(p) = std::env::var_os("MC_UNPACK_JAR") {
        let path = PathBuf::from(p);
        out.push(JarFile {
            version: crate::version::id_from_path(&path).unwrap_or_else(|| "unknown".into()),
            path,
            source: Source::Explicit,
        });
    }
    out.sort_by(|a, b| {
        version_key(&a.version)
            .cmp(&version_key(&b.version))
            .then(a.version.cmp(&b.version))
            .then((a.source as u8).cmp(&(b.source as u8)))
    });
    // The same file reached by two roots (a symlinked data folder) is listed once
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Sorts `1.2.5` before `1.12.2` before `1.21.1` before `26.3`: numeric parts, in order
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::region::tests::temp_dir;

    #[test]
    fn version_order() {
        let mut v = vec!["26.3", "1.12.2", "1.2.5", "1.21.1", "1.7.10"];
        v.sort_by_key(|s| version_key(s));
        assert_eq!(v, ["1.2.5", "1.7.10", "1.12.2", "1.21.1", "26.3"]);
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn places(home: &Path) -> Places {
        Places {
            home: Some(home.to_path_buf()),
            appdata: Some(home.join("AppData/Roaming")),
            data: Some(home.join(".local/share")),
            config: Some(home.join(".config")),
        }
    }

    /// The folder of `source` the catalogue lists at `home/tail`
    fn folder(home: &Path, source: Source, tail: &str) -> LauncherDir {
        known_folders(&places(home))
            .into_iter()
            .find(|l| l.source == source && l.dir == home.join(tail))
            .unwrap_or_else(|| panic!("{source:?} has no folder ending in {tail}"))
    }

    /// Finds the jar `rel` and the save `saves_rel` made inside `l`, as `find_jars` would
    fn check(l: &LauncherDir, version: &str, jar_rel: &str, saves_rel: &str, instance: &str) {
        touch(&l.dir.join(jar_rel));
        let world = l.dir.join(saves_rel).join("World");
        touch(&world.join("level.dat"));
        let dirs = with_roots(vec![l.clone()]);
        let jars: Vec<JarFile> = dirs.iter().flat_map(jars_in).collect();
        assert_eq!(jars.len(), 1, "{:?}: {jars:?}", l.source);
        assert_eq!(jars[0].version, version);
        assert_eq!(jars[0].source, l.source);
        assert_eq!(jars[0].path, l.dir.join(jar_rel));
        let saves: Vec<_> = dirs.iter().flat_map(save_folders_in).collect();
        let want = (
            (!instance.is_empty()).then(|| instance.to_string()),
            l.dir.join(saves_rel),
        );
        assert!(saves.contains(&want), "{:?}: {saves:?}", l.source);
    }

    #[test]
    fn vanilla_in_every_place() {
        let d = temp_dir("install-vanilla");
        for (tail, jar) in [
            ("Library/Application Support/minecraft", "1.20.1"),
            (".minecraft", "1.12.2"),
            (".var/app/com.mojang.Minecraft/.minecraft", "1.8.9"),
            ("AppData/Roaming/.minecraft", "1.16.5"),
        ] {
            let l = folder(&d, Source::Vanilla, tail);
            check(&l, jar, &format!("versions/{jar}/{jar}.jar"), "saves", "");
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn prism_and_multimc_keep_the_jar_in_libraries_and_worlds_in_minecraft() {
        let d = temp_dir("install-prism");
        for (source, tail) in [
            (Source::Prism, "Library/Application Support/PrismLauncher"),
            (Source::Prism, ".local/share/PrismLauncher"),
            (
                Source::Prism,
                ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher",
            ),
            (Source::Prism, "AppData/Roaming/PrismLauncher"),
            (Source::MultiMc, "Library/Application Support/MultiMC"),
            (Source::MultiMc, ".local/share/multimc"),
        ] {
            let l = folder(&d, source, tail);
            check(
                &l,
                "1.20.1",
                "libraries/com/mojang/minecraft/1.20.1/minecraft-1.20.1-client.jar",
                "instances/Pack/minecraft/saves",
                "Pack",
            );
        }
        // MultiMC's older game folder
        let l = folder(&d, Source::MultiMc, ".local/share/multimc");
        touch(&l.dir.join("instances/Old/.minecraft/saves/W/level.dat"));
        assert!(save_folders_in(&l).contains(&(
            Some("Old".to_string()),
            l.dir.join("instances/Old/.minecraft/saves")
        )));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn meta_main_jar_names_the_client() {
        let d = temp_dir("install-meta");
        let l = LauncherDir {
            source: Source::Prism,
            dir: d.join("Prism"),
        };
        fs::create_dir_all(l.dir.join("meta/net.minecraft")).unwrap();
        // a main jar of a name `libraries/com/mojang/minecraft` does not have
        fs::write(
            l.dir.join("meta/net.minecraft/9.9.json"),
            br#"{"mainJar":{"name":"net.example:client:9.9:special"}}"#,
        )
        .unwrap();
        touch(
            &l.dir
                .join("libraries/net/example/client/9.9/client-9.9-special.jar"),
        );
        // a name that tries to leave `libraries`, one for a jar that is not there, and index.json
        fs::write(
            l.dir.join("meta/net.minecraft/8.8.json"),
            br#"{"mainJar":{"name":"..:..:..:.."}}"#,
        )
        .unwrap();
        fs::write(
            l.dir.join("meta/net.minecraft/7.7.json"),
            br#"{"mainJar":{"name":"com.mojang:minecraft:7.7:client"}}"#,
        )
        .unwrap();
        fs::write(l.dir.join("meta/net.minecraft/index.json"), b"{}").unwrap();
        fs::write(l.dir.join("meta/net.minecraft/6.6.json"), b"not json").unwrap();
        let jars = jars_in(&l);
        assert_eq!(jars.len(), 1, "{jars:?}");
        assert_eq!(jars[0].version, "9.9");
        assert!(maven_path("a:b").is_none() && maven_path("a/b:c:d").is_none());
        assert_eq!(
            maven_path("com.mojang:minecraft:26.3:client").unwrap(),
            "com/mojang/minecraft/26.3/minecraft-26.3-client.jar"
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn modrinth_keeps_versions_in_meta_and_worlds_in_profiles() {
        let d = temp_dir("install-modrinth");
        for tail in [
            "Library/Application Support/ModrinthApp",
            "Library/Application Support/com.modrinth.theseus",
            ".local/share/ModrinthApp",
            ".config/ModrinthApp",
            ".var/app/com.modrinth.ModrinthApp/data/ModrinthApp",
            "AppData/Roaming/ModrinthApp",
        ] {
            let l = folder(&d, Source::Modrinth, tail);
            check(
                &l,
                "1.21.1",
                "meta/versions/1.21.1/1.21.1.jar",
                "profiles/Fabulously Optimized/saves",
                "Fabulously Optimized",
            );
        }
        // the vanilla jar a Fabric profile keeps under the loader's name
        let l = folder(&d, Source::Modrinth, ".local/share/ModrinthApp");
        touch(&l.dir.join("meta/versions/1.21.1-fabric/1.21.1-fabric.jar"));
        let versions: Vec<String> = jars_in(&l).into_iter().map(|j| j.version).collect();
        assert_eq!(versions, ["1.21.1", "1.21.1-fabric"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn atlauncher_keeps_the_jar_in_net_minecraft_client_and_the_game_in_the_instance() {
        let d = temp_dir("install-atl");
        for tail in [
            "Library/Application Support/ATLauncher",
            "Applications/ATLauncher.app/Contents/Java",
            ".local/share/atlauncher",
            ".var/app/com.atlauncher.ATLauncher/data/atlauncher",
            ".var/app/com.atlauncher.ATLauncher/data",
            "AppData/Roaming/ATLauncher",
        ] {
            let l = folder(&d, Source::AtLauncher, tail);
            check(
                &l,
                "1.19.2",
                "libraries/net/minecraft/client/1.19.2/client-1.19.2.jar",
                "instances/AllTheMods/saves",
                "AllTheMods",
            );
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn gdlauncher_keeps_everything_in_its_data_folder() {
        let d = temp_dir("install-gdl");
        for tail in [
            "Library/Application Support/gdlauncher_carbon",
            ".local/share/gdlauncher_carbon",
            "AppData/Roaming/gdlauncher_carbon",
        ] {
            let l = folder(&d, Source::GdLauncher, tail);
            check(
                &l,
                "1.20.1",
                "data/libraries/net/minecraft/client/1.20.1/client-1.20.1.jar",
                "data/instances/pack/instance/saves",
                "pack",
            );
        }
        // the version before Carbon: instances beside the libraries, the instance the game folder
        let l = folder(&d, Source::GdLauncher, ".config/gdlauncher_next");
        check(
            &l,
            "1.16.5",
            "libraries/net/minecraft/client/1.16.5/client-1.16.5.jar",
            "instances/old/saves",
            "old",
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn curseforge_keeps_the_jar_in_install_and_worlds_in_instances() {
        let d = temp_dir("install-curse");
        for tail in ["Documents/curseforge/minecraft", "curseforge/minecraft"] {
            let l = folder(&d, Source::CurseForge, tail);
            check(
                &l,
                "1.20.1",
                "Install/versions/1.20.1/1.20.1.jar",
                "Instances/All the Mods 9/saves",
                "All the Mods 9",
            );
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_folder_inside_instances_stands_for_the_launcher_above() {
        let d = temp_dir("install-root");
        let launcher = d.join("anywhere/MultiMC");
        touch(&launcher.join("libraries/com/mojang/minecraft/1.8.9/minecraft-1.8.9-client.jar"));
        touch(&launcher.join("instances/One/.minecraft/saves/W/level.dat"));
        touch(&launcher.join("instances/Two/minecraft/saves/W/level.dat"));
        assert_eq!(launcher_root(&launcher), None, "it is the launcher folder");
        for picked in [
            launcher.join("instances"),
            launcher.join("instances/One"),
            launcher.join("instances/One/.minecraft/saves"),
        ] {
            assert_eq!(launcher_root(&picked), Some(launcher.clone()), "{picked:?}");
        }
        let dirs = with_roots(vec![LauncherDir {
            source: Source::Folder,
            dir: launcher.join("instances/One"),
        }]);
        assert_eq!(dirs.len(), 2);
        let jars: Vec<_> = dirs.iter().flat_map(jars_in).collect();
        assert_eq!(jars.len(), 1);
        assert_eq!(jars[0].version, "1.8.9");
        let saves: Vec<_> = dirs.iter().flat_map(save_folders_in).collect();
        assert_eq!(saves.len(), 3, "{saves:?}");
        // a folder called instances that is not a launcher's is no launcher
        let plain = d.join("games/instances/mc");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(launcher_root(&plain), None);
        // `Folder` finds the other launchers' layouts without being told which
        let modrinth = d.join("elsewhere");
        touch(&modrinth.join("meta/versions/1.21.1/1.21.1.jar"));
        touch(&modrinth.join("profiles/P/saves/W/level.dat"));
        let l = LauncherDir {
            source: Source::Folder,
            dir: modrinth,
        };
        assert_eq!(jars_in(&l).len(), 1);
        assert_eq!(save_folders_in(&l).len(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_catalogue_has_every_launcher_once_per_folder() {
        let d = temp_dir("install-cat");
        let all = known_folders(&places(&d));
        for s in [
            Source::Vanilla,
            Source::Prism,
            Source::MultiMc,
            Source::Modrinth,
            Source::AtLauncher,
            Source::GdLauncher,
            Source::CurseForge,
        ] {
            assert!(all.iter().any(|l| l.source == s), "{s:?}");
        }
        let mut paths: Vec<_> = all.iter().map(|l| &l.dir).collect();
        let n = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), n, "a folder listed twice");
        // no home, no environment: nothing is guessed
        let none = Places {
            home: None,
            appdata: None,
            data: None,
            config: None,
        };
        assert!(known_folders(&none)
            .iter()
            .all(|l| l.dir.starts_with("/Applications")));
        fs::remove_dir_all(&d).unwrap();
    }
}
