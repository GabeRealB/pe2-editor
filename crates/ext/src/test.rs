//! What a test extension acts through: `pe2.test`, built into the host and there only in test mode, which it requires. Each
//! step is what a person does, and the editor has done it by the time the call returns.

use std::fmt::Write as _;

use crate::{Project, Result, Table, Value, anyhow};

/// The editor being tested.
pub struct Editor<'a>(&'a dyn Project);

/// A node for a test to work on, made through a template as the editor makes one.
pub struct NewNode<'a> {
    pub path: &'a str,
    pub uid: &'a str,
    /// The template: `skeleton`, `texture`.
    pub template: &'a str,
    /// What the template calls its file, where it takes one.
    pub file: &'a str,
    /// Its settings, as TOML: they go on the layers they belong on.
    pub settings: &'a str,
    /// What its bundle holds, by name.
    pub files: &'a [(&'a str, &'a [u8])],
}

/// Bytes as a value crosses as text.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

impl<'a> Editor<'a> {
    /// The editor, through what a test is given.
    #[must_use]
    pub fn of(project: &'a dyn Project) -> Editor<'a> {
        Editor(project)
    }

    fn call(&self, step: &str, arguments: Table) -> Result<Value> {
        self.0.call(&format!("pe2.test@{step}"), &Value::Table(arguments))
    }

    fn step(&self, step: &str, text: &str) -> Result<()> {
        self.call(step, Table::from_iter([("text".to_string(), Value::String(text.to_string()))])).map(drop)
    }

    fn list(&self, step: &str) -> Result<Vec<String>> {
        let answer = self.call(step, Table::new())?;
        let list = answer.as_array().ok_or_else(|| anyhow!("{step} is no list"))?;
        Ok(list.iter().filter_map(Value::as_str).map(str::to_string).collect())
    }

    /// A file of the test's `data/` folder.
    ///
    /// # Errors
    /// It has no such file.
    pub fn data(&self, name: &str) -> Result<Vec<u8>> {
        let answer = self.call("data", Table::from_iter([("text".to_string(), Value::String(name.to_string()))]))?;
        let text = answer.as_str().ok_or_else(|| anyhow!("{name} is not there"))?;
        let pair = |pair: &[u8]| std::str::from_utf8(pair).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok());
        text.as_bytes().chunks(2).map(pair).collect::<Option<Vec<u8>>>().ok_or_else(|| anyhow!("{name} is not there"))
    }

    /// Open an asset.
    ///
    /// # Errors
    /// There is no such asset.
    pub fn show(&self, path: &str) -> Result<()> {
        self.step("show", path)
    }

    /// Click what says this.
    ///
    /// # Errors
    /// Nothing says it, or more than one thing does.
    pub fn click(&self, text: &str) -> Result<()> {
        self.step("click", text)
    }

    /// Click what says exactly this: what is open in a menu, say.
    ///
    /// # Errors
    /// Nothing is called this, or more than one thing is.
    pub fn click_label(&self, text: &str) -> Result<()> {
        self.step("click_label", text)
    }

    /// Open a menu and go on through it: `["File", "New node", "Skeleton"]`.
    ///
    /// # Errors
    /// One of them is not there.
    pub fn menu(&self, path: &[&str]) -> Result<()> {
        path.iter().try_for_each(|item| self.click_label(item))
    }

    /// Type into what has the keyboard.
    ///
    /// # Errors
    /// The editor did not settle.
    pub fn typing(&self, text: &str) -> Result<()> {
        self.step("typing", text)
    }

    /// Press a key, by its name: `Enter`, `Escape`.
    ///
    /// # Errors
    /// There is no such key.
    pub fn press(&self, key: &str) -> Result<()> {
        self.step("press", key)
    }

    /// Undo the last step.
    ///
    /// # Errors
    /// There is nothing to undo.
    pub fn undo(&self) -> Result<()> {
        self.step("undo", "")
    }

    /// Drop a file on the window.
    ///
    /// # Errors
    /// It could not be written where the host keeps it.
    pub fn drop_file(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let arguments = [("text".to_string(), Value::String(name.to_string())), ("bytes".to_string(), Value::String(hex(bytes)))];
        self.call("drop_file", Table::from_iter(arguments)).map(drop)
    }

