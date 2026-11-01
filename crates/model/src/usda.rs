//! USD's text form, as far as a model's files need it: read into prims and their attributes, whatever wrote them, so
//! that a file saved again by another tool still reads. What is not a value here (metadata, time samples) is passed over.

use std::collections::HashMap;

use pe2_ext::{Result, anyhow, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    /// A string, a token, or a word such as `true`.
    Text(String),
    /// A list or a tuple.
    List(Vec<Value>),
    /// A path to a prim or property.
    Path(String),
}

impl Value {
    /// As a list of numbers, flattened however it is nested: points, matrices.
    #[must_use]
    pub fn numbers(&self) -> Vec<f64> {
        match self {
            Value::Number(number) => vec![*number],
            Value::List(items) => items.iter().flat_map(Value::numbers).collect(),
            _ => Vec::new(),
        }
    }

    /// As a list of words.
    #[must_use]
    pub fn words(&self) -> Vec<String> {
        match self {
            Value::Text(text) => vec![text.clone()],
            Value::List(items) => items.iter().flat_map(Value::words).collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prim {
    /// Its type: `Mesh`, `Skeleton`; none for a bare `def`.
    pub kind: String,
    pub name: String,
    pub attributes: HashMap<String, Value>,
    pub children: Vec<Prim>,
}

impl Prim {
    /// The prim down a path of names.
    #[must_use]
    pub fn at(&self, path: &[&str]) -> Option<&Prim> {
        path.iter().try_fold(self, |prim, name| prim.children.iter().find(|child| child.name == *name))
    }

    /// An attribute that has to be there.
    ///
    /// # Errors
    /// It is not.
    pub fn attribute(&self, name: &str) -> Result<&Value> {
        self.attributes.get(name).ok_or_else(|| anyhow!("{} has no {name}", self.name))
    }

    /// Numbers that have to be there.
    ///
    /// # Errors
    /// They are not.
    pub fn numbers(&self, name: &str) -> Result<Vec<f64>> {
        Ok(self.attribute(name)?.numbers())
    }
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

const SPECIFIERS: [&str; 3] = ["def", "over", "class"];
const QUALIFIERS: [&str; 9] = ["custom", "uniform", "varying", "config", "prepend", "append", "add", "delete", "reorder"];

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'.' | b'-' | b'+')
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn line(&self) -> usize {
        self.text[..self.at.min(self.text.len())].split(|byte| *byte == b'\n').count()
    }

    /// Past white space and comments.
    fn skip(&mut self) {
        while let Some(byte) = self.peek() {
            if byte == b'#' {
                while self.peek().is_some_and(|byte| byte != b'\n') {
                    self.at += 1;
                }
            } else if byte.is_ascii_whitespace() {
                self.at += 1;
            } else {
                return;
            }
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        self.skip();
        let taken = self.peek() == Some(byte);
        if taken {
            self.at += 1;
        }
        taken
    }

    fn word(&mut self) -> Result<String> {
        self.skip();
        let start = self.at;
        while self.peek().is_some_and(is_word) {
            self.at += 1;
        }
        if start == self.at {
            bail!("line {}: expected a word", self.line());
        }
        Ok(String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    /// The word coming, left where it is.
    fn next_word(&mut self) -> Option<String> {
        let start = self.at;
        let word = self.word().ok();
        self.at = start;
        word
    }

    fn string(&mut self) -> Result<String> {
        self.skip();
        let quote = self.peek().filter(|byte| matches!(byte, b'"' | b'\'')).ok_or_else(|| anyhow!("line {}: expected a string", self.line()))?;
        let long = self.text[self.at..].starts_with(&[quote; 3]);
        self.at += if long { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let Some(byte) = self.peek() else { bail!("a string does not end") };
            if byte == b'\\' {
                out.push(*self.text.get(self.at + 1).ok_or_else(|| anyhow!("a string does not end"))?);
                self.at += 2;
            } else if long && self.text[self.at..].starts_with(&[quote; 3]) {
                self.at += 3;
                break;
            } else if !long && byte == quote {
                self.at += 1;
                break;
            } else {
                out.push(byte);
                self.at += 1;
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// What runs to a closing byte.
    fn until(&mut self, close: u8) -> Result<String> {
        self.at += 1;
        let start = self.at;
        while self.peek().ok_or_else(|| anyhow!("line {}: it does not end", self.line()))? != close {
            self.at += 1;
        }
        self.at += 1;
        Ok(String::from_utf8_lossy(&self.text[start..self.at - 1]).into_owned())
    }

    /// Past a bracketed block, whatever it holds.
    fn pass_block(&mut self, open: u8, close: u8) -> Result<()> {
        let mut depth = 0;
        loop {
            self.skip();
            match self.peek().ok_or_else(|| anyhow!("line {}: a block does not end", self.line()))? {
                b'"' | b'\'' => drop(self.string()?),
                byte if byte == open => {
                    depth += 1;
                    self.at += 1;
                }
                byte if byte == close => {
                    depth -= 1;
                    self.at += 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => self.at += 1,
            }
        }
    }

    fn metadata(&mut self) -> Result<()> {
        self.skip();
        if self.peek() == Some(b'(') {
            self.pass_block(b'(', b')')?;
        }
        Ok(())
    }

    fn sequence(&mut self, close: u8) -> Result<Value> {
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip();
            if self.take(close) {
                return Ok(Value::List(items));
            }
            items.push(self.value()?);
            self.take(b',');
        }
    }

    fn value(&mut self) -> Result<Value> {
        self.skip();
        match self.peek().ok_or_else(|| anyhow!("a value is missing"))? {
            b'[' => self.sequence(b']'),
            b'(' => self.sequence(b')'),
            b'"' | b'\'' => self.string().map(Value::Text),
            b'<' => self.until(b'>').map(Value::Path),
            b'@' => self.until(b'@').map(Value::Text),
            _ => {
                let word = self.word()?;
                Ok(word.parse().map_or(Value::Text(word), Value::Number))
            }
        }
    }

    /// A property, or a statement of list editing, which the prim keeps under its name.
    fn property(&mut self, prim: &mut Prim) -> Result<()> {
        let mut word = self.word()?;
        while QUALIFIERS.contains(&word.as_str()) {
            word = self.word()?;
        }
        if self.take(b'[') && !self.take(b']') {
            bail!("line {}: expected a type's []", self.line());
        }
        self.skip();
        let name = if self.peek() == Some(b'=') { word } else { self.word()? };
        if self.take(b'=') {
            self.skip();
            if self.peek() == Some(b'{') {
                self.pass_block(b'{', b'}')?;
            } else {
                let value = self.value()?;
                prim.attributes.insert(name, value);
            }
        }
        self.metadata()
    }

    fn prim(&mut self) -> Result<Prim> {
        self.word()?;
        self.skip();
        let kind = if matches!(self.peek(), Some(b'"' | b'\'')) { String::new() } else { self.word()? };
        let name = self.string()?;
        let mut prim = Prim { kind, name, ..Prim::default() };
        self.metadata()?;
        if !self.take(b'{') {
            bail!("line {}: {} has no body", self.line(), prim.name);
        }
        while !self.take(b'}') {
            if self.peek().is_none() {
                bail!("{} does not end", prim.name);
            }
            match self.next_word() {
                Some(word) if SPECIFIERS.contains(&word.as_str()) => prim.children.push(self.prim()?),
                _ => self.property(&mut prim)?,
            }
        }
        Ok(prim)
    }
}

/// The prims of a file, under one nameless prim.
///
/// # Errors
/// It is not USD's text form.
pub fn read(text: &str) -> Result<Prim> {
    if !text.starts_with("#usda") {
        bail!("not USD's text form");
    }
    let mut reader = Reader { text: text.as_bytes(), at: 0 };
    reader.metadata()?;
    let mut root = Prim::default();
    loop {
        reader.skip();
        if reader.peek().is_none() {
            return Ok(root);
        }
        root.children.push(reader.prim()?);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As `usdcat` writes one.
    const SAVED: &str = r#"#usda 1.0
(
    defaultPrim = "Model"
    doc = """Generated from "somewhere"
    over two lines"""
    metersPerUnit = 0.001
)

def Xform "Model" (
    customData = {
        string source = "a (bracket) in a string"
    }
)
{
    custom token[] slots = ["page_1", "page_0"]
    float xformOp:rotateX = 180
    uniform token[] xformOpOrder = ["xformOp:rotateX"]

    def SkelRoot "Rig" (
        prepend apiSchemas = ["SkelBindingAPI"]
    )
    {
        rel skel:skeleton = </Model/Rig/Skeleton>

        def Mesh "Mesh"
        {
            point3f[] points = [(1, -2.5, 3e-2), (4, 5, 6)]
            int[] primvars:normalIndex = [0, -1] (
                interpolation = "faceVarying"
            )
            float a:time.timeSamples = {
                0: 1,
            }
            prepend rel material:binding = </Model/Materials/m>
            reorder nameChildren = ["b", "a"]
        }
    }

    def "Materials"
    {
    }
}
"#;

    #[test]
    fn a_file_reads_as_its_prims() {
        let root = read(SAVED).unwrap();
        let model = root.at(&["Model"]).unwrap();
        assert_eq!(model.kind, "Xform");
        assert_eq!(model.attributes["slots"].words(), ["page_1", "page_0"]);
        let mesh = root.at(&["Model", "Rig", "Mesh"]).unwrap();
        assert_eq!(mesh.kind, "Mesh");
        assert_eq!(mesh.numbers("points").unwrap(), [1.0, -2.5, 0.03, 4.0, 5.0, 6.0]);
        assert_eq!(mesh.numbers("primvars:normalIndex").unwrap(), [0.0, -1.0]);
        assert_eq!(mesh.attributes["material:binding"], Value::Path("/Model/Materials/m".to_string()));
        assert_eq!(root.at(&["Model", "Materials"]).unwrap().kind, "");
    }

    #[test]
    fn what_is_not_usd_says_so() {
        assert!(read("slots = []").is_err());
        assert!(read("#usda 1.0\ndef Xform \"Open\" {").is_err());
    }
}
