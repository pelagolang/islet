//! `islet.toml` and `monorepo.toml`: read, check against the rules in `schema/`, and resolve
//! `{ from = "monorepo" }` references. Every error carries a line and column.

use crate::toml::{self, Diag, Entry, Node, PErr, Table, Val};
use std::fmt;

type R<T> = Result<T, PErr>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        if let Some(p) = pre {
            if p.is_empty()
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            {
                return None;
            }
        }
        let mut it = core.split('.');
        let mut num = || -> Option<u64> {
            let part = it.next()?;
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            part.parse().ok()
        };
        let (major, minor, patch) = (num()?, num()?, num()?);
        if it.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre: pre.map(str::to_string),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    /// A release sorts after its own pre-releases. Pre-release text compares as plain strings (a simplification
    /// of semver's rule for numeric parts).
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Requirement {
    pub version: Version,
    /// The real package name, when the key is a local alias.
    pub package: Option<String>,
    pub target: Option<String>,
    pub git: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DepSpec {
    Req(Requirement),
    /// `{ from = "monorepo" }`
    FromMonorepo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    /// The key as written, with an optional major path: `json` or `json/v2`.
    pub key: String,
    pub spec: DepSpec,
    /// Byte offset of the key, for later errors.
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: Version,
    pub license: Option<String>,
    pub description: Option<String>,
    pub authors: Vec<String>,
    pub repository: Option<String>,
    pub publish: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Generator {
    pub name: String,
    pub tool: String,
    pub args: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub package: Package,
    pub dependencies: Vec<Dependency>,
    pub generate: Vec<Generator>,
    pub c_sources: Vec<String>,
    pub c_system: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exception {
    pub package: String,
    pub version: Version,
    pub members: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Monorepo {
    pub members: Vec<String>,
    pub exclude: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub exceptions: Vec<Exception>,
    pub remote_cache: Option<String>,
}

// ---- checking helpers ------------------------------------------------------------------------------------

fn table_of<'a>(n: &'a Node, what: &str) -> R<&'a Table> {
    match &n.v {
        Val::Table(t) => Ok(t),
        _ => Err(PErr::new(n.at, format!("{what} must be a table"))),
    }
}

/// Reject keys that are not in `allowed`, naming the ones that are.
fn only_keys(t: &Table, allowed: &[&str], section: &str) -> R<()> {
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

fn str_of(n: &Node, what: &str) -> R<String> {
    match &n.v {
        Val::Str(s) => Ok(s.clone()),
        _ => Err(PErr::new(n.at, format!("{what} must be a string"))),
    }
}

fn need<'a>(t: &'a Table, key: &str, at: usize, section: &str) -> R<&'a Node> {
    t.get(key)
        .ok_or_else(|| PErr::new(at, format!("{section} needs `{key}`")))
}

fn strings(n: &Node, what: &str, min: usize) -> R<Vec<String>> {
    match &n.v {
        Val::Arr { items, .. } => {
            if items.len() < min {
                return Err(PErr::new(
                    n.at,
                    format!(
                        "{what} needs at least {min} entr{}",
                        if min == 1 { "y" } else { "ies" }
                    ),
                ));
            }
            let mut out: Vec<String> = Vec::new();
            for i in items {
                let s = str_of(i, &format!("every entry of {what}"))?;
                if s.is_empty() {
                    return Err(PErr::new(
                        i.at,
                        format!("entries of {what} must not be empty"),
                    ));
                }
                out.push(s);
            }
            Ok(out)
        }
        _ => Err(PErr::new(
            n.at,
            format!("{what} must be an array of strings"),
        )),
    }
}

fn unique(items: &[String], n: &Node, what: &str) -> R<()> {
    for (i, a) in items.iter().enumerate() {
        if items[..i].contains(a) {
            return Err(PErr::new(n.at, format!("{what} lists `{a}` twice")));
        }
    }
    Ok(())
}

fn is_name(s: &str) -> bool {
    let b = s.as_bytes();
    !s.is_empty()
        && s.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `json` or `json/v2` (majors start at v2; v0 and v1 are the bare name).
pub fn is_dep_key(s: &str) -> bool {
    match s.split_once('/') {
        None => is_name(s),
        Some((n, m)) => {
            is_name(n)
                && m.strip_prefix('v').is_some_and(|d| {
                    !d.is_empty()
                        && !d.starts_with('0')
                        && d != "1"
                        && d.bytes().all(|c| c.is_ascii_digit())
                })
        }
    }
}

fn name_of(n: &Node, what: &str) -> R<String> {
    let s = str_of(n, what)?;
    if !is_name(&s) {
        return Err(PErr::new(n.at, format!("{what} must be lowercase letters, digits and hyphens, starting with a letter (at most 64): `{s}`")));
    }
    Ok(s)
}

fn version_of(n: &Node, what: &str) -> R<Version> {
    let s = str_of(n, what)?;
    Version::parse(&s).ok_or_else(|| {
        PErr::new(
            n.at,
            format!("{what} must be a version like 1.2.3 or 1.2.3-rc.1, not `{s}`"),
        )
    })
}

fn uri_of(n: &Node, what: &str) -> R<String> {
    let s = str_of(n, what)?;
    let ok = s.split_once("://").is_some_and(|(scheme, rest)| {
        !rest.is_empty()
            && scheme
                .bytes()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
            && scheme
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c))
    });
    if ok {
        Ok(s)
    } else {
        Err(PErr::new(
            n.at,
            format!("{what} must be a URI like https://example.org/x, not `{s}`"),
        ))
    }
}

