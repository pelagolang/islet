//! A small TOML reader: the subset Islet's files use, with byte offsets on every key and value so errors can
//! name a line and column.
//!
//! Supported: comments, bare and quoted keys, dotted keys, `[table]` and `[[array of tables]]` headers, basic
//! (`"..."`) and literal (`'...'`) strings, integers, booleans, arrays (several lines allowed) and inline tables.
//! Not supported, and reported as such: floats, dates and times, multi-line strings. A duplicate key or table is
//! an error, as in TOML itself.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
    /// `aot` is true for an array of tables written with `[[name]]` headers.
    Arr {
        items: Vec<Node>,
        aot: bool,
    },
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
    explicit: bool,
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
        let at = e.at.min(src.len());
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

struct P<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

type R<T> = Result<T, PErr>;

/// Parse a whole document into its root table.
pub fn parse(src: &str) -> R<Table> {
    let mut p = P {
        s: src,
        b: src.as_bytes(),
        i: 0,
    };
    let mut root = Table {
        explicit: true,
        ..Table::default()
    };
    let mut cur: Vec<String> = Vec::new();
    loop {
        p.skip_blank();
        if p.i >= p.b.len() {
            return Ok(root);
        }
        if p.b[p.i] == b'[' {
            cur = p.header(&mut root)?;
        } else {
            let (path, key_at) = p.key_path()?;
            p.ws();
            p.expect(b'=', "expected `=` after the key")?;
            p.ws();
            let node = p.value()?;
            p.end_of_line()?;
            let mut t = &mut root;
            for seg in &cur {
                t = enter(t, seg, key_at, true)?;
            }
            let (last, dirs) = path.split_last().expect("a key path has at least one part");
            for seg in dirs {
                t = enter(t, seg, key_at, false)?;
            }
            if t.entries.iter().any(|e| &e.key == last) {
                return Err(PErr::new(key_at, format!("duplicate key `{last}`")));
            }
            t.entries.push(Entry {
                key: last.clone(),
                key_at,
                node,
            });
        }
    }
}

