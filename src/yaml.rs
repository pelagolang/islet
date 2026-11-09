//! A strict reader for the small YAML subset Islet's files use, with a byte offset on every key and value so
//! errors can name a line and column. Every file Islet reads or writes is in this subset, and every file in it is
//! plain YAML that any YAML tool can read.
//!
//! Accepted:
//! - block mappings (`key: value`) and block lists (`- item`), nested by indentation (spaces only);
//! - comments (`# ...`);
//! - plain, `"double quoted"` and `'single quoted'` strings, which are always text;
//! - `true`, `false` and whole numbers; `{}` and `[]` for an empty mapping or list.
//!
//! Refused, each with its own message: tabs, flow collections with content, anchors, aliases, tags, block
//! scalars (`|`, `>`), several documents, `null` and `~`, empty values, duplicate keys, and plain words that YAML
//! tools disagree about (`yes`, `no`, `1.5`, `0x1F`, `2024-01-01`): quote them to make them text. A list must be
//! indented under its key.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<Node>),
    Table(Table),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub v: Val,
    /// Byte offset where the value starts.
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub key_at: usize,
    pub node: Node,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub entries: Vec<Entry>,
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entries.iter().find(|e| e.key == key).map(|e| &e.node)
    }
}

/// An error with a byte offset into the source.
#[derive(Debug, Clone, PartialEq)]
pub struct PErr {
    pub at: usize,
    pub msg: String,
}

impl PErr {
    pub fn new(at: usize, msg: impl Into<String>) -> PErr {
        PErr {
            at,
            msg: msg.into(),
        }
    }
}

/// An error placed in a named file at a line and column (both from 1).
#[derive(Debug, Clone, PartialEq)]
pub struct Diag {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl Diag {
    pub fn new(file: &str, src: &str, e: PErr) -> Diag {
        let mut at = e.at.min(src.len());
        while !src.is_char_boundary(at) {
            at -= 1;
        }
        let before = &src[..at];
        let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        Diag {
            file: file.to_string(),
            line,
            col,
            msg: e.msg,
        }
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.msg)
    }
}

impl std::error::Error for Diag {}

type R<T> = Result<T, PErr>;

// ---- writing helpers (the lock writer uses these) ---------------------------------------------------------

/// A double-quoted YAML string.
pub fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The text as a string value: plain when every YAML tool would read it back as the same text, quoted when not.
pub fn string(s: &str) -> String {
    if needs_quotes(s) {
        quoted(s)
    } else {
        s.to_string()
    }
}

fn needs_quotes(s: &str) -> bool {
    let first = match s.chars().next() {
        None => return true,
        Some(c) => c,
    };
    if "?:,[]{}#&*!|>'\"%@`".contains(first)
        || first.is_whitespace()
        || s.ends_with(char::is_whitespace)
    {
        return true;
    }
    if s == "-" || s.starts_with("- ") || s.ends_with(':') || s.contains(": ") || s.contains(" #") {
        return true;
    }
    if s.chars().any(|c| (c as u32) < 0x20) {
        return true;
    }
    word_is_special(s) || looks_numeric(s)
}

/// Plain words YAML 1.1 or 1.2 tools read as something other than text.
fn word_is_special(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n"
    )
}

/// Plain scalars that some YAML tool reads as a number or a date.
fn looks_numeric(s: &str) -> bool {
    let t = s.strip_prefix(['+', '-']).unwrap_or(s);
    let Some(c) = t.chars().next() else {
        return false;
    };
    if !(c.is_ascii_digit() || c == '.') {
        return false;
    }
    if t.len() > 2 && matches!(&t[..2], "0x" | "0o" | "0b") {
        return true;
    }
    if t.eq_ignore_ascii_case(".inf") || t.eq_ignore_ascii_case(".nan") {
        return true;
    }
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    if digits(t) {
        return true;
    }
    // Float: digits with one dot and/or an exponent.
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    if let Some(e) = exp {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let dots = mant.bytes().filter(|&b| b == b'.').count();
    let body_ok = mant
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'.' || b == b'_')
        && mant.bytes().any(|b| b.is_ascii_digit());
    if body_ok && (dots == 1 || (dots == 0 && exp.is_some())) {
        return true;
    }
    // A date such as 2024-01-01 (with an optional time after it).
    let b = t.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

// ---- reading ----------------------------------------------------------------------------------------------

struct Line<'a> {
    indent: usize,
    text: &'a str,
    at: usize,
}

