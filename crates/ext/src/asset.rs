//! An asset as an extension sees it: lent by the host for one call.

use crate::{Result, Table, Value};

/// What the host lends of an asset. The host implements it over its index; a component reaches the same through the
/// host's `asset` resource.
pub trait Source {
    /// Its resolved path.
    fn path(&self) -> &str;
    fn uid(&self) -> Option<&str>;
    /// Its declaration's fields.
    fn fields(&self) -> &Table;
    /// Its declaration, as written.
    fn declaration(&self) -> &str;
    /// Whether its bundle has a file of this name.
    fn has_file(&self, name: &str) -> bool;
    /// A file of its bundle, whole.
    ///
    /// # Errors
    /// It has no such file, or it cannot be read.
    fn read(&self, name: &str) -> Result<Vec<u8>>;
}

/// An asset, lent for one call: what an extension keeps of it, it copies. Its kind and role are qualified names
/// (`pe2.images@image`), resolved by the host from what its declaration says.
#[derive(Clone, Copy)]
pub struct Asset<'a> {
    source: &'a dyn Source,
    kind: Option<&'a str>,
    role: Option<&'a str>,
}

impl<'a> Asset<'a> {
    /// An asset lent by the host, with the kind and role it resolved.
    #[must_use]
    pub fn new(source: &'a dyn Source, kind: Option<&'a str>, role: Option<&'a str>) -> Asset<'a> {
        Asset { source, kind, role }
    }

    #[must_use]
    pub fn path(&self) -> &'a str {
        self.source.path()
    }

    #[must_use]
    pub fn uid(&self) -> Option<&'a str> {
        self.source.uid()
    }

    /// Its kind, qualified; none when no extension registers what it declares.
    #[must_use]
    pub fn kind(&self) -> Option<&'a str> {
        self.kind
    }

    /// Its role, qualified; none when it declares none, or none registered on its kind.
    #[must_use]
    pub fn role(&self) -> Option<&'a str> {
        self.role
    }

    /// Its role, where the extension `id` registers it: the role's own name.
    #[must_use]
    pub fn role_of(&self, id: &str) -> Option<&'a str> {
        self.role?.strip_prefix(id)?.strip_prefix('@')
    }

    /// A field of its declaration.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&'a Value> {
        self.source.fields().get(key)
    }

    /// Its declaration's fields.
    #[must_use]
    pub fn fields(&self) -> &'a Table {
        self.source.fields()
    }

    /// A table of parts keyed by name (`cells.disc_1 = { ... }`), in the order declared; empty where it has none.
    #[must_use]
    pub fn parts(&self, key: &str) -> Vec<(&'a str, &'a Value)> {
        self.field(key).and_then(Value::as_table).map_or_default(|t| t.iter().map(|(k, v)| (k.as_str(), v)).collect())
    }

    /// A field's array; empty where it has none.
    #[must_use]
    pub fn array(&self, key: &str) -> &'a [Value] {
        self.field(key).and_then(Value::as_array).map_or(&[][..], Vec::as_slice)
    }

    #[must_use]
    pub fn has_file(&self, name: &str) -> bool {
        self.source.has_file(name)
    }

    /// A file of its bundle, whole.
    ///
    /// # Errors
    /// It has no such file, or it cannot be read.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.source.read(name)
    }

    /// What the host lent, for a host passing the asset on (to a component).
    #[must_use]
    pub fn source(&self) -> &'a dyn Source {
        self.source
    }
}
