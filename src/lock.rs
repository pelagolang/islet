//! `islet.lock`: the committed record of what was resolved. YAML in the strict subset of `yaml.rs`, in one
//! canonical form, so the same resolution always produces the same bytes and a diff shows only real changes.
//!
//! ```yaml
//! # islet.lock: written by islet, do not edit.
//! format: 1
//!
//! catalog:                      # what the shared `dependencies` of monorepo.yaml resolved to
//!   json: 1.5.0
//!
//! members:                      # what each member resolved to
//!   libs/core:
//!     json:
//!       version: 1.5.0
//!       from: monorepo
//!     units: 0.2.4              # only a version: written on one line
//!     wave-core:
//!       path: libs/core
//!
//! packages:                     # every package in the closure
//!   json@1.5.0:
//!     hash: "<64 hex digits: the store address of the package's source>"
//!   json/v2@2.0.1:
//!     hash: "<64 hex digits>"
//!     needs:
//!       - units@0.2.4
//! ```
//!
//! A section is left out when empty. Everything is sorted: catalog and dependency keys, members by path,
//! packages by name then version, `needs` as text. A member entry has the keys `version`, `from`, `target` and
//! `exception` in that order, or just `path`. `name` in a package key is the key as written in a manifest, so a
//! second major is `json/v2`.

use crate::manifest::{is_dep_key, Version};
use crate::yaml::{self, Diag, Node, PErr, Table, Val};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Version {
        version: Version,
        /// `from: monorepo`: the requirement came from the shared table.
        from_monorepo: bool,
        target: Option<String>,
        /// A deliberate split from the shared version, matching an `exception` in monorepo.yaml.
        exception: bool,
    },
    /// Another member of the same workspace.
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDep {
    pub key: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub path: String,
    pub deps: Vec<MemberDep>,
}

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
    pub catalog: Vec<(String, Version)>,
    pub members: Vec<Member>,
    pub packages: Vec<Locked>,
}

impl Lock {
    /// The canonical text. Sorting happens here, so callers may add entries in any order.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# islet.lock: written by islet, do not edit.\nformat: 1\n");
        if !self.catalog.is_empty() {
            out.push_str("\ncatalog:\n");
            let mut c: Vec<&(String, Version)> = self.catalog.iter().collect();
            c.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in c {
                out.push_str(&format!(
                    "  {}: {}\n",
                    yaml::string(k),
                    yaml::string(&v.to_string())
                ));
            }
        }
        if !self.members.is_empty() {
            out.push_str("\nmembers:\n");
            let mut ms: Vec<&Member> = self.members.iter().collect();
            ms.sort_by(|a, b| a.path.cmp(&b.path));
            for m in ms {
                if m.deps.is_empty() {
                    out.push_str(&format!("  {}: {{}}\n", yaml::string(&m.path)));
                    continue;
                }
                out.push_str(&format!("  {}:\n", yaml::string(&m.path)));
                let mut ds: Vec<&MemberDep> = m.deps.iter().collect();
                ds.sort_by(|a, b| a.key.cmp(&b.key));
                for d in ds {
                    let key = yaml::string(&d.key);
                    match &d.source {
                        Source::Path(p) => {
                            out.push_str(&format!("    {key}:\n      path: {}\n", yaml::string(p)))
                        }
                        Source::Version {
                            version,
                            from_monorepo: false,
                            target: None,
                            exception: false,
                        } => out.push_str(&format!(
                            "    {key}: {}\n",
                            yaml::string(&version.to_string())
                        )),
                        Source::Version {
                            version,
                            from_monorepo,
                            target,
                            exception,
                        } => {
                            out.push_str(&format!(
                                "    {key}:\n      version: {}\n",
                                yaml::string(&version.to_string())
                            ));
                            if *from_monorepo {
                                out.push_str("      from: monorepo\n");
                            }
                            if let Some(t) = target {
                                out.push_str(&format!("      target: {}\n", yaml::string(t)));
                            }
                            if *exception {
                                out.push_str("      exception: true\n");
                            }
                        }
                    }
                }
            }
        }
        if !self.packages.is_empty() {
            out.push_str("\npackages:\n");
            let mut pkgs: Vec<&Locked> = self.packages.iter().collect();
            pkgs.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
            for p in pkgs {
                out.push_str(&format!(
                    "  {}:\n    hash: {}\n",
                    yaml::string(&p.key()),
                    yaml::quoted(&p.hash)
                ));
                if !p.dependencies.is_empty() {
                    let mut d = p.dependencies.clone();
                    d.sort();
                    out.push_str("    needs:\n");
                    for x in d {
                        out.push_str(&format!("      - {}\n", yaml::string(&x)));
                    }
                }
            }
        }
        out
    }
}

