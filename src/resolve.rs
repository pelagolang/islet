//! Resolution: from the requirements of a project or workspace to the contents of `islet.lock`.
//!
//! The rule is minimal version selection. Every requirement names a *minimum* version; the selected version of a
//! package is the highest minimum anyone in the graph asks for, and never a newer release that nobody asked for.
//! So the answer depends only on the manifests, never on what happens to be published today, and the lock is
//! reproducible without a solver. (Conventional idea, taken from Go modules; the major-in-path rule and the
//! workspace rules below are Pelago's own.)
//!
//! A *key* is a dependency name with its optional major path: `json` (majors 0 and 1) or `json/v2`. Different
//! keys are different packages and coexist; one key has exactly one selected version per workspace, unless an
//! `exception` in `monorepo.yaml` allows a member to differ.
//!
//! Where packages come from is behind the [`Index`] trait. Stage 0 has one index, [`DirIndex`], a folder of
//! plain package trees. A registry index comes later and plugs in at the same place.

use crate::lock::{Lock, Locked, Member, MemberDep, Source};
use crate::manifest::{self, DepSpec, Manifest, Monorepo, Version};
use crate::store;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// What the index knows about one released package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Store address of the package's tree.
    pub hash: String,
    /// Its own requirements: key and minimum version.
    pub deps: Vec<(String, Version)>,
}

pub trait Index {
    fn entry(&self, key: &str, version: &Version) -> Result<Entry, String>;
}

/// Split a key into its name and major path: `json/v2` is `("json", Some(2))`.
pub fn split_key(key: &str) -> (&str, Option<u64>) {
    match key.split_once('/') {
        Some((n, m)) => (n, m.strip_prefix('v').and_then(|d| d.parse().ok())),
        None => (key, None),
    }
}

/// A version must live under the key that names it: `json/v2` takes `2.x.y`; plain `json` takes `0.x.y` and `1.x.y`.
pub fn check_major(key: &str, v: &Version) -> Result<(), String> {
    match split_key(key).1 {
        Some(n) if v.major == n => Ok(()),
        Some(n) => Err(format!(
            "`{key}` is major {n}, but {v} is major {}; write the major path that matches",
            v.major
        )),
        None if v.major <= 1 => Ok(()),
        None => Err(format!(
            "{v} is major {}; its key must be `{key}/v{}`",
            v.major, v.major
        )),
    }
}

/// The result of one minimal version selection.
#[derive(Debug, Default)]
pub struct Selection {
    /// The selected version of every key reachable from the roots.
    pub selected: BTreeMap<String, Version>,
    /// What the index said about each selected package.
    pub entries: BTreeMap<String, Entry>,
}

fn node_name(key: &str, v: &Version) -> String {
    format!("{key}@{v}")
}

/// Walk every `(key, version)` reachable from `roots` (also the versions that end up not selected, since their
/// requirements count) and keep the highest version of each key.
pub fn select(roots: &[(String, Version)], idx: &dyn Index) -> Result<Selection, String> {
    let mut seen: BTreeSet<(String, Version)> = BTreeSet::new();
    let mut queue: VecDeque<(String, Version, String)> = VecDeque::new();
    let mut all: BTreeMap<(String, Version), Entry> = BTreeMap::new();
    for (k, v) in roots {
        check_major(k, v)?;
        queue.push_back((k.clone(), v.clone(), "the project".into()));
    }
    while let Some((k, v, why)) = queue.pop_front() {
        if !seen.insert((k.clone(), v.clone())) {
            continue;
        }
        let e = idx
            .entry(&k, &v)
            .map_err(|m| format!("{} (needed by {why}): {m}", node_name(&k, &v)))?;
        for (dk, dv) in &e.deps {
            check_major(dk, dv).map_err(|m| format!("{}: {m}", node_name(&k, &v)))?;
            queue.push_back((dk.clone(), dv.clone(), node_name(&k, &v)));
        }
        all.insert((k, v), e);
    }
    let mut sel = Selection::default();
    for (k, v) in all.keys() {
        let cur = sel.selected.entry(k.clone()).or_insert_with(|| v.clone());
        if v > cur {
            *cur = v.clone();
        }
    }
    for (k, v) in &sel.selected {
        sel.entries
            .insert(node_name(k, v), all[&(k.clone(), v.clone())].clone());
    }
    Ok(sel)
}

/// A workspace member as read from disk.
pub struct MemberSrc {
    /// Path relative to the workspace root, with `/`.
    pub path: String,
    pub manifest: Manifest,
}

