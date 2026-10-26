use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

use mc_unpack::assets::{AssetId, Assets, LeftOut};
use mc_unpack::check;
use mc_unpack::gltf;
use mc_unpack::install::{find_jars, JarFile, Source};
use mc_unpack::jar::escape_control as esc;
use mc_unpack::mesh::{self, MeshOptions, Origin};
use mc_unpack::text::{inspect_lines, states_lines};
use mc_unpack::texture::encode_png;
use mc_unpack::tint::{parse_rgb, ColormapKind, Tint, TintSet};
use mc_unpack::version::Support;

#[derive(Parser)]
#[command(
    name = "mc-unpack",
    version,
    about = "Reads block and item models and textures from your own Minecraft: Java Edition client jar (unofficial; not affiliated with Mojang or Microsoft)"
)]
struct Cli {
    /// A client jar to read, instead of searching the launchers' folders
    #[arg(long, global = true)]
    jar: Option<PathBuf>,
    /// Which installed version to read (`1.20.1`); the newest supported one when omitted
    #[arg(long = "mc-version", global = true, value_name = "VERSION")]
    mc_version: Option<String>,
    /// For the commands that go through the Provider (`check-conformance`): also offer the saves found
    /// in the launchers' folders as levels (`world/<save>/<x>,<z>`)
    #[arg(long, global = true)]
    with_worlds: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the jars found and what is supported for each
    Versions,
    /// List blocks and items that draw something (ids for `model`); `--left-out` lists the rest
    List {
        /// Only ids or names containing this text
        filter: Option<String>,
        /// Blocks only
        #[arg(long)]
        blocks: bool,
        /// Items only
        #[arg(long)]
        items: bool,
        /// Show the states of each block (variant keys, or properties and values for multipart)
        #[arg(long)]
        states: bool,
        /// List what the data has no geometry for (block entities and the like), with the reason
        #[arg(long)]
        left_out: bool,
    },
    /// Export a block or item as glTF: `block/<name>[?variant=<state>]` or `item/<name>`
    Model {
        id: String,
        /// Output folder
        #[arg(short, long, default_value = "out")]
        out: PathBuf,
        /// Sheet frame of animated textures (default: the first frame the animation shows)
        #[arg(long)]
        frame: Option<u32>,
        /// Colour for a tint index, repeatable: `0=#7bb34a`, `0=grass:0.8,0.4`, `0=foliage@plains`
        #[arg(long = "tint", value_name = "INDEX=COLOUR")]
        tints: Vec<String>,
        /// Put the origin at the block's corner (the game's frame) instead of its bottom centre
        #[arg(long)]
        corner_origin: bool,
        /// File name (without extension) for the output; the id with `_` for punctuation by default
        #[arg(long)]
        name: Option<String>,
    },
    /// Say what an id bakes to: the state used, what was ignored, every note, and what was left out
    Inspect { id: String },
    /// Write a texture (and with `--frames` every frame of an animated one) as PNG
    Texture {
        /// Path under textures/, such as `block/stone`
        path: String,
        #[arg(short, long, default_value = "out")]
        out: PathBuf,
        #[arg(long)]
        frames: bool,
    },
    /// Look a colour up in a biome colormap: by temperature and downfall, or by a biome of the jar
    Tint {
        /// grass, foliage or dry_foliage
        kind: String,
        #[arg(long, allow_negative_numbers = true)]
        temperature: Option<f64>,
        #[arg(long, allow_negative_numbers = true)]
        downfall: Option<f64>,
        /// A biome from the jar's data (1.19 and later), such as `plains`
        #[arg(long)]
        biome: Option<String>,
    },
    /// List the saves found in the launchers' folders (read only), with the version each was saved by
    Worlds,
    /// Say what a save folder holds: version, dimensions, regions and chunks, and which jar reads it
    WorldInfo {
        /// A save folder (the one with level.dat in it)
        save: PathBuf,
    },
    /// Export a part of a save as glTF: the blocks of a region (and `--radius` regions around it), baked
    /// from the jar's models, with hidden faces dropped and biome tints. Prints what was left out and why.
    World {
        /// A save folder (the one with level.dat in it)
        save: PathBuf,
        /// The region file's coordinates, `x,z` (`r.x.z.mca`)
        #[arg(long, value_name = "X,Z", allow_hyphen_values = true)]
        region: String,
        /// Regions to add on every side
        #[arg(long, default_value_t = 0)]
        radius: u32,
        /// Keep only these chunks of the window: `x0,z0,x1,z1` in chunk coordinates, inclusive
        #[arg(long, value_name = "X0,Z0,X1,Z1", allow_hyphen_values = true)]
        chunks: Option<String>,
        /// Keep only these block heights: `min,max`, inclusive
        #[arg(long, value_name = "MIN,MAX", allow_hyphen_values = true)]
        y: Option<String>,
        /// `overworld`, `the_nether`, `the_end`, or `namespace:name`
        #[arg(long, default_value = "overworld")]
        dimension: String,
        /// Output folder
        #[arg(short, long, default_value = "out")]
        out: PathBuf,
        /// File name (without extension) for the output
        #[arg(long)]
        name: Option<String>,
        /// Most quads the scene may have (about 100 bytes each in memory)
        #[arg(long)]
        max_quads: Option<u64>,
        /// Also read chunks the game had not finished generating (by default they are left out, as
        /// the game regenerates them)
        #[arg(long)]
        include_unfinished: bool,
    },
    /// Parse and bake everything in every installed version (or the one given); count failures
    Check {
        /// Also print every block and item that was left out, by name
        #[arg(long)]
        verbose: bool,
    },
    /// Load everything through the rewrite-bridge Provider and validate it
    #[cfg(feature = "bridge")]
    CheckBridge,
    /// Check what the Provider returns against the bridge's conventions; try `check-conformance --help`
    #[cfg(feature = "bridge")]
    #[command(disable_help_flag = true)]
    CheckConformance {
        /// Options of the checker, passed through (`--id`, `--filter`, `--kind`, `--limit`, `--strict`, `-v`, `--help`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn tilde(p: &Path) -> String {
    match std::env::var("HOME") {
        Ok(h) => tilde_with(p, &h),
        _ => p.display().to_string(),
    }
}

/// `~` for the home folder only at a path boundary: `/home/ann2/x` is not under `/home/ann`
fn tilde_with(p: &Path, home: &str) -> String {
    let s = p.display().to_string();
    let h = home.trim_end_matches('/');
    if h.is_empty() {
        return s;
    }
    match s.strip_prefix(h) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => s,
    }
}

