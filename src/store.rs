//! The per-volume content-addressed store.
//!
//! Layout under the store root (`.islet`):
//!
//! ```text
//! store/<64 hex>/   one folder per stored tree, files read-only
//! tmp/              staging area; a tree is built here and renamed into place
//! ```
//!
//! A tree's address is the BLAKE3 hash of its *manifest*: one line per regular file, sorted by path,
//! `<file hash> <length> <path>\n`, with `/` as the separator on every platform. Empty folders, timestamps and
//! permission bits are not part of the address. Symlinks inside a tree are refused.

use crate::blake3;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Error {
    Io(String, io::Error),
    Msg(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(what, e) => write!(f, "{what}: {e}"),
            Error::Msg(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

fn io_err<'a>(what: &'a Path) -> impl FnOnce(io::Error) -> Error + 'a {
    move |e| Error::Io(what.display().to_string(), e)
}

pub struct Store {
    root: PathBuf,
}

/// Result of checking one stored tree.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Mismatch { actual: String },
}

pub struct Info {
    pub root: PathBuf,
    pub trees: usize,
    pub bytes: u64,
}

fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut f = fs::File::open(path).map_err(io_err(path))?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(io_err(path))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((h.finalize(), total))
}

/// Regular files below `dir` as `(relative path with '/', absolute path)`, sorted by relative path.
fn list_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(base: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let here = if rel.is_empty() {
            base.to_path_buf()
        } else {
            base.join(rel)
        };
        let mut names = Vec::new();
        for e in fs::read_dir(&here).map_err(io_err(&here))? {
            let e = e.map_err(io_err(&here))?;
            let name = e.file_name().into_string().map_err(|_| {
                Error::Msg(format!("{}: file name is not valid UTF-8", here.display()))
            })?;
            names.push(name);
        }
        for name in names {
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let abs = base.join(&child_rel);
            let meta = fs::symlink_metadata(&abs).map_err(io_err(&abs))?;
            if meta.file_type().is_symlink() {
                return Err(Error::Msg(format!(
                    "{}: symbolic links are not allowed inside a package",
                    abs.display()
                )));
            } else if meta.is_dir() {
                walk(base, &child_rel, out)?;
            } else if meta.is_file() {
                out.push((child_rel, abs));
            } else {
                return Err(Error::Msg(format!(
                    "{}: not a regular file or folder",
                    abs.display()
                )));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The address of the tree rooted at `dir`.
pub fn tree_hash(dir: &Path) -> Result<String> {
    let mut manifest = String::new();
    for (rel, abs) in list_files(dir)? {
        let (h, len) = hash_file(&abs)?;
        manifest.push_str(&format!("{} {} {}\n", blake3::to_hex(&h), len, rel));
    }
    Ok(blake3::to_hex(&blake3::hash(manifest.as_bytes())))
}

fn set_readonly(path: &Path, ro: bool) -> Result<()> {
    let mut p = fs::metadata(path).map_err(io_err(path))?.permissions();
    p.set_readonly(ro);
    fs::set_permissions(path, p).map_err(io_err(path))
}

fn check_hex(hex: &str) -> Result<()> {
    if blake3::from_hex(hex).is_none() || hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(Error::Msg(format!(
            "not a store hash (64 lowercase hex digits): {hex}"
        )));
    }
    Ok(())
}

impl Store {
    pub fn at(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// Pick the store for work on `project`: `ISLET_STORE` if set; on Windows the `.islet` folder at the drive root
    /// of the project's volume; elsewhere `~/.islet`.
    pub fn for_project(project: &Path) -> Result<Store> {
        if let Some(p) = std::env::var_os("ISLET_STORE") {
            return Ok(Store::at(p));
        }
        let abs = fs::canonicalize(project).map_err(io_err(project))?;
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if let Some(Component::Prefix(p)) = abs.components().next() {
                let drive = match p.kind() {
                    Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(d as char),
                    _ => None,
                };
                if let Some(d) = drive {
                    return Ok(Store::at(format!("{d}:\\.islet")));
                }
            }
            Err(Error::Msg(
                "cannot find the drive of the project; set ISLET_STORE".into(),
            ))
        }
        #[cfg(not(windows))]
        {
            let _ = abs;
            match std::env::var_os("HOME") {
                Some(h) => Ok(Store::at(PathBuf::from(h).join(".islet"))),
                None => Err(Error::Msg("HOME is not set; set ISLET_STORE".into())),
            }
        }
    }

    pub fn path_of(&self, hex: &str) -> Result<PathBuf> {
        check_hex(hex)?;
        Ok(self.root.join("store").join(hex))
    }

    pub fn contains(&self, hex: &str) -> bool {
        self.path_of(hex).map(|p| p.is_dir()).unwrap_or(false)
    }

    /// Store a copy of the tree at `dir`; returns its address. Storing the same tree twice changes nothing.
    pub fn put(&self, dir: &Path) -> Result<String> {
        let hex = tree_hash(dir)?;
        let dest = self.path_of(&hex)?;
        if dest.is_dir() {
            return Ok(hex);
        }
        let tmp_root = self.root.join("tmp");
        fs::create_dir_all(&tmp_root).map_err(io_err(&tmp_root))?;
        fs::create_dir_all(self.root.join("store")).map_err(io_err(&self.root))?;
        let stage = tmp_root.join(format!("{hex}.{}", std::process::id()));
        if stage.exists() {
            Self::remove_tree(&stage)?;
        }
        fs::create_dir_all(&stage).map_err(io_err(&stage))?;
        for (rel, abs) in list_files(dir)? {
            let to = stage.join(&rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(io_err(parent))?;
            }
            fs::copy(&abs, &to).map_err(io_err(&abs))?;
            set_readonly(&to, true)?;
        }
        // The tree must still hash the same after copying, or the source changed under us.
        if tree_hash(&stage)? != hex {
            Self::remove_tree(&stage)?;
            return Err(Error::Msg(format!(
                "{} changed while it was being stored",
                dir.display()
            )));
        }
        match fs::rename(&stage, &dest) {
            Ok(()) => Ok(hex),
            Err(_) if dest.is_dir() => {
                // Another process stored the same tree first.
                Self::remove_tree(&stage)?;
                Ok(hex)
            }
            Err(e) => Err(Error::Io(dest.display().to_string(), e)),
        }
    }

    /// Recompute the address of a stored tree and compare.
    pub fn verify(&self, hex: &str) -> Result<Verdict> {
        let dir = self.path_of(hex)?;
        if !dir.is_dir() {
            return Err(Error::Msg(format!("not in the store: {hex}")));
        }
        let actual = tree_hash(&dir)?;
        Ok(if actual == hex {
            Verdict::Ok
        } else {
            Verdict::Mismatch { actual }
        })
    }

    /// Every stored address, sorted.
    pub fn list(&self) -> Result<Vec<String>> {
        let dir = self.root.join("store");
        let mut out = Vec::new();
        if !dir.is_dir() {
            return Ok(out);
        }
        for e in fs::read_dir(&dir).map_err(io_err(&dir))? {
            let e = e.map_err(io_err(&dir))?;
            if let Ok(name) = e.file_name().into_string() {
                if check_hex(&name).is_ok() {
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn info(&self) -> Result<Info> {
        let trees = self.list()?;
        let mut bytes = 0;
        for t in &trees {
            for (_, abs) in list_files(&self.path_of(t)?)? {
                bytes += fs::metadata(&abs).map_err(io_err(&abs))?.len();
            }
        }
        Ok(Info {
            root: self.root.clone(),
            trees: trees.len(),
            bytes,
        })
    }

    /// Link the stored tree `hex` at `dest` (a symlink; a junction on Windows). `dest` must not exist.
    pub fn link(&self, hex: &str, dest: &Path) -> Result<()> {
        let target = self.path_of(hex)?;
        if !target.is_dir() {
            return Err(Error::Msg(format!(
                "not in the store: {hex}; run `islet store put` or `islet install`"
            )));
        }
        if fs::symlink_metadata(dest).is_ok() {
            return Err(Error::Msg(format!("{} already exists", dest.display())));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let target = fs::canonicalize(&target).map_err(io_err(&target))?;
        make_link(&target, dest)
    }

    /// Remove a staged tree (files are read-only, which blocks deletion on Windows).
    fn remove_tree(dir: &Path) -> Result<()> {
        for (_, abs) in list_files(dir)? {
            let _ = set_readonly(&abs, false);
        }
        fs::remove_dir_all(dir).map_err(io_err(dir))
    }
}

#[cfg(unix)]
fn make_link(target: &Path, dest: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, dest).map_err(io_err(dest))
}

#[cfg(windows)]
fn make_link(target: &Path, dest: &Path) -> Result<()> {
    // A junction needs no special privilege, unlike a directory symlink.
    let t = target
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dest)
        .arg(&t)
        .output()
        .map_err(io_err(dest))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Msg(format!(
            "mklink /J failed: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("islet-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("islet.toml"), "format = 1\n").unwrap();
        fs::write(dir.join("src/main.pel"), "fn main() {}\n").unwrap();
    }

    #[test]
    fn same_content_same_address_and_order_independent() {
        let a = scratch("a");
        let b = scratch("b");
        sample(&a);
        // Write the files in the opposite order.
        fs::create_dir_all(b.join("src")).unwrap();
        fs::write(b.join("src/main.pel"), "fn main() {}\n").unwrap();
        fs::write(b.join("islet.toml"), "format = 1\n").unwrap();
        assert_eq!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
        fs::write(b.join("islet.toml"), "format = 2\n").unwrap();
        assert_ne!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
    }

    #[test]
    fn put_get_verify_and_idempotent() {
        let src = scratch("put-src");
        sample(&src);
        let store = Store::at(scratch("put-store"));
        let h1 = store.put(&src).unwrap();
        let h2 = store.put(&src).unwrap();
        assert_eq!(h1, h2);
        assert!(store.contains(&h1));
        assert_eq!(store.list().unwrap(), vec![h1.clone()]);
        assert_eq!(store.verify(&h1).unwrap(), Verdict::Ok);
        let info = store.info().unwrap();
        assert_eq!((info.trees, info.bytes), (1, 11 + 13));
        // Stored files are read-only.
        let f = store.path_of(&h1).unwrap().join("islet.toml");
        assert!(fs::metadata(&f).unwrap().permissions().readonly());
    }

    #[test]
    fn verify_detects_tampering() {
        let src = scratch("tamper-src");
        sample(&src);
        let store = Store::at(scratch("tamper-store"));
        let h = store.put(&src).unwrap();
        let f = store.path_of(&h).unwrap().join("src/main.pel");
        set_readonly(&f, false).unwrap();
        fs::write(&f, "fn main() { evil() }\n").unwrap();
        assert!(matches!(
            store.verify(&h).unwrap(),
            Verdict::Mismatch { .. }
        ));
    }

    #[test]
    fn link_points_into_the_store() {
        let src = scratch("link-src");
        sample(&src);
        let store = Store::at(scratch("link-store"));
        let h = store.put(&src).unwrap();
        let proj = scratch("link-proj");
        let dest = proj.join("deps").join("wave");
        store.link(&h, &dest).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("islet.toml")).unwrap(),
            "format = 1\n"
        );
        assert!(store.link(&h, &dest).is_err(), "existing link is refused");
    }

    #[test]
    fn bad_input_is_refused() {
        let store = Store::at(scratch("bad-store"));
        assert!(store.path_of("nothex").is_err());
        assert!(store.path_of(&"A".repeat(64)).is_err());
        assert!(store.verify(&"0".repeat(64)).is_err());
        assert!(store.link(&"0".repeat(64), Path::new("x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_in_a_tree_are_refused() {
        let d = scratch("symlink");
        sample(&d);
        std::os::unix::fs::symlink("/etc/passwd", d.join("evil")).unwrap();
        assert!(tree_hash(&d).is_err());
    }
}
