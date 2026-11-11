//! Islet, the package manager for Pelago. Stage 0: the store, the manifests, the lock and the resolver.

mod blake3;
mod edit;
mod install;
mod keys;
mod lock;
mod manifest;
mod registry;
mod resolve;
mod sign;
mod store;
#[allow(dead_code)] // a library until the local registry (S-4) uses it
mod tlog;
mod trust;
mod yaml;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use store::{Store, Verdict};

const USAGE: &str = "islet 0.0.1, the package manager for Pelago

usage:
  islet store put <dir>            copy a folder into the store; prints its hash
  islet store path <hash>          print where a stored tree lives
  islet store info                 where the store is, how many trees, how many bytes
  islet store verify [<hash>]      recompute the hash of one tree, or of every tree
  islet store link <hash> <dest>   link a stored tree at <dest> (symlink; junction on Windows)
  islet hash <dir>                 print a folder's hash without storing it
  islet check <file-or-dir>        check islet.yaml, monorepo.yaml and islet.lock, with line numbers
  islet lock fmt <file>            rewrite an islet.lock in its canonical form
  islet lock <dir> [--index <dir>] [--check] [--require-signed]
                                   resolve a project or workspace and write its islet.lock, pinning each
                                   package's signer (--check: fail if the lock on disk is not what resolving
                                   gives; --require-signed: fail on a package nobody signed)
  islet trust <dir> <name>[/vN] <key id>
                                   pin a package to another signer by hand (run `islet lock` after)
  islet rotate <name>[/vN] --old <key id> --new <key id> [--out <file>]
                                   the old key signs that the package is now published by the new key; the new
                                   key co-signs at once when it is on this machine
  islet rotate --cosign <file> --key <key id>
                                   add the new key's co-signature to a rotation file (a rotation without it is
                                   not followed)
  islet lock <dir> --registry <dir-or-address>
                                   as above, but the packages come from a registry folder and every one is
                                   checked against its transparency log (see `islet registry`)
  islet registry serve <dir> [--port <n>]
                                   serve a registry folder over http://127.0.0.1 (static files, GET only)
  islet registry init <dir> --origin <name> --log-key <id>
                                   make an empty registry folder whose log is signed by that key
  islet registry publish <dir> <package-dir> --key <id> --log-key <id>
                                   copy a package in, sign it, add a release entry to the log
  islet registry yank <dir> <name>[/vN]@<version> --key <id> --log-key <id>
                                   withdraw a release (a new log entry; nothing is deleted)
  islet registry revoke <dir> <key id> --log-key <id> [--key <id>]   (--key: the key holder's own request)
                                   record that a key is compromised
  islet registry cosign <dir> --key <id> --log-key <id>
                                   a witness checks the log has only grown, then cosigns the head
  islet registry add <dir> --log-key <id> [--witness <id>]... [--need <n>]
                                   trust this registry on this machine: its log key and witnesses
  islet registry check <dir>       check the registry's head and log as a client would
  islet add <dir> <name>[/vN][@<version>] [--from-monorepo] [--index <dir>]
                                   add or change a dependency in <dir>/islet.yaml (the newest in --index when no version)
  islet remove <dir> <name>[/vN]   remove a dependency from <dir>/islet.yaml
  islet key gen                    make a signing key (OS key store first, then a key file); prints its id
  islet key list                   the keys on this machine
  islet key show <id>              print a key's public half
  islet sign <dir> --key <id> [--out <file>]
                                   sign a package folder; kept in the store's sigs/ unless --out
  islet verify <dir> [--sig <file>] [--signer <id>]
                                   check the signatures for a package folder
  islet install <dir> [--index <dir>] [--registry <dir-or-address>]
                                   store and link every locked package under deps/, write deps/map.yaml
  islet tree <dir>                 print each member's dependencies from the islet.lock

Packages come from the folder given by --index or ISLET_INDEX: <index>/<name>[/vN]/<version>/ holds one package.

ISLET_KEY_BACKEND=file keeps new private keys in a file instead of the system key store.

The store is the folder named by ISLET_STORE if set; otherwise `.islet` at the drive root on Windows
and `~/.islet` elsewhere.";

/// Check one file by its name. Returns the diagnostics as text; empty means the file is fine.
fn check_file(path: &Path) -> Result<String, String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let shown = path.display().to_string();
    let src = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    match name {
        "islet.yaml" => {
            let m = manifest::parse_manifest(&shown, &src).map_err(|d| d.to_string())?;
            let mut note = String::new();
            if m.dependencies
                .iter()
                .any(|d| d.spec == manifest::DepSpec::FromMonorepo)
            {
                let root = find_monorepo(path).ok_or_else(|| {
                    let d = m
                        .dependencies
                        .iter()
                        .find(|d| d.spec == manifest::DepSpec::FromMonorepo)
                        .expect("checked above");
                    yaml::Diag::new(
                        &shown,
                        &src,
                        yaml::PErr::new(
                            d.at,
                            "uses `from: monorepo` but no monorepo.yaml was found above this file",
                        ),
                    )
                    .to_string()
                })?;
                let rsrc = std::fs::read_to_string(&root)
                    .map_err(|e| format!("{}: {e}", root.display()))?;
                let r = manifest::parse_monorepo(&root.display().to_string(), &rsrc)
                    .map_err(|d| d.to_string())?;
                manifest::resolve(&m, &r).map_err(|(at, msg)| {
                    yaml::Diag::new(&shown, &src, yaml::PErr::new(at, msg)).to_string()
                })?;
                note = format!(" (shared requirements from {})", root.display());
            }
            Ok(format!("ok {shown}{note}"))
        }
        "monorepo.yaml" => {
            manifest::parse_monorepo(&shown, &src).map_err(|d| d.to_string())?;
            Ok(format!("ok {shown}"))
        }
        "islet.lock" => {
            let l = lock::parse_lock(&shown, &src).map_err(|d| d.to_string())?;
            if !lock::is_canonical(&l, &src) {
                return Err(format!(
                    "{shown}: not in canonical form; run `islet lock fmt {shown}`"
                ));
            }
            Ok(format!("ok {shown}"))
        }
        _ => Err(format!(
            "{shown}: not an islet.yaml, monorepo.yaml or islet.lock"
        )),
    }
}