/// The jars a command should look at: the given one, the one named, or everything found
fn selected_jars(cli: &Cli) -> Result<Vec<JarFile>> {
    if let Some(p) = &cli.jar {
        return Ok(vec![JarFile {
            version: mc_unpack::version::id_from_path(p).unwrap_or_else(|| "unknown".into()),
            path: p.clone(),
            source: Source::Explicit,
        }]);
    }
    let all = find_jars();
    if all.is_empty() {
        bail!("no Minecraft client jar found: looked in Prism Launcher's and the vanilla launcher's folders (see the README); pass --jar <path>");
    }
    if let Some(v) = &cli.mc_version {
        let m: Vec<JarFile> = all.into_iter().filter(|j| &j.version == v).collect();
        if m.is_empty() {
            bail!("no installed jar for version {v}; `mc-unpack versions` lists them");
        }
        return Ok(m);
    }
    Ok(all)
}

/// The one jar a single-jar command reads: the explicit or named one, else the newest that is fully supported
fn one_assets(cli: &Cli) -> Result<Assets> {
    let jars = selected_jars(cli)?;
    if cli.jar.is_some() || cli.mc_version.is_some() {
        return Assets::open(&jars[0].path);
    }
    for j in jars.iter().rev() {
        if let Ok(a) = Assets::open(&j.path) {
            if a.version().support() == Support::Full {
                eprintln!(
                    "(reading Minecraft {}; --mc-version picks another)",
                    a.version().id
                );
                return Ok(a);
            }
        }
    }
    bail!("no installed jar with JSON models was found; `mc-unpack versions` lists what is there")
}

