//! `assets/minecraft/blockstates/<name>.json`: which model (and rotation) a block uses in a state.
//!
//! Two forms exist, and a file has one of them:
//!
//! - `variants`: a map from a state key (`"facing=east,half=bottom"`, or `""`, or in old versions
//!   `"normal"`) to one variant or a list of weighted ones;
//! - `multipart`: a list of parts, each `apply`ing a model when its `when` condition holds (or
//!   always, without `when`). Several parts can apply at once (fence arms, redstone wire).
//!
//! A *state* here is a string of `property=value` pairs separated by commas. The game's own default
//! state (what a placed block starts as) is decided in code, not in the data, so "the default"
//! below is only a rule about the file: see [`BlockState::select`].

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

/// One model choice in a blockstate
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    /// As written in the file (`minecraft:block/stone`, or `stone` in 1.8 to 1.12)
    pub model: String,
    /// Rotation about X in degrees, a multiple of 90 in 0..360
    pub x: i32,
    pub y: i32,
    /// 26.x blockstates may turn about Z too (none of the jars here do); the game applies x, then
    /// y, then z. Older versions only know x and y: [`crate::assets::Assets`] leaves `z` out there
    pub z: i32,
    pub uvlock: bool,
    pub weight: u32,
}

/// The entries of one `variants` key or one `apply`: one variant, or several to choose from by
/// weight (the game picks by position hash; this crate offers all and uses the first)
pub type Choice = Vec<Variant>;

