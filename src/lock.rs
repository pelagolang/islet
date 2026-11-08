//! `islet.lock`: the committed record of what was resolved. One canonical form, so the same resolution always
//! produces the same bytes and a diff shows only real changes. One line per package.
//!
//! ```text
//! # islet.lock: written by islet, do not edit.
//! format = 1
//!
//! [packages]
//! "json@1.5.0"    = { hash = "<64 hex digits>" }
//! "json/v2@2.0.1" = { hash = "<64 hex digits>", needs = ["uart@0.3.0"] }
//! "uart@0.3.0"    = { hash = "<64 hex digits>" }
//! ```
//!
//! The key is `name@version`, where `name` is the key as written in a manifest, so a second major is `json/v2`.
//! `hash` is the store address of the package's source. `needs` lists the packages it depends on, sorted, as
//! `name@version`; it is written only when not empty, and every entry must be a key of the same file. Keys are
//! sorted by name, then version, and padded so the `=` signs line up.

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

impl Locked {
    fn key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
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
        if pkgs.is_empty() {
            return out;
        }
        out.push_str("\n[packages]\n");
        let width = pkgs
            .iter()
            .map(|p| quote(&p.key()).chars().count())
            .max()
            .unwrap_or(0);
        for p in pkgs {
            let key = quote(&p.key());
            let pad = " ".repeat(width - key.chars().count());
            out.push_str(&format!("{key}{pad} = {{ hash = {}", quote(&p.hash)));
            if !p.dependencies.is_empty() {
                let mut d = p.dependencies.clone();
                d.sort();
                let list: Vec<String> = d.iter().map(|x| quote(x)).collect();
                out.push_str(&format!(", needs = [{}]", list.join(", ")));
            }
            out.push_str(" }\n");
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
            if e.key != "format" && e.key != "packages" {
                return Err(PErr::new(
                    e.key_at,
                    format!("unknown key `{}` in the lock", e.key),
                ));
            }
        }
        let mut lock = Lock::default();
        let mut needs_at: Vec<(usize, String)> = Vec::new();
        if let Some(pn) = root.get("packages") {
            let table = match &pn.v {
                Val::Table(t) => t,
                _ => return Err(PErr::new(pn.at, "`packages` must be a table")),
            };
            for e in &table.entries {
                let bad_key = || PErr::new(e.key_at, format!("`{}` is not `name@version`", e.key));
                let (name, vs) = e.key.rsplit_once('@').ok_or_else(bad_key)?;
                if !is_dep_key(name) {
                    return Err(PErr::new(
                        e.key_at,
                        format!("`{name}` is not a package name"),
                    ));
                }
                let version = Version::parse(vs).ok_or_else(bad_key)?;
                let t = match &e.node.v {
                    Val::Table(t) => t,
                    _ => {
                        return Err(PErr::new(
                            e.node.at,
                            "a package is written `{ hash = \"...\" }`, with `needs` only when it has some",
                        ))
                    }
                };
                for k in &t.entries {
                    if k.key != "hash" && k.key != "needs" {
                        return Err(PErr::new(
                            k.key_at,
                            format!("unknown key `{}` in a package; allowed: hash, needs", k.key),
                        ));
                    }
                }
                let hn = t
                    .get("hash")
                    .ok_or_else(|| PErr::new(e.node.at, format!("`{}` needs `hash`", e.key)))?;
                let hash = string(hn, "`hash`")?;
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(PErr::new(hn.at, "`hash` must be 64 lowercase hex digits"));
                }
                let mut dependencies = Vec::new();
                if let Some(dn) = t.get("needs") {
                    match &dn.v {
                        Val::Arr { items, .. } => {
                            for i in items {
                                let s = string(i, "every entry of `needs`")?;
                                needs_at.push((i.at, s.clone()));
                                dependencies.push(s);
                            }
                        }
                        _ => return Err(PErr::new(dn.at, "`needs` must be an array of strings")),
                    }
                }
                lock.packages.push(Locked {
                    name: name.to_string(),
                    version,
                    hash,
                    dependencies,
                });
            }
        }
        for (at, entry) in needs_at {
            let known = entry.split_once('@').is_some_and(|(n, v)| {
                Version::parse(v)
                    .is_some_and(|v| lock.packages.iter().any(|p| p.name == n && p.version == v))
            });
            if !known {
                return Err(PErr::new(
                    at,
                    format!("`{entry}` is not a package in this lock (expected `name@version`)"),
                ));
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
    fn output_is_one_line_per_package_sorted_and_aligned() {
        let want = format!(
            "# islet.lock: written by islet, do not edit.\nformat = 1\n\n[packages]\n\"json@1.5.0\"    = {{ hash = \"{a}\" }}\n\"json/v2@2.0.1\" = {{ hash = \"{b}\", needs = [\"uart@0.3.0\"] }}\n\"uart@0.3.0\"    = {{ hash = \"{c}\" }}\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
            c = "c".repeat(64)
        );
        assert_eq!(sample().to_text(), want);
        // Any input order gives the same bytes.
        let mut rev = sample();
        rev.packages.reverse();
        assert_eq!(rev.to_text(), want);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let text = sample().to_text();
        let parsed = parse_lock("islet.lock", &text).unwrap();
        assert_eq!(parsed.to_text(), text);
        assert!(is_canonical(&text).unwrap());
        assert!(!is_canonical(&text.replace("\"json@1.5.0\"    =", "\"json@1.5.0\" =")).unwrap());
    }

    #[test]
    fn an_empty_lock_round_trips() {
        let text = Lock::default().to_text();
        assert_eq!(
            text,
            "# islet.lock: written by islet, do not edit.\nformat = 1\n"
        );
        assert_eq!(parse_lock("islet.lock", &text).unwrap(), Lock::default());
    }

    #[test]
    fn needs_are_sorted_on_write() {
        let lock = Lock {
            packages: vec![
                pkg("a", "1.0.0", 'a', &["c@1.0.0", "b@1.0.0"]),
                pkg("b", "1.0.0", 'b', &[]),
                pkg("c", "1.0.0", 'c', &[]),
            ],
        };
        assert!(lock
            .to_text()
            .contains("needs = [\"b@1.0.0\", \"c@1.0.0\"]"));
    }

    #[test]
    fn bad_locks_are_refused_with_a_position() {
        let h = "a".repeat(64);
        let head = "format = 1\n[packages]\n";
        let e = |body: &str| parse_lock("islet.lock", &format!("{head}{body}")).unwrap_err();
        let d = e("\"a@1.0.0\" = { hash = \"xyz\" }\n");
        assert_eq!((d.line, d.col), (3, 22));
        assert!(d.msg.contains("64 lowercase hex"));
        assert!(e(&format!(
            "\"a@1.0.0\" = {{ hash = \"{h}\" }}\n\"a@1.0.0\" = {{ hash = \"{h}\" }}\n"
        ))
        .msg
        .contains("duplicate key"));
        assert!(e(&format!(
            "\"a@1.0.0\" = {{ hash = \"{h}\", needs = [\"b@1.0.0\"] }}\n"
        ))
        .msg
        .contains("not a package in this lock"));
        assert!(e("\"a@1.0.0\" = { }\n").msg.contains("needs `hash`"));
        assert!(
            e(&format!("\"a@1.0.0\" = {{ hash = \"{h}\", extra = 1 }}\n"))
                .msg
                .contains("unknown key `extra`")
        );
        assert!(e(&format!("\"a\" = {{ hash = \"{h}\" }}\n"))
            .msg
            .contains("not `name@version`"));
        assert!(e(&format!("\"A@1.0.0\" = {{ hash = \"{h}\" }}\n"))
            .msg
            .contains("not a package name"));
        assert!(e("\"a@1.0.0\" = \"abc\"\n")
            .msg
            .contains("is written `{ hash"));
        assert!(parse_lock("islet.lock", "[packages]\n")
            .unwrap_err()
            .msg
            .contains("format = 1"));
        assert!(parse_lock("islet.lock", "format = 7\n")
            .unwrap_err()
            .msg
            .contains("only format 1"));
        assert!(parse_lock("islet.lock", "format = 1\npackages = 1\n")
            .unwrap_err()
            .msg
            .contains("must be a table"));
    }
}