fn cmd_versions(cli: &Cli) -> Result<()> {
    let jars = selected_jars(cli)?;
    println!(
        "{:<8} {:<17} {:<14} {:<7}  note",
        "version", "source", "support", "id from"
    );
    for j in jars {
        match Assets::open(&j.path) {
            Ok(a) => {
                let v = a.version();
                let sup = match v.support() {
                    Support::Full => "full",
                    Support::TexturesOnly => "textures only",
                    Support::Unsupported => "unsupported",
                };
                let from = match v.id_from {
                    mc_unpack::version::IdSource::VersionJson => "version.json",
                    mc_unpack::version::IdSource::Path => "path",
                    mc_unpack::version::IdSource::Unknown => "-",
                };
                println!(
                    "{:<8} {:<17} {:<14} {:<7}  {}",
                    esc(&v.id),
                    j.source.name(),
                    sup,
                    from,
                    v.layout.name()
                );
                println!("         {}", esc(&tilde(&j.path)));
            }
            Err(e) => println!(
                "{:<8} {:<17} unreadable: {}",
                esc(&j.version),
                j.source.name(),
                esc(&format!("{e:#}"))
            ),
        }
    }
    Ok(())
}

fn cmd_list(
    a: Assets,
    filter: Option<String>,
    blocks: bool,
    items: bool,
    states: bool,
    left_out: bool,
) -> Result<()> {
    a.require_models()?;
    let want_blocks = blocks || !items;
    let want_items = items || !blocks;
    let f = filter.unwrap_or_default().to_ascii_lowercase();
    let matches = |s: &str| f.is_empty() || s.to_ascii_lowercase().contains(&f);
    let mut left: Vec<(String, LeftOut)> = Vec::new();
    let mut shown = 0usize;
    if want_blocks {
        for n in a.block_names().into_iter().filter(|n| matches(n)) {
            match a.classify_block(&n) {
                Ok(None) if !left_out => {
                    println!("block/{n}");
                    shown += 1;
                    if states {
                        print_states(&a, &n)?;
                    }
                }
                Ok(None) => {}
                Ok(Some(lo)) => left.push((format!("block/{n}"), lo)),
                Err(e) => eprintln!("block/{n}: {}", esc(&format!("{e:#}"))),
            }
        }
    }
    if want_items {
        for n in a.item_names().into_iter().filter(|n| matches(n)) {
            match a.classify_item(&n) {
                Ok(None) if !left_out => {
                    println!("item/{n}");
                    shown += 1;
                }
                Ok(None) => {}
                Ok(Some(LeftOut::Template)) => {}
                Ok(Some(lo)) => left.push((format!("item/{n}"), lo)),
                Err(e) => eprintln!("item/{n}: {}", esc(&format!("{e:#}"))),
            }
        }
    }
    if left_out {
        let mut by: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (id, lo) in left {
            by.entry(lo.to_string()).or_default().push(id);
        }
        for (why, ids) in by {
            println!("{} ({}): {}", esc(&why), ids.len(), ids.join(" "));
        }
    } else {
        eprintln!("{shown} listed");
    }
    Ok(())
}

fn print_states(a: &Assets, name: &str) -> Result<()> {
    for l in states_lines(a, name)? {
        println!("{l}");
    }
    Ok(())
}

fn parse_tint(a: &Assets, spec: &str) -> Result<(u32, Tint)> {
    let (idx, what) = spec
        .split_once('=')
        .ok_or_else(|| anyhow!("--tint {spec}: expected INDEX=COLOUR"))?;
    let idx: u32 = idx
        .trim()
        .parse()
        .with_context(|| format!("tint index {idx:?}"))?;
    let what = what.trim();
    if let Some((kind, rest)) = what.split_once(':') {
        let kind = ColormapKind::parse(kind)
            .ok_or_else(|| anyhow!("{kind:?}: grass, foliage or dry_foliage"))?;
        let (t, d) = rest
            .split_once(',')
            .ok_or_else(|| anyhow!("{what}: expected {}:TEMPERATURE,DOWNFALL", kind.name()))?;
        return Ok((
            idx,
            Tint::Colormap {
                kind,
                temperature: t.trim().parse()?,
                downfall: d.trim().parse()?,
            },
        ));
    }
    if let Some((kind, biome)) = what.split_once('@') {
        let kind = ColormapKind::parse(kind)
            .ok_or_else(|| anyhow!("{kind:?}: grass, foliage or dry_foliage"))?;
        let (t, notes) = a.tint_for_biome(kind, biome)?;
        for n in notes {
            eprintln!("note: {}", esc(&n));
        }
        return Ok((idx, t));
    }
    Ok((idx, Tint::Rgb(parse_rgb(what)?)))
}