fn check_format(root: &Table) -> R<()> {
    match root.get("format") {
        None => Err(PErr::new(0, "the file needs `format = 1` at the top")),
        Some(Node { v: Val::Int(1), .. }) => Ok(()),
        Some(n) => Err(PErr::new(
            n.at,
            "`format` must be 1; this version of islet reads only format 1",
        )),
    }
}

fn dependencies(n: &Node, allow_git: bool, allow_from: bool) -> R<Vec<Dependency>> {
    let t = table_of(n, "`dependencies`")?;
    let mut out = Vec::new();
    for Entry { key, key_at, node } in &t.entries {
        if !is_dep_key(key) {
            return Err(PErr::new(*key_at, format!("`{key}` is not a package name; use lowercase letters, digits and hyphens, with an optional /v2 style major")));
        }
        let spec = match &node.v {
            Val::Str(_) => DepSpec::Req(Requirement {
                version: version_of(node, "a version")?,
                package: None,
                target: None,
                git: None,
            }),
            Val::Table(rt) => {
                if let Some(from) = rt.get("from") {
                    if !allow_from {
                        return Err(PErr::new(
                            rt.entries[0].key_at,
                            "`from` is only meaningful in a member's islet.toml",
                        ));
                    }
                    if rt.entries.len() != 1 {
                        return Err(PErr::new(
                            rt.entries[1].key_at,
                            "`{ from = \"monorepo\" }` takes no other keys",
                        ));
                    }
                    match &from.v {
                        Val::Str(s) if s == "monorepo" => DepSpec::FromMonorepo,
                        _ => return Err(PErr::new(from.at, "`from` must be \"monorepo\"")),
                    }
                } else {
                    let allowed: &[&str] = if allow_git {
                        &["version", "package", "target", "git"]
                    } else {
                        &["version", "package", "target"]
                    };
                    only_keys(rt, allowed, &format!("the requirement for `{key}`"))?;
                    let version = version_of(
                        need(
                            rt,
                            "version",
                            node.at,
                            &format!("the requirement for `{key}`"),
                        )?,
                        "`version`",
                    )?;
                    let package = rt
                        .get("package")
                        .map(|p| name_of(p, "`package`"))
                        .transpose()?;
                    let target = match rt.get("target") {
                        Some(x) => {
                            let s = str_of(x, "`target`")?;
                            if s.is_empty() {
                                return Err(PErr::new(x.at, "`target` must not be empty"));
                            }
                            Some(s)
                        }
                        None => None,
                    };
                    let git = rt.get("git").map(|g| uri_of(g, "`git`")).transpose()?;
                    DepSpec::Req(Requirement {
                        version,
                        package,
                        target,
                        git,
                    })
                }
            }
            _ => {
                return Err(PErr::new(
                    node.at,
                    "a dependency is a version string or a table",
                ))
            }
        };
        out.push(Dependency {
            key: key.clone(),
            spec,
            at: *key_at,
        });
    }
    Ok(out)
}