/// The nearest monorepo.yaml in a parent folder of `file`.
fn find_monorepo(file: &Path) -> Option<std::path::PathBuf> {
    let abs = std::fs::canonicalize(file).ok()?;
    abs.ancestors()
        .skip(1)
        .map(|d| d.join("monorepo.yaml"))
        .find(|p| p.is_file())
}

fn run(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let open = |near: &str| Store::for_project(Path::new(near)).map_err(|e| e.to_string());
    match a.as_slice() {
        ["hash", dir] => {
            println!(
                "{}",
                store::tree_hash(Path::new(dir)).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        ["check", target] => {
            let p = Path::new(target);
            let files: Vec<std::path::PathBuf> = if p.is_dir() {
                ["islet.yaml", "monorepo.yaml", "islet.lock"]
                    .iter()
                    .map(|n| p.join(n))
                    .filter(|f| f.is_file())
                    .collect()
            } else {
                vec![p.to_path_buf()]
            };
            if files.is_empty() {
                return Err(format!(
                    "{target}: no islet.yaml, monorepo.yaml or islet.lock here"
                ));
            }
            let mut failed = Vec::new();
            for f in &files {
                match check_file(f) {
                    Ok(line) => println!("{line}"),
                    Err(e) => failed.push(e),
                }
            }
            if failed.is_empty() {
                Ok(())
            } else {
                Err(failed.join("\n"))
            }
        }
        ["lock", "fmt", file] => {
            let src = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
            let parsed = lock::parse_lock(file, &src).map_err(|d| d.to_string())?;
            if !lock::is_canonical(&parsed, &src) {
                std::fs::write(file, parsed.to_text()).map_err(|e| format!("{file}: {e}"))?;
                println!("rewrote {file}");
            } else {
                println!("already canonical {file}");
            }
            Ok(())
        }
        ["lock", dir, rest @ ..] => {
            let mut index: Option<String> = std::env::var("ISLET_INDEX").ok();
            let mut check = false;
            let mut require_signed = false;
            let mut registry_loc: Option<String> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--check" => check = true,
                    "--require-signed" => require_signed = true,
                    "--registry" => {
                        registry_loc = Some(
                            it.next()
                                .ok_or("--registry needs a folder or address")?
                                .to_string(),
                        )
                    }
                    "--index" => {
                        index = Some(it.next().ok_or("--index needs a folder")?.to_string())
                    }
                    _ => return Err(USAGE.to_string()),
                }
            }
            let dir = Path::new(dir);
            let (members, root) = load_workspace(dir)?;
            let store = open(&dir.display().to_string())?;
            let client = match &registry_loc {
                Some(r) => {
                    let c = registry::Client::open(r, store.root())?;
                    let head = c.check_head()?;
                    let audit = c.audit(&head)?;
                    index = Some(c.cache_dir().display().to_string());
                    Some((c, head, audit))
                }
                None => None,
            };
            let idx = resolve::DirIndex {
                root: PathBuf::from(index.unwrap_or_default()),
            };
            let remote;
            let resolver: &dyn resolve::Index = match &client {
                Some((c, head, _)) => {
                    remote = registry::RemoteIndex::new(c, head);
                    &remote
                }
                None => &idx,
            };
            let mut lock = resolve::resolve_workspace(&members, &root, resolver)?;
            let file = dir.join("islet.lock");
            let previous = match std::fs::read_to_string(&file) {
                Ok(src) => Some(
                    lock::parse_lock(&file.display().to_string(), &src)
                        .map_err(|d| d.to_string())?,
                ),
                Err(_) => None,
            };
            let notes = trust::apply(
                &mut lock,
                previous.as_ref(),
                store.root(),
                &idx,
                require_signed,
            )?;
            let mut notes = notes;
            if let Some((c, head, audit)) = &client {
                notes.extend(c.check_lock(head, audit, &lock, previous.as_ref())?);
            }
            let text = lock.to_text();
            if check {
                let on_disk = std::fs::read_to_string(&file)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                if on_disk.replace("\r\n", "\n") != text {
                    return Err(format!(
                        "{}: is not what resolving gives; run `islet lock {}`",
                        file.display(),
                        dir.display()
                    ));
                }
                println!("ok {}", file.display());
            } else {
                std::fs::write(&file, &text).map_err(|e| format!("{}: {e}", file.display()))?;
                for n in &notes {
                    println!("{n}");
                }
                let unsigned = lock.packages.iter().filter(|p| p.signer.is_none()).count();
                if unsigned > 0 {
                    println!(
                        "note: {unsigned} of {} packages are not signed",
                        lock.packages.len()
                    );
                }
                println!(
                    "wrote {} ({} members, {} packages)",
                    file.display(),
                    lock.members.len(),
                    lock.packages.len()
                );
            }
            Ok(())
        }
        ["registry", sub, dir, rest @ ..] => registry_command(sub, dir, rest),
        ["trust", dir, key, signer] => {
            let file = Path::new(dir).join("islet.lock");
            let shown = file.display().to_string();
            let src = std::fs::read_to_string(&file)
                .map_err(|e| format!("{shown}: {e}; run `islet lock {dir}` first"))?;
            let mut l = lock::parse_lock(&shown, &src).map_err(|d| d.to_string())?;
            let n = trust::trust(&mut l, key, signer)?;
            std::fs::write(&file, l.to_text()).map_err(|e| format!("{shown}: {e}"))?;
            println!(
                "{key} ({n} locked version{}) now pinned to {signer}; run `islet lock {dir}` to check that it signs them",
                if n == 1 { "" } else { "s" }
            );
            Ok(())
        }
        ["rotate", "--cosign", file, "--key", id] => {
            let shown = file.to_string();
            let src = std::fs::read_to_string(file).map_err(|e| format!("{shown}: {e}"))?;
            let mut r = sign::Rotation::parse(&shown, &src).map_err(|d| d.to_string())?;
            r.verify()?;
            let secret = keys::load(open(".")?.root(), id)?;
            r.cosign(&secret)?;
            std::fs::write(file, r.to_text()).map_err(|e| format!("{shown}: {e}"))?;
            println!(
                "{id} co-signed the rotation of {} from {}",
                r.package, r.old
            );
            Ok(())
        }
        ["rotate", key, rest @ ..] => {
            let (mut old, mut new, mut out): (Option<&str>, Option<&str>, Option<&str>) =
                (None, None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--old" => old = Some(it.next().ok_or("--old needs a key id")?),
                    "--new" => new = Some(it.next().ok_or("--new needs a key id")?),
                    "--out" => out = Some(it.next().ok_or("--out needs a file")?),
                    _ => return Err(USAGE.to_string()),
                }
            }
            let old = old.ok_or("rotate needs --old <key id>")?;
            let new = new.ok_or("rotate needs --new <key id>")?;
            let s = open(".")?;
            let secret = keys::load(s.root(), old)?;
            let mut r = sign::Rotation::make(&secret, key, new)?;
            let cosigned = match keys::load(s.root(), new) {
                Ok(new_secret) => r.cosign(&new_secret).is_ok(),
                Err(_) => false,
            };
            let dest = match out {
                Some(o) => PathBuf::from(o),
                None => trust::store_rotations(s.root(), key).join(trust::rotation_file_name(&r)),
            };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&dest, r.to_text()).map_err(|e| format!("{}: {e}", dest.display()))?;
            println!("{old} hands {key} to {new}\nwrote {}", dest.display());
            if !cosigned {
                println!(
                    "the new key is not on this machine, so the rotation is not complete yet; on the machine that has it run\n  islet rotate --cosign {} --key {new}",
                    dest.display()
                );
            }
            Ok(())
        }
        ["add", dir, spec, rest @ ..] => {
            let mut index: Option<String> = std::env::var("ISLET_INDEX").ok();
            let mut shared = false;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--from-monorepo" => shared = true,
                    "--index" => {
                        index = Some(it.next().ok_or("--index needs a folder")?.to_string())
                    }
                    _ => return Err(USAGE.to_string()),
                }
            }
            let (key, ver) = match spec.split_once('@') {
                Some((k, v)) => (k, Some(v)),
                None => (*spec, None),
            };
            if !manifest::is_dep_key(key) {
                return Err(format!(
                    "`{key}` is not a dependency name (like `json` or `json/v2`)"
                ));
            }
            let value = if shared {
                if ver.is_some() {
                    return Err(
                        "--from-monorepo takes the version from monorepo.yaml; leave out @version"
                            .into(),
                    );
                }
                edit::Spec::FromMonorepo
            } else {
                let v = match ver {
                    Some(v) => manifest::Version::parse(v)
                        .ok_or_else(|| format!("`{v}` is not a version like 1.2.3"))?,
                    None => {
                        let root = index.ok_or("no version given and no --index to look in")?;
                        resolve::newest(Path::new(&root), key)?
                    }
                };
                resolve::check_major(key, &v)?;
                edit::Spec::Version(v.to_string())
            };
            let file = Path::new(dir).join("islet.yaml");
            let shown = file.display().to_string();
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
            let out = edit::add_dep(&src, key, &value)?;
            check_edited(&file, &out)?;
            std::fs::write(&file, &out).map_err(|e| format!("{shown}: {e}"))?;
            println!("updated {shown}; run `islet lock {dir}` next");
            Ok(())
        }
        ["remove", dir, key] => {
            let file = Path::new(dir).join("islet.yaml");
            let shown = file.display().to_string();
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
            let out = edit::remove_dep(&src, key)?;
            check_edited(&file, &out)?;
            std::fs::write(&file, &out).map_err(|e| format!("{shown}: {e}"))?;
            println!("updated {shown}; run `islet lock {dir}` next");
            Ok(())
        }
        ["key", "gen"] => {
            let s = open(".")?;
            let secret = sign::generate()?;
            let prefer = match std::env::var("ISLET_KEY_BACKEND").as_deref() {
                Ok("file") => keys::Prefer::File,
                _ => keys::Prefer::Auto,
            };
            let (id, backend) = keys::store(s.root(), &secret, prefer)?;
            println!("{id}\nprivate key kept in {}", backend.describe());
            Ok(())
        }
        ["key", "list"] => {
            for (id, _) in keys::list(open(".")?.root())? {
                println!("{id}");
            }
            Ok(())
        }
        ["key", "show", id] => {
            let found = keys::list(open(".")?.root())?
                .into_iter()
                .find(|(k, _)| k == id)
                .ok_or_else(|| format!("no key {id} on this machine"))?;
            println!("{}", sign::hex(&found.1));
            Ok(())
        }
        ["sign", dir, rest @ ..] => {
            let mut key: Option<&str> = None;
            let mut out: Option<&str> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--key" => key = Some(it.next().ok_or("--key needs a key id")?),
                    "--out" => out = Some(it.next().ok_or("--out needs a file")?),
                    _ => return Err(USAGE.to_string()),
                }
            }
            let key = key.ok_or("sign needs --key <id> (see `islet key list`)")?;
            let (name, version) = package_of(Path::new(dir))?;
            let tree = store::tree_hash(Path::new(dir)).map_err(|e| e.to_string())?;
            let s = open(dir)?;
            let secret = keys::load(s.root(), key)?;
            let sig = sign::Sig::make(&secret, &name, &version, &tree)?;
            let dest = match out {
                Some(o) => PathBuf::from(o),
                None => sigs_dir(&s, &tree).join(format!("{}.sig", sig.signer().replace(':', "-"))),
            };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&dest, sig.to_text()).map_err(|e| format!("{}: {e}", dest.display()))?;
            println!(
                "{} signed {name}@{version} (tree {tree})\nwrote {}",
                sig.signer(),
                dest.display()
            );
            Ok(())
        }
        ["verify", dir, rest @ ..] => {
            let mut file: Option<&str> = None;
            let mut signer: Option<&str> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--sig" => file = Some(it.next().ok_or("--sig needs a file")?),
                    "--signer" => signer = Some(it.next().ok_or("--signer needs a key id")?),
                    _ => return Err(USAGE.to_string()),
                }
            }
            let (name, version) = package_of(Path::new(dir))?;
            let tree = store::tree_hash(Path::new(dir)).map_err(|e| e.to_string())?;
            let s = open(dir)?;
            let files: Vec<PathBuf> = match file {
                Some(f) => vec![PathBuf::from(f)],
                None => match std::fs::read_dir(sigs_dir(&s, &tree)) {
                    Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
                    Err(_) => Vec::new(),
                },
            };
            let mut good: Vec<String> = Vec::new();
            for f in &files {
                let shown = f.display().to_string();
                let src = std::fs::read_to_string(f).map_err(|e| format!("{shown}: {e}"))?;
                let sig = sign::Sig::parse(&shown, &src).map_err(|d| d.to_string())?;
                if sig.package != name || sig.version != version || sig.tree != tree {
                    println!(
                        "MISMATCH {shown}: it signs {}@{} (tree {}), not this folder",
                        sig.package, sig.version, sig.tree
                    );
                    continue;
                }
                match sig.verify() {
                    Ok(()) => {
                        println!("ok {} signs {name}@{version}", sig.signer());
                        good.push(sig.signer());
                    }
                    Err(e) => println!("BAD {shown}: {e}"),
                }
            }
            match signer {
                Some(want) if !good.iter().any(|g| g == want) => {
                    Err(format!("no valid signature by {want}"))
                }
                None if good.is_empty() => Err(format!(
                    "{name}@{version} (tree {tree}) has no valid signature"
                )),
                _ => Ok(()),
            }
        }
        ["install", dir, rest @ ..] => {
            let mut index: Option<String> = std::env::var("ISLET_INDEX").ok();
            let mut registry_loc: Option<String> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--index" => {
                        index = Some(it.next().ok_or("--index needs a folder")?.to_string())
                    }
                    "--registry" => {
                        registry_loc = Some(
                            it.next()
                                .ok_or("--registry needs a folder or address")?
                                .to_string(),
                        )
                    }
                    _ => return Err(USAGE.to_string()),
                }
            }
            let file = Path::new(dir).join("islet.lock");
            let shown = file.display().to_string();
            let src = std::fs::read_to_string(&file)
                .map_err(|e| format!("{shown}: {e}; run `islet lock {dir}` first"))?;
            let l = lock::parse_lock(&shown, &src).map_err(|d| d.to_string())?;
            let store = open(dir)?;
            if let Some(loc) = &registry_loc {
                // Fetch what the lock names: each file is checked against the hashes in the lock before it is kept.
                let c = registry::Client::open(loc, store.root())?;
                for p in &l.packages {
                    c.fetch_package(
                        &p.name,
                        &p.version.to_string(),
                        &p.hash,
                        p.signer.as_deref(),
                    )?;
                }
                index = Some(c.cache_dir().display().to_string());
            }
            let idx = resolve::DirIndex {
                root: PathBuf::from(index.unwrap_or_default()),
            };
            let r = install::install(Path::new(dir), &l, &idx, &store)?;
            println!(
                "installed {} packages ({} newly stored, {} linked) into {}",
                l.packages.len(),
                r.stored,
                r.linked,
                Path::new(dir).join("deps").display()
            );
            Ok(())
        }
        ["tree", dir] => {
            let file = Path::new(dir).join("islet.lock");
            let shown = file.display().to_string();
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
            let l = lock::parse_lock(&shown, &src).map_err(|d| d.to_string())?;
            print!("{}", lock::tree_text(&l));
            Ok(())
        }
        ["store", "put", dir] => {
            let h = open(".")?.put(Path::new(dir)).map_err(|e| e.to_string())?;
            println!("{h}");
            Ok(())
        }
        ["store", "path", hash] => {
            let s = open(".")?;
            if !s.contains(hash) {
                return Err(format!("not in the store: {hash}"));
            }
            println!("{}", s.path_of(hash).map_err(|e| e.to_string())?.display());
            Ok(())
        }
        ["store", "info"] => {
            let i = open(".")?.info().map_err(|e| e.to_string())?;
            println!(
                "store  {}\ntrees  {}\nbytes  {}",
                i.root.display(),
                i.trees,
                i.bytes
            );
            Ok(())
        }
        ["store", "verify"] => {
            let s = open(".")?;
            let all = s.list().map_err(|e| e.to_string())?;
            let mut bad = 0;
            for h in &all {
                match s.verify(h).map_err(|e| e.to_string())? {
                    Verdict::Ok => {}
                    Verdict::Mismatch { actual } => {
                        bad += 1;
                        println!("CORRUPT {h} (now hashes to {actual})");
                    }
                }
            }
            println!("{} trees checked, {bad} corrupt", all.len());
            if bad == 0 {
                Ok(())
            } else {
                Err("store verification failed".into())
            }
        }
        ["store", "verify", hash] => match open(".")?.verify(hash).map_err(|e| e.to_string())? {
            Verdict::Ok => {
                println!("ok {hash}");
                Ok(())
            }
            Verdict::Mismatch { actual } => Err(format!("CORRUPT {hash} (now hashes to {actual})")),
        },
        ["store", "link", hash, dest] => {
            open(".")?
                .link(hash, Path::new(dest))
                .map_err(|e| e.to_string())?;
            println!("linked {dest}");
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

/// Read the workspace at `dir`: its `monorepo.yaml` and every member, or the single project with an `islet.yaml`.
fn load_workspace(dir: &Path) -> Result<(Vec<resolve::MemberSrc>, manifest::Monorepo), String> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let mono = dir.join("monorepo.yaml");
    let member = |rel: &str, file: PathBuf| -> Result<resolve::MemberSrc, String> {
        let src = read(&file)?;
        let manifest = manifest::parse_manifest(&file.display().to_string(), &src)
            .map_err(|d| d.to_string())?;
        Ok(resolve::MemberSrc {
            path: rel.to_string(),
            manifest,
        })
    };
    if mono.is_file() {
        let root = manifest::parse_monorepo(&mono.display().to_string(), &read(&mono)?)
            .map_err(|d| d.to_string())?;
        let mut out = Vec::new();
        for rel in resolve::member_paths(dir, &root)? {
            out.push(member(&rel, dir.join(&rel).join("islet.yaml"))?);
        }
        Ok((out, root))
    } else if dir.join("islet.yaml").is_file() {
        let empty = manifest::Monorepo {
            members: Vec::new(),
            exclude: Vec::new(),
            dependencies: Vec::new(),
            exceptions: Vec::new(),
            remote_cache: None,
        };
        Ok((vec![member(".", dir.join("islet.yaml"))?], empty))
    } else {
        Err(format!(
            "{}: no monorepo.yaml or islet.yaml here",
            dir.display()
        ))
    }
}

