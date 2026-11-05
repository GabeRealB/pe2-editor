//! The macros `pe2.h` gives a source, as this extension reads them: one reading, of plain source or of what a first pass
//! left, finding each anywhere on a line, comments and strings left out.

/// Every macro read.
const KNOWN: [&str; 14] = [
    "PE2_BIND",
    "PE2_BIND_ARG",
    "PE2_BIND_SELF",
    "PE2_EDITOR_LABEL",
    "PE2_VALUE",
    "PE2_QUERY",
    "PE2_OPTION_DEF",
    "PE2_OPTION",
    "PE2_OPTION_IS",
    "PE2_OPTION_AT",
    "PE2_OPTION_COUNT",
    "PE2_TYPE",
    "PE2_CONFIG",
    "PE2_CONFIG_IS",
];

/// A use of one, with its arguments.
#[derive(Debug, PartialEq, Eq)]
pub struct Use<'a> {
    pub name: &'a str,
    pub args: Vec<&'a str>,
    pub line: usize,
    /// Where its name starts in the text.
    pub offset: usize,
}

/// Past a string or a character, from its opening quote.
fn quoted(bytes: &[u8], mut at: usize) -> usize {
    let quote = bytes[at];
    at += 1;
    while at < bytes.len() && bytes[at] != quote {
        at += if bytes[at] == b'\\' { 2 } else { 1 };
    }
    at + 1
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A call's arguments, from its opening parenthesis; and where it ends. A comma in braces is an initializer's.
fn arguments(text: &str, open: usize) -> Option<(Vec<&str>, usize)> {
    let bytes = text.as_bytes();
    let (mut at, mut depth, mut start, mut args) = (open + 1, 0, open + 1, Vec::new());
    while at < bytes.len() {
        match bytes[at] {
            b'"' | b'\'' => {
                at = quoted(bytes, at);
                continue;
            }
            b'(' | b'{' => depth += 1,
            b')' if depth == 0 => {
                args.push(text[start..at].trim());
                return Some((args, at + 1));
            }
            b')' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                args.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// Whether a word at `start` is the name a `#define` defines: a definition, not a use.
fn defining(text: &str, start: usize) -> bool {
    let head = &text[text[..start].rfind('\n').map_or(0, |at| at + 1)..start];
    let rest = head.trim_start().strip_prefix('#').map(str::trim_start).and_then(|rest| rest.strip_prefix("define"));
    rest.is_some_and(|rest| !rest.is_empty() && rest.trim().is_empty())
}

/// Whether `at` is in the body of a macro taking parameters, whose uses are of them and say something only where it is
/// expanded: its logical line, lines a backslash continues taken as one, a `#define` of a name and a parenthesis.
fn in_taking(text: &str, at: usize) -> bool {
    let mut start = text[..at].rfind('\n').map_or(0, |end| end + 1);
    while start > 1 && text.as_bytes()[start - 2] == b'\\' {
        start = text[..start - 1].rfind('\n').map_or(0, |end| end + 1);
    }
    let Some(rest) =
        text[start..].trim_start().strip_prefix('#').map(str::trim_start).and_then(|rest| rest.strip_prefix("define"))
    else {
        return false;
    };
    let rest = rest.trim_start();
    let name = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_').len();
    name > 0 && rest[name..].starts_with('(')
}

/// Every use in a text.
#[must_use]
pub fn uses(text: &str) -> Vec<Use<'_>> {
    let bytes = text.as_bytes();
    let (mut at, mut line, mut found) = (0, 1, Vec::new());
    while at < bytes.len() {
        match bytes[at] {
            b'\n' => {
                line += 1;
                at += 1;
            }
            b'"' | b'\'' => at = quoted(bytes, at),
            b'/' if bytes.get(at + 1) == Some(&b'/') => at = text[at..].find('\n').map_or(bytes.len(), |end| at + end),
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let end = text[at + 2..].find("*/").map_or(bytes.len(), |end| at + end + 4);
                line += text[at..end].matches('\n').count();
                at = end;
            }
            byte if is_word(byte) => {
                let start = at;
                while at < bytes.len() && is_word(bytes[at]) {
                    at += 1;
                }
                let word = &text[start..at];
                let open = at + text[at..].len() - text[at..].trim_start().len();
                if KNOWN.contains(&word)
                    && bytes.get(open) == Some(&b'(')
                    && !defining(text, start)
                    && !in_taking(text, start)
                    && let Some((args, end)) = arguments(text, open)
                {
                    found.push(Use { name: word, args, line, offset: start });
                    line += text[at..end].matches('\n').count();
                    at = end;
                }
            }
            _ => at += 1,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_use_is_found_anywhere_and_its_arguments_read() {
        let found = uses("int a = PE2_VALUE(body, materials,\n    page_0, _tpage);\nPE2_BIND(body, model)\n");
        assert_eq!(found[0].args, ["body", "materials", "page_0", "_tpage"]);
        assert_eq!((found[1].name, found[1].line), ("PE2_BIND", 3));
    }

    #[test]
    fn comments_and_strings_ask_for_nothing() {
        let text = "/* PE2_VALUE(a) */ // PE2_VALUE(b)\nchar* s = \"PE2_VALUE(c)\"; char q = '\"'; PE2_VALUE(d)\n";
        let found = uses(text);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].args[0], found[0].line), ("d", 2));
    }

    #[test]
    fn a_longer_name_is_another_name() {
        assert_eq!(uses("MY_PE2_VALUE(a) PE2_VALUES(b)"), []);
        assert_eq!(uses("PE2_OPTION_IS(a, b)")[0].name, "PE2_OPTION_IS");
    }

    #[test]
    fn a_definition_uses_what_it_expands_to() {
        assert_eq!(uses("#define HIDDEN PE2_VALUE(body)\n")[0].args, ["body"]);
    }

    #[test]
    fn a_macro_taking_parameters_uses_nothing_until_expanded() {
        let found = uses(
            "#define PE2_BIND_SELF(binding) \\\n    PE2_BIND(binding, file)\n#define ONE PE2_VALUE(a)\nPE2_BIND_SELF(file)\n",
        );
        let found: Vec<(&str, usize)> = found.iter().map(|found| (found.name, found.line)).collect();
        assert_eq!(found, [("PE2_VALUE", 3), ("PE2_BIND_SELF", 4)]);
    }

    #[test]
    fn the_name_a_definition_defines_is_no_use() {
        let found = uses("#define PE2_OPTION(name) pe2_option_##name\n#  define PE2_VALUE(a) b\nPE2_OPTION(x)\n");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].args[0], found[0].line), ("x", 3));
    }
}