fn locked_from(sel: &Selection, into: &mut BTreeMap<String, Locked>) -> Result<(), String> {
    for (k, v) in &sel.selected {
        let name = node_name(k, v);
        let e = &sel.entries[&name];
        let mut needs: Vec<String> = Vec::new();
        for (dk, _) in &e.deps {
            needs.push(node_name(dk, &sel.selected[dk]));
        }
        needs.sort();
        needs.dedup();
        let l = Locked {
            name: k.clone(),
            version: v.clone(),
            hash: e.hash.clone(),
            dependencies: needs,
        };
        match into.get(&name) {
            Some(old) if *old != l => {
                return Err(format!(
                    "{name} would need different dependencies in two places in the workspace; \
                     an exception cannot split a package that other members share"
                ))
            }
            Some(_) => {}
            None => {
                into.insert(name, l);
            }
        }
    }
    Ok(())
}

/// Resolve a whole workspace (or, with no shared table, one project) into a lock.
///
/// Members that depend on each other (a dependency key equal to another member's package name) link by path and
/// need no index entry. Every other dependency goes through the index. All members share one selection, except
/// for the dependencies that `monorepo.yaml` lists as an `exception` for that member.
pub fn resolve_workspace(
    members: &[MemberSrc],
    root: &Monorepo,
    idx: &dyn Index,
) -> Result<Lock, String> {
    let mut by_name: BTreeMap<&str, &MemberSrc> = BTreeMap::new();
    for m in members {
        if let Some(old) = by_name.insert(&m.manifest.package.name, m) {
            return Err(format!(
                "two members are called `{}`: {} and {}",
                m.manifest.package.name, old.path, m.path
            ));
        }
    }
    let mut shared: Vec<(String, Version)> = Vec::new();
    let mut excepted: Vec<(String, Version)> = Vec::new();
    let mut lock = Lock::default();
    let mut used_exceptions: BTreeSet<usize> = BTreeSet::new();

    for d in &root.dependencies {
        if let DepSpec::Req(r) = &d.spec {
            check_major(&d.key, &r.version)
                .map_err(|m| format!("monorepo.yaml: dependencies: {m}"))?;
            lock.catalog.push((d.key.clone(), r.version.clone()));
        }
    }

    // Pass 1: classify each member's requirements.
    struct Want {
        key: String,
        req: manifest::Requirement,
        from_monorepo: bool,
        exception: bool,
    }
    let mut wants: Vec<(&MemberSrc, Vec<Want>)> = Vec::new();
    for m in members {
        let reqs = manifest::resolve(&m.manifest, root)
            .map_err(|(_, msg)| format!("{}/islet.yaml: {msg}", m.path))?;
        let mut ws = Vec::new();
        for ((key, req), dep) in reqs.into_iter().zip(&m.manifest.dependencies) {
            if req.package.is_some() || req.git.is_some() {
                return Err(format!(
                    "{}/islet.yaml: `{key}`: `package` and `git` sources are not supported yet",
                    m.path
                ));
            }
            check_major(&key, &req.version).map_err(|e| format!("{}/islet.yaml: {e}", m.path))?;
            let exc = root.exceptions.iter().position(|x| {
                x.package == key && x.version == req.version && x.members.contains(&m.path)
            });
            if let Some(i) = exc {
                used_exceptions.insert(i);
            }
            ws.push(Want {
                key,
                req,
                from_monorepo: dep.spec == DepSpec::FromMonorepo,
                exception: exc.is_some(),
            });
        }
        wants.push((m, ws));
    }
    for (i, x) in root.exceptions.iter().enumerate() {
        if !used_exceptions.contains(&i) {
            return Err(format!(
                "monorepo.yaml: the exception for {}@{} matches no member requirement",
                x.package, x.version
            ));
        }
    }
    for (m, ws) in &wants {
        for w in ws {
            if let Some(other) = by_name.get(split_key(&w.key).0) {
                if other.manifest.package.version < w.req.version {
                    return Err(format!(
                        "{}/islet.yaml: `{}` needs {} but member {} is at {}",
                        m.path, w.key, w.req.version, other.path, other.manifest.package.version
                    ));
                }
                continue;
            }
            let root_dep = (w.key.clone(), w.req.version.clone());
            if w.exception {
                excepted.push(root_dep);
            } else {
                shared.push(root_dep);
            }
        }
    }

    // Pass 2: one selection for the shared requirements, one small selection per excepted requirement.
    let mut packages: BTreeMap<String, Locked> = BTreeMap::new();
    let shared_sel = select(&shared, idx)?;
    locked_from(&shared_sel, &mut packages)?;
    let mut exc_sels: BTreeMap<(String, Version), Selection> = BTreeMap::new();
    for root_dep in excepted {
        if exc_sels.contains_key(&root_dep) {
            continue;
        }
        let sel = select(std::slice::from_ref(&root_dep), idx)?;
        locked_from(&sel, &mut packages)?;
        exc_sels.insert(root_dep, sel);
    }

    // Pass 3: each member's own view.
    for (m, ws) in &wants {
        let mut deps = Vec::new();
        for w in ws {
            if let Some(other) = by_name.get(split_key(&w.key).0) {
                deps.push(MemberDep {
                    key: w.key.clone(),
                    source: Source::Path(other.path.clone()),
                });
                continue;
            }
            let version = if w.exception {
                w.req.version.clone()
            } else {
                shared_sel.selected[&w.key].clone()
            };
            deps.push(MemberDep {
                key: w.key.clone(),
                source: Source::Version {
                    version,
                    from_monorepo: w.from_monorepo,
                    target: w.req.target.clone(),
                    exception: w.exception,
                },
            });
        }
        lock.members.push(Member {
            path: m.path.clone(),
            deps,
        });
    }
    lock.packages = packages.into_values().collect();
    Ok(lock)
}

