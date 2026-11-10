//! Islet, the package manager for Pelago. Stage 0: the store, the manifests, the lock and the resolver.

mod blake3;
mod lock;
mod manifest;
mod resolve;
mod store;
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
  islet lock <dir> [--index <dir>] [--check]
                                   resolve a project or workspace and write its islet.lock
                                   (--check: fail if the lock on disk is not what resolving gives)
  islet tree <dir>                 print each member's dependencies from the islet.lock

Packages come from the folder given by --index or ISLET_INDEX: <index>/<name>[/vN]/<version>/ holds one package.

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
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--check" => check = true,
                    "--index" => {
                        index = Some(it.next().ok_or("--index needs a folder")?.to_string())
                    }
                    _ => return Err(USAGE.to_string()),
                }
            }
            let dir = Path::new(dir);
            let (members, root) = load_workspace(dir)?;
            let idx = resolve::DirIndex {
                root: PathBuf::from(index.unwrap_or_default()),
            };
            let lock = resolve::resolve_workspace(&members, &root, &idx)?;
            let text = lock.to_text();
            let file = dir.join("islet.lock");
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
                println!(
                    "wrote {} ({} members, {} packages)",
                    file.display(),
                    lock.members.len(),
                    lock.packages.len()
                );
            }
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
