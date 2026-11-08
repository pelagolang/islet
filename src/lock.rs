//! `islet.lock`: the committed record of what was resolved. One canonical form, so the same resolution always
//! produces the same bytes and a diff shows only real changes.
//!
//! ```text
//! # islet.lock: written by islet, do not edit.
//! format = 1
//!
//! [[package]]
//! name = "json"
//! version = "1.5.0"
//! hash = "<64 hex digits: the store address of the package's source>"
//! dependencies = ["uart@0.3.0"]
//! ```
//!
//! Packages are sorted by name, then version. `name` is the key as written in a manifest, so a second major is
//! `json/v2`. `dependencies` is sorted, written only when not empty, and every entry must be a package of the
//! same file.

use crate::manifest::{is_dep_key, Version};
use crate::toml::{self, Diag, Node, PErr, Val};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    pub name: String,
    pub version: Version,
    /// Store address (64 lowercase hex digits).
    pub hash: String,
    /// `name@version` of other packages in the lock.
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lock {
    pub packages: Vec<Locked>,
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Lock {
    /// The canonical text. Sorting happens here, so callers may add packages in any order.
    pub fn to_text(&self) -> String {
        let mut pkgs: Vec<&Locked> = self.packages.iter().collect();
        pkgs.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
        let mut out = String::from("# islet.lock: written by islet, do not edit.\nformat = 1\n");
        for p in pkgs {
            out.push_str("\n[[package]]\n");
            out.push_str(&format!(
                "name = {}\nversion = {}\nhash = {}\n",
                quote(&p.name),
                quote(&p.version.to_string()),
                quote(&p.hash)
            ));
            if !p.dependencies.is_empty() {
                let mut d = p.dependencies.clone();
                d.sort();
                let list: Vec<String> = d.iter().map(|x| quote(x)).collect();
                out.push_str(&format!("dependencies = [{}]\n", list.join(", ")));
            }
        }
        out
    }
}

type R<T> = Result<T, PErr>;

fn string(n: &Node, what: &str) -> R<String> {
    match &n.v {
        Val::Str(s) => Ok(s.clone()),
        _ => Err(PErr::new(n.at, format!("{what} must be a string"))),
    }
}

pub fn parse_lock(file: &str, src: &str) -> Result<Lock, Diag> {
    let inner = || -> R<Lock> {
        let root = toml::parse(src)?;
        match root.get("format") {
            Some(Node { v: Val::Int(1), .. }) => {}
            Some(n) => {
                return Err(PErr::new(
                    n.at,
                    "`format` must be 1; this version of islet reads only format 1",
                ))
            }
            None => return Err(PErr::new(0, "the lock needs `format = 1`")),
        }
        for e in &root.entries {
            if e.key != "format" && e.key != "package" {
                return Err(PErr::new(
                    e.key_at,
                    format!("unknown key `{}` in the lock", e.key),
                ));
            }
        }
        let mut lock = Lock::default();
        let mut dep_at: Vec<(usize, usize, String)> = Vec::new(); // (package index, offset, entry)
        if let Some(pn) = root.get("package") {
            let items = match &pn.v {
                Val::Arr { items, aot: true } => items,
                _ => {
                    return Err(PErr::new(
                        pn.at,
                        "`package` must be written as `[[package]]` tables",
                    ))
                }
            };
            for item in items {
                let t = match &item.v {
                    Val::Table(t) => t,
                    _ => return Err(PErr::new(item.at, "a `[[package]]` must be a table")),
                };
                for e in &t.entries {
                    if !["name", "version", "hash", "dependencies"].contains(&e.key.as_str()) {
                        return Err(PErr::new(
                            e.key_at,
                            format!("unknown key `{}` in [[package]]", e.key),
                        ));
                    }
                }
                let get = |k: &str| {
                    t.get(k)
                        .ok_or_else(|| PErr::new(item.at, format!("[[package]] needs `{k}`")))
                };
                let nn = get("name")?;
                let name = string(nn, "`name`")?;
                if !is_dep_key(&name) {
                    return Err(PErr::new(nn.at, format!("`{name}` is not a package name")));
                }
                let vn = get("version")?;
                let vs = string(vn, "`version`")?;
                let version = Version::parse(&vs)
                    .ok_or_else(|| PErr::new(vn.at, format!("`{vs}` is not a version")))?;
                let hn = get("hash")?;
                let hash = string(hn, "`hash`")?;
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(PErr::new(hn.at, "`hash` must be 64 lowercase hex digits"));
                }
                let mut dependencies = Vec::new();
                if let Some(dn) = t.get("dependencies") {
                    match &dn.v {
                        Val::Arr { items, .. } => {
                            for i in items {
                                let s = string(i, "every dependency")?;
                                dep_at.push((lock.packages.len(), i.at, s.clone()));
                                dependencies.push(s);
                            }
                        }
                        _ => {
                            return Err(PErr::new(
                                dn.at,
                                "`dependencies` must be an array of strings",
                            ))
                        }
                    }
                }
                if lock
                    .packages
                    .iter()
                    .any(|p| p.name == name && p.version == version)
                {
                    return Err(PErr::new(
                        nn.at,
                        format!("`{name}@{version}` is listed twice"),
                    ));
                }
                lock.packages.push(Locked {
                    name,
                    version,
                    hash,
                    dependencies,
                });
            }
        }
        for (_, at, entry) in dep_at {
            let known = entry.split_once('@').is_some_and(|(n, v)| {
                Version::parse(v)
                    .is_some_and(|v| lock.packages.iter().any(|p| p.name == n && p.version == v))
            });
            if !known {
                return Err(PErr::new(at, format!("dependency `{entry}` is not a package in this lock (expected `name@version`)")));
            }
        }
        Ok(lock)
    };
    inner().map_err(|e| Diag::new(file, src, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_canonical(src: &str) -> Result<bool, Diag> {
        Ok(parse_lock("islet.lock", src)?.to_text() == src)
    }

    fn pkg(name: &str, v: &str, h: char, deps: &[&str]) -> Locked {
        Locked {
            name: name.into(),
            version: Version::parse(v).unwrap(),
            hash: h.to_string().repeat(64),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sample() -> Lock {
        Lock {
            packages: vec![
                pkg("uart", "0.3.0", 'c', &[]),
                pkg("json/v2", "2.0.1", 'b', &["uart@0.3.0"]),
                pkg("json", "1.5.0", 'a', &[]),
            ],
        }
    }

    #[test]
    fn output_is_sorted_and_stable() {
        let text = sample().to_text();
        let want = format!(
            "# islet.lock: written by islet, do not edit.\nformat = 1\n\n[[package]]\nname = \"json\"\nversion = \"1.5.0\"\nhash = \"{a}\"\n\n[[package]]\nname = \"json/v2\"\nversion = \"2.0.1\"\nhash = \"{b}\"\ndependencies = [\"uart@0.3.0\"]\n\n[[package]]\nname = \"uart\"\nversion = \"0.3.0\"\nhash = \"{c}\"\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
            c = "c".repeat(64)
        );
        assert_eq!(text, want);
        // Any input order gives the same bytes.
        let mut rev = sample();
        rev.packages.reverse();
        assert_eq!(rev.to_text(), text);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let text = sample().to_text();
        let parsed = parse_lock("islet.lock", &text).unwrap();
        assert_eq!(parsed.to_text(), text);
        assert!(is_canonical(&text).unwrap());
        assert!(!is_canonical(&text.replace(
            "\n\n[[package]]\nname = \"uart\"",
            "\n[[package]]\nname = \"uart\""
        ))
        .unwrap());
    }

    #[test]
    fn an_empty_lock_round_trips() {
        let text = Lock::default().to_text();
        assert_eq!(parse_lock("islet.lock", &text).unwrap(), Lock::default());
    }

    #[test]
    fn dependencies_are_sorted_on_write() {
        let lock = Lock {
            packages: vec![
                pkg("a", "1.0.0", 'a', &["c@1.0.0", "b@1.0.0"]),
                pkg("b", "1.0.0", 'b', &[]),
                pkg("c", "1.0.0", 'c', &[]),
            ],
        };
        assert!(lock
            .to_text()
            .contains("dependencies = [\"b@1.0.0\", \"c@1.0.0\"]"));
    }

    #[test]
    fn bad_locks_are_refused_with_a_position() {
        let h = "a".repeat(64);
        let head = "format = 1\n";
        let d = parse_lock(
            "islet.lock",
            &format!("{head}[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nhash = \"xyz\"\n"),
        )
        .unwrap_err();
        assert_eq!((d.line, d.col), (5, 8));
        assert!(d.msg.contains("64 lowercase hex"));
        let e = |body: &str| {
            parse_lock("islet.lock", &format!("{head}{body}"))
                .unwrap_err()
                .msg
        };
        assert!(e(&format!("[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nhash = \"{h}\"\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nhash = \"{h}\"\n")).contains("listed twice"));
        assert!(e(&format!("[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nhash = \"{h}\"\ndependencies = [\"b@1.0.0\"]\n")).contains("not a package in this lock"));
        assert!(
            e(&format!("[[package]]\nname = \"a\"\nhash = \"{h}\"\n")).contains("needs `version`")
        );
        assert!(e(&format!(
            "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nhash = \"{h}\"\nextra = 1\n"
        ))
        .contains("unknown key `extra`"));
        assert!(parse_lock("islet.lock", "[[package]]\n")
            .unwrap_err()
            .msg
            .contains("format = 1"));
        assert!(parse_lock("islet.lock", "format = 7\n")
            .unwrap_err()
            .msg
            .contains("only format 1"));
        assert!(e("[package]\n").contains("[[package]]"));
    }
}