/// A folder of package trees: `<root>/<key>/<version>/` holds one package, with its `islet.yaml` inside, so
/// `json/v2` version 2.0.1 lives at `<root>/json/v2/2.0.1`. The address is the tree hash of that folder.
pub struct DirIndex {
    pub root: PathBuf,
}

impl Index for DirIndex {
    fn entry(&self, key: &str, version: &Version) -> Result<Entry, String> {
        let dir = self.root.join(key).join(version.to_string());
        if !dir.is_dir() {
            return Err(format!("not found in {}", self.root.display()));
        }
        let file = dir.join("islet.yaml");
        let shown = file.display().to_string();
        let src = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
        let m = manifest::parse_manifest(&shown, &src).map_err(|d| d.to_string())?;
        if m.package.name != split_key(key).0 || m.package.version != *version {
            return Err(format!(
                "{shown}: says it is {}@{}, which is not where it was found",
                m.package.name, m.package.version
            ));
        }
        let mut deps = Vec::new();
        for d in &m.dependencies {
            match &d.spec {
                DepSpec::Req(r) => {
                    if r.package.is_some() || r.git.is_some() {
                        return Err(format!(
                            "{shown}: `{}`: `package` and `git` sources are not supported yet",
                            d.key
                        ));
                    }
                    deps.push((d.key.clone(), r.version.clone()));
                }
                DepSpec::FromMonorepo => {
                    return Err(format!(
                        "{shown}: `{}` uses `from: monorepo`, which a published package cannot",
                        d.key
                    ))
                }
            }
        }
        let hash = store::tree_hash(&dir).map_err(|e| e.to_string())?;
        Ok(Entry { hash, deps })
    }
}