/// Step into the table called `key`, creating it if it is missing. With `through_arrays`, an array of tables
/// steps into its last element (what a `[a.b]` header means after `[[a]]`).
fn enter<'a>(t: &'a mut Table, key: &str, at: usize, through_arrays: bool) -> R<&'a mut Table> {
    let idx = match t.entries.iter().position(|e| e.key == key) {
        Some(i) => i,
        None => {
            t.entries.push(Entry {
                key: key.to_string(),
                key_at: at,
                node: Node {
                    v: Val::Table(Table::default()),
                    at,
                },
            });
            t.entries.len() - 1
        }
    };
    match &mut t.entries[idx].node.v {
        Val::Table(sub) => Ok(sub),
        Val::Arr { items, aot: true } if through_arrays => match items.last_mut() {
            Some(Node {
                v: Val::Table(sub), ..
            }) => Ok(sub),
            _ => Err(PErr::new(at, format!("`{key}` is not a table"))),
        },
        _ => Err(PErr::new(
            at,
            format!("`{key}` is already defined and is not a table"),
        )),
    }
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] == b' ' || self.b[self.i] == b'\t') {
            self.i += 1;
        }
    }

    fn comment(&mut self) {
        if self.i < self.b.len() && self.b[self.i] == b'#' {
            while self.i < self.b.len() && self.b[self.i] != b'\n' {
                self.i += 1;
            }
        }
    }

    /// Skip spaces, comments and line ends between statements.
    fn skip_blank(&mut self) {
        loop {
            self.ws();
            self.comment();
            if self.i < self.b.len() && (self.b[self.i] == b'\n' || self.b[self.i] == b'\r') {
                self.i += 1;
            } else {
                return;
            }
        }
    }

    fn end_of_line(&mut self) -> R<()> {
        self.ws();
        self.comment();
        match self.b.get(self.i) {
            None => Ok(()),
            Some(b'\n') | Some(b'\r') => Ok(()),
            Some(_) => Err(PErr::new(self.i, "unexpected text after the value")),
        }
    }

    fn expect(&mut self, c: u8, msg: &str) -> R<()> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(PErr::new(self.i, msg))
        }
    }

    fn key_part(&mut self) -> R<String> {
        match self.b.get(self.i) {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            _ => {
                let start = self.i;
                while self.i < self.b.len()
                    && (self.b[self.i].is_ascii_alphanumeric() || b"_-".contains(&self.b[self.i]))
                {
                    self.i += 1;
                }
                if start == self.i {
                    Err(PErr::new(start, "expected a key"))
                } else {
                    Ok(self.s[start..self.i].to_string())
                }
            }
        }
    }

    fn key_path(&mut self) -> R<(Vec<String>, usize)> {
        let at = self.i;
        let mut path = vec![self.key_part()?];
        loop {
            self.ws();
            if self.b.get(self.i) == Some(&b'.') {
                self.i += 1;
                self.ws();
                path.push(self.key_part()?);
            } else {
                return Ok((path, at));
            }
        }
    }

    /// Parse `[a.b]` or `[[a.b]]` and open that table; returns the path that later key lines belong to.
    fn header(&mut self, root: &mut Table) -> R<Vec<String>> {
        let at = self.i;
        let aot = self.b.get(self.i + 1) == Some(&b'[');
        self.i += if aot { 2 } else { 1 };
        self.ws();
        let (path, _) = self.key_path()?;
        self.ws();
        self.expect(b']', "expected `]` to close the header")?;
        if aot {
            self.expect(b']', "expected `]]` to close the header")?;
        }
        self.end_of_line()?;
        let (last, dirs) = path.split_last().expect("a header has at least one part");
        let mut t = &mut *root;
        for seg in dirs {
            t = enter(t, seg, at, true)?;
        }
        let existing = t.entries.iter().position(|e| &e.key == last);
        let fresh = Table {
            explicit: true,
            ..Table::default()
        };
        match (existing, aot) {
            (None, false) => {
                t.entries.push(Entry {
                    key: last.clone(),
                    key_at: at,
                    node: Node {
                        v: Val::Table(fresh),
                        at,
                    },
                });
            }
            (None, true) => {
                let item = Node {
                    v: Val::Table(fresh),
                    at,
                };
                t.entries.push(Entry {
                    key: last.clone(),
                    key_at: at,
                    node: Node {
                        v: Val::Arr {
                            items: vec![item],
                            aot: true,
                        },
                        at,
                    },
                });
            }
            (Some(i), false) => match &mut t.entries[i].node.v {
                Val::Table(sub) if !sub.explicit => sub.explicit = true,
                _ => {
                    return Err(PErr::new(
                        at,
                        format!("duplicate table `{}`", path.join(".")),
                    ))
                }
            },
            (Some(i), true) => match &mut t.entries[i].node.v {
                Val::Arr { items, aot: true } => items.push(Node {
                    v: Val::Table(fresh),
                    at,
                }),
                _ => {
                    return Err(PErr::new(
                        at,
                        format!(
                            "`{}` is already defined and is not an array of tables",
                            path.join(".")
                        ),
                    ))
                }
            },
        }
        Ok(path)
    }

    fn value(&mut self) -> R<Node> {
        let at = self.i;
        let v = match self.b.get(self.i) {
            None | Some(b'\n') | Some(b'\r') | Some(b'#') => {
                return Err(PErr::new(at, "expected a value"))
            }
            Some(b'"') => {
                if self.s[self.i..].starts_with("\"\"\"") {
                    return Err(PErr::new(at, "multi-line strings are not supported"));
                }
                Val::Str(self.basic_string()?)
            }
            Some(b'\'') => {
                if self.s[self.i..].starts_with("'''") {
                    return Err(PErr::new(at, "multi-line strings are not supported"));
                }
                Val::Str(self.literal_string()?)
            }
            Some(b'[') => self.array()?,
            Some(b'{') => self.inline_table()?,
            Some(_) => self.bare_value()?,
        };
        Ok(Node { v, at })
    }

    fn bare_value(&mut self) -> R<Val> {
        let start = self.i;
        while self.i < self.b.len() && !b" \t\r\n,]}#".contains(&self.b[self.i]) {
            self.i += 1;
        }
        let word = &self.s[start..self.i];
        match word {
            "true" => Ok(Val::Bool(true)),
            "false" => Ok(Val::Bool(false)),
            _ => {
                let digits = word.strip_prefix(['+', '-']).unwrap_or(word);
                if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
                    word.parse::<i64>()
                        .map(Val::Int)
                        .map_err(|_| PErr::new(start, "integer is too large"))
                } else if digits.bytes().next().is_some_and(|c| c.is_ascii_digit()) {
                    Err(PErr::new(
                        start,
                        "floats, dates and times are not supported",
                    ))
                } else {
                    Err(PErr::new(
                        start,
                        format!("unknown value `{word}`; strings need quotes"),
                    ))
                }
            }
        }
    }

    fn basic_string(&mut self) -> R<String> {
        let start = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.s[self.i..].chars().next() else {
                return Err(PErr::new(start, "string is not closed"));
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '\n' | '\r' => return Err(PErr::new(start, "string is not closed on its line")),
                '\\' => {
                    let at = self.i;
                    self.i += 1;
                    let e = self.s[self.i..]
                        .chars()
                        .next()
                        .ok_or_else(|| PErr::new(at, "string is not closed"))?;
                    self.i += e.len_utf8();
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let hex = self
                                .s
                                .get(self.i..self.i + 4)
                                .ok_or_else(|| PErr::new(at, "bad \\u escape"))?;
                            let n = u32::from_str_radix(hex, 16)
                                .map_err(|_| PErr::new(at, "bad \\u escape"))?;
                            out.push(
                                char::from_u32(n).ok_or_else(|| PErr::new(at, "bad \\u escape"))?,
                            );
                            self.i += 4;
                        }
                        other => return Err(PErr::new(at, format!("unknown escape `\\{other}`"))),
                    }
                }
                c => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }

    fn literal_string(&mut self) -> R<String> {
        let start = self.i;
        self.i += 1;
        let from = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'\'' && self.b[self.i] != b'\n' {
            self.i += 1;
        }
        if self.b.get(self.i) != Some(&b'\'') {
            return Err(PErr::new(start, "string is not closed"));
        }
        let s = self.s[from..self.i].to_string();
        self.i += 1;
        Ok(s)
    }

    fn array(&mut self) -> R<Val> {
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            match self.b.get(self.i) {
                None => return Err(PErr::new(self.i, "array is not closed")),
                Some(b']') => {
                    self.i += 1;
                    return Ok(Val::Arr { items, aot: false });
                }
                _ => {}
            }
            items.push(self.value()?);
            self.skip_blank();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {}
                None => return Err(PErr::new(self.i, "array is not closed")),
                _ => return Err(PErr::new(self.i, "expected `,` or `]` in the array")),
            }
        }
    }

    fn inline_table(&mut self) -> R<Val> {
        self.i += 1;
        let mut t = Table {
            explicit: true,
            ..Table::default()
        };
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Val::Table(t));
        }
        loop {
            self.ws();
            let (path, key_at) = self.key_path()?;
            self.ws();
            self.expect(b'=', "expected `=` after the key")?;
            self.ws();
            let node = self.value()?;
            let mut cur = &mut t;
            let (last, dirs) = path.split_last().expect("a key path has at least one part");
            for seg in dirs {
                cur = enter(cur, seg, key_at, false)?;
            }
            if cur.entries.iter().any(|e| &e.key == last) {
                return Err(PErr::new(key_at, format!("duplicate key `{last}`")));
            }
            cur.entries.push(Entry {
                key: last.clone(),
                key_at,
                node,
            });
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Val::Table(t));
                }
                _ => {
                    return Err(PErr::new(
                        self.i,
                        "expected `,` or `}` in the inline table (it must fit on one line)",
                    ))
                }
            }
        }
    }
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
                _ => panic!("{p} is not a table"),
            };
        }
        unreachable!()
    }

    fn err(src: &str) -> Diag {
        Diag::new("t.toml", src, parse(src).unwrap_err())
    }

    #[test]
    fn scalars_tables_and_comments() {
        let t = parse("# top\nformat = 1\n[package]\nname = \"a\" # note\nok = true\nlit = 'x\\y'\nneg = -3\n").unwrap();
        assert_eq!(get(&t, "format"), &Val::Int(1));
        assert_eq!(get(&t, "package.name"), &Val::Str("a".into()));
        assert_eq!(get(&t, "package.ok"), &Val::Bool(true));
        assert_eq!(get(&t, "package.lit"), &Val::Str("x\\y".into()));
        assert_eq!(get(&t, "package.neg"), &Val::Int(-3));
    }

    #[test]
    fn quoted_dotted_and_inline() {
        let t = parse(
            "[dependencies]\n\"json/v2\" = { version = \"2.0.1\", package = \"json\" }\na.b = 1\n",
        )
        .unwrap();
        assert!(matches!(get(&t, "dependencies.json/v2"), Val::Table(_)));
        assert_eq!(
            get(&t, "dependencies.json/v2.version"),
            &Val::Str("2.0.1".into())
        );
        assert_eq!(get(&t, "dependencies.a.b"), &Val::Int(1));
    }

    #[test]
    fn arrays_span_lines_and_allow_trailing_comma() {
        let t = parse("m = [\n  \"a\", # first\n  \"b\",\n]\n").unwrap();
        match get(&t, "m") {
            Val::Arr { items, aot } => {
                assert!(!aot);
                assert_eq!(items.len(), 2);
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn array_of_tables_and_subtables() {
        let t = parse("[[exception]]\npackage = \"a\"\n[exception.extra]\nn = 1\n[[exception]]\npackage = \"b\"\n").unwrap();
        match get(&t, "exception") {
            Val::Arr { items, aot } => {
                assert!(aot);
                assert_eq!(items.len(), 2);
                match &items[0].v {
                    Val::Table(t0) => assert!(t0.get("extra").is_some()),
                    v => panic!("{v:?}"),
                }
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn string_escapes() {
        let t = parse("s = \"a\\\"b\\n\\u00e9\"\n").unwrap();
        assert_eq!(get(&t, "s"), &Val::Str("a\"b\n\u{e9}".into()));
    }

    #[test]
    fn errors_have_line_and_column() {
        let d = err("format = 1\nname = nope\n");
        assert_eq!((d.line, d.col), (2, 8));
        assert!(d.msg.contains("strings need quotes"), "{}", d.msg);
        assert_eq!(d.to_string(), format!("t.toml:2:8: {}", d.msg));

        let d = err("a = 1\na = 2\n");
        assert_eq!((d.line, d.col), (2, 1));
        assert!(d.msg.contains("duplicate key"));

        assert!(err("[a]\n[a]\n").msg.contains("duplicate table"));
        assert!(err("a = \"open\n").msg.contains("not closed"));
        assert!(err("a = 1 2\n").msg.contains("after the value"));
        assert!(err("a = 1.5\n").msg.contains("not supported"));
        assert!(err("a = \"\"\"x\"\"\"\n").msg.contains("multi-line"));
        assert!(err("a = [1, 2\n").msg.contains("not closed"));
        assert!(err("a = 1\n[a.b]\n").msg.contains("not a table"));
        assert!(err("a = {\n").msg.contains("expected"));
        assert!(err("= 1\n").msg.contains("expected a key"));
    }

    #[test]
    fn crlf_and_missing_final_newline() {
        let t = parse("a = 1\r\n[t]\r\nb = 2").unwrap();
        assert_eq!(get(&t, "t.b"), &Val::Int(2));
    }

    #[test]
    fn never_panics_on_garbage() {
        let mut seed = 0x1234_5678u32;
        let alphabet = b"[]{}=\"'.,#\\ \n\tabc019-+u";
        for _ in 0..3000 {
            let mut s = String::new();
            for _ in 0..(seed % 40) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                s.push(alphabet[(seed >> 16) as usize % alphabet.len()] as char);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let _ = parse(&s);
        }
    }
}