/// True when `src` is exactly what the writer produces. Line endings do not count: a Windows checkout with
/// `core.autocrlf` turns every `\n` into `\r\n`, and the file is still canonical.
pub fn is_canonical(lock: &Lock, src: &str) -> bool {
    lock.to_text() == src.replace("\r\n", "\n")
}

type R<T> = Result<T, PErr>;

fn string(n: &Node, what: &str) -> R<String> {
    match &n.v {
        Val::Str(s) => Ok(s.clone()),
        _ => Err(PErr::new(n.at, format!("{what} must be text"))),
    }
}

fn mapping<'a>(n: &'a Node, what: &str) -> R<&'a Table> {
    match &n.v {
        Val::Table(t) => Ok(t),
        _ => Err(PErr::new(n.at, format!("{what} must be a mapping"))),
    }
}

fn version(n: &Node, what: &str) -> R<Version> {
    let s = string(n, what)?;
    Version::parse(&s).ok_or_else(|| PErr::new(n.at, format!("`{s}` is not a version like 1.2.3")))
}

fn only(t: &Table, allowed: &[&str], section: &str) -> R<()> {
    for e in &t.entries {
        if !allowed.contains(&e.key.as_str()) {
            return Err(PErr::new(
                e.key_at,
                format!(
                    "unknown key `{}` in {section}; allowed: {}",
                    e.key,
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

pub fn parse_lock(file: &str, src: &str) -> Result<Lock, Diag> {
    let inner = || -> R<Lock> {
        let root = yaml::parse(src)?;
        match root.get("format") {
            Some(Node { v: Val::Int(1), .. }) => {}
            Some(n) => {
                return Err(PErr::new(
                    n.at,
                    "`format` must be 1; this version of islet reads only format 1",
                ));
            }
            None => return Err(PErr::new(0, "the lock needs `format: 1`")),
        }
        only(
            &root,
            &["format", "catalog", "members", "packages"],
            "the lock",
        )?;
        let mut lock = Lock::default();

        if let Some(cn) = root.get("catalog") {
            for e in &mapping(cn, "`catalog`")?.entries {
                if !is_dep_key(&e.key) {
                    return Err(PErr::new(
                        e.key_at,
                        format!("`{}` is not a package name", e.key),
                    ));
                }
                lock.catalog
                    .push((e.key.clone(), version(&e.node, "a catalog version")?));
            }
        }

        if let Some(mn) = root.get("members") {
            for m in &mapping(mn, "`members`")?.entries {
                let mut member = Member {
                    path: m.key.clone(),
                    deps: Vec::new(),
                };
                for d in &mapping(&m.node, "a member")?.entries {
                    if !member_name(&d.key) {
                        return Err(PErr::new(
                            d.key_at,
                            format!("`{}` is not a package name", d.key),
                        ));
                    }
                    let source = match &d.node.v {
                        Val::Str(_) => Source::Version {
                            version: version(&d.node, "a version")?,
                            from_monorepo: false,
                            target: None,
                            exception: false,
                        },
                        Val::Table(t) => {
                            only(
                                t,
                                &["version", "from", "target", "exception", "path"],
                                "a member entry",
                            )?;
                            if let Some(p) = t.get("path") {
                                if t.entries.len() != 1 {
                                    return Err(PErr::new(
                                        t.entries[1].key_at,
                                        "`path` takes no other keys",
                                    ));
                                }
                                Source::Path(string(p, "`path`")?)
                            } else {
                                let vn = t.get("version").ok_or_else(|| {
                                    PErr::new(
                                        d.node.at,
                                        format!("`{}` needs `version` or `path`", d.key),
                                    )
                                })?;
                                let from_monorepo = match t.get("from") {
                                    None => false,
                                    Some(Node { v: Val::Str(s), .. }) if s == "monorepo" => true,
                                    Some(n) => {
                                        return Err(PErr::new(n.at, "`from` must be monorepo"))
                                    }
                                };
                                let exception = match t.get("exception") {
                                    None => false,
                                    Some(Node {
                                        v: Val::Bool(b), ..
                                    }) => *b,
                                    Some(n) => {
                                        return Err(PErr::new(
                                            n.at,
                                            "`exception` must be true or false",
                                        ))
                                    }
                                };
                                Source::Version {
                                    version: version(vn, "`version`")?,
                                    from_monorepo,
                                    target: t
                                        .get("target")
                                        .map(|n| string(n, "`target`"))
                                        .transpose()?,
                                    exception,
                                }
                            }
                        }
                        _ => {
                            return Err(PErr::new(
                                d.node.at,
                                "a member entry is a version or a mapping",
                            ))
                        }
                    };
                    member.deps.push(MemberDep {
                        key: d.key.clone(),
                        source,
                    });
                }
                lock.members.push(member);
            }
        }

        let mut needs_at: Vec<(usize, String)> = Vec::new();
        if let Some(pn) = root.get("packages") {
            for e in &mapping(pn, "`packages`")?.entries {
                let bad_key = || PErr::new(e.key_at, format!("`{}` is not `name@version`", e.key));
                let (name, vs) = e.key.rsplit_once('@').ok_or_else(bad_key)?;
                if !is_dep_key(name) {
                    return Err(PErr::new(
                        e.key_at,
                        format!("`{name}` is not a package name"),
                    ));
                }
                let ver = Version::parse(vs).ok_or_else(bad_key)?;
                let t = mapping(&e.node, &format!("`{}`", e.key))?;
                only(t, &["hash", "needs"], "a package")?;
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
                        Val::Arr(items) => {
                            for i in items {
                                let s = string(i, "every entry of `needs`")?;
                                needs_at.push((i.at, s.clone()));
                                dependencies.push(s);
                            }
                        }
                        _ => return Err(PErr::new(dn.at, "`needs` must be a list")),
                    }
                }
                lock.packages.push(Locked {
                    name: name.to_string(),
                    version: ver,
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

/// A workspace member's own name, as written under `members`: any non-empty text without spaces. Entries under
/// a member are dependency keys or the names of sibling members, which are plain names too.
fn member_name(s: &str) -> bool {
    !s.is_empty() && !s.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn canonical(src: &str) -> Result<bool, Diag> {
        Ok(is_canonical(&parse_lock("islet.lock", src)?, src))
    }

    fn pkg(name: &str, ver: &str, h: char, deps: &[&str]) -> Locked {
        Locked {
            name: name.into(),
            version: v(ver),
            hash: h.to_string().repeat(64),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ver(version: &str) -> Source {
        Source::Version {
            version: v(version),
            from_monorepo: false,
            target: None,
            exception: false,
        }
    }

    fn dep(key: &str, source: Source) -> MemberDep {
        MemberDep {
            key: key.into(),
            source,
        }
    }

    fn sample() -> Lock {
        Lock {
            catalog: vec![
                ("uart".into(), v("0.3.0")),
                ("json".into(), v("1.5.0")),
                ("json/v2".into(), v("2.0.1")),
            ],
            members: vec![
                Member {
                    path: "libs/sensor".into(),
                    deps: vec![
                        dep("wire", ver("0.7.2")),
                        dep(
                            "uart",
                            Source::Version {
                                version: v("0.3.0"),
                                from_monorepo: true,
                                target: Some("avr-atmega328p".into()),
                                exception: false,
                            },
                        ),
                    ],
                },
                Member {
                    path: "apps/legacy-export".into(),
                    deps: vec![
                        dep("core", Source::Path("libs/core".into())),
                        dep(
                            "json",
                            Source::Version {
                                version: v("1.2.0"),
                                from_monorepo: false,
                                target: None,
                                exception: true,
                            },
                        ),
                    ],
                },
            ],
            packages: vec![
                pkg("wire", "0.7.2", 'd', &["uart@0.3.0"]),
                pkg("uart", "0.3.0", 'c', &[]),
                pkg("json/v2", "2.0.1", 'b', &["uart@0.3.0"]),
                pkg("json", "1.5.0", 'a', &[]),
            ],
        }
    }

    #[test]
    fn the_example_lock_is_canonical() {
        let text = include_str!("../examples/islet.lock");
        let lock = parse_lock("islet.lock", text).unwrap();
        assert!(is_canonical(&lock, text));
        assert_eq!(lock.members.len(), 4);
        assert_eq!(lock.packages.len(), 8);
    }

    #[test]
    fn output_is_sorted_and_stable() {
        let want = format!(
            "# islet.lock: written by islet, do not edit.
format: 1

catalog:
  json: 1.5.0
  json/v2: 2.0.1
  uart: 0.3.0

members:
  apps/legacy-export:
    core:
      path: libs/core
    json:
      version: 1.2.0
      exception: true
  libs/sensor:
    uart:
      version: 0.3.0
      from: monorepo
      target: avr-atmega328p
    wire: 0.7.2

packages:
  json@1.5.0:
    hash: \"{a}\"
  json/v2@2.0.1:
    hash: \"{b}\"
    needs:
      - uart@0.3.0
  uart@0.3.0:
    hash: \"{c}\"
  wire@0.7.2:
    hash: \"{d}\"
    needs:
      - uart@0.3.0
",
            a = "a".repeat(64),
            b = "b".repeat(64),
            c = "c".repeat(64),
            d = "d".repeat(64)
        );
        assert_eq!(sample().to_text(), want);
        // Any input order gives the same bytes.
        let mut rev = sample();
        rev.catalog.reverse();
        rev.members.reverse();
        rev.packages.reverse();
        assert_eq!(rev.to_text(), want);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let text = sample().to_text();
        let parsed = parse_lock("islet.lock", &text).unwrap();
        assert_eq!(parsed.to_text(), text);
        assert!(canonical(&text).unwrap());
        assert!(!canonical(&text.replace("    wire: 0.7.2", "    wire:   0.7.2")).unwrap());
        // Windows line endings do not make a lock non-canonical.
        assert!(canonical(&text.replace('\n', "\r\n")).unwrap());
    }

    #[test]
    fn a_single_package_lock_and_the_empty_lock() {
        let text = Lock {
            catalog: vec![],
            members: vec![Member {
                path: ".".into(),
                deps: vec![dep("json", ver("1.5.0"))],
            }],
            packages: vec![pkg("json", "1.5.0", 'a', &[])],
        }
        .to_text();
        assert!(text.contains("members:\n  .:\n    json: 1.5.0\n"), "{text}");
        assert_eq!(parse_lock("islet.lock", &text).unwrap().to_text(), text);
        let empty = Lock::default().to_text();
        assert_eq!(
            empty,
            "# islet.lock: written by islet, do not edit.\nformat: 1\n"
        );
        assert_eq!(parse_lock("islet.lock", &empty).unwrap(), Lock::default());
        let no_deps = Lock {
            members: vec![Member {
                path: "libs/x".into(),
                deps: vec![],
            }],
            ..Lock::default()
        }
        .to_text();
        assert!(no_deps.contains("  libs/x: {}\n"));
        assert_eq!(
            parse_lock("islet.lock", &no_deps).unwrap().to_text(),
            no_deps
        );
    }

    #[test]
    fn needs_are_sorted_on_write() {
        let lock = Lock {
            packages: vec![
                pkg("a", "1.0.0", 'a', &["c@1.0.0", "b@1.0.0"]),
                pkg("b", "1.0.0", 'b', &[]),
                pkg("c", "1.0.0", 'c', &[]),
            ],
            ..Lock::default()
        };
        assert!(lock
            .to_text()
            .contains("needs:\n      - b@1.0.0\n      - c@1.0.0\n"));
    }

    #[test]
    fn bad_locks_are_refused_with_a_position() {
        let h = "a".repeat(64);
        let e = |body: &str| parse_lock("islet.lock", &format!("format: 1\n{body}")).unwrap_err();
        let d = e("packages:\n  a@1.0.0:\n    hash: xyz\n");
        assert_eq!((d.line, d.col), (4, 11));
        assert!(d.msg.contains("64 lowercase hex"));
        assert!(e(&format!(
            "packages:\n  a@1.0.0:\n    hash: \"{h}\"\n  a@1.0.0:\n    hash: \"{h}\"\n"
        ))
        .msg
        .contains("duplicate key"));
        assert!(e(&format!(
            "packages:\n  a@1.0.0:\n    hash: \"{h}\"\n    needs:\n      - b@1.0.0\n"
        ))
        .msg
        .contains("not a package in this lock"));
        assert!(e("packages:\n  a@1.0.0: {}\n").msg.contains("needs `hash`"));
        assert!(e(&format!(
            "packages:\n  a@1.0.0:\n    hash: \"{h}\"\n    extra: 1\n"
        ))
        .msg
        .contains("unknown key `extra`"));
        assert!(e(&format!("packages:\n  a:\n    hash: \"{h}\"\n"))
            .msg
            .contains("not `name@version`"));
        assert!(e(&format!("packages:\n  A@1.0.0:\n    hash: \"{h}\"\n"))
            .msg
            .contains("not a package name"));
        assert!(e("catalog:\n  json: 1.0\n").msg.contains("number or date"));
        assert!(e("catalog:\n  json: \"1.0\"\n")
            .msg
            .contains("not a version"));
        assert!(e("members:\n  libs/x:\n    json:\n      from: monorepo\n")
            .msg
            .contains("needs `version` or `path`"));
        assert!(
            e("members:\n  libs/x:\n    json:\n      version: 1.0.0\n      from: elsewhere\n")
                .msg
                .contains("must be monorepo")
        );
        assert!(
            e("members:\n  libs/x:\n    core:\n      path: a\n      version: 1.0.0\n")
                .msg
                .contains("takes no other keys")
        );
        assert!(e("extra: 1\n")
            .msg
            .contains("unknown key `extra` in the lock"));
        assert!(parse_lock("islet.lock", "catalog: {}\n")
            .unwrap_err()
            .msg
            .contains("format: 1"));
        assert!(parse_lock("islet.lock", "format: 7\n")
            .unwrap_err()
            .msg
            .contains("only format 1"));
        assert!(e("packages: []\n").msg.contains("must be a mapping"));
    }
}