/// Expand the `members` globs of a workspace: a literal folder, or `dir/*` for every folder below `dir`
/// that holds an `islet.yaml`. Sorted, without the excluded ones.
pub fn member_paths(root_dir: &Path, root: &Monorepo) -> Result<Vec<String>, String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for pat in &root.members {
        if let Some(base) = pat.strip_suffix("/*") {
            let dir = root_dir.join(base);
            let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for e in rd {
                let e = e.map_err(|e| e.to_string())?;
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().join("islet.yaml").is_file() {
                    out.insert(format!("{base}/{name}"));
                }
            }
        } else if pat.contains('*') {
            return Err(format!(
                "monorepo.yaml: `{pat}`: only a folder or `folder/*` is supported"
            ));
        } else if root_dir.join(pat).join("islet.yaml").is_file() {
            out.insert(pat.clone());
        } else {
            return Err(format!("monorepo.yaml: member `{pat}` has no islet.yaml"));
        }
    }
    for x in &root.exclude {
        out.remove(x);
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    struct Mem(HashMap<String, Vec<(&'static str, &'static str)>>);
    impl Index for Mem {
        fn entry(&self, key: &str, version: &Version) -> Result<Entry, String> {
            let name = node_name(key, version);
            let deps = self.0.get(&name).ok_or("not in the index")?;
            Ok(Entry {
                hash: format!("{:0>64}", name.len()),
                deps: deps.iter().map(|(k, x)| (k.to_string(), v(x))).collect(),
            })
        }
    }
    fn mem(items: &[(&'static str, Vec<(&'static str, &'static str)>)]) -> Mem {
        Mem(items
            .iter()
            .map(|(k, d)| (k.to_string(), d.clone()))
            .collect())
    }
    fn roots(items: &[(&str, &str)]) -> Vec<(String, Version)> {
        items.iter().map(|(k, x)| (k.to_string(), v(x))).collect()
    }

    #[test]
    fn highest_minimum_wins_and_unrequested_releases_are_ignored() {
        // b 1.0.0 asks for c 1.2.0, a 1.0.0 asks for c 1.1.0; c 1.9.0 exists but nobody asked for it.
        let idx = mem(&[
            ("a@1.0.0", vec![("c", "1.1.0")]),
            ("b@1.0.0", vec![("c", "1.2.0")]),
            ("c@1.1.0", vec![]),
            ("c@1.2.0", vec![]),
            ("c@1.9.0", vec![]),
        ]);
        let s = select(&roots(&[("a", "1.0.0"), ("b", "1.0.0")]), &idx).unwrap();
        assert_eq!(s.selected["c"], v("1.2.0"));
        assert_eq!(s.selected.len(), 3);
    }

    #[test]
    fn requirements_of_unselected_versions_still_count() {
        // a asks for b 1.0.0 (which needs c 1.5.0); the project also asks for b 1.1.0 (which needs nothing).
        let idx = mem(&[
            ("a@1.0.0", vec![("b", "1.0.0")]),
            ("b@1.0.0", vec![("c", "1.5.0")]),
            ("b@1.1.0", vec![]),
            ("c@1.5.0", vec![]),
        ]);
        let s = select(&roots(&[("a", "1.0.0"), ("b", "1.1.0")]), &idx).unwrap();
        assert_eq!(s.selected["b"], v("1.1.0"));
        assert_eq!(s.selected["c"], v("1.5.0"));
    }

    #[test]
    fn two_majors_coexist_and_the_path_must_match() {
        let idx = mem(&[("json@1.2.0", vec![]), ("json/v2@2.0.1", vec![])]);
        let s = select(&roots(&[("json", "1.2.0"), ("json/v2", "2.0.1")]), &idx).unwrap();
        assert_eq!(s.selected.len(), 2);
        let e = select(&roots(&[("json", "2.0.1")]), &idx).unwrap_err();
        assert!(e.contains("json/v2"), "{e}");
        let e = select(&roots(&[("json/v2", "1.2.0")]), &idx).unwrap_err();
        assert!(e.contains("major 2"), "{e}");
    }

    #[test]
    fn cycles_end_and_missing_packages_name_who_needed_them() {
        let idx = mem(&[
            ("a@1.0.0", vec![("b", "1.0.0")]),
            ("b@1.0.0", vec![("a", "1.0.0")]),
        ]);
        assert!(select(&roots(&[("a", "1.0.0")]), &idx).is_ok());
        let idx = mem(&[("a@1.0.0", vec![("zz", "1.0.0")])]);
        let e = select(&roots(&[("a", "1.0.0")]), &idx).unwrap_err();
        assert!(e.contains("zz@1.0.0 (needed by a@1.0.0)"), "{e}");
    }

    fn member(path: &str, name: &str, version: &str, deps: &str) -> MemberSrc {
        let src = format!(
            "format: 1\npackage:\n  name: {name}\n  version: {version}\n{}",
            if deps.is_empty() {
                String::new()
            } else {
                format!("dependencies:\n{deps}")
            }
        );
        MemberSrc {
            path: path.into(),
            manifest: manifest::parse_manifest("t", &src).unwrap(),
        }
    }
    fn monorepo(extra: &str) -> Monorepo {
        manifest::parse_monorepo(
            "t",
            &format!("format: 1\nworkspace:\n  members:\n    - libs/*\n{extra}"),
        )
        .unwrap()
    }

    #[test]
    fn a_workspace_shares_one_version_and_members_link_by_path() {
        let idx = mem(&[
            ("json@1.2.0", vec![]),
            ("json@1.5.0", vec![("units", "0.2.4")]),
            ("units@0.2.4", vec![]),
        ]);
        let root = monorepo("dependencies:\n  json: 1.5.0\n");
        let ms = vec![
            member(
                "libs/core",
                "core",
                "0.1.0",
                "  json:\n    from: monorepo\n",
            ),
            member("apps/cli", "cli", "0.1.0", "  json: 1.2.0\n  core: 0.1.0\n"),
        ];
        let l = resolve_workspace(&ms, &root, &idx).unwrap();
        // cli asked for json 1.2.0, but the workspace has one version: 1.5.0.
        let cli = l.members.iter().find(|m| m.path == "apps/cli").unwrap();
        assert_eq!(
            cli.deps[1].source,
            Source::Path("libs/core".into()),
            "core links by path"
        );
        match &cli.deps[0].source {
            Source::Version {
                version, exception, ..
            } => {
                assert_eq!(*version, v("1.5.0"));
                assert!(!exception);
            }
            s => panic!("{s:?}"),
        }
        assert_eq!(l.catalog, vec![("json".into(), v("1.5.0"))]);
        let names: Vec<String> = l
            .packages
            .iter()
            .map(|p| node_name(&p.name, &p.version))
            .collect();
        assert_eq!(names, vec!["json@1.5.0", "units@0.2.4"]);
        assert_eq!(l.packages[0].dependencies, vec!["units@0.2.4"]);
        // The lock we wrote reads back.
        let text = l.to_text();
        assert!(crate::lock::parse_lock("t", &text).is_ok(), "{text}");
    }

    #[test]
    fn an_exception_keeps_one_member_on_its_own_version() {
        let idx = mem(&[("json@1.2.0", vec![]), ("json@1.5.0", vec![])]);
        let root = monorepo(
            "dependencies:\n  json: 1.5.0\nexception:\n  - package: json\n    version: 1.2.0\n    members:\n      - apps/legacy\n    reason: old output\n",
        );
        let ms = vec![
            member(
                "libs/core",
                "core",
                "0.1.0",
                "  json:\n    from: monorepo\n",
            ),
            member("apps/legacy", "legacy", "0.1.0", "  json: 1.2.0\n"),
        ];
        let l = resolve_workspace(&ms, &root, &idx).unwrap();
        assert_eq!(l.packages.len(), 2);
        let legacy = l.members.iter().find(|m| m.path == "apps/legacy").unwrap();
        match &legacy.deps[0].source {
            Source::Version {
                version, exception, ..
            } => assert_eq!((version.to_string(), *exception), ("1.2.0".into(), true)),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn an_exception_that_matches_nothing_is_an_error() {
        let idx = mem(&[("json@1.5.0", vec![])]);
        let root = monorepo(
            "dependencies:\n  json: 1.5.0\nexception:\n  - package: json\n    version: 1.2.0\n    members:\n      - apps/legacy\n    reason: old\n",
        );
        let ms = vec![member(
            "libs/core",
            "core",
            "0.1.0",
            "  json:\n    from: monorepo\n",
        )];
        let e = resolve_workspace(&ms, &root, &idx).unwrap_err();
        assert!(e.contains("matches no member requirement"), "{e}");
    }

    #[test]
    fn a_path_dependency_needs_a_new_enough_member() {
        let idx = mem(&[]);
        let root = monorepo("");
        let ms = vec![
            member("libs/core", "core", "0.1.0", ""),
            member("apps/cli", "cli", "0.1.0", "  core: 0.2.0\n"),
        ];
        let e = resolve_workspace(&ms, &root, &idx).unwrap_err();
        assert!(e.contains("member libs/core is at 0.1.0"), "{e}");
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("islet-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn the_folder_index_reads_manifests_and_hashes_trees() {
        let root = scratch("dir");
        let put = |key: &str, ver: &str, name: &str, deps: &str| {
            let d = root.join(key).join(ver);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("islet.yaml"),
                format!("format: 1\npackage:\n  name: {name}\n  version: {ver}\n{deps}"),
            )
            .unwrap();
        };
        put(
            "json/v2",
            "2.0.1",
            "json",
            "dependencies:\n  units: 0.2.4\n",
        );
        put("units", "0.2.4", "units", "");
        let idx = DirIndex { root: root.clone() };
        let e = idx.entry("json/v2", &v("2.0.1")).unwrap();
        assert_eq!(e.deps, vec![("units".to_string(), v("0.2.4"))]);
        assert_eq!(e.hash.len(), 64);
        let s = select(&roots(&[("json/v2", "2.0.1")]), &idx).unwrap();
        assert_eq!(s.selected.len(), 2);
        // A tree under the wrong name is refused.
        put("units", "0.3.0", "other", "");
        assert!(idx.entry("units", &v("0.3.0")).is_err());
        assert!(idx.entry("nope", &v("1.0.0")).is_err());
    }
}
