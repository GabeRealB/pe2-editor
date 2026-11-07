//! Tokenizers: what a value is in the language a package is written in. One is declared for a type, with what it makes of the
//! whole value and the derived values a target needs and no node shows (a material's `tpage`), each with what it generates
//! and the args it reads. It is an ordinary operation, kept and traced as any, reached as the output `tokens/<address>` of a
//! node; `tokenizer` says what it declares, without making anything. A target's own, for one target alone, overrides.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use crate::data::{Call, File, Retain, Typed, Value as Data};
use crate::registry::Registry;
use crate::ty::{self, Registered};
use crate::{Result, Table, Value, anyhow, bail};

/// The output making tokens, and the one saying what a tokenizer declares.
pub const OUTPUT: &str = "tokens";
pub const DECLARED: &str = "tokenizer";

/// What an output a sub-path goes through is: a table holding one of these, the node it leads to, or a plain value it ends at.
pub const NODE: &str = "node";
pub const VALUE: &str = "value";
/// Beside `NODE`: what that node is here where that is not what the node is, and the args its tokenizer is then read with.
pub const AS: &str = "as";
pub const ARGS: &str = "args";

/// An output leading to the node a reference names.
///
/// # Errors
/// It names nothing.
pub fn link(reference: Option<&Value>) -> Result<Table> {
    let target = reference.and_then(crate::target_of).ok_or_else(|| anyhow!("it names nothing there"))?;
    Ok(Table::from_iter([(NODE.to_string(), target.into())]))
}

/// An output leading on from a node as something else than it is, which its tokenizer reads by these args: a prefab's member
/// is a model, read from the prefab.
#[must_use]
pub fn leads_on(node: &str, reads_as: &str, args: &[(&str, &str)]) -> Table {
    let args = args.iter().map(|(name, value)| ((*name).to_string(), Value::String((*value).to_string())));
    Table::from_iter([
        (NODE.to_string(), node.into()),
        (AS.to_string(), reads_as.into()),
        (ARGS.to_string(), Value::Table(args.collect())),
    ])
}

/// What an entry generates, as a person using it needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Yields {
    IntLiteral,
    FloatLiteral,
    StringLiteral,
    /// A brace initializer, braces and all: what follows `=` in a definition.
    Initializer,
    /// A definition of an object of this type, under the name its args give.
    Definition(&'static str),
}

impl fmt::Display for Yields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Yields::IntLiteral => f.write_str("int_literal"),
            Yields::FloatLiteral => f.write_str("float_literal"),
            Yields::StringLiteral => f.write_str("string_literal"),
            Yields::Initializer => f.write_str("initializer"),
            Yields::Definition(ty) => write!(f, "definition {ty}"),
        }
    }
}

/// What a source gave an address with `PE2_BIND_ARG`: each arg's values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args(pub BTreeMap<String, Vec<String>>);

impl Args {
    /// An arg's one value.
    ///
    /// # Errors
    /// It was not given, or given several.
    pub fn one(&self, name: &str) -> Result<&str> {
        match self.0.get(name).map(Vec::as_slice) {
            Some([value]) => Ok(value),
            Some(_) => bail!("{name} takes one value"),
            None => bail!("it needs {name}"),
        }
    }
}

/// How an entry is made.
pub type Make = fn(&Call<'_>, &Args) -> Result<String>;

#[derive(Clone)]
struct Entry {
    name: Option<&'static str>,
    yields: Yields,
    args: &'static [&'static str],
    make: Make,
}

/// The address of what a tokenizer makes of an entry (`None`: the whole value), with its args.
#[must_use]
pub fn address(entry: Option<&str>, args: &Args) -> String {
    let mut out = format!("{OUTPUT}/{}", entry.unwrap_or_default());
    for (name, values) in &args.0 {
        let _ = write!(out, ";{name}={}", values.join(","));
    }
    out
}

fn parsed(text: &str) -> (Option<&str>, Args) {
    let mut pieces = text.split(';');
    let entry = pieces.next().filter(|entry| !entry.is_empty());
    let args = pieces
        .filter_map(|piece| piece.split_once('='))
        .map(|(name, values)| (name.to_string(), values.split(',').map(str::to_string).collect()));
    (entry, Args(args.collect()))
}

/// A tokenizer being declared: `Registry::tokenizer`.
pub struct Tokenizer<'a> {
    registry: &'a mut Registry,
    /// The value types it tokenizes, by their qualified names; the first names it.
    values: Vec<String>,
    target: Option<String>,
    entries: Vec<Entry>,
    /// Its entries read other nodes as they go.
    follows: bool,
}