#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// `{"prop": "a|b", ...}`: every property must match one of its alternatives (`!a` negates)
    Props(Vec<(String, String)>),
    Or(Vec<When>),
    And(Vec<When>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub when: Option<When>,
    pub apply: Choice,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockState {
    /// In file order
    pub variants: Vec<(String, Choice)>,
    pub multipart: Vec<Part>,
}

/// `property=value` pairs
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State(pub BTreeMap<String, String>);

impl State {
    /// Parses `a=b,c=d`. An empty string, `normal` and `inventory` are the empty state: they are
    /// how old and new files write "no properties".
    pub fn parse(s: &str) -> Result<State> {
        let s = s.trim();
        let mut m = BTreeMap::new();
        if s.is_empty() || s == "normal" || s == "inventory" {
            return Ok(State(m));
        }
        for pair in s.split(',') {
            let Some((k, v)) = pair.split_once('=') else {
                bail!("{pair:?} in {s:?} is not property=value");
            };
            let (k, v) = (k.trim(), v.trim());
            if k.is_empty() || v.is_empty() {
                bail!("{pair:?} in {s:?} is not property=value");
            }
            m.insert(k.to_string(), v.to_string());
        }
        Ok(State(m))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `a=b,c=d` with the keys sorted
    pub fn to_key(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Which variants or parts a state picked, and what was ignored
#[derive(Clone, Debug, PartialEq)]
pub struct Selection<'a> {
    /// The variants that apply: one for a `variants` file, any number for `multipart`. For a choice
    /// with several weighted entries, the first.
    pub chosen: Vec<&'a Variant>,
    /// The key of the `variants` entry that was picked (None for multipart)
    pub key: Option<String>,
    /// Properties in the given state that no key or condition of the file mentions
    pub ignored: Vec<String>,
    /// How many `variants` keys fit the state (more than one: it was partial and the first won)
    pub candidates: usize,
    /// Conditions this crate would not decide, and why (see [`NegationRule`])
    pub notes: Vec<String>,
}

/// How a `!` in a multipart condition value works. It differs between versions of the game:
/// 1.8 to 1.12 read a leading `!` as "not any of what follows" (`!a|b` is neither `a` nor `b`);
/// 26.x negates each term on its own (`!a|b` is "not `a`, or `b`"). A value with one term reads the
/// same either way. The jars between (1.13 to 1.21.3) are not confirmed, so there a negation
/// combined with `|` is refused: the part does not apply, and a note says so. None of the jars
/// here has a `!` anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegationRule {
    WholeValue,
    PerTerm,
    RefuseMixed,
}

impl BlockState {
    pub fn parse(text: impl AsRef<[u8]>) -> Result<BlockState> {
        let v: Value = crate::jar::parse_json(text.as_ref()).context("blockstate is not JSON")?;
        let Some(o) = v.as_object() else {
            bail!("blockstate is not an object");
        };
        let mut variants = Vec::new();
        if let Some(vs) = o.get("variants") {
            let Some(m) = vs.as_object() else {
                bail!("`variants` is not an object");
            };
            for (k, c) in m {
                variants.push((
                    k.clone(),
                    parse_choice(c).with_context(|| format!("variant {k:?}"))?,
                ));
            }
        }
        let mut multipart = Vec::new();
        if let Some(mp) = o.get("multipart") {
            let Some(list) = mp.as_array() else {
                bail!("`multipart` is not a list");
            };
            for (i, p) in list.iter().enumerate() {
                let Some(po) = p.as_object() else {
                    bail!("multipart[{i}] is not an object");
                };
                let apply = parse_choice(
                    po.get("apply")
                        .with_context(|| format!("multipart[{i}] has no `apply`"))?,
                )
                .with_context(|| format!("multipart[{i}]"))?;
                let when = match po.get("when") {
                    None => None,
                    Some(w) => Some(parse_when(w).with_context(|| format!("multipart[{i}].when"))?),
                };
                multipart.push(Part { when, apply });
            }
        }
        if variants.is_empty() && multipart.is_empty() {
            bail!("neither `variants` nor `multipart`");
        }
        Ok(BlockState {
            variants,
            multipart,
        })
    }

    pub fn is_multipart(&self) -> bool {
        !self.multipart.is_empty()
    }

    /// Every distinct state key of a `variants` file, in file order
    pub fn keys(&self) -> Vec<&str> {
        self.variants.iter().map(|(k, _)| k.as_str()).collect()
    }

    /// Every variant mentioned anywhere (all entries of all choices), for sweeps that must bake
    /// every model a file refers to
    pub fn all_variants(&self) -> impl Iterator<Item = &Variant> {
        self.variants
            .iter()
            .flat_map(|(_, c)| c.iter())
            .chain(self.multipart.iter().flat_map(|p| p.apply.iter()))
    }

    /// The property names a file mentions
    pub fn properties(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for (k, _) in &self.variants {
            if let Ok(s) = State::parse(k) {
                set.extend(s.0.into_keys());
            }
        }
        fn walk(w: &When, set: &mut std::collections::BTreeSet<String>) {
            match w {
                When::Props(p) => set.extend(p.iter().map(|(k, _)| k.clone())),
                When::Or(l) | When::And(l) => l.iter().for_each(|x| walk(x, set)),
            }
        }
        for p in &self.multipart {
            if let Some(w) = &p.when {
                walk(w, &mut set);
            }
        }
        set.into_iter().collect()
    }

    /// A state that makes at least one multipart part apply, built from the first conditional part's
    /// condition (its first alternative for every property). Used when a multipart block is asked
    /// for with no state at all and nothing unconditional applies (redstone wire, vines ...).
    pub fn first_applying_state(&self) -> Option<State> {
        fn build(w: &When, st: &mut BTreeMap<String, String>) -> bool {
            match w {
                When::Props(ps) => {
                    for (k, v) in ps {
                        if v.starts_with('!') {
                            continue;
                        }
                        let first = v.split('|').next().unwrap_or("");
                        st.insert(k.clone(), first.to_string());
                    }
                    true
                }
                When::Or(l) => l.first().is_some_and(|x| build(x, st)),
                When::And(l) => l.iter().all(|x| build(x, st)),
            }
        }
        for p in &self.multipart {
            if let Some(w) = &p.when {
                let mut m = BTreeMap::new();
                if build(w, &mut m) {
                    let st = State(m);
                    if self.select(&st).is_ok_and(|s| !s.chosen.is_empty()) {
                        return Some(st);
                    }
                }
            }
        }
        None
    }

    /// Every property the file mentions with the values it mentions for it, for error messages and
    /// listings (`facing: [east, north ...]`)
    pub fn property_values(&self) -> BTreeMap<String, std::collections::BTreeSet<String>> {
        let mut m: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for (k, _) in &self.variants {
            if let Ok(s) = State::parse(k) {
                for (p, v) in s.0 {
                    m.entry(p).or_default().insert(v);
                }
            }
        }
        fn walk(w: &When, m: &mut BTreeMap<String, std::collections::BTreeSet<String>>) {
            match w {
                When::Props(ps) => {
                    for (k, v) in ps {
                        for alt in v.trim_start_matches('!').split('|') {
                            m.entry(k.clone()).or_default().insert(alt.to_string());
                        }
                    }
                }
                When::Or(l) | When::And(l) => l.iter().for_each(|x| walk(x, m)),
            }
        }
        for p in &self.multipart {
            if let Some(w) = &p.when {
                walk(w, &mut m);
            }
        }
        m
    }

    /// Splits `state` into the properties this file mentions and the ones it does not
    pub fn split_known(&self, state: &State) -> (State, Vec<String>) {
        let known = self.properties();
        let mut keep = BTreeMap::new();
        let mut ignored = Vec::new();
        for (k, v) in &state.0 {
            if known.contains(k) {
                keep.insert(k.clone(), v.clone());
            } else {
                ignored.push(k.clone());
            }
        }
        (State(keep), ignored)
    }

    /// Picks what `state` selects.
    ///
    /// - **variants**: the first entry (in file order) whose key agrees with every property of
    ///   `state` that the key also names. With an empty `state` that is the entry with key `""` or
    ///   `normal` if there is one, else the first entry of the file. The game's real default state
    ///   is in code, so for a block such as stairs, whose keys are all complete, name the state you
    ///   want.
    ///   A property the file does not mention is ignored and listed in [`Selection::ignored`].
    /// - **multipart**: every part without `when`, and every part whose `when` holds. A property
    ///   missing from `state` never matches a positive condition (and always matches a negated one).
    pub fn select(&self, state: &State) -> Result<Selection<'_>> {
        self.select_with(state, NegationRule::PerTerm)
    }

    /// [`BlockState::select`] with the version's rule for `!`
    pub fn select_with(&self, state: &State, rule: NegationRule) -> Result<Selection<'_>> {
        // A property the file never mentions cannot choose anything. A state made only of such
        // properties is the empty state: the default is used and the properties are listed.
        let (state, ignored) = self.split_known(state);
        let state = &state;
        if !self.variants.is_empty() {
            let mut candidates = 0;
            let mut found: Option<&(String, Choice)> = None;
            if state.is_empty() {
                found = self
                    .variants
                    .iter()
                    .find(|(k, _)| k.is_empty() || k == "normal")
                    .or_else(|| self.variants.first());
                candidates = self.variants.len();
            } else {
                for e in &self.variants {
                    if e.0 == "inventory" {
                        // the item-in-hand entry of 1.8 to 1.12 files, never a placed state
                        continue;
                    }
                    let ks = State::parse(&e.0)?;
                    let agrees =
                        ks.0.iter()
                            .all(|(k, v)| state.0.get(k).is_none_or(|sv| sv == v));
                    // A key with no property in common with the state would "agree" with anything
                    let shares = ks.0.is_empty() || ks.0.keys().any(|k| state.0.contains_key(k));
                    if agrees && shares {
                        candidates += 1;
                        if found.is_none() {
                            found = Some(e);
                        }
                    }
                }
            }
            let Some((key, choice)) = found else {
                bail!(
                    "no variant fits `{}`; the file has: {}",
                    state.to_key(),
                    self.keys().join(" | ")
                );
            };
            return Ok(Selection {
                chosen: choice.first().into_iter().collect(),
                key: Some(key.clone()),
                ignored,
                candidates,
                notes: Vec::new(),
            });
        }
        let mut chosen = Vec::new();
        let mut notes = Vec::new();
        for p in &self.multipart {
            let ok = match &p.when {
                None => true,
                Some(w) => eval_when(w, state, rule, &mut notes),
            };
            if ok {
                if let Some(v) = p.apply.first() {
                    chosen.push(v);
                }
            }
        }
        Ok(Selection {
            chosen,
            key: None,
            ignored,
            candidates: 1,
            notes,
        })
    }
}

