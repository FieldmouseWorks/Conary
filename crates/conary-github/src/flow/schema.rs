//! The published schema of the flow's configuration ([`super::Config`]),
//! conaryd's events configuration ([`crate::events::Runtime`]) and its
//! gate-reuse configuration ([`crate::reuse::Settings`]), as
//! `cn flow settings --schema` prints it. It's generated from the types their
//! loaders parse, so no option exists without its entry, and [`of`] holds
//! every entry to what the schema promises its readers.
//!
//! Beside JSON Schema's own keywords, each option says:
//! - `x-spender`: whether it starts paid work. One that does defaults off or
//!   zero, so nothing is spent until a machine turns it on.
//! - `x-live`: whether a change takes effect at the next decision that reads
//!   it. One that doesn't says in `x-needs` what a change needs, and why.
//! - `x-secret`: whether its value is kept out of what Conary prints.
use super::persist::Result;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The flow configuration's schema.
pub fn flow() -> Result<Value> {
    of::<super::Config>()
}
/// conaryd's events configuration's schema.
pub fn events() -> Result<Value> {
    of::<crate::events::Runtime>()
}
/// conaryd's gate-reuse configuration's schema.
pub fn reuse() -> Result<Value> {
    of::<crate::reuse::Settings>()
}

/// `T`'s schema, with every option's `x-live` said and its description on
/// one line, once every option of every struct in it has passed [`check`].
pub fn of<T: JsonSchema>() -> Result<Value> {
    let mut schema = schemars::schema_for!(T).to_value();
    one_line(&mut schema);
    let problems = check(&mut schema);
    if problems.is_empty() {
        Ok(schema)
    } else {
        Err(format!(
            "the configuration schema is incomplete: {}",
            problems.join("; ")
        )
        .into())
    }
}