/// Cut a trailing comment: a `#` at the start of the text or after a space, outside quotes.
fn strip_comment(body: &str) -> &str {
    let b = body.as_bytes();
    let mut in_double = false;
    let mut in_single = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_double {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_double = false;
            }
        } else if in_single {
            if c == b'\'' {
                in_single = false;
            }
        } else if c == b'#' && (i == 0 || b[i - 1] == b' ') {
            return &body[..i];
        } else if (c == b'"' || c == b'\'') && opens_scalar(b, i) {
            if c == b'"' {
                in_double = true;
            } else {
                in_single = true;
            }
        }
        i += 1;
    }
    body
}

/// A quote opens a string only at the start of a scalar: first thing on the line, or after `- ` or `: `.
fn opens_scalar(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && b[j - 1] == b' ' {
        j -= 1;
    }
    j == 0 || (j < i && (b[j - 1] == b':' || b[j - 1] == b'-'))
}

fn scan_lines(src: &str) -> R<Vec<Line<'_>>> {
    let mut lines = Vec::new();
    let mut off = 0;
    for raw in src.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let indent = line.bytes().take_while(|&b| b == b' ').count();
        if line[indent..].starts_with('\t') {
            return Err(PErr::new(
                off + indent,
                "tabs are not allowed in indentation; use spaces",
            ));
        }
        let text = strip_comment(&line[indent..]).trim_end();
        if !text.is_empty() {
            if text == "---" || text == "..." || text.starts_with("--- ") {
                return Err(PErr::new(
                    off + indent,
                    "document markers are not supported; a file is one document",
                ));
            }
            lines.push(Line {
                indent,
                text,
                at: off + indent,
            });
        }
        off += raw.len();
    }
    Ok(lines)
}

/// Parse a whole document; the top level must be a mapping.
pub fn parse(src: &str) -> R<Table> {
    let mut lines = scan_lines(src)?;
    if lines.is_empty() {
        return Ok(Table::default());
    }
    if lines[0].indent != 0 {
        return Err(PErr::new(
            lines[0].at,
            "the file must start at the first column",
        ));
    }
    let mut i = 0;
    let first = &lines[0];
    if is_item(first.text) {
        return Err(PErr::new(
            first.at,
            "the top level must be a mapping (`key: value`), not a list",
        ));
    }
    let node = mapping(&mut lines, &mut i, 0)?;
    if i < lines.len() {
        return Err(PErr::new(lines[i].at, "unexpected indentation"));
    }
    match node.v {
        Val::Table(t) => Ok(t),
        _ => unreachable!("mapping returns a table"),
    }
}

fn is_item(t: &str) -> bool {
    t == "-" || t.starts_with("- ")
}

fn block(ls: &mut Vec<Line<'_>>, i: &mut usize, indent: usize) -> R<Node> {
    if is_item(ls[*i].text) {
        sequence(ls, i, indent)
    } else {
        mapping(ls, i, indent)
    }
}

