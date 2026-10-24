//! The text `list --states` and `inspect` print, built as lines so that it can be tested. Anything
//! that came from a jar (a state key, a texture name, a note) is escaped with
//! [`crate::jar::escape_control`], so that a hostile jar cannot rewrite a terminal. The names that
//! [`crate::bridge`] gives (`Model::name`, `AssetInfo::name`) carry the raw state text instead:
//! they are for programs, which should escape what they print.

use anyhow::Result;

use crate::assets::{Assets, BakedModel};
use crate::jar::escape_control as esc;

/// The lines `list --states` prints for one block. Text from the jar is escaped.
pub fn states_lines(a: &Assets, name: &str) -> Result<Vec<String>> {
    let bs = a.blockstate(name)?;
    let default = a.default_state(name)?;
    let mut out = Vec::new();
    if !bs.variants.is_empty() {
        for k in bs.keys() {
            let mark = if default.as_deref() == Some(k) {
                "   <- what `block/<name>` with no state builds"
            } else {
                ""
            };
            let shown = if k.is_empty() {
                "(no properties)".to_string()
            } else {
                esc(k)
            };
            out.push(format!("    {shown}{mark}"));
        }
    } else {
        for (p, vs) in bs.property_values() {
            out.push(format!(
                "    {}: {}",
                esc(&p),
                esc(&vs.into_iter().collect::<Vec<_>>().join(" | "))
            ));
        }
        if let Some(d) = default {
            out.push(format!(
                "    with no state, `block/<name>` builds `{}`",
                esc(&d)
            ));
        }
    }
    Ok(out)
}

/// The lines `inspect` prints. Text from the jar is escaped.
pub fn inspect_lines(id: &str, b: &BakedModel) -> Vec<String> {
    let r = &b.report;
    let mut out = vec![
        id.to_string(),
        format!("  quads: {}, parts: {}", b.quad_count(), b.parts.len()),
    ];
    match &r.state_used {
        Some(s) => out.push(format!("  state used: {}", esc(s))),
        None => out.push("  state used: (none: the file has no properties to choose)".into()),
    }
    if r.candidates > 1 {
        out.push(format!(
            "  fits {} entries of the blockstate; the first was used",
            r.candidates
        ));
    }
    if !r.ignored_properties.is_empty() {
        out.push(format!(
            "  ignored properties: {}",
            esc(&r.ignored_properties.join(", "))
        ));
    }
    for t in b.texture_paths() {
        out.push(format!("  texture: {}", esc(t)));
    }
    let tints: Vec<String> = b.tint_indices().iter().map(|i| i.to_string()).collect();
    if !tints.is_empty() {
        out.push(format!("  tintindex used: {}", tints.join(", ")));
    }
    for n in &r.notes {
        out.push(format!("  note: {}", esc(n)));
    }
    for f in &r.failed_parts {
        out.push(format!("  FAILED part: {}", esc(f)));
    }
    for d in &r.dropped_parts {
        out.push(format!("  left-out part: {}", esc(d)));
    }
    if let Some(lo) = &r.left_out {
        out.push(format!("  LEFT OUT: {}", esc(&lo.to_string())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_a_jar_is_escaped_when_printed() {
        use crate::jar::testjar::{png, JarBuilder};
        // a state key, a texture name and a model note with an escape sequence in each
        let jar = JarBuilder::new()
            .add(
                "assets/minecraft/blockstates/x.json",
                "{\"variants\":{\"a=\\u001b[31mred\":{\"model\":\"block/m\"},\"a=b\":{\"model\":\"block/m\"}}}",
            )
            .add(
                "assets/minecraft/models/block/m.json",
                "{\"textures\":{\"all\":\"block/t\\u001b[2J\"},\"elements\":[{\"from\":[0,0,0],\"to\":[16,16,16],\"faces\":{\"up\":{\"texture\":\"#all\"},\"down\":{\"texture\":\"#\\u001b[0mnone\"}}}]}",
            )
            .add("assets/minecraft/textures/block/t\u{1b}[2J.png", png(1, 1, |_, _| [1, 1, 1, 255]))
            .build();
        let a = Assets::from_jar(jar).unwrap();
        let lines = states_lines(&a, "x").unwrap();
        assert!(
            lines.iter().any(|l| l.contains("\\u{1b}[31mred")),
            "{lines:?}"
        );
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')), "{lines:?}");
        let b = a
            .bake_block(
                "x",
                &crate::blockstate::State::parse("a=\u{1b}[31mred").unwrap(),
            )
            .unwrap();
        let lines = inspect_lines("block/x", &b);
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("state used") && l.contains("\\u{1b}")),
            "{lines:?}"
        );
    }
}