fn parse_choice(v: &Value) -> Result<Choice> {
    match v {
        Value::Object(_) => Ok(vec![parse_variant(v)?]),
        Value::Array(a) if !a.is_empty() => a.iter().map(parse_variant).collect(),
        _ => bail!("expected a variant object or a non-empty list of them"),
    }
}

fn parse_variant(v: &Value) -> Result<Variant> {
    let Some(o) = v.as_object() else {
        bail!("variant is not an object");
    };
    let model = o
        .get("model")
        .and_then(|m| m.as_str())
        .context("variant without a `model`")?
        .to_string();
    let rot = |k: &str| -> Result<i32> {
        match o.get(k) {
            None => Ok(0),
            Some(n) => {
                let d = n
                    .as_i64()
                    .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                    .with_context(|| format!("`{k}` is not a whole number"))?;
                if d % 90 != 0 {
                    bail!("`{k}` is {d}: the game only accepts multiples of 90");
                }
                Ok(d.rem_euclid(360) as i32)
            }
        }
    };
    Ok(Variant {
        model,
        x: rot("x")?,
        y: rot("y")?,
        z: rot("z")?,
        uvlock: match o.get("uvlock") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => bail!("`uvlock` is not true or false"),
        },
        // `weight` defaults to 1 in the format
        weight: match o.get("weight") {
            None | Some(Value::Null) => 1,
            Some(w) => w
                .as_u64()
                .filter(|w| *w >= 1 && *w <= u32::MAX as u64)
                .ok_or_else(|| anyhow::anyhow!("`weight` is not a positive whole number"))?
                as u32,
        },
    })
}