fn mapping(ls: &mut Vec<Line<'_>>, i: &mut usize, indent: usize) -> R<Node> {
    let start = ls[*i].at;
    let mut t = Table::default();
    while *i < ls.len() {
        let (ind, text, at) = (ls[*i].indent, ls[*i].text, ls[*i].at);
        if ind < indent {
            break;
        }
        if ind > indent {
            return Err(PErr::new(at, "unexpected indentation"));
        }
        if is_item(text) {
            return Err(PErr::new(at, "list items must be indented under their key"));
        }
        let (key, rest_idx) = split_key(text, at)?;
        let rest = text[rest_idx..].trim_start();
        let rest_at = at + (text.len() - rest.len());
        *i += 1;
        if t.entries.iter().any(|e| e.key == key) {
            return Err(PErr::new(at, format!("duplicate key `{key}`")));
        }
        let node = if rest.is_empty() {
            if *i < ls.len() && ls[*i].indent > indent {
                let child = ls[*i].indent;
                block(ls, i, child)?
            } else if *i < ls.len() && ls[*i].indent == indent && is_item(ls[*i].text) {
                return Err(PErr::new(
                    ls[*i].at,
                    "list items must be indented under their key",
                ));
            } else {
                return Err(PErr::new(at, format!("`{key}:` needs a value or an indented block (null is not supported; write `{{}}` or `[]` for empty)")));
            }
        } else {
            scalar(rest, rest_at)?
        };
        t.entries.push(Entry {
            key,
            key_at: at,
            node,
        });
    }
    Ok(Node {
        v: Val::Table(t),
        at: start,
    })
}

fn sequence(ls: &mut Vec<Line<'_>>, i: &mut usize, indent: usize) -> R<Node> {
    let start = ls[*i].at;
    let mut items = Vec::new();
    while *i < ls.len() {
        let (ind, text, at) = (ls[*i].indent, ls[*i].text, ls[*i].at);
        if ind < indent {
            break;
        }
        if ind > indent {
            return Err(PErr::new(at, "unexpected indentation"));
        }
        if !is_item(text) {
            return Err(PErr::new(at, "expected a list item starting with `- `"));
        }
        let rest = text[1..].trim_start();
        let rest_at = at + (text.len() - rest.len());
        if rest.is_empty() {
            *i += 1;
            if *i < ls.len() && ls[*i].indent > indent {
                let child = ls[*i].indent;
                items.push(block(ls, i, child)?);
            } else {
                return Err(PErr::new(at, "an empty list item is not supported"));
            }
        } else if is_item(rest) {
            return Err(PErr::new(rest_at, "a list inside a list is not supported"));
        } else if split_key(rest, rest_at).is_ok() {
            // `- key: value` opens a mapping whose other keys line up under the first.
            let col = indent + (text.len() - rest.len());
            ls[*i] = Line {
                indent: col,
                text: rest,
                at: rest_at,
            };
            items.push(mapping(ls, i, col)?);
        } else {
            *i += 1;
            items.push(scalar(rest, rest_at)?);
        }
    }
    Ok(Node {
        v: Val::Arr(items),
        at: start,
    })
}

/// Split `key: rest`; returns the key and the index in `text` just after the colon.
fn split_key(text: &str, at: usize) -> R<(String, usize)> {
    let b = text.as_bytes();
    if b[0] == b'"' || b[0] == b'\'' {
        let (key, used) = quoted_at(text, at)?;
        let after = text[used..].trim_start();
        if !after.starts_with(':') || !(after.len() == 1 || after.as_bytes()[1] == b' ') {
            return Err(PErr::new(at, "expected `key: value`"));
        }
        let colon = text.len() - after.len();
        return Ok((key, colon + 1));
    }
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ') {
            let key = text[..i].trim_end();
            if key.is_empty() {
                return Err(PErr::new(at, "expected a key before `:`"));
            }
            if "[{&*!|>%@`".contains(&key[..1]) {
                return Err(PErr::new(
                    at,
                    "a key cannot start with that character; quote it",
                ));
            }
            return Ok((key.to_string(), i + 1));
        }
        i += 1;
    }
    Err(PErr::new(at, "expected `key: value`"))
}

