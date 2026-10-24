use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

use mc_unpack::assets::{AssetId, Assets, LeftOut};
use mc_unpack::check;
use mc_unpack::gltf;
use mc_unpack::install::{find_jars, JarFile, Source};
use mc_unpack::jar::escape_control as esc;
use mc_unpack::mesh::{self, MeshOptions, Origin};
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
                    v.id,
                    j.source.name(),
                    sup,
                    from,
                    v.layout.name()
                );
                println!("         {}", tilde(&j.path));
            }
            Err(e) => println!("{:<8} {:<17} unreadable: {e:#}", j.version, j.source.name()),
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
            println!("{} ({}): {}", why, ids.len(), ids.join(" "));
        }
    } else {
        eprintln!("{shown} listed");
    }
    Ok(())
}

fn print_states(a: &Assets, name: &str) -> Result<()> {
    let bs = a.blockstate(name)?;
    let default = a.default_state(name)?;
    if !bs.variants.is_empty() {
        for k in bs.keys() {
            let mark = if default.as_deref() == Some(k) {
                "   <- what `block/<name>` with no state builds"
            } else {
                ""
            };
            println!(
                "    {}{mark}",
                if k.is_empty() { "(no properties)" } else { k }
            );
        }
    } else {
        for (p, vs) in bs.property_values() {
            println!(
                "    {p}: {}",
                vs.into_iter().collect::<Vec<_>>().join(" | ")
            );
        }
        if let Some(d) = default {
            println!("    with no state, `block/<name>` builds `{d}`");
        }
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
        println!("  {} {:?}{tint}", g.texture, g.alpha);
    }
    Ok(())
}

fn cmd_inspect(a: Assets, id: String) -> Result<()> {
    let b = a.bake_id(&id)?;
    let r = &b.report;
    println!("{id}");
    println!("  quads: {}, parts: {}", b.quad_count(), b.parts.len());
    match &r.state_used {
        Some(s) => println!("  state used: {s}"),
        None => println!("  state used: (none: the file has no properties to choose)"),
    }
    if r.candidates > 1 {
        println!(
            "  fits {} entries of the blockstate; the first was used",
            r.candidates
        );
    }
    if !r.ignored_properties.is_empty() {
        println!("  ignored properties: {}", r.ignored_properties.join(", "));
    }
    for t in b.texture_paths() {
        println!("  texture: {t}");
    }
    let tints: Vec<String> = b.tint_indices().iter().map(|i| i.to_string()).collect();
    if !tints.is_empty() {
        println!("  tintindex used: {}", tints.join(", "));
    }
    for n in &r.notes {
        println!("  note: {}", esc(n));
    }
    for f in &r.failed_parts {
        println!("  FAILED part: {}", esc(f));
    }
    for d in &r.dropped_parts {
        println!("  left-out part: {}", esc(d));
    }
    if let Some(lo) = &r.left_out {
        println!("  LEFT OUT: {}", esc(&lo.to_string()));
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
        for (i, f) in t.all_frames().iter().enumerate() {
            std::fs::write(out.join(format!("{base}_{i}.png")), encode_png(f)?)?;
        }
        println!("wrote {} frames to {}", t.sheet_frames(), tilde(&out));
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
                println!("{} ({}): unreadable: {e:#}", j.version, j.source.name());
                continue;
            }
        };
        let v = a.version();
        println!(
            "== Minecraft {} ({}) - {}",
            v.id,
            j.source.name(),
            v.layout.name()
        );
        match check::run(&a) {
            Err(e) => println!("   {e:#}"),
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
                        println!("   blocks left out ({}): {}", ids.len(), why);
                        if verbose {
                            println!("      {}", ids.join(" "));
                        }
                    }
                    for (why, ids) in &r.items_left_out {
                        println!("   items left out ({}): {}", ids.len(), why);
                        if verbose {
                            println!("      {}", ids.join(" "));
                        }
                    }
                }
                for f in &r.failures {
                    println!("   FAIL {f}");
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
                println!("{}: unreadable: {e:#}", j.version);
                continue;
            }
        };
        let id = a.version().id.clone();
        let mc = match Minecraft::from_assets(a) {
            Ok(m) => m,
            Err(e) => {
                println!("== Minecraft {id}: no provider: {e:#}");
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
            "== Minecraft {id}: {} listed, {ok} loaded ({partial} of them with a part left out), {failed} failed ({tris} triangles, {mats} materials, {:.1} s)",
            list.len(),
            start.elapsed().as_secs_f64()
        );
        for f in fails.iter().take(40) {
            println!("   FAIL {f}");
        }
        all_ok &= failed == 0;
    }
    println!("peak RSS {} MB", check::peak_rss() / 1_000_000);
    Ok(all_ok)
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
        Cmd::Check { verbose } => cmd_check(&cli, *verbose),
        #[cfg(feature = "bridge")]
        Cmd::CheckBridge => cmd_check_bridge(&cli),
        #[cfg(feature = "bridge")]
        Cmd::CheckConformance { args } => {
            let mc = mc_unpack::bridge::Minecraft::from_assets(one_assets(&cli)?)?;
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