// ---- islet.toml -------------------------------------------------------------------------------------------

pub fn parse_manifest(file: &str, src: &str) -> Result<Manifest, Diag> {
    let inner = || -> R<Manifest> {
        let root = toml::parse(src)?;
        check_format(&root)?;
        only_keys(
            &root,
            &["format", "package", "dependencies", "generate", "c"],
            "the file",
        )?;
        let pn = need(&root, "package", 0, "the file")?;
        let pt = table_of(pn, "`[package]`")?;
        only_keys(
            pt,
            &[
                "name",
                "version",
                "license",
                "description",
                "authors",
                "repository",
                "publish",
            ],
            "[package]",
        )?;
        let name = name_of(need(pt, "name", pn.at, "[package]")?, "`name`")?;
        let version = version_of(need(pt, "version", pn.at, "[package]")?, "`version`")?;
        let license = match pt.get("license") {
            Some(n) => {
                let s = str_of(n, "`license`")?;
                if s.is_empty() {
                    return Err(PErr::new(n.at, "`license` must not be empty"));
                }
                Some(s)
            }
            None => None,
        };
        let description = match pt.get("description") {
            Some(n) => {
                let s = str_of(n, "`description`")?;
                if s.chars().count() > 200 {
                    return Err(PErr::new(
                        n.at,
                        "`description` is longer than 200 characters",
                    ));
                }
                Some(s)
            }
            None => None,
        };
        let authors = pt
            .get("authors")
            .map(|n| strings(n, "`authors`", 0))
            .transpose()?
            .unwrap_or_default();
        let repository = pt
            .get("repository")
            .map(|n| uri_of(n, "`repository`"))
            .transpose()?;
        let publish = match pt.get("publish") {
            None => true,
            Some(Node {
                v: Val::Bool(b), ..
            }) => *b,
            Some(n) => return Err(PErr::new(n.at, "`publish` must be true or false")),
        };
        let deps = root
            .get("dependencies")
            .map(|n| dependencies(n, true, true))
            .transpose()?
            .unwrap_or_default();

        let mut generate = Vec::new();
        if let Some(gn) = root.get("generate") {
            for e in &table_of(gn, "`[generate]`")?.entries {
                let gt = table_of(&e.node, &format!("`[generate.{}]`", e.key))?;
                let sec = format!("[generate.{}]", e.key);
                only_keys(gt, &["tool", "args", "inputs", "outputs"], &sec)?;
                let tool = str_of(need(gt, "tool", e.node.at, &sec)?, "`tool`")?;
                if tool.is_empty() {
                    return Err(PErr::new(e.node.at, "`tool` must not be empty"));
                }
                generate.push(Generator {
                    name: e.key.clone(),
                    tool,
                    args: match gt.get("args") {
                        Some(n) => match &n.v {
                            Val::Arr { items, .. } => items
                                .iter()
                                .map(|i| str_of(i, "every entry of `args`"))
                                .collect::<R<_>>()?,
                            _ => return Err(PErr::new(n.at, "`args` must be an array of strings")),
                        },
                        None => Vec::new(),
                    },
                    inputs: strings(need(gt, "inputs", e.node.at, &sec)?, "`inputs`", 1)?,
                    outputs: strings(need(gt, "outputs", e.node.at, &sec)?, "`outputs`", 1)?,
                });
            }
        }

        let (mut c_sources, mut c_system) = (Vec::new(), Vec::new());
        if let Some(cn) = root.get("c") {
            let ct = table_of(cn, "`[c]`")?;
            only_keys(ct, &["sources", "system"], "[c]")?;
            c_sources = ct
                .get("sources")
                .map(|n| strings(n, "`sources`", 0))
                .transpose()?
                .unwrap_or_default();
            c_system = ct
                .get("system")
                .map(|n| strings(n, "`system`", 0))
                .transpose()?
                .unwrap_or_default();
        }
        Ok(Manifest {
            package: Package {
                name,
                version,
                license,
                description,
                authors,
                repository,
                publish,
            },
            dependencies: deps,
            generate,
            c_sources,
            c_system,
        })
    };
    inner().map_err(|e| Diag::new(file, src, e))
}