/// Parse a quoted string at the start of `text`; returns the string and how many bytes it used.
fn quoted_at(text: &str, at: usize) -> R<(String, usize)> {
    let quote = text.as_bytes()[0];
    let mut out = String::new();
    let mut i = 1;
    loop {
        let Some(c) = text[i..].chars().next() else {
            return Err(PErr::new(at, "string is not closed"));
        };
        i += c.len_utf8();
        if quote == b'\'' {
            if c == '\'' {
                if text[i..].starts_with('\'') {
                    out.push('\'');
                    i += 1;
                    continue;
                }
                return Ok((out, i));
            }
            out.push(c);
            continue;
        }
        match c {
            '"' => return Ok((out, i)),
            '\\' => {
                let esc_at = at + i - 1;
                let e = text[i..]
                    .chars()
                    .next()
                    .ok_or_else(|| PErr::new(at, "string is not closed"))?;
                i += e.len_utf8();
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'u' => {
                        let hex = text
                            .get(i..i + 4)
                            .ok_or_else(|| PErr::new(esc_at, "bad \\u escape"))?;
                        let n = u32::from_str_radix(hex, 16)
                            .map_err(|_| PErr::new(esc_at, "bad \\u escape"))?;
                        out.push(
                            char::from_u32(n).ok_or_else(|| PErr::new(esc_at, "bad \\u escape"))?,
                        );
                        i += 4;
                    }
                    other => return Err(PErr::new(esc_at, format!("unknown escape `\\{other}`"))),
                }
            }
            c => out.push(c),
        }
    }
}

fn scalar(text: &str, at: usize) -> R<Node> {
    let first = text.as_bytes()[0];
    let v = match first {
        b'"' | b'\'' => {
            let (s, used) = quoted_at(text, at)?;
            if !text[used..].trim().is_empty() {
                return Err(PErr::new(at + used, "unexpected text after the string"));
            }
            Val::Str(s)
        }
        b'{' | b'[' => {
            let empty = if first == b'{' { "{}" } else { "[]" };
            if text.replace(' ', "") != empty {
                return Err(PErr::new(at, "flow style (`{ }` and `[ ]` with content) is not supported; use indented blocks"));
            }
            if first == b'{' {
                Val::Table(Table::default())
            } else {
                Val::Arr(Vec::new())
            }
        }
        b'&' | b'*' | b'!' => {
            return Err(PErr::new(at, "anchors, aliases and tags are not supported"))
        }
        b'|' | b'>' => {
            return Err(PErr::new(
                at,
                "block scalars (`|` and `>`) are not supported",
            ))
        }
        b'%' | b'@' | b'`' => {
            return Err(PErr::new(
                at,
                "a plain value cannot start with that character; quote it",
            ))
        }
        _ => plain(text, at)?,
    };
    Ok(Node { v, at })
}