/// What's wrong with each option of each struct in `schema`, filling in the
/// `x-live` an option leaves to its default (live) as it goes.
fn check(schema: &mut Value) -> Vec<String> {
    let mut problems = Vec::new();
    let title = schema["title"]
        .as_str()
        .unwrap_or("the configuration")
        .to_owned();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        options(&title, properties, &mut problems);
    }
    if let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) {
        for (name, def) in defs {
            if let Some(properties) = def.get_mut("properties").and_then(Value::as_object_mut) {
                options(name, properties, &mut problems);
            }
        }
    }
    problems
}
fn options(owner: &str, properties: &mut Map<String, Value>, problems: &mut Vec<String>) {
    for (name, option) in properties {
        let Some(option) = option.as_object_mut() else {
            problems.push(format!("{owner}.{name} has no entry"));
            continue;
        };
        let mut problem = |what: &str| problems.push(format!("{owner}.{name} {what}"));
        if !option
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
        {
            problem("has no description");
        }
        match option.get("x-spender") {
            Some(Value::Bool(true)) => {
                if !option.get("default").is_some_and(off) {
                    problem("starts paid work, so it must default off or zero");
                }
            }
            Some(Value::Bool(false)) => {}
            _ => problem("doesn't say whether it starts paid work (x-spender)"),
        }
        let needs = option
            .get("x-needs")
            .and_then(Value::as_str)
            .is_some_and(|n| !n.trim().is_empty());
        match option.get("x-live") {
            None if !needs => {
                option.insert("x-live".into(), true.into());
            }
            Some(Value::Bool(true)) if !needs => {}
            Some(Value::Bool(false)) if needs => {}
            Some(Value::Bool(false)) => {
                problem("isn't live, so it must say what a change needs (x-needs)");
            }
            _ => problem("must say it's live (x-live) or what a change needs, not both"),
        }
        if !matches!(option.get("x-secret"), None | Some(Value::Bool(_))) {
            problem("has an x-secret that isn't true or false");
        }
    }
}
/// Every description in `schema` on one line: a doc comment's line breaks
/// are its source's, not its meaning's.
fn one_line(schema: &mut Value) {
    match schema {
        Value::Object(fields) => {
            for (key, value) in fields {
                match value {
                    Value::String(text) if key == "description" => {
                        *text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    }
                    _ => one_line(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(one_line),
        _ => {}
    }
}
/// Off or zero: nothing is spent until a machine turns it on.
fn off(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// The top-level options of `T` that a change to takes effect only at a
/// restart: the ones its schema declares `x-live: false`.
pub fn restart_only<T: JsonSchema>() -> Vec<String> {
    let schema = schemars::schema_for!(T).to_value();
    schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, option)| option["x-live"] == Value::Bool(false))
        .map(|(name, _)| name.clone())
        .collect()
}

/// A configuration file as a running daemon reads it: again at each
/// decision, with the options the schema declares `x-live: false` kept as
/// the daemon started with them. A change to one of those waits for a
/// restart, and is logged once, by its name.
pub struct Live<T> {
    path: PathBuf,
    /// The started configuration, as its loader writes it.
    started: Value,
    value: T,
    /// The options the schema declares `x-live: false`.
    restart: Vec<String>,
    waiting: Mutex<Vec<String>>,
}
impl<T: JsonSchema + Serialize + DeserializeOwned> Live<T> {
    pub fn open(path: &Path) -> Result<Self> {
        let (value, _) = load::<T>(&std::fs::read(path)?)?;
        Ok(Self {
            path: path.into(),
            started: serde_json::to_value(&value)?,
            value,
            restart: restart_only::<T>(),
            waiting: Mutex::default(),
        })
    }
    /// The configuration the daemon started with.
    pub fn started(&self) -> &T {
        &self.value
    }
    /// The configuration now: the file as it reads, but for the options
    /// that take effect only at a restart.
    pub fn read(&self) -> Result<T> {
        // Both read by the one loader, and compared as it writes them, so
        // neither a duplicate key nor another spelling of a value counts.
        let (now, _) = load::<T>(&std::fs::read(&self.path)?)?;
        let mut now = serde_json::to_value(now)?;
        let mut waiting = Vec::new();
        if let Some(fields) = now.as_object_mut() {
            for name in &self.restart {
                let started = self.started.get(name);
                if fields.get(name) != started {
                    waiting.push(name.clone());
                }
                match started {
                    Some(value) => fields.insert(name.clone(), value.clone()),
                    None => fields.remove(name),
                };
            }
        }
        let value = serde_json::from_value(now).map_err(quiet)?;
        let mut noted = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        if *noted != waiting {
            if !waiting.is_empty() {
                eprintln!(
                    "conaryd: {} changed in {}: conaryd keeps what it started with until it restarts",
                    waiting.join(", "),
                    self.path.display()
                );
            }
            *noted = waiting;
        }
        Ok(value)
    }
    /// The options changed in the file that wait for a restart, as the last
    /// read saw them.
    pub fn waiting(&self) -> Vec<String> {
        self.waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Where an option's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum From {
    File,
    Default,
}
/// One option's value, as the loader reads it from the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Effective {
    /// Its path in the file, sections joined by dots.
    pub name: String,
    pub value: Value,
    pub from: From,
    /// What a change to it needs, for one that isn't live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
}
/// Every option of a configuration file, the live ones apart from the
/// exceptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub options: Vec<Effective>,
    pub not_live: Vec<Effective>,
}

/// The one loader of a configuration file as `T`: typed from its bytes, so
/// a duplicate or unknown key is refused, and with every struct in it
/// written as an object, the one form the schema describes (serde would
/// also take a struct as a positional array). Returns the file as JSON too.
/// Its errors never quote a value: see [`quiet`].
pub fn load<T: JsonSchema + DeserializeOwned>(file: &[u8]) -> Result<(T, Value)> {
    let raw = serde_json::from_slice::<Unique>(file).map_err(quiet)?.0;
    let loaded: T = serde_json::from_slice(file).map_err(quiet)?;
    let schema = of::<T>()?;
    objects(&schema, &schema, &raw, "")?;
    Ok((loaded, raw))
}
/// Why a configuration file doesn't read, without any of its values: serde
/// quotes a value of the wrong type or form, and a secret's value (a token
/// command's credential) may be the one quoted, even as the whole document.
/// So only what names the schema's own options, one missing or given twice,
/// is said as serde says it; anything else, an option it doesn't know
/// included (its name is the file's), is said by its kind and place.
fn quiet(e: serde_json::Error) -> String {
    use serde_json::error::Category;
    let at = format!("at line {} column {}", e.line(), e.column());
    match e.classify() {
        Category::Io => format!("it can't be read {at}"),
        Category::Syntax | Category::Eof => format!("it isn't JSON {at}"),
        Category::Data => {
            let said = e.to_string();
            if said.starts_with("missing field `") || said.starts_with("duplicate field `") {
                said
            } else if said.starts_with(DUPLICATE) {
                format!("{DUPLICATE} {at}")
            } else if said.starts_with("unknown field `") {
                format!("it names an option the schema doesn't declare {at}")
            } else {
                format!("a value isn't of the type or form its option takes {at}")
            }
        }
    }
}

const DUPLICATE: &str = "an object gives one key twice";
/// A file as JSON, refused if any object in it gives one key twice: serde
/// refuses that for a struct's fields, but a map (`proofs`,
/// `priority_labels`) takes the last of them, and the file would mean
/// something its reader can't see.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(Unique(Value::Null))
    }
}
impl<'de> serde::de::Visitor<'de> for Unique {
    type Value = Unique;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON")
    }
    fn visit_unit<E>(self) -> std::result::Result<Self, E> {
        Ok(Self(Value::Null))
    }
    fn visit_bool<E>(self, v: bool) -> std::result::Result<Self, E> {
        Ok(Self(v.into()))
    }
    fn visit_i64<E>(self, v: i64) -> std::result::Result<Self, E> {
        Ok(Self(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> std::result::Result<Self, E> {
        Ok(Self(v.into()))
    }
    fn visit_f64<E>(self, v: f64) -> std::result::Result<Self, E> {
        Ok(Self(v.into()))
    }
    fn visit_str<E>(self, v: &str) -> std::result::Result<Self, E> {
        Ok(Self(v.into()))
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self, A::Error> {
        let mut items = Vec::new();
        while let Some(Unique(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Self(Value::Array(items)))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let Unique(value) = map.next_value()?;
            if object.insert(key, value).is_some() {
                return Err(serde::de::Error::custom(DUPLICATE));
            }
        }
        Ok(Self(Value::Object(object)))
    }
}

/// Whether each struct `raw` holds where `owner` says, at `path`, is an object.
fn objects(schema: &Value, owner: &Value, raw: &Value, path: &str) -> Result<()> {
    let Some(fields) = raw.as_object() else {
        let at = if path.is_empty() { "the file" } else { path };
        return Err(format!("{at} must be a JSON object, its options named").into());
    };
    for (name, value) in fields {
        let Some(option) = owner["properties"].get(name) else {
            continue;
        };
        let at = format!("{path}{name}");
        match shape(schema, option) {
            Shape::Struct(def) => objects(schema, def, value, &format!("{at}."))?,
            Shape::Nullable(def) if !value.is_null() => {
                objects(schema, def, value, &format!("{at}."))?;
            }
            Shape::Map(def) => {
                for (key, entry) in value.as_object().into_iter().flatten() {
                    objects(schema, def, entry, &format!("{at}.{key}."))?;
                }
            }
            Shape::Nullable(_) | Shape::Leaf => {}
        }
    }
    Ok(())
}

/// What an option holds: a struct of options of its own, one that may be
/// `null`, a map of them, or a value.
enum Shape<'a> {
    Struct(&'a Value),
    Nullable(&'a Value),
    Map(&'a Value),
    Leaf,
}
fn shape<'a>(schema: &'a Value, option: &'a Value) -> Shape<'a> {
    let def = |reference: &Value| {
        reference["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix("#/$defs/"))
            .map(|name| &schema["$defs"][name])
            .filter(|def| def["properties"].is_object())
    };
    if let Some(def) = def(option) {
        return Shape::Struct(def);
    }
    if let Some(def) = option["anyOf"]
        .as_array()
        .filter(|any| any.iter().any(|a| a["type"] == "null"))
        .and_then(|any| any.iter().find_map(def))
    {
        return Shape::Nullable(def);
    }
    if let Some(def) = def(&option["additionalProperties"]) {
        return Shape::Map(def);
    }
    Shape::Leaf
}

/// Every option of the configuration `file` holds, read by [`load`]: its
/// effective value, and whether the file set it or it's the default. The
/// options of a section, of a set `null`able one and of each entry of a map
/// of them are listed one by one. A secret's value is never shown, only that
/// it's set.
pub fn effective<T: JsonSchema + Serialize + DeserializeOwned>(file: &[u8]) -> Result<Listing> {
    let schema = of::<T>()?;
    let (loaded, raw) = load::<T>(file)?;
    let value = serde_json::to_value(loaded)?;
    let mut listing = Listing {
        options: Vec::new(),
        not_live: Vec::new(),
    };
    walk(&schema, &schema, "", Some(&raw), &value, &mut listing);
    Ok(listing)
}
fn walk(
    schema: &Value,
    owner: &Value,
    prefix: &str,
    raw: Option<&Value>,
    value: &Value,
    listing: &mut Listing,
) {
    for (name, option) in owner["properties"].as_object().into_iter().flatten() {
        let path = format!("{prefix}{name}");
        let set = raw.and_then(|r| r.get(name));
        let current = value.get(name).cloned().unwrap_or(Value::Null);
        match shape(schema, option) {
            Shape::Struct(def) => {
                walk(schema, def, &format!("{path}."), set, &current, listing);
                continue;
            }
            Shape::Nullable(def) if !current.is_null() => {
                walk(schema, def, &format!("{path}."), set, &current, listing);
                continue;
            }
            Shape::Map(def) if current.as_object().is_some_and(|m| !m.is_empty()) => {
                for (key, entry) in current.as_object().into_iter().flatten() {
                    let set = set.and_then(|s| s.get(key));
                    walk(schema, def, &format!("{path}.{key}."), set, entry, listing);
                }
                continue;
            }
            _ => {}
        }
        let secret = option["x-secret"] == Value::Bool(true) && !current.is_null();
        let entry = Effective {
            name: path,
            value: if secret { "[redacted]".into() } else { current },
            from: if set.is_some() {
                From::File
            } else {
                From::Default
            },
            needs: option["x-needs"].as_str().map(str::to_owned),
        };
        if option["x-live"] == Value::Bool(false) {
            listing.not_live.push(entry);
        } else {
            listing.options.push(entry);
        }
    }
}