fn prop_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_when(v: &Value) -> Result<When> {
    let Some(o) = v.as_object() else {
        bail!("`when` is not an object");
    };
    if o.len() == 1 {
        for (key, ctor) in [
            ("OR", When::Or as fn(Vec<When>) -> When),
            ("AND", When::And),
        ] {
            if let Some(list) = o.get(key) {
                let Some(a) = list.as_array() else {
                    bail!("`{key}` is not a list");
                };
                return Ok(ctor(a.iter().map(parse_when).collect::<Result<_>>()?));
            }
        }
    }
    props(o)
}

fn props(o: &Map<String, Value>) -> Result<When> {
    let mut v = Vec::new();
    for (k, x) in o {
        let Some(t) = prop_text(x) else {
            bail!("condition on {k:?} is not a string");
        };
        v.push((k.clone(), t));
    }
    Ok(When::Props(v))
}

fn eval_when(w: &When, state: &State, rule: NegationRule, notes: &mut Vec<String>) -> bool {
    match w {
        When::Or(l) => l.iter().any(|x| eval_when(x, state, rule, notes)),
        When::And(l) => l.iter().all(|x| eval_when(x, state, rule, notes)),
        When::Props(p) => p.iter().all(|(k, spec)| {
            let terms: Vec<&str> = spec.split('|').collect();
            let have = state.0.get(k);
            let hit = |t: &str| have.is_some_and(|h| h == t);
            match rule {
                NegationRule::WholeValue => {
                    let (neg, rest) = match spec.strip_prefix('!') {
                        Some(r) => (true, r),
                        None => (false, spec.as_str()),
                    };
                    let any = rest.split('|').any(hit);
                    // a property the state lacks matches nothing, so its negation holds
                    any != neg
                }
                NegationRule::PerTerm => terms.iter().any(|t| match t.strip_prefix('!') {
                    Some(n) => !hit(n),
                    None => hit(t),
                }),
                NegationRule::RefuseMixed => {
                    if terms.len() > 1 && terms.iter().any(|t| t.starts_with('!')) {
                        notes.push(format!(
                            "the condition `{k}: {spec}` mixes `!` with `|`, which these versions of the game are not confirmed to read one way: the part does not apply"
                        ));
                        return false;
                    }
                    match spec.strip_prefix('!') {
                        Some(n) => !hit(n),
                        // plain alternatives (`2|3|4`): any of them
                        None => terms.iter().any(|t| hit(t)),
                    }
                }
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAIRS: &str = r#"{"variants":{
        "facing=east,half=bottom":{"model":"minecraft:block/s"},
        "facing=west,half=bottom":{"model":"minecraft:block/s","y":180,"uvlock":true},
        "facing=east,half=top":{"model":"minecraft:block/s","x":180}}}"#;

    #[test]
    fn variants_pick_by_state() {
        let b = BlockState::parse(STAIRS).unwrap();
        let s = b
            .select(&State::parse("facing=west,half=bottom").unwrap())
            .unwrap();
        assert_eq!(s.chosen[0].y, 180);
        assert!(s.chosen[0].uvlock);
        assert_eq!(s.candidates, 1);
        let s = b
            .select(&State::parse("half=top,facing=east").unwrap())
            .unwrap();
        assert_eq!(s.chosen[0].x, 180);
    }

    #[test]
    fn partial_state_takes_the_first_fit_and_says_so() {
        let b = BlockState::parse(STAIRS).unwrap();
        let s = b.select(&State::parse("half=bottom").unwrap()).unwrap();
        assert_eq!(s.key.as_deref(), Some("facing=east,half=bottom"));
        assert_eq!(s.candidates, 2);
    }

    #[test]
    fn unknown_property_is_ignored_and_reported() {
        let b = BlockState::parse(STAIRS).unwrap();
        let s = b
            .select(&State::parse("facing=east,half=top,waterlogged=true").unwrap())
            .unwrap();
        assert_eq!(s.ignored, vec!["waterlogged"]);
        assert_eq!(s.chosen[0].x, 180);
    }

    #[test]
    fn no_match_lists_the_keys() {
        let b = BlockState::parse(STAIRS).unwrap();
        let e = b.select(&State::parse("facing=up").unwrap()).unwrap_err();
        assert!(e.to_string().contains("facing=east,half=bottom"), "{e}");
    }

    #[test]
    fn empty_state_prefers_the_empty_or_normal_key() {
        let b =
            BlockState::parse(r#"{"variants":{"a=1":{"model":"m1"},"":{"model":"m2"}}}"#).unwrap();
        assert_eq!(b.select(&State::default()).unwrap().chosen[0].model, "m2");
        let n = BlockState::parse(r#"{"variants":{"a=1":{"model":"m1"},"normal":{"model":"m3"}}}"#)
            .unwrap();
        assert_eq!(n.select(&State::default()).unwrap().chosen[0].model, "m3");
        let f = BlockState::parse(r#"{"variants":{"a=1":{"model":"m1"},"a=2":{"model":"m4"}}}"#)
            .unwrap();
        assert_eq!(f.select(&State::default()).unwrap().chosen[0].model, "m1");
    }

    #[test]
    fn weighted_list_takes_first() {
        let b = BlockState::parse(
            r#"{"variants":{"":[{"model":"a","weight":5},{"model":"b","y":90}]}}"#,
        )
        .unwrap();
        let s = b.select(&State::default()).unwrap();
        assert_eq!(s.chosen.len(), 1);
        assert_eq!(s.chosen[0].model, "a");
        assert_eq!(b.all_variants().count(), 2);
    }

    const FENCE: &str = r#"{"multipart":[
        {"apply":{"model":"post"}},
        {"when":{"north":"true"},"apply":{"model":"side","uvlock":true}},
        {"when":{"east":"true"},"apply":{"model":"side","y":90}},
        {"when":{"OR":[{"north":"false","east":"false"},{"south":"true"}]},"apply":{"model":"lone"}},
        {"when":{"half":"!top"},"apply":{"model":"notTop"}}]}"#;

    #[test]
    fn multipart_conditions() {
        let b = BlockState::parse(FENCE).unwrap();
        let m = |s: &str| -> Vec<String> {
            b.select(&State::parse(s).unwrap())
                .unwrap()
                .chosen
                .iter()
                .map(|v| format!("{}{}", v.model, v.y))
                .collect()
        };
        assert_eq!(m(""), vec!["post0", "notTop0"]);
        assert_eq!(
            m("north=true,east=true"),
            vec!["post0", "side0", "side90", "notTop0"]
        );
        assert_eq!(m("north=false,east=false,half=top"), vec!["post0", "lone0"]);
        assert_eq!(
            m("south=true,half=bottom"),
            vec!["post0", "lone0", "notTop0"]
        );
    }

    #[test]
    fn alternatives_and_negation() {
        let b = BlockState::parse(
            r#"{"multipart":[{"when":{"north":"side|up"},"apply":{"model":"s"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            b.select(&State::parse("north=up").unwrap())
                .unwrap()
                .chosen
                .len(),
            1
        );
        assert_eq!(
            b.select(&State::parse("north=none").unwrap())
                .unwrap()
                .chosen
                .len(),
            0
        );
    }

    #[test]
    fn rotations_must_be_quarter_turns() {
        assert!(BlockState::parse(r#"{"variants":{"":{"model":"m","y":45}}}"#).is_err());
        let b = BlockState::parse(r#"{"variants":{"":{"model":"m","y":-90}}}"#).unwrap();
        assert_eq!(b.variants[0].1[0].y, 270);
    }

    #[test]
    fn structure_errors() {
        assert!(BlockState::parse("{}").is_err());
        assert!(BlockState::parse("[]").is_err());
        assert!(BlockState::parse(r#"{"variants":{"":{}}}"#).is_err());
        assert!(State::parse("a").is_err());
    }

    #[test]
    fn a_state_that_applies_something_is_found_from_the_data() {
        let b = BlockState::parse(
            r#"{"multipart":[{"when":{"OR":[{"north":"none","south":"none"},{"east":"side|up"}]},"apply":{"model":"dot"}},
                             {"when":{"north":"side|up"},"apply":{"model":"line"}}]}"#,
        )
        .unwrap();
        assert!(b.select(&State::default()).unwrap().chosen.is_empty());
        let st = b.first_applying_state().unwrap();
        assert_eq!(st.to_key(), "north=none,south=none");
        assert_eq!(b.select(&st).unwrap().chosen[0].model, "dot");
        // a file with variants has no use for it
        let v = BlockState::parse(r#"{"variants":{"":{"model":"m"}}}"#).unwrap();
        assert_eq!(v.first_applying_state(), None);
    }

    #[test]
    fn state_keys_round_trip() {
        let s = State::parse("b=2,a=1").unwrap();
        assert_eq!(s.to_key(), "a=1,b=2");
    }

    #[test]
    fn a_state_made_only_of_unknown_properties_is_the_empty_state() {
        let b = BlockState::parse(STAIRS).unwrap();
        let s = b
            .select(&State::parse("waterlogged=true").unwrap())
            .unwrap();
        assert_eq!(s.ignored, vec!["waterlogged"]);
        assert_eq!(s.key.as_deref(), Some("facing=east,half=bottom"));
        assert_eq!(s.chosen.len(), 1);
        let f = BlockState::parse(FENCE).unwrap();
        let s = f.select(&State::parse("power=2").unwrap()).unwrap();
        assert_eq!(s.chosen[0].model, "post");
        assert_eq!(s.ignored, vec!["power"]);
    }

    #[test]
    fn variant_fields_of_the_wrong_type_are_errors() {
        for bad in [
            r#"{"variants":{"":{"model":"m","uvlock":"yes"}}}"#,
            r#"{"variants":{"":{"model":"m","weight":0}}}"#,
            r#"{"variants":{"":{"model":"m","weight":"3"}}}"#,
        ] {
            assert!(BlockState::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn plain_alternatives_work_under_every_negation_rule() {
        // `2|3|4` has no `!`: all three rules read it as "any of these" (pink_petals in 1.20 and
        // 1.21 writes its conditions that way)
        let b =
            BlockState::parse(r#"{"multipart":[{"when":{"n":"2|3|4"},"apply":{"model":"m"}}]}"#)
                .unwrap();
        for rule in [
            NegationRule::WholeValue,
            NegationRule::PerTerm,
            NegationRule::RefuseMixed,
        ] {
            for (n, want) in [("1", 0), ("2", 1), ("4", 1)] {
                let s = b
                    .select_with(&State::parse(&format!("n={n}")).unwrap(), rule)
                    .unwrap();
                assert_eq!(s.chosen.len(), want, "{rule:?} n={n}");
                assert!(s.notes.is_empty());
            }
        }
    }

    #[test]
    fn z_is_read_from_a_variant() {
        let b =
            BlockState::parse(r#"{"variants":{"":{"model":"m","x":90,"y":180,"z":270}}}"#).unwrap();
        let v = &b.variants[0].1[0];
        assert_eq!((v.x, v.y, v.z), (90, 180, 270));
        assert_eq!(
            BlockState::parse(r#"{"variants":{"":{"model":"m"}}}"#)
                .unwrap()
                .variants[0]
                .1[0]
                .z,
            0
        );
        assert!(BlockState::parse(r#"{"variants":{"":{"model":"m","z":45}}}"#).is_err());
    }

    #[test]
    fn the_two_negation_rules_differ_only_when_a_value_has_alternatives() {
        let b = BlockState::parse(
            r#"{"multipart":[{"when":{"p":"!a|b"},"apply":{"model":"m"}},{"when":{"q":"!a"},"apply":{"model":"n"}}]}"#,
        )
        .unwrap();
        let hit = |rule, s: &str| -> Vec<String> {
            b.select_with(&State::parse(s).unwrap(), rule)
                .unwrap()
                .chosen
                .iter()
                .map(|v| v.model.clone())
                .collect()
        };
        // a single negated term reads the same either way
        for rule in [
            NegationRule::WholeValue,
            NegationRule::PerTerm,
            NegationRule::RefuseMixed,
        ] {
            assert_eq!(hit(rule, "q=b,p=a"), ["n"]);
            assert!(hit(rule, "q=a,p=a").is_empty());
        }
        // `!a|b`: whole value = neither; per term = not a, or b
        assert_eq!(hit(NegationRule::WholeValue, "p=c,q=a"), ["m"]);
        assert!(hit(NegationRule::WholeValue, "p=b,q=a").is_empty());
        assert_eq!(hit(NegationRule::PerTerm, "p=b,q=a"), ["m"]);
        assert_eq!(hit(NegationRule::PerTerm, "p=c,q=a"), ["m"]);
        assert!(hit(NegationRule::PerTerm, "p=a,q=a").is_empty());
        let s = b
            .select_with(&State::parse("p=c,q=a").unwrap(), NegationRule::RefuseMixed)
            .unwrap();
        assert!(s.chosen.is_empty());
        assert_eq!(s.notes.len(), 1);
    }
}