fn plain(text: &str, at: usize) -> R<Val> {
    if text.contains(": ") || text.ends_with(':') {
        return Err(PErr::new(
            at,
            "unexpected `:` in a value; quote the value if it is text",
        ));
    }
    match text {
        "true" => return Ok(Val::Bool(true)),
        "false" => return Ok(Val::Bool(false)),
        _ => {}
    }
    if word_is_special(text) {
        let hint = if text.eq_ignore_ascii_case("null") || text == "~" {
            "null is not supported"
        } else {
            "YAML tools disagree about this word; write true or false, or quote it to make it text"
        };
        return Err(PErr::new(at, format!("`{text}`: {hint}")));
    }
    if looks_numeric(text) {
        let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            if digits.len() > 1 && digits.starts_with('0') {
                return Err(PErr::new(at, format!("`{text}`: leading zeros are read differently by different YAML tools; quote it")));
            }
            return text
                .parse::<i64>()
                .map(Val::Int)
                .map_err(|_| PErr::new(at, "integer is too large"));
        }
        return Err(PErr::new(at, format!("`{text}` would be read as a number or date by other YAML tools; quote it to make it text")));
    }
    Ok(Val::Str(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(t: &'a Table, path: &str) -> &'a Val {
        let mut cur = t;
        let parts: Vec<&str> = path.split('.').collect();
        for (n, p) in parts.iter().enumerate() {
            let node = cur.get(p).unwrap_or_else(|| panic!("missing {p}"));
            if n + 1 == parts.len() {
                return &node.v;
            }
            cur = match &node.v {
                Val::Table(t) => t,
                _ => panic!("{p} is not a mapping"),
            };
        }
        unreachable!()
    }

    fn err(src: &str) -> Diag {
        Diag::new("t.yaml", src, parse(src).unwrap_err())
    }

    #[test]
    fn scalars_mappings_and_comments() {
        let t = parse("# top\nformat: 1\npackage:\n  name: a # note\n  ok: true\n  lit: 'x\\y'\n  neg: -3\n  v: 1.0.3\n  lic: MIT OR Apache-2.0\n").unwrap();
        assert_eq!(get(&t, "format"), &Val::Int(1));
        assert_eq!(get(&t, "package.name"), &Val::Str("a".into()));
        assert_eq!(get(&t, "package.ok"), &Val::Bool(true));
        assert_eq!(get(&t, "package.lit"), &Val::Str("x\\y".into()));
        assert_eq!(get(&t, "package.neg"), &Val::Int(-3));
        assert_eq!(get(&t, "package.v"), &Val::Str("1.0.3".into()));
        assert_eq!(
            get(&t, "package.lic"),
            &Val::Str("MIT OR Apache-2.0".into())
        );
    }

    #[test]
    fn slashes_at_signs_and_quoted_keys() {
        let t = parse("deps:\n  json/v2: 2.0.1\n  cli-args@1.0.3: x\n  \"no\": w\n  libs/*: z\n")
            .unwrap();
        assert_eq!(get(&t, "deps.json/v2"), &Val::Str("2.0.1".into()));
        match get(&t, "deps") {
            Val::Table(d) => assert_eq!(d.get("cli-args@1.0.3").unwrap().v, Val::Str("x".into())),
            v => panic!("{v:?}"),
        }
        assert_eq!(get(&t, "deps.no"), &Val::Str("w".into()));
        assert_eq!(get(&t, "deps.libs/*"), &Val::Str("z".into()));
    }

    #[test]
    fn lists_of_scalars_and_of_mappings() {
        let t = parse("members:\n  - libs/*\n  - apps/*\nexception:\n  - package: json\n    version: 1.2.0\n    members:\n      - a\n  - package: uart\n    version: 0.3.0\n").unwrap();
        match get(&t, "members") {
            Val::Arr(items) => assert_eq!(items.len(), 2),
            v => panic!("{v:?}"),
        }
        match get(&t, "exception") {
            Val::Arr(items) => {
                assert_eq!(items.len(), 2);
                match &items[0].v {
                    Val::Table(t0) => {
                        assert_eq!(t0.get("version").unwrap().v, Val::Str("1.2.0".into()));
                        assert!(matches!(t0.get("members").unwrap().v, Val::Arr(_)));
                    }
                    v => panic!("{v:?}"),
                }
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn empty_collections() {
        let t = parse("a: {}\nb: []\n").unwrap();
        assert_eq!(get(&t, "a"), &Val::Table(Table::default()));
        assert_eq!(get(&t, "b"), &Val::Arr(Vec::new()));
    }

    #[test]
    fn string_escapes_and_hash_inside_quotes() {
        let t = parse("s: \"a\\\"b\\n\\u00e9 # not a comment\"\nq: 'it''s'\n").unwrap();
        assert_eq!(
            get(&t, "s"),
            &Val::Str("a\"b\n\u{e9} # not a comment".into())
        );
        assert_eq!(get(&t, "q"), &Val::Str("it's".into()));
    }

    #[test]
    fn apostrophe_in_a_plain_value_is_not_a_quote() {
        let t = parse("d: don't stop # comment\n").unwrap();
        assert_eq!(get(&t, "d"), &Val::Str("don't stop".into()));
    }

    #[test]
    fn errors_have_line_and_column() {
        let d = err("format: 1\nname: yes\n");
        assert_eq!((d.line, d.col), (2, 7));
        assert!(d.msg.contains("YAML tools disagree"), "{}", d.msg);
        assert_eq!(d.to_string(), format!("t.yaml:2:7: {}", d.msg));

        let d = err("a: 1\na: 2\n");
        assert_eq!((d.line, d.col), (2, 1));
        assert!(d.msg.contains("duplicate key"));

        assert!(err("a: 1.5\n").msg.contains("number or date"));
        assert!(err("a: 0x1F\n").msg.contains("number or date"));
        assert!(err("a: 2024-01-01\n").msg.contains("number or date"));
        assert!(err("a: 007\n").msg.contains("leading zeros"));
        assert!(err("a: null\n").msg.contains("null is not supported"));
        assert!(err("a: ~\n").msg.contains("null is not supported"));
        assert!(err("a:\nb: 1\n").msg.contains("needs a value"));
        assert!(err("a: \"open\n").msg.contains("not closed"));
        assert!(err("a: \"x\" y\n").msg.contains("after the string"));
        assert!(err("a: [1, 2]\n").msg.contains("flow style"));
        assert!(err("a: {b: 1}\n").msg.contains("flow style"));
        assert!(err("a: &x 1\n").msg.contains("anchors"));
        assert!(err("a: *x\n").msg.contains("anchors"));
        assert!(err("a: !tag 1\n").msg.contains("anchors"));
        assert!(err("a: |\n").msg.contains("block scalars"));
        assert!(err("---\na: 1\n").msg.contains("document markers"));
        assert!(err("a:\n\tb: 1\n").msg.contains("tabs"));
        assert!(err("a: b: c\n").msg.contains("unexpected `:`"));
        assert!(err("just text\n").msg.contains("key: value"));
        assert!(err("- a\n").msg.contains("top level"));
        assert!(err("  a: 1\n").msg.contains("first column"));
        assert!(err("a: 1\n  b: 2\n").msg.contains("unexpected indentation"));
        assert!(err("a:\n    b: 1\n  c: 2\n")
            .msg
            .contains("unexpected indentation"));
        assert!(err("a:\n- x\n").msg.contains("indented under their key"));
        assert!(err("a:\n  - - x\n").msg.contains("list inside a list"));
        assert!(err("a:\n  -\nb: 1\n").msg.contains("empty list item"));
    }

    #[test]
    fn crlf_and_missing_final_newline_and_blank_lines() {
        let t = parse("a: 1\r\n\r\nt:\r\n  b: 2").unwrap();
        assert_eq!(get(&t, "t.b"), &Val::Int(2));
        assert_eq!(parse("# only a comment\n").unwrap(), Table::default());
        assert_eq!(parse("").unwrap(), Table::default());
    }

    #[test]
    fn writer_quotes_what_other_tools_would_misread() {
        for s in [
            "",
            "yes",
            "No",
            "null",
            "~",
            "true",
            "1.5",
            "007",
            "0x1F",
            "2024-01-01",
            "a: b",
            "a #b",
            "@x",
            "*.pel",
            "- x",
            " pad",
            "x:",
            "1e3",
            "12345",
        ] {
            assert!(needs_quotes(s), "{s:?} should be quoted");
            let doc = format!("k: {}\n", string(s));
            assert_eq!(
                get(&parse(&doc).unwrap(), "k"),
                &Val::Str(s.into()),
                "{doc}"
            );
        }
        for s in [
            "1.0.3",
            "MIT OR Apache-2.0",
            "libs/*",
            "json/v2",
            "cli-args@1.0.3",
            "-x",
            "a.b",
            "apps/legacy-export",
            ".",
        ] {
            assert!(!needs_quotes(s), "{s:?} should stay plain");
            let doc = format!("k: {}\n", string(s));
            assert_eq!(
                get(&parse(&doc).unwrap(), "k"),
                &Val::Str(s.into()),
                "{doc}"
            );
        }
        assert_eq!(quoted("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn never_panics_on_garbage() {
        let mut seed = 0x1234_5678u32;
        let alphabet = b"[]{}:\"'-#\\ \n\n  abc019&*!|>~.,@%\t";
        for _ in 0..4000 {
            let mut s = String::new();
            for _ in 0..(seed % 50) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                s.push(alphabet[(seed >> 16) as usize % alphabet.len()] as char);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let _ = parse(&s);
        }
    }
}
