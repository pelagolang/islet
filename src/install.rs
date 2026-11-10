//! `islet install`: make a project ready to build from its `islet.lock`.
//!
//! For each locked package it puts the package's tree into the store (checking it against the hash in the lock),
//! links it into `deps/` as `deps/<key>@<version>`, and writes `deps/map.yaml`, the file `pelago build` reads to
//! find source (see `docs/resolved-map.md`). `deps/` is a cache of the lock: it holds nothing that the lock and
//! the store cannot recreate, and it has its own `.gitignore`.

use crate::lock::{Lock, Locked, Source};
use crate::resolve::DirIndex;
use crate::store::Store;
use crate::yaml;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Report {
    pub stored: usize,
    pub linked: usize,
}

fn pkg_id(p: &Locked) -> String {
    format!("{}@{}", p.name, p.version)
}

/// The text of `deps/map.yaml`: one import scope per package and per member, sorted.
pub fn map_text(lock: &Lock) -> String {
    let mut out = String::from(
        "# deps/map.yaml: written by islet, read by pelago, do not edit.\nformat: 1\n",
    );
    if !lock.packages.is_empty() {
        out.push_str("\npackages:\n");
        let mut ps: Vec<&Locked> = lock.packages.iter().collect();
        ps.sort_by_key(|p| pkg_id(p));
        for p in ps {
            let id = pkg_id(p);
            out.push_str(&format!(
                "  {}:\n    dir: {}\n",
                yaml::string(&id),
                yaml::string(&id)
            ));
            if !p.dependencies.is_empty() {
                out.push_str("    imports:\n");
                let mut ns: Vec<(&str, &str)> = p
                    .dependencies
                    .iter()
                    .map(|n| (n.rsplit_once('@').map_or(n.as_str(), |x| x.0), n.as_str()))
                    .collect();
                ns.sort();
                for (key, id) in ns {
                    out.push_str(&format!(
                        "      {}: {}\n",
                        yaml::string(key),
                        yaml::string(id)
                    ));
                }
            }
        }
    }
    if !lock.members.is_empty() {
        out.push_str("\nmembers:\n");
        let mut ms: Vec<_> = lock.members.iter().collect();
        ms.sort_by(|a, b| a.path.cmp(&b.path));
        for m in ms {
            if m.deps.is_empty() {
                out.push_str(&format!("  {}: {{}}\n", yaml::string(&m.path)));
                continue;
            }
            out.push_str(&format!("  {}:\n    imports:\n", yaml::string(&m.path)));
            let mut ds: Vec<_> = m.deps.iter().collect();
            ds.sort_by(|a, b| a.key.cmp(&b.key));
            for d in ds {
                let target = match &d.source {
                    Source::Path(p) => format!("member:{p}"),
                    Source::Version { version, .. } => format!("{}@{version}", d.key),
                };
                out.push_str(&format!(
                    "      {}: {}\n",
                    yaml::string(&d.key),
                    yaml::string(&target)
                ));
            }
        }
    }
    out
}

/// Does `dest` lead to `target`? Compared after resolving, because a Windows junction reads back with a
/// different spelling (`\\?\` prefix) than the path it was made from.
fn is_link_to(dest: &Path, target: &Path) -> bool {
    fs::symlink_metadata(dest)
        .map(|m| m.file_type().is_symlink() || fs::read_link(dest).is_ok())
        .unwrap_or(false)
        && fs::canonicalize(dest).map(|d| d == target).unwrap_or(false)
}