impl Tokenizer<'_> {
    /// For this target alone, over what is declared for all.
    #[must_use]
    pub fn target(mut self, target: &str) -> Self {
        self.target = Some(target.to_string());
        self
    }

    /// Its entries read other nodes, following what they find: what they read is found by running them.
    #[must_use]
    pub fn follows(mut self) -> Self {
        self.follows = true;
        self
    }

    /// What it makes of the whole value.
    #[must_use]
    pub fn whole(mut self, yields: Yields, args: &'static [&'static str], make: Make) -> Self {
        self.entries.push(Entry { name: None, yields, args, make });
        self
    }

    /// A value derived from it, which no node shows; it reads its parent's args.
    #[must_use]
    pub fn derived(mut self, name: &'static str, yields: Yields, args: &'static [&'static str], make: Make) -> Self {
        self.entries.push(Entry { name: Some(name), yields, args, make });
        self
    }

    /// Register it.
    pub fn register(self) {
        let Tokenizer { registry, values, target, entries, follows } = self;
        let local = values.first().and_then(|value| value.rsplit('@').next()).unwrap_or_default().to_string();
        let suffix = target.as_ref().map_or_else(String::new, |target| format!("_{target}"));
        let declared = declaration(&entries);
        let held = entries.clone();
        let names = |name: &str| target.as_ref().map_or_else(|| name.to_string(), |target| format!("{name}.{target}"));
        let taken: Vec<&str> = values.iter().map(String::as_str).collect();
        let op = registry
            .op(&format!("tokens_{local}{suffix}"))
            .label("Tokens")
            .takes("value", &taken)
            .output::<ty::Tokens>()
            .parameter::<String>("address")
            .provides(&names(OUTPUT))
            .keeps(Retain::Cache);
        let made = move |call: &mut Call<'_>| {
            let asked: String = call.parameter()?;
            let (name, args) = parsed(&asked);
            let call: &Call<'_> = call;
            let entry =
                held.iter().find(|entry| entry.name == name).ok_or_else(|| anyhow!("{}", unknown(&held, name)))?;
            if let Some(missing) = entry.args.iter().find(|arg| !args.0.contains_key(**arg)) {
                bail!("it needs {missing}");
            }
            let tokens = (entry.make)(call, &args)?;
            Ok(Typed::new(ty::Tokens::NAME, vec![File::new(ty::Tokens::FILE, tokens.into_bytes())]))
        };
        // Entries following what they read find it by running, replayed until they read nothing new.
        let _ = if follows { op.steps(crate::stepped::replayed(made)) } else { op.run(made) };
        let _ = registry
            .op(&format!("tokenizer_{local}{suffix}"))
            .label("What it tokenizes to")
            .takes("value", &taken)
            .output::<ty::Table>()
            .provides(&names(DECLARED))
            .run(move |_| Ok(declared.clone()));
    }
}

fn unknown(entries: &[Entry], name: Option<&str>) -> String {
    let listed: Vec<&str> = entries.iter().filter_map(|entry| entry.name).collect();
    match name {
        None => "it has no value as a whole".to_string(),
        Some(name) if listed.is_empty() => format!("it derives no {name}"),
        Some(name) => format!("it derives no {name}; it derives {}", listed.join(", ")),
    }
}

fn declaration(entries: &[Entry]) -> Table {
    let described = |entry: &Entry| {
        let mut table = Table::new();
        if let Some(name) = entry.name {
            table.insert("name".to_string(), name.into());
        }
        table.insert("yields".to_string(), entry.yields.to_string().into());
        table.insert("args".to_string(), Value::Array(entry.args.iter().map(|arg| (*arg).into()).collect()));
        Value::Table(table)
    };
    Table::from_iter([("entries".to_string(), Value::Array(entries.iter().map(described).collect()))])
}

impl Registry {
    /// A tokenizer of values of these types, which this extension's dependencies register.
    pub fn tokenizer(&mut self, values: &[&str]) -> Tokenizer<'_> {
        Tokenizer {
            registry: self,
            values: values.iter().map(|value| (*value).to_string()).collect(),
            target: None,
            entries: Vec::new(),
            follows: false,
        }
    }
}

/// What a value of the `tokens` type holds.
///
/// # Errors
/// It is no such value.
pub fn text_of(call: &Call<'_>, value: &Data) -> Result<String> {
    let Data::Typed(typed) = value else { bail!("no tokens") };
    Ok(String::from_utf8(call.read(typed.file()?)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_says_what_is_asked_and_with_what() {
        let args = Args(BTreeMap::from([("symbol".to_string(), vec!["gBody".to_string()])]));
        assert_eq!(address(None, &args), "tokens/;symbol=gBody");
        assert_eq!(address(Some("tpage"), &Args::default()), "tokens/tpage");
        let (entry, read) = parsed("tpage;symbol=gBody;many=a,b");
        assert_eq!(entry, Some("tpage"));
        assert_eq!(read.0["many"], ["a", "b"]);
        assert_eq!(parsed("").0, None);
    }

    #[test]
    fn what_is_generated_is_said_as_people_say_it() {
        assert_eq!(Yields::Definition("TmdSource").to_string(), "definition TmdSource");
        assert_eq!(Yields::IntLiteral.to_string(), "int_literal");
    }
}