// ---- monorepo.toml ----------------------------------------------------------------------------------------

pub fn parse_monorepo(file: &str, src: &str) -> Result<Monorepo, Diag> {
    let inner = || -> R<Monorepo> {
        let root = toml::parse(src)?;
        check_format(&root)?;
        only_keys(
            &root,
            &["format", "workspace", "dependencies", "exception", "cache"],
            "the file",
        )?;
        let wn = need(&root, "workspace", 0, "the file")?;
        let wt = table_of(wn, "`[workspace]`")?;
        only_keys(wt, &["members", "exclude"], "[workspace]")?;
        let mn = need(wt, "members", wn.at, "[workspace]")?;
        let members = strings(mn, "`members`", 1)?;
        unique(&members, mn, "`members`")?;
        let exclude = match wt.get("exclude") {
            Some(n) => {
                let v = strings(n, "`exclude`", 0)?;
                unique(&v, n, "`exclude`")?;
                v
            }
            None => Vec::new(),
        };
        let deps = root
            .get("dependencies")
            .map(|n| dependencies(n, false, false))
            .transpose()?
            .unwrap_or_default();

        let mut exceptions = Vec::new();
        if let Some(en) = root.get("exception") {
            let items = match &en.v {
                Val::Arr { items, .. } => items,
                _ => {
                    return Err(PErr::new(
                        en.at,
                        "`exception` must be written as `[[exception]]` tables",
                    ))
                }
            };
            for item in items {
                let et = table_of(item, "an `[[exception]]`")?;
                only_keys(
                    et,
                    &["package", "version", "members", "reason"],
                    "[[exception]]",
                )?;
                let pn = need(et, "package", item.at, "[[exception]]")?;
                let package = str_of(pn, "`package`")?;
                if !is_dep_key(&package) {
                    return Err(PErr::new(
                        pn.at,
                        format!("`{package}` is not a package name"),
                    ));
                }
                let mem = need(et, "members", item.at, "[[exception]]")?;
                let members = strings(mem, "`members`", 1)?;
                unique(&members, mem, "`members`")?;
                let rn = need(et, "reason", item.at, "[[exception]]")?;
                let reason = str_of(rn, "`reason`")?;
                if reason.is_empty() || reason.chars().count() > 200 {
                    return Err(PErr::new(
                        rn.at,
                        "`reason` must be 1 to 200 characters; say why the split exists",
                    ));
                }
                exceptions.push(Exception {
                    package,
                    version: version_of(
                        need(et, "version", item.at, "[[exception]]")?,
                        "`version`",
                    )?,
                    members,
                    reason,
                });
            }
        }
        let remote_cache = match root.get("cache") {
            Some(cn) => {
                let ct = table_of(cn, "`[cache]`")?;
                only_keys(ct, &["remote"], "[cache]")?;
                ct.get("remote")
                    .map(|n| uri_of(n, "`remote`"))
                    .transpose()?
            }
            None => None,
        };
        Ok(Monorepo {
            members,
            exclude,
            dependencies: deps,
            exceptions,
            remote_cache,
        })
    };
    inner().map_err(|e| Diag::new(file, src, e))
}

