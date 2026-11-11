//! Who may publish a package: the signer pin in `islet.lock` (`docs/signing.md`, card S-2).
//!
//! Nothing here trusts a name. A package's publisher is a key, and the lock remembers which one:
//!
//! 1. **First use pins.** A package signed by exactly one valid key is pinned to it, and the key id is shown so a
//!    person can compare it with the one the publisher announced.
//! 2. **A different signer is an error.** A release signed by another key is refused until `islet trust` records
//!    the new key, and the lock diff shows that change in review (a hijacked account does not hold the old key).
//! 3. **Rotation is signed by the old key.** A rotation statement from the pinned key to a new one is followed
//!    without a prompt, through any chain of them.
//! 4. **Offline is the normal case.** Only the signature files and the lock are needed, never a network.
//!
//! Signature files are found in the store (`<store>/sigs/<tree hash>/`) and next to the package in the folder
//! index (`<index>/<key>/<version>.sigs/`); rotations in `<store>/rotations/<key>/` and `<index>/<key>/rotations/`.
//! A file that does not parse, names another package or tree, or does not verify is ignored, never trusted.

use crate::lock::Lock;
use crate::manifest::Version;
use crate::resolve::DirIndex;
use crate::sign::{Rotation, Sig};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The folder name for a package key in the store: `json/v2` is `json@v2` (`@` is never in a name).
pub fn key_dir(key: &str) -> String {
    key.replace('/', "@")
}

pub fn store_sigs(store_root: &Path, tree: &str) -> PathBuf {
    store_root.join("sigs").join(tree)
}

pub fn store_rotations(store_root: &Path, key: &str) -> PathBuf {
    store_root.join("rotations").join(key_dir(key))
}

pub fn sig_file_name(signer: &str) -> String {
    format!("{}.sig", signer.replace(':', "-"))
}

pub fn rotation_file_name(r: &Rotation) -> String {
    format!(
        "{}_{}.rot",
        r.old.replace(':', "-"),
        r.new.replace(':', "-")
    )
}

fn files_in(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == ext))
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

/// Every valid signature for this exact package, version and tree, from the given folders, one per signer.
pub fn valid_sigs(dirs: &[PathBuf], key: &str, version: &Version, tree: &str) -> Vec<Sig> {
    let mut out: Vec<Sig> = Vec::new();
    for d in dirs {
        for f in files_in(d, "sig") {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let Ok(sig) = Sig::parse(&f.display().to_string(), &src) else {
                continue;
            };
            if sig.package == key
                && sig.version == version.to_string()
                && sig.tree == tree
                && sig.verify().is_ok()
                && !out.iter().any(|o| o.signer() == sig.signer())
            {
                out.push(sig);
            }
        }
    }
    out
}