/// `--flag value` pairs, in order. A flag may repeat.
fn flag_pairs<'a>(rest: &[&'a str], allowed: &[&str]) -> Result<Vec<(&'a str, &'a str)>, String> {
    let mut out = Vec::new();
    let mut it = rest.iter();
    while let Some(f) = it.next() {
        if !allowed.contains(f) {
            return Err(USAGE.to_string());
        }
        let v = it.next().ok_or_else(|| format!("{f} needs a value"))?;
        out.push((*f, *v));
    }
    Ok(out)
}

fn one<'a>(pairs: &[(&'a str, &'a str)], flag: &str) -> Result<&'a str, String> {
    pairs
        .iter()
        .find(|(f, _)| *f == flag)
        .map(|(_, v)| *v)
        .ok_or_else(|| format!("this needs {flag} <value>"))
}

fn registry_command(sub: &str, location: &str, rest: &[&str]) -> Result<(), String> {
    let dir = Path::new(location);
    let store = Store::for_project(Path::new(".")).map_err(|e| e.to_string())?;
    let sroot = store.root().to_path_buf();
    let secret = |id: &str| registry::secret(&sroot, id);
    match sub {
        "serve" => {
            let p = flag_pairs(rest, &["--port"])?;
            let port: u16 = match p.first() {
                Some((_, n)) => n.parse().map_err(|_| "--port is a number")?,
                None => 8080,
            };
            registry::Registry::open(dir)?;
            let listener = std::net::TcpListener::bind(("127.0.0.1", port))
                .map_err(|e| format!("port {port}: {e}"))?;
            println!(
                "serving {} at http://127.0.0.1:{port}/ (ctrl-c to stop)",
                dir.display()
            );
            registry::serve(dir, listener, None)
        }
        "init" => {
            let p = flag_pairs(rest, &["--origin", "--log-key"])?;
            let (origin, log) = (one(&p, "--origin")?, one(&p, "--log-key")?);
            registry::Registry::init(dir, origin, &secret(log)?)?;
            println!(
                "made the registry `{origin}` in {}; its log is signed by {log}",
                dir.display()
            );
            Ok(())
        }
        "publish" => {
            let (pkg, rest) = rest.split_first().ok_or(USAGE)?;
            let p = flag_pairs(rest, &["--key", "--log-key"])?;
            let (key, version) = package_of(Path::new(pkg))?;
            let r = registry::Registry::open(dir)?;
            let i = r.publish(
                Path::new(pkg),
                &key,
                &version,
                &secret(one(&p, "--key")?)?,
                &secret(one(&p, "--log-key")?)?,
            )?;
            println!("published {key}@{version} as entry {i} of the log");
            Ok(())
        }
        "yank" => {
            let (spec, rest) = rest.split_first().ok_or(USAGE)?;
            let (key, version) = spec
                .split_once('@')
                .ok_or("name the release as <name>[/vN]@<version>")?;
            let p = flag_pairs(rest, &["--key", "--log-key"])?;
            let i = registry::Registry::open(dir)?.yank(
                key,
                version,
                &secret(one(&p, "--key")?)?,
                &secret(one(&p, "--log-key")?)?,
            )?;
            println!("yanked {key}@{version} (entry {i}; nothing was deleted)");
            Ok(())
        }
        "revoke" => {
            let (id, rest) = rest.split_first().ok_or(USAGE)?;
            let p = flag_pairs(rest, &["--log-key", "--key"])?;
            if !lock::is_key_id(id) {
                return Err(format!("`{id}` is not a key id"));
            }
            let holder = match p.iter().find(|(f, _)| *f == "--key") {
                Some((_, v)) => Some(secret(v)?),
                None => None,
            };
            let i = registry::Registry::open(dir)?.revoke(
                id,
                holder.as_ref(),
                &secret(one(&p, "--log-key")?)?,
            )?;
            let by = if holder.is_some() {
                "by its holder"
            } else {
                "by the operator"
            };
            println!("revoked {id} {by} (entry {i})");
            Ok(())
        }
        "cosign" => {
            let p = flag_pairs(rest, &["--key", "--log-key"])?;
            let w = registry::Registry::open(dir)?.cosign(
                &sroot,
                &secret(one(&p, "--key")?)?,
                one(&p, "--log-key")?,
            )?;
            println!("{w} cosigned the head");
            Ok(())
        }
        "add" => {
            let p = flag_pairs(rest, &["--log-key", "--witness", "--need"])?;
            let log_key = one(&p, "--log-key")?;
            let witnesses: Vec<String> = p
                .iter()
                .filter(|(f, _)| *f == "--witness")
                .map(|(_, v)| v.to_string())
                .collect();
            let need = match p.iter().find(|(f, _)| *f == "--need") {
                Some((_, n)) => n.parse::<usize>().map_err(|_| "--need is a whole number")?,
                None => 0,
            };
            let t = registry::Trust {
                origin: registry::origin_of(location)?,
                log_key: log_key.to_string(),
                witnesses,
                need,
            };
            // Parse our own text back, so what is stored is what will be read.
            registry::Trust::parse("trust", &t.to_text()).map_err(|d| d.to_string())?;
            let path = registry::Trust::path(&sroot, &t.origin);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&path, t.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
            println!(
                "trusting `{}`: log key {log_key}, {} of {} witnesses needed ({})",
                t.origin,
                t.need,
                t.witnesses.len(),
                path.display()
            );
            Ok(())
        }
        "check" => {
            if !rest.is_empty() {
                return Err(USAGE.to_string());
            }
            let c = registry::Client::open(location, &sroot)?;
            let head = c.check_head()?;
            let a = c.audit(&head)?;
            println!(
                "ok `{}`: {} entries, root {}, {} witness cosignatures, {} releases, {} yanked, {} keys revoked",
                head.head.origin,
                head.head.size,
                tlog::hex(&head.head.root),
                head.witnesses.len(),
                a.releases.len(),
                a.yanked.len(),
                a.revoked.len()
            );
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

/// The package a folder holds: its key (`json`, or `json/v2` from major 2) and version, from its `islet.yaml`.
fn package_of(dir: &Path) -> Result<(String, String), String> {
    let file = dir.join("islet.yaml");
    let shown = file.display().to_string();
    let src = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
    let m = manifest::parse_manifest(&shown, &src).map_err(|d| d.to_string())?;
    let v = &m.package.version;
    let name = if v.major >= 2 {
        format!("{}/v{}", m.package.name, v.major)
    } else {
        m.package.name.clone()
    };
    Ok((name, v.to_string()))
}

fn sigs_dir(store: &Store, tree: &str) -> PathBuf {
    store.root().join("sigs").join(tree)
}

/// An edited `islet.yaml` must still be a good one, and its `from: monorepo` entries must still resolve.
fn check_edited(file: &Path, text: &str) -> Result<(), String> {
    let shown = file.display().to_string();
    let m = manifest::parse_manifest(&shown, text).map_err(|d| d.to_string())?;
    if m.dependencies
        .iter()
        .any(|d| d.spec == manifest::DepSpec::FromMonorepo)
    {
        let root =
            find_monorepo(file).ok_or("`from: monorepo` needs a monorepo.yaml above this file")?;
        let rsrc =
            std::fs::read_to_string(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        let r = manifest::parse_monorepo(&root.display().to_string(), &rsrc)
            .map_err(|d| d.to_string())?;
        manifest::resolve(&m, &r).map_err(|(_, msg)| format!("{shown}: {msg}"))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("{m}");
            ExitCode::from(if m == USAGE { 2 } else { 1 })
        }
    }
}