/// Replace every `{ from = "monorepo" }` in a member with the shared table's requirement. The error names the
/// member's key, at the line where the member wrote it.
pub fn resolve(
    member: &Manifest,
    root: &Monorepo,
) -> Result<Vec<(String, Requirement)>, (usize, String)> {
    let mut out = Vec::new();
    for d in &member.dependencies {
        let req = match &d.spec {
            DepSpec::Req(r) => r.clone(),
            DepSpec::FromMonorepo => match root.dependencies.iter().find(|r| r.key == d.key) {
                Some(Dependency {
                    spec: DepSpec::Req(r),
                    ..
                }) => r.clone(),
                _ => {
                    return Err((
                        d.at,
                        format!("`{}` is not in the monorepo's shared [dependencies]", d.key),
                    ))
                }
            },
        };
        out.push((d.key.clone(), req));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISLET: &str = include_str!("../examples/islet.toml");
    const MONOREPO: &str = include_str!("../examples/monorepo.toml");
    const MEMBER: &str = include_str!("../examples/monorepo-member/islet.toml");

    fn bad_manifest(src: &str) -> Diag {
        parse_manifest("islet.toml", src).unwrap_err()
    }

    #[test]
    fn the_examples_parse() {
        let m = parse_manifest("islet.toml", ISLET).unwrap();
        assert_eq!(m.package.name, "demo");
        assert_eq!(m.package.version.to_string(), "0.1.0");
        assert!(m.package.publish);
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dependencies[1].key, "json/v2");
        let r = parse_monorepo("monorepo.toml", MONOREPO).unwrap();
        assert_eq!(r.members, ["libs/*", "apps/*"]);
        assert_eq!(r.dependencies.len(), 3);
        assert_eq!(r.exceptions[0].members, ["apps/legacy-export"]);
        assert!(r.remote_cache.is_some());
    }

    #[test]
    fn from_monorepo_resolves_to_the_shared_requirement() {
        let r = parse_monorepo("monorepo.toml", MONOREPO).unwrap();
        let m = parse_manifest("islet.toml", MEMBER).unwrap();
        assert!(m
            .dependencies
            .iter()
            .all(|d| d.spec == DepSpec::FromMonorepo));
        let got = resolve(&m, &r).unwrap();
        assert_eq!(got[0].0, "json");
        assert_eq!(got[0].1.version.to_string(), "1.5.0");
        assert_eq!(got[1].0, "json/v2");
        assert_eq!(got[1].1.version.to_string(), "2.0.1");
    }

    #[test]
    fn a_missing_shared_entry_names_the_key_and_its_line() {
        let r = parse_monorepo("monorepo.toml", MONOREPO).unwrap();
        let src = "format = 1\n[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nnope = { from = \"monorepo\" }\n";
        let m = parse_manifest("islet.toml", src).unwrap();
        let (at, msg) = resolve(&m, &r).unwrap_err();
        let d = Diag::new("islet.toml", src, PErr::new(at, msg));
        assert_eq!((d.line, d.col), (6, 1));
        assert!(d.msg.contains("`nope` is not in the monorepo's shared"));
    }

    #[test]
    fn manifest_errors_point_at_the_problem() {
        let head = "format = 1\n[package]\nname = \"a\"\nversion = \"0.1.0\"\n";
        let d = bad_manifest("[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
        assert_eq!((d.line, d.col), (1, 1));
        assert!(d.msg.contains("format = 1"));
        assert!(bad_manifest("format = 2\n").msg.contains("only format 1"));
        let d = bad_manifest(&format!("{head}[dependencies]\nJson = \"1.0.0\"\n"));
        assert_eq!(d.line, 6);
        assert!(d.msg.contains("not a package name"));
        let d = bad_manifest(&format!("{head}[dependencies]\njson = \"1.0\"\n"));
        assert_eq!((d.line, d.col), (6, 8));
        assert!(d.msg.contains("version like 1.2.3"));
        assert!(
            bad_manifest(&format!("{head}[dependencies]\n\"json/v1\" = \"1.0.0\"\n"))
                .msg
                .contains("not a package name")
        );
        assert!(bad_manifest(&format!(
            "{head}[dependencies]\njson = {{ version = \"1.0.0\", colour = 1 }}\n"
        ))
        .msg
        .contains("unknown key `colour`"));
        assert!(bad_manifest(&format!(
            "{head}[dependencies]\njson = {{ from = \"monorepo\", version = \"1.0.0\" }}\n"
        ))
        .msg
        .contains("takes no other keys"));
        assert!(bad_manifest(&format!(
            "{head}[dependencies]\njson = {{ from = \"elsewhere\" }}\n"
        ))
        .msg
        .contains("must be \"monorepo\""));
        assert!(
            bad_manifest("format = 1\n[package]\nname = \"A\"\nversion = \"0.1.0\"\n")
                .msg
                .contains("lowercase")
        );
        assert!(bad_manifest("format = 1\n[package]\nname = \"a\"\n")
            .msg
            .contains("needs `version`"));
        assert!(bad_manifest(&format!("{head}colour = 1\n"))
            .msg
            .contains("unknown key `colour` in [package]"));
        assert!(bad_manifest(
            "format = 1\n[package]\nname = \"a\"\nversion = \"0.1.0\"\nrepository = \"nope\"\n"
        )
        .msg
        .contains("URI"));
        assert!(bad_manifest(&format!(
            "{head}[generate.x]\ntool = \"t\"\ninputs = []\noutputs = [\"o\"]\n"
        ))
        .msg
        .contains("at least 1 entry"));
    }

    #[test]
    fn generate_and_c_sections_parse() {
        let src = "format = 1\n[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n[generate.tables]\ntool = \"gen\"\nargs = [\"-x\"]\ninputs = [\"a.def\"]\noutputs = [\"a.pel\"]\n[c]\nsources = [\"x.c\"]\nsystem = [\"m\"]\n";
        let m = parse_manifest("islet.toml", src).unwrap();
        assert!(!m.package.publish);
        assert_eq!(m.generate[0].name, "tables");
        assert_eq!(m.generate[0].args, ["-x"]);
        assert_eq!(m.c_sources, ["x.c"]);
        assert_eq!(m.c_system, ["m"]);
    }

    #[test]
    fn monorepo_errors() {
        let bad = |s: &str| parse_monorepo("monorepo.toml", s).unwrap_err();
        assert!(bad("format = 1\n").msg.contains("needs `workspace`"));
        assert!(bad("format = 1\n[workspace]\nmembers = []\n")
            .msg
            .contains("at least 1 entry"));
        assert!(bad("format = 1\n[workspace]\nmembers = [\"a\", \"a\"]\n")
            .msg
            .contains("twice"));
        let d = bad("format = 1\n[workspace]\nmembers = [\"a\"]\n[dependencies]\njson = { from = \"monorepo\" }\n");
        assert_eq!(d.line, 5);
        assert!(d.msg.contains("only meaningful in a member"));
        assert!(bad("format = 1\n[workspace]\nmembers = [\"a\"]\n[[exception]]\npackage = \"json\"\nversion = \"1.2.0\"\nmembers = [\"a\"]\n").msg.contains("needs `reason`"));
        assert!(
            bad("format = 1\n[workspace]\nmembers = [\"a\"]\n[exception]\n")
                .msg
                .contains("[[exception]]")
        );
    }

    #[test]
    fn version_order() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("1.2.3") < v("1.10.0"));
        assert!(v("2.0.0-rc.1") < v("2.0.0"));
        assert!(v("2.0.0-alpha") < v("2.0.0-beta"));
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        for bad in ["1.2", "01.2.3", "1.2.3.4", "1.2.x", "1.2.3-", "", "v1.2.3"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }
}