/// Every valid rotation for this package from the given folders.
pub fn valid_rotations(dirs: &[PathBuf], key: &str) -> Vec<Rotation> {
    let mut out: Vec<Rotation> = Vec::new();
    for d in dirs {
        for f in files_in(d, "rot") {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let Ok(r) = Rotation::parse(&f.display().to_string(), &src) else {
                continue;
            };
            if r.package == key && r.verify().is_ok() && !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No valid signature and nothing pinned.
    Unsigned,
    /// Signed by the key that was already pinned.
    Pinned(String),
    /// A new package: pinned to the only key that signs it.
    FirstUse(String),
    /// The pinned key rotated to this one, by a statement signed with the old key.
    Rotated { from: String, to: String },
}

/// The rule of the module. `pinned` is what the lock already records for this package name, `signers` the keys
/// with a valid signature on this version, `rots` the valid rotations.
pub fn decide(
    key: &str,
    pinned: Option<&str>,
    signers: &BTreeSet<String>,
    rots: &[Rotation],
) -> Result<Decision, String> {
    match pinned {
        None => match signers.len() {
            0 => Ok(Decision::Unsigned),
            1 => Ok(Decision::FirstUse(signers.iter().next().unwrap().clone())),
            _ => Err(format!(
                "{key}: signed by {} keys ({}) and none is pinned yet; pick one with `islet trust <dir> {key} <key id>`",
                signers.len(),
                signers.iter().cloned().collect::<Vec<_>>().join(", ")
            )),
        },
        Some(p) => {
            if signers.contains(p) {
                return Ok(Decision::Pinned(p.to_string()));
            }
            // Follow rotations from the pin; the first reachable key that signs this version wins.
            let mut seen: BTreeSet<&str> = BTreeSet::from([p]);
            let mut frontier: Vec<&str> = vec![p];
            while let Some(cur) = frontier.pop() {
                for r in rots.iter().filter(|r| r.old == cur) {
                    if !seen.insert(r.new.as_str()) {
                        continue;
                    }
                    if signers.contains(&r.new) {
                        return Ok(Decision::Rotated {
                            from: p.to_string(),
                            to: r.new.clone(),
                        });
                    }
                    frontier.push(r.new.as_str());
                }
            }
            if signers.is_empty() {
                Err(format!(
                    "{key}: has no valid signature, but the lock pins {p}; refusing to accept an unsigned release"
                ))
            } else {
                Err(format!(
                    "{key}: signed by {}, but the lock pins {p}; if the change is intended run `islet trust <dir> {key} <key id>`",
                    signers.iter().cloned().collect::<Vec<_>>().join(", ")
                ))
            }
        }
    }
}

/// Fill in `signer` for every package of a freshly resolved lock, from the signatures on disk and the pins in the
/// `previous` lock. Returns what changed in the pins, as lines to show. All refusals are reported together.
pub fn apply(
    lock: &mut Lock,
    previous: Option<&Lock>,
    store_root: &Path,
    idx: &DirIndex,
    require_signed: bool,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let mut errors = Vec::new();
    for p in &mut lock.packages {
        let id = format!("{}@{}", p.name, p.version);
        let pinned: Option<String> = previous.and_then(|l| {
            l.packages
                .iter()
                .filter(|q| q.name == p.name)
                .find_map(|q| q.signer.clone())
        });
        let dirs = [
            store_sigs(store_root, &p.hash),
            idx.sigs_dir(&p.name, &p.version),
        ];
        let signers: BTreeSet<String> = valid_sigs(&dirs, &p.name, &p.version, &p.hash)
            .iter()
            .map(|s| s.signer())
            .collect();
        let rots = valid_rotations(
            &[
                store_rotations(store_root, &p.name),
                idx.rotations_dir(&p.name),
            ],
            &p.name,
        );
        match decide(&p.name, pinned.as_deref(), &signers, &rots) {
            Ok(Decision::Unsigned) => {
                if require_signed {
                    errors.push(format!("{id}: is not signed (--require-signed)"));
                }
            }
            Ok(Decision::Pinned(k)) => p.signer = Some(k),
            Ok(Decision::FirstUse(k)) => {
                notes.push(format!(
                    "pinned {id} to {k} (first use; compare it with the publisher's)"
                ));
                p.signer = Some(k);
            }
            Ok(Decision::Rotated { from, to }) => {
                notes.push(format!("{id}: followed a rotation from {from} to {to}"));
                p.signer = Some(to);
            }
            Err(e) => errors.push(format!("{e} (at {id})")),
        }
    }
    // A pin must be the same for every version of a package, so the pin survives a rotation everywhere.
    let mut by_name: std::collections::BTreeMap<String, String> = Default::default();
    for p in &lock.packages {
        if let Some(s) = &p.signer {
            by_name.entry(p.name.clone()).or_insert_with(|| s.clone());
        }
    }
    for p in &lock.packages {
        if let (Some(s), Some(first)) = (&p.signer, by_name.get(&p.name)) {
            if s != first {
                errors.push(format!(
                    "{}: versions of one package are signed by different keys ({first} and {s}); run `islet trust <dir> {} <key id>` once to choose",
                    p.name, p.name
                ));
                break;
            }
        }
    }
    if errors.is_empty() {
        Ok(notes)
    } else {
        Err(errors.join("\n"))
    }
}

/// Record a new pin by hand: every locked version of `key` now names `signer`. The next `islet lock` checks that
/// the key really signs them.
pub fn trust(lock: &mut Lock, key: &str, signer: &str) -> Result<usize, String> {
    if !crate::lock::is_key_id(signer) {
        return Err(format!(
            "`{signer}` is not a key id (`key:` and 64 lowercase hex digits)"
        ));
    }
    let mut n = 0;
    for p in lock.packages.iter_mut().filter(|p| p.name == key) {
        p.signer = Some(signer.to_string());
        n += 1;
    }
    if n == 0 {
        return Err(format!("`{key}` is not in this lock"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign;

    const TREE: &str = "2708a2eef28076c39dd92a656d62e0259a7011ad7d7e4e6dcddff60474e936aa";

    fn kid(secret: &[u8; 32]) -> String {
        sign::key_id(&sign::public_of(secret))
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn first_use_pins_one_signer_and_asks_when_there_are_two() {
        let a = kid(&sign::generate().unwrap());
        let b = kid(&sign::generate().unwrap());
        assert_eq!(decide("json", None, &set(&[]), &[]), Ok(Decision::Unsigned));
        assert_eq!(
            decide("json", None, &set(&[&a]), &[]),
            Ok(Decision::FirstUse(a.clone()))
        );
        let e = decide("json", None, &set(&[&a, &b]), &[]).unwrap_err();
        assert!(e.contains("none is pinned yet"), "{e}");
    }

    #[test]
    fn a_different_signer_or_a_lost_signature_is_refused() {
        let a = kid(&sign::generate().unwrap());
        let b = kid(&sign::generate().unwrap());
        assert_eq!(
            decide("json", Some(&a), &set(&[&a]), &[]),
            Ok(Decision::Pinned(a.clone()))
        );
        // Both keys sign: the pinned one still wins.
        assert_eq!(
            decide("json", Some(&a), &set(&[&a, &b]), &[]),
            Ok(Decision::Pinned(a.clone()))
        );
        let e = decide("json", Some(&a), &set(&[&b]), &[]).unwrap_err();
        assert!(e.contains("islet trust"), "{e}");
        let e = decide("json", Some(&a), &set(&[]), &[]).unwrap_err();
        assert!(e.contains("unsigned release"), "{e}");
    }

    #[test]
    fn a_rotation_by_the_old_key_is_followed_through_a_chain() {
        let (sa, sb, sc) = (
            sign::generate().unwrap(),
            sign::generate().unwrap(),
            sign::generate().unwrap(),
        );
        let (a, b, c) = (kid(&sa), kid(&sb), kid(&sc));
        let ab = Rotation::make(&sa, "json", &b).unwrap();
        let bc = Rotation::make(&sb, "json", &c).unwrap();
        assert_eq!(
            decide("json", Some(&a), &set(&[&b]), std::slice::from_ref(&ab)),
            Ok(Decision::Rotated {
                from: a.clone(),
                to: b.clone()
            })
        );
        assert_eq!(
            decide("json", Some(&a), &set(&[&c]), &[ab.clone(), bc]),
            Ok(Decision::Rotated {
                from: a.clone(),
                to: c.clone()
            })
        );
        // A rotation the new key made for itself (wrong direction) does not help, and neither does a gap.
        let ba = Rotation::make(&sb, "json", &a).unwrap();
        assert!(decide("json", Some(&a), &set(&[&c]), &[ab, ba]).is_err());
    }

    #[test]
    fn trust_rewrites_every_version_of_the_package() {
        let mut l = Lock::default();
        for v in ["1.2.0", "1.5.0"] {
            l.packages.push(crate::lock::Locked {
                name: "json".into(),
                version: Version::parse(v).unwrap(),
                hash: TREE.into(),
                signer: None,
                dependencies: vec![],
            });
        }
        let k = kid(&sign::generate().unwrap());
        assert_eq!(trust(&mut l, "json", &k), Ok(2));
        assert!(l.packages.iter().all(|p| p.signer.as_deref() == Some(&*k)));
        assert!(trust(&mut l, "yaml", &k).is_err());
        assert!(trust(&mut l, "json", "key:abc").is_err());
    }

    /// The whole path on disk: sign into a folder index, lock pins, a second key is refused, `trust` and a
    /// rotation each let it through.
    #[test]
    fn apply_reads_signatures_from_the_store_and_the_index() {
        let root = std::env::temp_dir().join(format!("islet-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let idx = DirIndex {
            root: root.join("index"),
        };
        let store = root.join("store");
        let v = Version::parse("1.5.0").unwrap();
        let mk = |h: &str| Lock {
            packages: vec![crate::lock::Locked {
                name: "json".into(),
                version: v.clone(),
                hash: h.into(),
                signer: None,
                dependencies: vec![],
            }],
            ..Lock::default()
        };
        let (sa, sb) = (sign::generate().unwrap(), sign::generate().unwrap());
        let (a, b) = (kid(&sa), kid(&sb));
        let put = |dir: PathBuf, s: &Sig| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(sig_file_name(&s.signer())), s.to_text()).unwrap();
        };

        // Unsigned: nothing pinned, and --require-signed refuses.
        let mut l = mk(TREE);
        assert!(apply(&mut l, None, &store, &idx, false).unwrap().is_empty());
        assert_eq!(l.packages[0].signer, None);
        assert!(apply(&mut l, None, &store, &idx, true).is_err());

        // Signed by A in the index: first use pins A.
        put(
            idx.sigs_dir("json", &v),
            &Sig::make(&sa, "json", "1.5.0", TREE).unwrap(),
        );
        let mut first = mk(TREE);
        let notes = apply(&mut first, None, &store, &idx, true).unwrap();
        assert_eq!(first.packages[0].signer.as_deref(), Some(a.as_str()));
        assert!(notes[0].contains(&a), "{notes:?}");

        // A later release, signed only by B, is refused against the pin...
        let tree2 = "0".repeat(64);
        put(
            store_sigs(&store, &tree2),
            &Sig::make(&sb, "json", "1.5.0", &tree2).unwrap(),
        );
        let mut next = mk(&tree2);
        let e = apply(&mut next, Some(&first), &store, &idx, false).unwrap_err();
        assert!(e.contains("islet trust"), "{e}");

        // ...a signature file that names another tree is ignored...
        let mut stray = mk(&"1".repeat(64));
        put(
            store_sigs(&store, &"1".repeat(64)),
            &Sig::make(&sb, "json", "1.5.0", &tree2).unwrap(),
        );
        assert_eq!(
            apply(&mut stray, None, &store, &idx, false).unwrap().len(),
            0
        );
        assert_eq!(stray.packages[0].signer, None);

        // ...and a rotation signed by A lets B through without a prompt.
        let rot = Rotation::make(&sa, "json", &b).unwrap();
        let rdir = store_rotations(&store, "json");
        std::fs::create_dir_all(&rdir).unwrap();
        std::fs::write(rdir.join(rotation_file_name(&rot)), rot.to_text()).unwrap();
        let notes = apply(&mut next, Some(&first), &store, &idx, false).unwrap();
        assert_eq!(next.packages[0].signer.as_deref(), Some(b.as_str()));
        assert!(notes[0].contains("rotation"), "{notes:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