/// Put every locked package in the store, link it under `deps/`, and write the map.
pub fn install(
    project: &Path,
    lock: &Lock,
    idx: &DirIndex,
    store: &Store,
) -> Result<Report, String> {
    let deps = project.join("deps");
    let mut report = Report {
        stored: 0,
        linked: 0,
    };
    for p in &lock.packages {
        let id = pkg_id(p);
        let src = idx.dir_of(&p.name, &p.version);
        if !src.is_dir() {
            return Err(format!("{id}: not found in {}", idx.root.display()));
        }
        let had = store.contains(&p.hash);
        let got = store.put(&src).map_err(|e| format!("{id}: {e}"))?;
        if got != p.hash {
            return Err(format!(
                "{id}: the files in {} hash to {got}, but islet.lock says {}",
                src.display(),
                p.hash
            ));
        }
        if !had {
            report.stored += 1;
        }
        let dest: PathBuf = deps.join(&id);
        let target = fs::canonicalize(store.path_of(&p.hash).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{id}: {e}"))?;
        if is_link_to(&dest, &target) {
            continue;
        }
        if fs::symlink_metadata(&dest).is_ok() {
            // Only ever remove a link here, never a real folder.
            fs::remove_file(&dest)
                .or_else(|_| fs::remove_dir(&dest))
                .map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        store
            .link(&p.hash, &dest)
            .map_err(|e| format!("{id}: {e}"))?;
        report.linked += 1;
    }
    fs::create_dir_all(&deps).map_err(|e| format!("{}: {e}", deps.display()))?;
    let ig = deps.join(".gitignore");
    fs::write(&ig, "*\n").map_err(|e| format!("{}: {e}", ig.display()))?;
    let map = deps.join("map.yaml");
    fs::write(&map, map_text(lock)).map_err(|e| format!("{}: {e}", map.display()))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::parse_lock;

    #[test]
    fn the_map_for_the_example_lock_reads_back_and_matches_the_proposal() {
        let l = parse_lock("x", include_str!("../examples/islet.lock")).unwrap();
        let t = map_text(&l);
        assert!(yaml::parse(&t).is_ok(), "{t}");
        assert!(t.contains("  json/v2@2.0.1:\n    dir: json/v2@2.0.1\n    imports:\n      units: units@0.2.4\n"), "{t}");
        assert!(t.contains("      core: member:libs/core\n"), "{t}");
    }

    #[test]
    fn install_stores_links_and_writes_the_map_and_is_idempotent() {
        let base = std::env::temp_dir().join(format!("islet-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let put = |rel: &str, name: &str, ver: &str, deps: &str| {
            let d = base.join("idx").join(rel);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("islet.yaml"),
                format!("format: 1\npackage:\n  name: {name}\n  version: {ver}\n{deps}"),
            )
            .unwrap();
            fs::write(d.join("a.pel"), "fn f() {\r\n}\r\n").unwrap();
        };
        put(
            "json/1.5.0",
            "json",
            "1.5.0",
            "dependencies:\n  units: 0.2.4\n",
        );
        put("units/0.2.4", "units", "0.2.4", "");
        let proj = base.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("islet.yaml"),
            "format: 1\npackage:\n  name: app\n  version: 0.1.0\ndependencies:\n  json: 1.5.0\n",
        )
        .unwrap();
        let idx = DirIndex {
            root: base.join("idx"),
        };
        let m = crate::manifest::parse_manifest(
            "t",
            &fs::read_to_string(proj.join("islet.yaml")).unwrap(),
        )
        .unwrap();
        let empty = crate::manifest::Monorepo {
            members: vec![],
            exclude: vec![],
            dependencies: vec![],
            exceptions: vec![],
            remote_cache: None,
        };
        let lock = crate::resolve::resolve_workspace(
            &[crate::resolve::MemberSrc {
                path: ".".into(),
                manifest: m,
            }],
            &empty,
            &idx,
        )
        .unwrap();
        let store = Store::at(base.join("store"));
        let r = install(&proj, &lock, &idx, &store).unwrap();
        assert_eq!((r.stored, r.linked), (2, 2));
        assert!(proj.join("deps/json@1.5.0/a.pel").is_file());
        assert_eq!(
            fs::read(proj.join("deps/json@1.5.0/a.pel")).unwrap(),
            b"fn f() {\n}\n"
        );
        assert!(fs::read_to_string(proj.join("deps/map.yaml"))
            .unwrap()
            .contains("units: units@0.2.4"));
        assert_eq!(
            fs::read_to_string(proj.join("deps/.gitignore")).unwrap(),
            "*\n"
        );
        let again = install(&proj, &lock, &idx, &store).unwrap();
        assert_eq!((again.stored, again.linked), (0, 0));
        // A package that changed since it was locked is refused.
        fs::write(base.join("idx/units/0.2.4/a.pel"), "changed").unwrap();
        let e = install(&proj, &lock, &idx, &store).unwrap_err();
        assert!(e.contains("islet.lock says"), "{e}");
    }
}