    /// Make a node for the test, in the active layer. It is undone after the test.
    ///
    /// # Errors
    /// There is no such template, or it cannot be written.
    pub fn node(&self, node: &NewNode) -> Result<()> {
        let text = |text: &str| Value::String(text.to_string());
        let files = node.files.iter().map(|(name, bytes)| ((*name).to_string(), Value::String(hex(bytes))));
        let arguments = [
            ("path".to_string(), text(node.path)),
            ("uid".to_string(), text(node.uid)),
            ("template".to_string(), text(node.template)),
            ("file".to_string(), text(node.file)),
            ("settings".to_string(), text(node.settings)),
            ("files".to_string(), Value::Table(files.collect())),
        ];
        self.call("node", Table::from_iter(arguments)).map(drop)
    }

    /// Make a node of every template that needs no file: how many.
    ///
    /// # Errors
    /// One could not be made.
    pub fn make_every_template(&self) -> Result<i64> {
        let answer = self.call("make_every_template", Table::new())?;
        answer.as_integer().ok_or_else(|| anyhow!("make_every_template is no number"))
    }

    /// That something says this.
    ///
    /// # Errors
    /// Nothing does.
    pub fn expect(&self, text: &str) -> Result<()> {
        self.step("expect", text)
    }

    /// That nothing says this.
    ///
    /// # Errors
    /// Something does.
    pub fn expect_not(&self, text: &str) -> Result<()> {
        self.step("expect_not", text)
    }

    /// That nothing is wrong with the project.
    ///
    /// # Errors
    /// Something is, and what.
    pub fn expect_no_problems(&self) -> Result<()> {
        let problems = self.problems()?;
        if problems.is_empty() { Ok(()) } else { Err(anyhow!("{}", problems.join("; "))) }
    }

    /// What is wrong with the project, in a line each.
    ///
    /// # Errors
    /// The editor is gone.
    pub fn problems(&self) -> Result<Vec<String>> {
        self.list("problems")
    }

    /// The assets the project has, by path.
    ///
    /// # Errors
    /// The editor is gone.
    pub fn assets(&self) -> Result<Vec<String>> {
        self.list("assets")
    }

    /// What is selected.
    ///
    /// # Errors
    /// Nothing is.
    pub fn selected(&self) -> Result<String> {
        let answer = self.call("selected", Table::new())?;
        answer.as_str().map(str::to_string).ok_or_else(|| anyhow!("nothing is selected"))
    }

    /// Whether an asset makes this picture.
    ///
    /// # Errors
    /// There is no such asset.
    pub fn pictured(&self, path: &str, address: &str) -> Result<bool> {
        let text = |text: &str| Value::String(text.to_string());
        let answer = self.call("pictured", Table::from_iter([("text".to_string(), text(path)), ("address".to_string(), text(address))]))?;
        Ok(answer.as_bool().unwrap_or(false))
    }

    /// Ask to override an entry, as a layer's button does: an entry of a files layer asks which layer first.
    ///
    /// # Errors
    /// There is no such entry.
    pub fn override_entry(&self, path: &str) -> Result<()> {
        self.step("override_entry", path)
    }

    /// Hold the worker as though it were reading the project, until `release_reading`. What waits for that waits, as long
    /// as this lasts: nothing else lets a test find the editor at this moment, and one that never lets go hangs it.
    ///
    /// # Errors
    /// It is held already.
    pub fn hold_reading(&self) -> Result<()> {
        self.step("hold_reading", "")
    }

    /// Let the worker go, and wait for what was waiting.
    ///
    /// # Errors
    /// It is not held.
    pub fn release_reading(&self) -> Result<()> {
        self.step("release_reading", "")
    }

    /// A node of this declaration, whatever it says: nothing checks that a template would make it.
    ///
    /// # Errors
    /// It cannot be written.
    pub fn declare(&self, path: &str, uid: &str, declaration: &str) -> Result<()> {
        let arguments = [("text", path), ("uid", uid), ("declaration", declaration)];
        self.call("declare", arguments.iter().map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string()))).collect()).map(drop)
    }

    /// Make an output of a node, as a view asking for it does, nothing kept: what went wrong, if anything did.
    ///
    /// # Errors
    /// There is no such node.
    pub fn made(&self, path: &str, address: &str) -> Result<Option<String>> {
        let arguments = [("text", path), ("address", address)];
        let answer = self.call("made", arguments.iter().map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string()))).collect())?;
        Ok(answer.as_str().filter(|why| !why.is_empty()).map(str::to_string))
    }
}