fn file_name_for(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn cmd_model(
    a: Assets,
    id: String,
    out: PathBuf,
    frame: Option<u32>,
    tints: Vec<String>,
    corner_origin: bool,
    name: Option<String>,
) -> Result<()> {
    let baked = a.bake_id(&id)?;
    for n in &baked.report.notes {
        eprintln!("note: {}", esc(n));
    }
    if !baked.report.ignored_properties.is_empty() {
        eprintln!(
            "note: the blockstate has no property {}; ignored",
            baked.report.ignored_properties.join(", ")
        );
    }
    if baked.is_empty() {
        let why = baked.report.left_out.clone().unwrap_or(LeftOut::NoElements);
        bail!("{id}: left out: {why}");
    }
    let mut set = TintSet::new();
    for t in &tints {
        let (i, tint) = parse_tint(&a, t)?;
        set.0.insert(i, tint);
    }
    let opts = MeshOptions {
        frame,
        tints: set,
        origin: if corner_origin {
            Origin::BlockCorner
        } else {
            Origin::BottomCentre
        },
    };
    let groups = mesh::build(&a, &baked, &opts)?;
    let name = name.unwrap_or_else(|| file_name_for(&id));
    let path = gltf::write(&baked, &groups, &name, &out)?;
    println!(
        "{id}: {} quads, {} materials -> {}",
        baked.quad_count(),
        groups.len(),
        tilde(&path)
    );
    for g in &groups {
        let tint = match (g.tint_index, g.tint) {
            (Some(i), Some(c)) => format!(", tint {i} = #{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
            (Some(i), None) => format!(", tintindex {i} (not coloured: no colour given)"),
            _ => String::new(),
        };
        println!("  {} {:?}{tint}", esc(&g.texture), g.alpha);
    }
    Ok(())
}

fn cmd_inspect(a: Assets, id: String) -> Result<()> {
    let b = a.bake_id(&id)?;
    for l in inspect_lines(&id, &b) {
        println!("{l}");
    }
    Ok(())
}

fn cmd_texture(a: Assets, path: String, out: PathBuf, frames: bool) -> Result<()> {
    let t = a.texture(&path)?;
    std::fs::create_dir_all(&out)?;
    let base = file_name_for(&path);
    match &t.animation {
        None => println!("{path}: {}x{}, not animated", t.sheet.width, t.sheet.height),
        Some(an) => println!(
            "{path}: sheet {}x{}, {} frames of {}x{}, frametime {}, interpolate {}, plays {} steps",
            t.sheet.width,
            t.sheet.height,
            t.sheet_frames(),
            t.frame_width,
            t.frame_height,
            an.frametime,
            an.interpolate,
            an.frames.len()
        ),
    }
    if frames {
        // the sheet frames the game shows, named by their place on the sheet
        let shown = t.shown_frames();
        for (i, f) in shown.iter().zip(t.all_frames()) {
            std::fs::write(out.join(format!("{base}_{i}.png")), encode_png(&f)?)?;
        }
        println!("wrote {} frames to {}", shown.len(), tilde(&out));
    } else {
        let p = out.join(format!("{base}.png"));
        std::fs::write(&p, encode_png(&t.default_frame())?)?;
        println!(
            "wrote frame {} (the first the animation shows) to {}",
            t.default_frame_index(),
            tilde(&p)
        );
    }
    Ok(())
}

fn cmd_tint(
    a: Assets,
    kind: String,
    temperature: Option<f64>,
    downfall: Option<f64>,
    biome: Option<String>,
) -> Result<()> {
    let kind = ColormapKind::parse(&kind)
        .ok_or_else(|| anyhow!("{kind:?}: grass, foliage or dry_foliage"))?;
    let tint = match (&biome, temperature, downfall) {
        (Some(b), None, None) => {
            let (t, notes) = a.tint_for_biome(kind, b)?;
            for n in notes {
                eprintln!("note: {}", esc(&n));
            }
            t
        }
        (None, Some(t), Some(d)) => Tint::Colormap {
            kind,
            temperature: t,
            downfall: d,
        },
        _ => bail!("give --biome <name>, or both --temperature and --downfall"),
    };
    let c = a.tint_rgb(&tint)?;
    println!(
        "#{:02x}{:02x}{:02x}  rgb({}, {}, {})",
        c[0], c[1], c[2], c[0], c[1], c[2]
    );
    Ok(())
}

fn fmt_count(c: check::Count) -> String {
    format!("{} ok, {} failed", c.ok, c.failed)
}

fn cmd_check(cli: &Cli, verbose: bool) -> Result<bool> {
    let jars = selected_jars(cli)?;
    let mut total_failed = 0u64;
    for j in jars {
        let a = match Assets::open(&j.path) {
            Ok(a) => a,
            Err(e) => {
                println!(
                    "{} ({}): unreadable: {}",
                    esc(&j.version),
                    j.source.name(),
                    esc(&format!("{e:#}"))
                );
                continue;
            }
        };
        let v = a.version();
        println!(
            "== Minecraft {} ({}) - {}",
            esc(&v.id),
            j.source.name(),
            v.layout.name()
        );
        match check::run(&a) {
            Err(e) => println!("   {}", esc(&format!("{e:#}"))),
            Ok(r) => {
                if v.support() == Support::TexturesOnly {
                    println!("   textures only: {}", v.support_note());
                }
                println!(
                    "   textures      {}  ({} animated)",
                    fmt_count(r.textures),
                    r.animated_textures
                );
                println!("   .mcmeta       {}", fmt_count(r.mcmetas));
                println!("   colormaps     {}", fmt_count(r.colormaps));
                if v.layout.has_json_models() {
                    println!("   blockstates   {}", fmt_count(r.blockstates));
                    println!("   block models  {}", fmt_count(r.block_models));
                    println!(
                        "   item models   {}  ({} templates)",
                        fmt_count(r.item_models),
                        r.item_templates
                    );
                    if r.item_defs.ok + r.item_defs.failed > 0 {
                        println!("   item defs     {}", fmt_count(r.item_defs));
                    }
                    println!("   blocks baked  {}", fmt_count(r.blocks));
                    println!("   items baked   {}", fmt_count(r.items));
                    println!("   quads         {}", r.quads);
                    if r.items_partial > 0 {
                        println!(
                            "   items that lost a part to code or a refusal: {}",
                            r.items_partial
                        );
                    }
                    for (why, ids) in &r.blocks_left_out {
                        println!("   blocks left out ({}): {}", ids.len(), esc(why));
                        if verbose {
                            println!("      {}", ids.join(" "));
                        }
                    }
                    for (why, ids) in &r.items_left_out {
                        println!("   items left out ({}): {}", ids.len(), esc(why));
                        if verbose {
                            println!("      {}", ids.join(" "));
                        }
                    }
                }
                for f in &r.failures {
                    println!("   FAIL {}", esc(f));
                }
                if !r.left_out_inside_models.is_empty() {
                    println!(
                        "   faces or elements left out inside models: {}",
                        r.left_out_inside_models.len()
                    );
                    if verbose {
                        for n in &r.left_out_inside_models {
                            println!("      {}", esc(n));
                        }
                    }
                }
                println!(
                    "   biggest met (limits): png {} px ({}), sheet frames {} ({}), frame list {} ({}), item quads {}, one bake: {} parts ({}), {} quads ({}), {} image bytes ({})",
                    r.max_png_pixels,
                    mc_unpack::texture::MAX_PIXELS,
                    r.max_sheet_frames,
                    mc_unpack::texture::MAX_SHEET_FRAMES,
                    r.max_frame_list,
                    mc_unpack::texture::MAX_FRAME_LIST,
                    r.max_item_quads,
                    r.max_bake_parts,
                    mc_unpack::assets::MAX_BAKE_PARTS,
                    r.max_bake_quads,
                    mc_unpack::assets::MAX_BAKE_QUADS,
                    r.max_bake_image_bytes,
                    mc_unpack::assets::MAX_BAKE_IMAGE_BYTES
                );
                println!("   failures: {}  ({:.1} s)", r.total_failed(), r.seconds);
                total_failed += r.total_failed();
            }
        }
    }
    println!("peak RSS {} MB", check::peak_rss() / 1_000_000);
    Ok(total_failed == 0)
}

#[cfg(feature = "bridge")]
fn cmd_check_bridge(cli: &Cli) -> Result<bool> {
    use mc_unpack::bridge::{LeftOutError, Minecraft};
    use rewrite_bridge::Provider;
    let mut all_ok = true;
    for j in selected_jars(cli)? {
        let a = match Assets::open(&j.path) {
            Ok(a) => a,
            Err(e) => {
                println!(
                    "{}: unreadable: {}",
                    esc(&j.version),
                    esc(&format!("{e:#}"))
                );
                continue;
            }
        };
        let id = a.version().id.clone();
        let mc = match Minecraft::from_assets(a) {
            Ok(m) => m,
            Err(e) => {
                println!(
                    "== Minecraft {}: no provider: {}",
                    esc(&id),
                    esc(&format!("{e:#}"))
                );
                continue;
            }
        };
        let start = std::time::Instant::now();
        let list = mc.list("").map_err(|e| anyhow!("{e}"))?;
        let (mut ok, mut failed, mut tris, mut mats, mut partial) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut fails = Vec::new();
        for info in &list {
            match mc.load_report(&info.id) {
                Ok((m, r)) => {
                    ok += 1;
                    if !r.dropped_parts.is_empty() {
                        partial += 1;
                    }
                    tris += m
                        .meshes
                        .iter()
                        .map(|x| x.indices.len() as u64 / 3)
                        .sum::<u64>();
                    mats += r.materials as u64;
                    // the loaded model must be 1 block wide at most unless the data says otherwise
                    if let Some((lo, hi)) = m.bounds() {
                        if lo.iter().chain(hi.iter()).any(|c| !c.is_finite()) {
                            failed += 1;
                            fails.push(format!("{}: non-finite bounds", info.id));
                        }
                    }
                }
                Err(e) if e.downcast_ref::<LeftOutError>().is_some() => {
                    failed += 1;
                    fails.push(format!("{}: listed but left out: {e}", info.id));
                }
                Err(e) => {
                    failed += 1;
                    if fails.len() < 40 {
                        fails.push(format!("{}: {e:#}", info.id));
                    }
                }
            }
        }
        println!(
            "== Minecraft {}: {} listed, {ok} loaded ({partial} of them with a part left out), {failed} failed ({tris} triangles, {mats} materials, {:.1} s)",
            esc(&id),
            list.len(),
            start.elapsed().as_secs_f64()
        );
        for f in fails.iter().take(40) {
            println!("   FAIL {}", esc(f));
        }
        all_ok &= failed == 0;
    }
    println!("peak RSS {} MB", check::peak_rss() / 1_000_000);
    Ok(all_ok)
}

/// `1,-2,3` to numbers
fn parse_ints(s: &str, n: usize, what: &str) -> Result<Vec<i32>> {
    let v: Vec<i32> = s
        .split(',')
        .map(|p| p.trim().parse::<i32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| anyhow!("{what} {s:?}: expected {n} whole numbers separated by commas"))?;
    if v.len() != n {
        bail!("{what} {s:?}: expected {n} whole numbers separated by commas");
    }
    Ok(v)
}

/// The jar that reads a save: the explicit or named one, else an installed one of the version the
/// save was made by
fn assets_for_world(cli: &Cli, world: &mc_unpack::world::save::World) -> Result<Assets> {
    if cli.jar.is_some() || cli.mc_version.is_some() {
        return one_assets(cli);
    }
    let lv = &world.level;
    let mut seen = Vec::new();
    for j in find_jars() {
        let Ok(a) = Assets::open(&j.path) else {
            continue;
        };
        let v = a.version();
        let dv = v.json.as_ref().and_then(|x| x.world_version);
        let by_name = lv.version_name.as_deref() == Some(v.id.as_str());
        let by_data = lv.data_version.is_some() && lv.data_version == dv;
        seen.push(v.id.clone());
        if (by_name || by_data) && v.support() == Support::Full {
            eprintln!(
                "(reading Minecraft {}, the version this save was made by)",
                v.id
            );
            return Ok(a);
        }
    }
    bail!(
        "this save was made by Minecraft {} (data version {}), and no installed jar is that version (installed: {}); pass --jar or --mc-version to read it with another",
        esc(lv.version_name.as_deref().unwrap_or("?")),
        lv.data_version.map(|d| d.to_string()).unwrap_or_else(|| "?".into()),
        if seen.is_empty() { "none".into() } else { seen.join(", ") }
    )
}

fn cmd_worlds() -> Result<()> {
    use mc_unpack::world::save::{discover_saves, World};
    let saves = discover_saves();
    if saves.is_empty() {
        println!("no saves found in the launchers' folders (MC_UNPACK_SAVES adds folders)");
    }
    for s in saves {
        match World::open(&s.path) {
            Ok(w) => println!(
                "{:<24} {:<8} data {:<6} {}",
                esc(&s.label),
                esc(w.level.version_name.as_deref().unwrap_or("?")),
                w.level
                    .data_version
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into()),
                esc(&tilde(&s.path))
            ),
            Err(e) => println!(
                "{:<24} unreadable: {}",
                esc(&s.label),
                esc(&format!("{e:#}"))
            ),
        }
    }
    Ok(())
}

fn cmd_world_info(cli: &Cli, save: &Path) -> Result<()> {
    use mc_unpack::world::region::Region;
    use mc_unpack::world::save::World;
    let w = World::open(save)?;
    let lv = &w.level;
    println!("save: {}", esc(&w.name()));
    println!(
        "saved by Minecraft {} (data version {}){}",
        esc(lv.version_name.as_deref().unwrap_or("? (before 1.9)")),
        lv.data_version
            .map(|d| d.to_string())
            .unwrap_or_else(|| "none".into()),
        if lv.snapshot == Some(true) {
            ", a snapshot"
        } else {
            ""
        }
    );
    if let Some(s) = lv.spawn {
        println!("spawn: {},{},{}", s[0], s[1], s[2]);
    }
    for d in w.dimensions() {
        let regions = w.regions(&d)?;
        let mut chunks = 0usize;
        let dir = w.region_dir(&d)?;
        for (x, z) in &regions {
            if let Ok(r) = Region::open(&dir.join(format!("r.{x}.{z}.mca"))) {
                chunks += r.chunk_count();
            }
        }
        let list: Vec<String> = regions
            .iter()
            .take(24)
            .map(|(x, z)| format!("{x},{z}"))
            .collect();
        println!(
            "{d}: {} region(s), {chunks} chunk(s): {}{}",
            regions.len(),
            list.join("  "),
            if regions.len() > 24 { "  ..." } else { "" }
        );
    }
    match assets_for_world(cli, &w) {
        Ok(a) => println!("read with: Minecraft {}", a.version().id),
        Err(e) => println!("read with: nothing ({})", esc(&format!("{e:#}"))),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_world(
    cli: &Cli,
    save: &Path,
    region: &str,
    radius: u32,
    chunks: Option<&str>,
    y: Option<&str>,
    dimension: &str,
    out: &Path,
    name: Option<&str>,
    max_quads: Option<u64>,
    include_unfinished: bool,
) -> Result<()> {
    use mc_unpack::world::mesher::{build, BuildOptions, Window};
    use mc_unpack::world::save::World;
    let w = World::open(save)?;
    let assets = assets_for_world(cli, &w)?;
    let r = parse_ints(region, 2, "--region")?;
    let mut win = Window::region(r[0], r[1]);
    win.radius = radius;
    if let Some(c) = chunks {
        let v = parse_ints(c, 4, "--chunks")?;
        win.chunks = Some([v[0], v[1], v[2], v[3]]);
    }
    if let Some(y) = y {
        let v = parse_ints(y, 2, "--y")?;
        win.y = Some((v[0], v[1]));
    }
    let mut opts = BuildOptions::new(win);
    opts.dimension = dimension.to_string();
    if let Some(m) = max_quads {
        opts.limits.max_quads = m;
    }
    opts.limits.include_unfinished = include_unfinished;
    let start = std::time::Instant::now();
    let (mesh, report) = build(&assets, &w, &opts)?;
    for l in report.lines() {
        println!("{l}");
    }
    let stem = name
        .map(String::from)
        .unwrap_or_else(|| file_name_for(&format!("world_{}_{}_{}", w.name(), r[0], r[1])));
    let p = gltf::write_world(&mesh, &stem, out)?;
    println!(
        "wrote {} ({} chunk columns, {} quads, {} materials) in {:.1} s; peak RSS {} MB",
        esc(&tilde(&p)),
        mesh.chunks.len(),
        mesh.quad_count(),
        mesh.materials.len(),
        start.elapsed().as_secs_f64(),
        check::peak_rss() / 1_000_000
    );
    Ok(())
}

fn main() {
    // Die quietly when the reader of our output goes away (`list | head`)
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let res: Result<bool> = (|| match &cli.cmd {
        Cmd::Versions => cmd_versions(&cli).map(|_| true),
        Cmd::List {
            filter,
            blocks,
            items,
            states,
            left_out,
        } => cmd_list(
            one_assets(&cli)?,
            filter.clone(),
            *blocks,
            *items,
            *states,
            *left_out,
        )
        .map(|_| true),
        Cmd::Model {
            id,
            out,
            frame,
            tints,
            corner_origin,
            name,
        } => {
            let _ = AssetId::parse(id)?;
            cmd_model(
                one_assets(&cli)?,
                id.clone(),
                out.clone(),
                *frame,
                tints.clone(),
                *corner_origin,
                name.clone(),
            )
            .map(|_| true)
        }
        Cmd::Inspect { id } => cmd_inspect(one_assets(&cli)?, id.clone()).map(|_| true),
        Cmd::Texture { path, out, frames } => {
            cmd_texture(one_assets(&cli)?, path.clone(), out.clone(), *frames).map(|_| true)
        }
        Cmd::Tint {
            kind,
            temperature,
            downfall,
            biome,
        } => cmd_tint(
            one_assets(&cli)?,
            kind.clone(),
            *temperature,
            *downfall,
            biome.clone(),
        )
        .map(|_| true),
        Cmd::Worlds => cmd_worlds().map(|_| true),
        Cmd::WorldInfo { save } => cmd_world_info(&cli, save).map(|_| true),
        Cmd::World {
            save,
            region,
            radius,
            chunks,
            y,
            dimension,
            out,
            name,
            max_quads,
            include_unfinished,
        } => cmd_world(
            &cli,
            save,
            region,
            *radius,
            chunks.as_deref(),
            y.as_deref(),
            dimension,
            out,
            name.as_deref(),
            *max_quads,
            *include_unfinished,
        )
        .map(|_| true),
        Cmd::Check { verbose } => cmd_check(&cli, *verbose),
        #[cfg(feature = "bridge")]
        Cmd::CheckBridge => cmd_check_bridge(&cli),
        #[cfg(feature = "bridge")]
        Cmd::CheckConformance { args } => {
            let mut mc = mc_unpack::bridge::Minecraft::from_assets(one_assets(&cli)?)?;
            if cli.with_worlds {
                mc = mc.with_discovered_saves();
            }
            std::process::exit(rewrite_bridge::conformance::cli::run(&mc, args.clone()));
        }
    })();
    match res {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("error: {}", esc(&format!("{e:#}")));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_respects_the_path_boundary() {
        let t = |p: &str| tilde_with(Path::new(p), "/home/ann");
        assert_eq!(t("/home/ann"), "~");
        assert_eq!(t("/home/ann/x/y.jar"), "~/x/y.jar");
        assert_eq!(t("/home/ann2/x"), "/home/ann2/x");
        assert_eq!(t("/srv/home/ann/x"), "/srv/home/ann/x");
        assert_eq!(tilde_with(Path::new("/x"), "/"), "/x");
    }
}
