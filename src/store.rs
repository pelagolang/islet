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
use std::io;
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

/// The bytes a file contributes to a tree. A text file (valid UTF-8, no NUL byte) has every `\r\n` turned into
/// `\n`, so a Windows checkout and a Linux checkout of the same package get the same address. A lone `\r` and
/// every other file are left alone. The store keeps these bytes too, so a stored tree always re-hashes to itself.
pub fn canonical_bytes(raw: Vec<u8>) -> Vec<u8> {
    if raw.contains(&0)
        || std::str::from_utf8(&raw).is_err()
        || !raw.windows(2).any(|w| w == b"\r\n")
    {
        return raw;
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\r' && raw.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn read_canonical(path: &Path) -> Result<Vec<u8>> {
    Ok(canonical_bytes(fs::read(path).map_err(io_err(path))?))
}

fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let bytes = read_canonical(path)?;
    Ok((blake3::hash(&bytes), bytes.len() as u64))
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

/// The manifest of the tree rooted at `dir`: one `<file hash> <length> <path>` line per file, sorted by path. The
/// address of the tree is the BLAKE3 hash of exactly this text.
pub fn tree_manifest(dir: &Path) -> Result<String> {
    let mut manifest = String::new();
    for (rel, abs) in list_files(dir)? {
        let (h, len) = hash_file(&abs)?;
        manifest.push_str(&format!("{} {} {}\n", blake3::to_hex(&h), len, rel));
    }
    Ok(manifest)
}

/// The address of the tree rooted at `dir`.
pub fn tree_hash(dir: &Path) -> Result<String> {
    Ok(blake3::to_hex(&blake3::hash(
        tree_manifest(dir)?.as_bytes(),
    )))
}

/// Every file of the tree as `(path, bytes as the store keeps them)`, sorted by path. These bytes (line endings
/// normalised) are what a file's hash in the manifest is the hash of.
pub fn tree_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    list_files(dir)?
        .into_iter()
        .map(|(rel, abs)| Ok((rel, read_canonical(&abs)?)))
        .collect()
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

    pub fn root(&self) -> &Path {
        &self.root
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
            fs::write(&to, read_canonical(&abs)?).map_err(io_err(&to))?;
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
        fs::write(dir.join("islet.yaml"), "format: 1\n").unwrap();
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
        fs::write(b.join("islet.yaml"), "format: 1\n").unwrap();
        assert_eq!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
        fs::write(b.join("islet.yaml"), "format: 2\n").unwrap();
        assert_ne!(tree_hash(&a).unwrap(), tree_hash(&b).unwrap());
    }

    #[test]
    fn canonical_bytes_rules() {
        assert_eq!(canonical_bytes(b"a\r\nb\r\n".to_vec()), b"a\nb\n");
        assert_eq!(canonical_bytes(b"a\rb\n".to_vec()), b"a\rb\n"); // lone CR stays
        assert_eq!(canonical_bytes(b"a\r\r\nb".to_vec()), b"a\r\nb"); // only the pair goes
        assert_eq!(canonical_bytes(b"a\r\n\0".to_vec()), b"a\r\n\0"); // NUL: binary
        assert_eq!(canonical_bytes(b"\xff\r\n".to_vec()), b"\xff\r\n"); // not UTF-8: binary
        assert_eq!(canonical_bytes(Vec::new()), b"");
    }

    #[test]
    fn crlf_and_lf_checkouts_have_the_same_address() {
        let lf = scratch("eol-lf");
        let crlf = scratch("eol-crlf");
        fs::write(lf.join("a.pel"), "fn main() {\n}\n").unwrap();
        fs::write(crlf.join("a.pel"), "fn main() {\r\n}\r\n").unwrap();
        fs::write(lf.join("b.bin"), [1u8, 13, 10, 0]).unwrap();
        fs::write(crlf.join("b.bin"), [1u8, 13, 10, 0]).unwrap();
        assert_eq!(tree_hash(&lf).unwrap(), tree_hash(&crlf).unwrap());
        // A binary file keeps its bytes, so a change there does change the address.
        fs::write(crlf.join("b.bin"), [1u8, 10, 0]).unwrap();
        assert_ne!(tree_hash(&lf).unwrap(), tree_hash(&crlf).unwrap());
    }

    #[test]
    fn the_store_keeps_lf_text_and_still_verifies() {
        let src = scratch("eol-put-src");
        fs::write(src.join("a.pel"), "x\r\ny\r\n").unwrap();
        let store = Store::at(scratch("eol-put-store"));
        let h = store.put(&src).unwrap();
        let kept = fs::read(store.path_of(&h).unwrap().join("a.pel")).unwrap();
        assert_eq!(kept, b"x\ny\n");
        assert_eq!(store.verify(&h).unwrap(), Verdict::Ok);
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
        assert_eq!((info.trees, info.bytes), (1, 10 + 13));
        // Stored files are read-only.
        let f = store.path_of(&h1).unwrap().join("islet.yaml");
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
            fs::read_to_string(dest.join("islet.yaml")).unwrap(),
            "format: 1\n"
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
