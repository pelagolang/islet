//! The local test registry and the client that checks it (`docs/signing.md`, card S-4).
//!
//! A registry here is a folder of static files, the same files a CDN would serve, so the real one changes only the
//! transport:
//!
//! ```text
//! registry.yaml                 format: 1, origin: <log name>
//! head.yaml                     the signed tree head (log signature, witness cosignatures)
//! tile/<level>/<n>[.p/<w>]      hash tiles          (tlog-tiles layout, src/tlog.rs)
//! tile/entries/<n>[.p/<w>]      entry tiles: release, yank and revoke entries
//! lookup/<key>/<version>        `index: <n>`: where the release entry is in the log
//! pkg/<key>/<version>/          the package files; pkg/<key>/<version>.sigs/ the publisher's signature
//! ```
//!
//! The client trusts nothing in the folder. What it trusts is on its own machine, in the store
//! (`<store>/registries/<origin>.yaml`): the log's key id, the witnesses' key ids and how many must cosign. From
//! that it checks the head, checks that the log only grew since the head it saw last, and for every locked
//! package checks that the log holds exactly this release (tree hash and signer) with an inclusion proof that
//! leads to the head's root. It also refuses yanked releases and revoked keys.
//!
//! Not here: an HTTP transport (the files are read from a folder), and a lookup of yanks and revocations that
//! scales (the audit reads every entry; a registry with a billion needs an index, which is the registry card's).

use crate::blake3;
use crate::keys;
use crate::lock::{is_key_id, Lock};
use crate::sign;
use crate::store;
use crate::tlog::{self, Entry, Hash, Head, Log, SignedHead, TileHashes, TileSource, TILE_WIDTH};
use crate::yaml::{self, Diag, PErr, Val};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Where a registry's files come from: a folder, or a web address. Paths are relative (`head.yaml`, `tile/0/000`).
pub trait Fetch {
    fn get(&self, path: &str) -> Result<Vec<u8>, String>;
}

/// A path the registry layout can contain: plain segments only, so a hostile name cannot leave the folder.
fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && !seg.contains(['\\', ':', '\0', '?', '#', '%', ' '])
        })
}

pub struct DirFetch(pub PathBuf);

impl Fetch for DirFetch {
    fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        if !safe_rel(path) {
            return Err(format!("`{path}` is not a registry path"));
        }
        read(&file_of(&self.0, path))
    }
}

/// A registry at an `http://` or `https://` address, read with the system's `curl` (as the key store uses the
/// system's tools: Islet links no TLS library). No redirects are followed, and nothing is trusted because of how
/// it was fetched: every file is checked against the signed head or a hash.
pub struct HttpFetch {
    base: String,
}

impl HttpFetch {
    pub fn new(base: &str) -> Result<HttpFetch, String> {
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(format!("`{base}` is not an http:// or https:// address"));
        }
        Ok(HttpFetch {
            base: base.trim_end_matches('/').to_string(),
        })
    }
}

impl Fetch for HttpFetch {
    fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        if !safe_rel(path) {
            return Err(format!("`{path}` is not a registry path"));
        }
        let url = format!("{}/{path}", self.base);
        let out = std::process::Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "60",
                "--max-filesize",
                "268435456",
                "--proto",
                "=http,https",
                &url,
            ])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "reading a registry over HTTP needs `curl`, which was not found".to_string()
                } else {
                    format!("curl: {e}")
                }
            })?;
        if !out.status.success() {
            return Err(format!(
                "{url}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout)
    }
}

/// The origin a registry names itself, read from `registry.yaml` (to find the trust anchor for it).
pub fn origin_of(location: &str) -> Result<String, String> {
    read_origin(&*fetch_for(location)?)
}

/// The source for a location: a web address or a folder.
pub fn fetch_for(location: &str) -> Result<Box<dyn Fetch>, String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        Ok(Box::new(HttpFetch::new(location)?))
    } else {
        Ok(Box::new(DirFetch(PathBuf::from(location))))
    }
}

/// Serve a registry folder as static files on `listener`, until `limit` requests have been answered (forever
/// when `None`). GET only; nothing outside the folder; the same bytes a CDN would hold.
pub fn serve(
    dir: &Path,
    listener: std::net::TcpListener,
    limit: Option<usize>,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    let mut served = 0usize;
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|e| e.to_string())?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (status, body): (&str, Vec<u8>) = match parts.as_slice() {
            ["GET", target, _] => {
                let path = target.trim_start_matches('/');
                match DirFetch(dir.to_path_buf()).get(path) {
                    Ok(b) => ("200 OK", b),
                    Err(_) => ("404 Not Found", b"not found\n".to_vec()),
                }
            }
            _ => ("405 Method Not Allowed", b"GET only\n".to_vec()),
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
        served += 1;
        if limit.is_some_and(|l| served >= l) {
            break;
        }
    }
    Ok(())
}

/// The path of an entry tile or hash tile on disk.
fn file_of(dir: &Path, tile: &str) -> PathBuf {
    tile.split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part))
}

fn lookup_file(dir: &Path, key: &str, version: &str) -> PathBuf {
    dir.join("lookup").join(key).join(version)
}

// ---- the server side: a folder that grows ----------------------------------------------------------------------

pub struct Registry {
    pub dir: PathBuf,
}

impl Registry {
    /// Where the package folders live: a `DirIndex` root.
    pub fn pkg_root(&self) -> PathBuf {
        self.dir.join("pkg")
    }

    pub fn init(dir: &Path, origin: &str, log_secret: &[u8; 32]) -> Result<Registry, String> {
        if dir.join("head.yaml").exists() {
            return Err(format!("{}: is already a registry", dir.display()));
        }
        let head = Head {
            origin: origin.to_string(),
            size: 0,
            root: tlog::empty_root(),
        };
        let signed = SignedHead::sign(head, log_secret)?;
        write(
            &dir.join("registry.yaml"),
            format!("format: 1\norigin: {}\n", yaml::string(origin)).as_bytes(),
        )?;
        write(&dir.join("head.yaml"), signed.to_text()?.as_bytes())?;
        Ok(Registry {
            dir: dir.to_path_buf(),
        })
    }

    pub fn open(dir: &Path) -> Result<Registry, String> {
        if !dir.join("head.yaml").is_file() {
            return Err(format!(
                "{}: not a registry (no head.yaml); `islet registry init` makes one",
                dir.display()
            ));
        }
        Ok(Registry {
            dir: dir.to_path_buf(),
        })
    }

    pub fn origin(&self) -> Result<String, String> {
        read_origin(&DirFetch(self.dir.clone()))
    }

    pub fn head(&self) -> Result<SignedHead, String> {
        read_head(&DirFetch(self.dir.clone()))
    }

    /// The log, rebuilt from its entry tiles and checked against the head's root.
    pub fn log(&self) -> Result<Log, String> {
        let head = self.head()?;
        let log = Log::from_entries(read_entries(&DirFetch(self.dir.clone()), head.head.size)?);
        if log.root() != head.head.root {
            return Err("the entries on disk do not hash to the head's root".into());
        }
        Ok(log)
    }

    /// Add an entry, rewrite the tiles and sign a new head. Returns the entry's index.
    pub fn append(&self, entry: &Entry, log_secret: &[u8; 32]) -> Result<u64, String> {
        let origin = self.origin()?;
        let mut log = self.log()?;
        let index = log.append(&entry.to_bytes()?);
        for (path, data) in &log.tile_files().0 {
            write(&file_of(&self.dir, path), data)?;
        }
        let head = Head {
            origin,
            size: log.size(),
            root: log.root(),
        };
        let signed = SignedHead::sign(head, log_secret)?;
        write(&self.dir.join("head.yaml"), signed.to_text()?.as_bytes())?;
        Ok(index)
    }

    /// Publish a package: copy it, sign it with the publisher's key, record the release in the log.
    pub fn publish(
        &self,
        package_dir: &Path,
        key: &str,
        version: &str,
        publisher_secret: &[u8; 32],
        log_secret: &[u8; 32],
    ) -> Result<u64, String> {
        let tree_file = self.pkg_root().join(key).join(format!("{version}.tree"));
        if tree_file.exists() {
            return Err(format!(
                "{key}@{version} is already published; a release never changes (publish a new version)"
            ));
        }
        let manifest = store::tree_manifest(package_dir).map_err(|e| e.to_string())?;
        let tree = blake3::to_hex(&blake3::hash(manifest.as_bytes()));
        let sig = sign::Sig::make(publisher_secret, key, version, &tree)?;
        let manifest_file = package_dir.join("islet.yaml");
        let shown = manifest_file.display().to_string();
        let m = crate::manifest::parse_manifest(&shown, &read_text(&manifest_file)?)
            .map_err(|d| d.to_string())?;
        if let Some(p) = &m.package.publisher {
            if *p != sig.signer() {
                return Err(format!(
                    "{shown} names the publisher {p}, but the signing key is {}",
                    sig.signer()
                ));
            }
        }
        // A package is its manifest plus one blob per file, each named by its hash: no archive, nothing to unpack.
        for (_, bytes) in store::tree_files(package_dir).map_err(|e| e.to_string())? {
            let blob = self
                .dir
                .join("blob")
                .join(blake3::to_hex(&blake3::hash(&bytes)));
            if !blob.exists() {
                write(&blob, &bytes)?;
            }
        }
        write(
            &self
                .pkg_root()
                .join(key)
                .join(format!("{version}.sigs"))
                .join(crate::trust::sig_file_name(&sig.signer())),
            sig.to_text().as_bytes(),
        )?;
        write(&tree_file, manifest.as_bytes())?;
        let index = self.append(
            &Entry::Release {
                package: key.to_string(),
                version: version.to_string(),
                tree,
                signer: sig.signer(),
            },
            log_secret,
        )?;
        write(
            &lookup_file(&self.dir, key, version),
            format!("index: {index}\n").as_bytes(),
        )?;
        Ok(index)
    }

    /// Withdraw a release. Only its own signer may: the entry names the same tree and signer as the release.
    pub fn yank(
        &self,
        key: &str,
        version: &str,
        publisher_secret: &[u8; 32],
        log_secret: &[u8; 32],
    ) -> Result<u64, String> {
        let me = sign::key_id(&sign::public_of(publisher_secret));
        let log = self.log()?;
        let found = (0..log.size())
            .filter_map(|i| Entry::parse(log.entry(i)?).ok())
            .find_map(|e| match e {
                Entry::Release {
                    package,
                    version: v,
                    tree,
                    signer,
                } if package == key && v == version => Some((tree, signer)),
                _ => None,
            });
        let (tree, signer) = found.ok_or_else(|| format!("{key}@{version} is not in the log"))?;
        if signer != me {
            return Err(format!("{key}@{version} was signed by {signer}, not {me}"));
        }
        self.append(
            &Entry::Yank {
                package: key.to_string(),
                version: version.to_string(),
                tree,
                signer,
            },
            log_secret,
        )
    }

    /// Record that a key is compromised. (Who may ask for this is the real registry's business; here the
    /// operator, who holds the log key, decides.)
    pub fn revoke(&self, key_id: &str, log_secret: &[u8; 32]) -> Result<u64, String> {
        self.append(
            &Entry::Revoke {
                key: key_id.to_string(),
            },
            log_secret,
        )
    }

    /// Add a witness's cosignature to the current head after checking it as a witness must: the log's signature,
    /// and a consistency proof from the last head this witness cosigned (kept in `<store>/witness/`).
    pub fn cosign(
        &self,
        store_root: &Path,
        witness_secret: &[u8; 32],
        log_key: &str,
    ) -> Result<String, String> {
        let mut head = self.head()?;
        let witness_id = sign::key_id(&sign::public_of(witness_secret));
        let state = store_root
            .join("witness")
            .join(witness_id.replace(':', "-"))
            .join(format!("{}.yaml", head.head.origin.replace('/', "_")));
        let previous: Option<SignedHead> = match fs::read_to_string(&state) {
            Ok(src) => Some(
                SignedHead::parse(&state.display().to_string(), &src).map_err(|d| d.to_string())?,
            ),
            Err(_) => None,
        };
        let proof = match &previous {
            Some(p) if p.head.size <= head.head.size => {
                let src = DirFetch(self.dir.clone());
                let tiles = FetchTiles(&src);
                let th = TileHashes {
                    tiles: &tiles,
                    size: head.head.size,
                };
                tlog::consistency_proof(&th, p.head.size, head.head.size)?
            }
            _ => Vec::new(),
        };
        let cosig = tlog::witness_cosign(
            witness_secret,
            log_key,
            previous.as_ref().map(|p| &p.head),
            &proof,
            &head,
        )?;
        if !head.witnesses.iter().any(|w| w.public == cosig.public) {
            head.witnesses.push(cosig);
        }
        write(&self.dir.join("head.yaml"), head.to_text()?.as_bytes())?;
        write(&state, head.to_text()?.as_bytes())?;
        Ok(witness_id)
    }
}

fn read_origin(src: &dyn Fetch) -> Result<String, String> {
    let text = String::from_utf8(src.get("registry.yaml")?)
        .map_err(|_| "registry.yaml is not text".to_string())?;
    let inner = || -> Result<String, PErr> {
        let root = yaml::parse(&text)?;
        match root.get("origin").map(|n| &n.v) {
            Some(Val::Str(s)) => Ok(s.clone()),
            _ => Err(PErr::new(0, "`origin` is missing")),
        }
    };
    inner().map_err(|e| Diag::new("registry.yaml", &text, e).to_string())
}

fn read_head(src: &dyn Fetch) -> Result<SignedHead, String> {
    let text = String::from_utf8(src.get("head.yaml")?)
        .map_err(|_| "head.yaml is not text".to_string())?;
    SignedHead::parse("head.yaml", &text).map_err(|d| d.to_string())
}

/// All entries of the log of `size` leaves, read from the entry tiles.
fn read_entries(src: &dyn Fetch, size: u64) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    for n in 0..size.div_ceil(TILE_WIDTH) {
        let width = (size - n * TILE_WIDTH).min(TILE_WIDTH);
        let path = tlog::entry_tile_path(n, width);
        let entries =
            tlog::decode_entry_tile(&src.get(&path)?).map_err(|e| format!("{path}: {e}"))?;
        if entries.len() as u64 != width {
            return Err(format!(
                "{path}: has {} entries, not {width}",
                entries.len()
            ));
        }
        out.extend(entries);
    }
    Ok(out)
}

/// Hash tiles read from a registry.
pub struct FetchTiles<'a>(pub &'a dyn Fetch);

impl TileSource for FetchTiles<'_> {
    fn tile(&self, level: u32, n: u64, width: u64) -> Result<Vec<Hash>, String> {
        let path = tlog::tile_path(level, n, width);
        tlog::parse_tile(&self.0.get(&path)?, width).map_err(|e| format!("{path}: {e}"))
    }
}

// ---- the client: what this machine trusts ------------------------------------------------------------------------

/// The trust anchor for one registry, kept in the store, never in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    pub origin: String,
    pub log_key: String,
    pub witnesses: Vec<String>,
    /// How many of `witnesses` must have cosigned a head.
    pub need: usize,
}

impl Trust {
    pub fn path(store_root: &Path, origin: &str) -> PathBuf {
        store_root
            .join("registries")
            .join(format!("{}.yaml", origin.replace('/', "_")))
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "# islet registry trust: written by `islet registry add`.\nformat: 1\norigin: {}\nlog_key: {}\nneed: {}\n",
            yaml::string(&self.origin),
            yaml::quoted(&self.log_key),
            self.need
        );
        if !self.witnesses.is_empty() {
            out.push_str("witnesses:\n");
            for w in &self.witnesses {
                out.push_str(&format!("  - {}\n", yaml::quoted(w)));
            }
        }
        out
    }

    pub fn parse(file: &str, src: &str) -> Result<Trust, Diag> {
        let inner = || -> Result<Trust, PErr> {
            let root = yaml::parse(src)?;
            let text = |k: &str| match root.get(k).map(|n| (&n.v, n.at)) {
                Some((Val::Str(s), _)) => Ok(s.clone()),
                Some((_, at)) => Err(PErr::new(at, format!("`{k}` must be text"))),
                None => Err(PErr::new(0, format!("`{k}` is missing"))),
            };
            if root.get("format").map(|n| &n.v) != Some(&Val::Int(1)) {
                return Err(PErr::new(0, "this islet reads `format: 1`"));
            }
            let log_key = text("log_key")?;
            if !is_key_id(&log_key) {
                return Err(PErr::new(0, "`log_key` must be a key id"));
            }
            let mut witnesses = Vec::new();
            if let Some(n) = root.get("witnesses") {
                let Val::Arr(items) = &n.v else {
                    return Err(PErr::new(n.at, "`witnesses` must be a list"));
                };
                for i in items {
                    match &i.v {
                        Val::Str(s) if is_key_id(s) => witnesses.push(s.clone()),
                        _ => return Err(PErr::new(i.at, "a witness is a key id")),
                    }
                }
            }
            let need = match root.get("need").map(|n| (&n.v, n.at)) {
                Some((Val::Int(n), _)) if *n >= 0 => *n as usize,
                Some((_, at)) => return Err(PErr::new(at, "`need` is a whole number")),
                None => 0,
            };
            if need > witnesses.len() {
                return Err(PErr::new(0, "`need` is more than the witnesses listed"));
            }
            Ok(Trust {
                origin: text("origin")?,
                log_key,
                witnesses,
                need,
            })
        };
        inner().map_err(|e| Diag::new(file, src, e))
    }
}

/// What the whole log says, checked to be the log the head vouches for.
#[derive(Debug, Default)]
pub struct Audit {
    /// For each release, the (tree, signer) of every entry that names it. More than one is a conflict.
    pub releases: BTreeMap<(String, String), Vec<(String, String)>>,
    /// Releases withdrawn by their own signer.
    pub yanked: BTreeSet<(String, String)>,
    pub revoked: BTreeSet<String>,
    pub entries: u64,
}

pub struct Client {
    src: Box<dyn Fetch>,
    pub store_root: PathBuf,
    pub trust: Trust,
}

impl Client {
    /// The client for a registry, a folder or a web address: its trust anchor must already be in the store.
    pub fn open(location: &str, store_root: &Path) -> Result<Client, String> {
        let source = fetch_for(location)?;
        let origin = read_origin(&*source)?;
        let path = Trust::path(store_root, &origin);
        let src = fs::read_to_string(&path).map_err(|_| {
            format!(
                "the registry `{origin}` is not trusted on this machine; run `islet registry add {location} --log-key <id>` (the key id comes from the registry's operator, not from the registry)"
            )
        })?;
        let trust = Trust::parse(&path.display().to_string(), &src).map_err(|d| d.to_string())?;
        if trust.origin != origin {
            return Err(format!(
                "{}: is for `{}`, not `{origin}`",
                path.display(),
                trust.origin
            ));
        }
        Ok(Client {
            src: source,
            store_root: store_root.to_path_buf(),
            trust,
        })
    }

    fn state_path(&self) -> PathBuf {
        self.store_root
            .join("registries")
            .join(format!("{}.head.yaml", self.trust.origin.replace('/', "_")))
    }

    /// The registry's current head, if it is the log's (signed by the trusted key, cosigned by enough witnesses)
    /// and the log has only grown since the head this machine saw last. Remembers it.
    pub fn check_head(&self) -> Result<SignedHead, String> {
        let head = read_head(&*self.src)?;
        head.verify(
            &self.trust.origin,
            &self.trust.log_key,
            &self.trust.witnesses,
            self.trust.need,
        )?;
        let state = self.state_path();
        if let Ok(src) = fs::read_to_string(&state) {
            let prev =
                SignedHead::parse(&state.display().to_string(), &src).map_err(|d| d.to_string())?;
            let proof = if prev.head.size > 0 && prev.head.size < head.head.size {
                let tiles = FetchTiles(&*self.src);
                let th = TileHashes {
                    tiles: &tiles,
                    size: head.head.size,
                };
                tlog::consistency_proof(&th, prev.head.size, head.head.size)?
            } else {
                Vec::new()
            };
            tlog::check_update(&prev.head, &head.head, &proof).map_err(|e| {
                format!(
                    "the registry's log is not an extension of the one seen before ({} entries): {e}",
                    prev.head.size
                )
            })?;
        }
        write(&state, head.to_text()?.as_bytes())?;
        Ok(head)
    }

    /// Read every entry, check that they hash to the head's root, and sum up releases, yanks and revocations.
    pub fn audit(&self, head: &SignedHead) -> Result<Audit, String> {
        let entries = read_entries(&*self.src, head.head.size)?;
        if Log::from_entries(entries.clone()).root() != head.head.root {
            return Err("the entries do not hash to the signed head's root".into());
        }
        let mut a = Audit {
            entries: head.head.size,
            ..Audit::default()
        };
        let mut yanks = Vec::new();
        for (i, bytes) in entries.iter().enumerate() {
            match Entry::parse(bytes).map_err(|e| format!("entry {i}: {e}"))? {
                Entry::Release {
                    package,
                    version,
                    tree,
                    signer,
                } => {
                    let v = a.releases.entry((package, version)).or_default();
                    if !v.contains(&(tree.clone(), signer.clone())) {
                        v.push((tree, signer));
                    }
                }
                Entry::Yank {
                    package,
                    version,
                    signer,
                    ..
                } => yanks.push((package, version, signer)),
                Entry::Revoke { key } => {
                    a.revoked.insert(key);
                }
            }
        }
        for (package, version, signer) in yanks {
            // Only the release's own signer can withdraw it.
            if a.releases
                .get(&(package.clone(), version.clone()))
                .is_some_and(|v| v.iter().any(|(_, s)| *s == signer))
            {
                a.yanked.insert((package, version));
            }
        }
        Ok(a)
    }

    /// The entry the log holds for a release, found through `lookup/` and proved in the log by an inclusion
    /// proof from the tiles. A lying lookup file cannot make a different entry pass: the proof is of the entry.
    pub fn release_entry(
        &self,
        head: &SignedHead,
        key: &str,
        version: &str,
    ) -> Result<Entry, String> {
        let file = format!("lookup/{key}/{version}");
        let text = self
            .src
            .get(&file)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(|| format!("{key}@{version} is not in the log"))?;
        let index: u64 = text
            .strip_prefix("index: ")
            .and_then(|r| r.trim_end().parse().ok())
            .ok_or_else(|| format!("{file}: not `index: <n>`"))?;
        if index >= head.head.size {
            return Err(format!(
                "{key}@{version}: the lookup points at entry {index}, past the end of the log ({})",
                head.head.size
            ));
        }
        let n = index / TILE_WIDTH;
        let width = (head.head.size - n * TILE_WIDTH).min(TILE_WIDTH);
        let path = tlog::entry_tile_path(n, width);
        let entries = tlog::decode_entry_tile(&self.src.get(&path)?)?;
        let bytes = entries
            .get((index - n * TILE_WIDTH) as usize)
            .ok_or_else(|| format!("{path}: entry {index} is missing"))?;
        let tiles = FetchTiles(&*self.src);
        let th = TileHashes {
            tiles: &tiles,
            size: head.head.size,
        };
        let proof = tlog::inclusion_proof(&th, head.head.size, index)?;
        tlog::verify_inclusion(
            &tlog::leaf_hash(bytes),
            index,
            head.head.size,
            &proof,
            &head.head.root,
        )
        .map_err(|e| {
            format!("{key}@{version}: entry {index} is not in the log the head vouches for: {e}")
        })?;
        Entry::parse(bytes)
    }

    /// Where downloaded packages are kept: a folder in the shape of a `DirIndex`.
    pub fn cache_dir(&self) -> PathBuf {
        self.store_root
            .join("registries")
            .join(self.trust.origin.replace('/', "_"))
            .join("pkg")
    }

    /// Make sure the package is in the cache, whole and as the lock says. The tree manifest must hash to `tree`;
    /// every file is fetched by its hash and checked against it, so a registry or a network that changes a byte
    /// is caught before anything is stored. The publisher's signature (by `signer`) is brought along for
    /// `islet lock` and `islet install` to check.
    pub fn fetch_package(
        &self,
        key: &str,
        version: &str,
        tree: &str,
        signer: Option<&str>,
    ) -> Result<(), String> {
        let dest = self.cache_dir().join(key).join(version);
        if dest.is_dir() && store::tree_hash(&dest).ok().as_deref() == Some(tree) {
            // Already here and still intact.
        } else {
            let manifest = self.src.get(&format!("pkg/{key}/{version}.tree"))?;
            if blake3::to_hex(&blake3::hash(&manifest)) != tree {
                return Err(format!(
                    "{key}@{version}: the registry's file list does not hash to {tree}"
                ));
            }
            let text = String::from_utf8(manifest).map_err(|_| "a file list is not text")?;
            let stage = self
                .cache_dir()
                .join(format!(".fetch-{}-{}", tree, std::process::id()));
            let _ = fs::remove_dir_all(&stage);
            let mut seen = BTreeSet::new();
            for line in text.lines() {
                let mut it = line.splitn(3, ' ');
                let (Some(hash), Some(len), Some(path)) = (it.next(), it.next(), it.next()) else {
                    return Err(format!("{key}@{version}: bad file list line `{line}`"));
                };
                let len: usize = len
                    .parse()
                    .map_err(|_| format!("{key}@{version}: bad length in `{line}`"))?;
                if blake3::from_hex(hash).is_none() || !safe_rel(path) || !seen.insert(path) {
                    return Err(format!(
                        "{key}@{version}: unacceptable file list line `{line}`"
                    ));
                }
                let bytes = self.src.get(&format!("blob/{hash}"))?;
                if bytes.len() != len || blake3::to_hex(&blake3::hash(&bytes)) != hash {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(format!(
                        "{key}@{version}: {path} is not the file the list names"
                    ));
                }
                write(&file_of(&stage, path), &bytes)?;
            }
            if store::tree_hash(&stage).map_err(|e| e.to_string())? != tree {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!("{key}@{version}: the files do not make up {tree}"));
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let _ = fs::remove_dir_all(&dest);
            fs::rename(&stage, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        if let Some(signer) = signer {
            let name = crate::trust::sig_file_name(signer);
            let sig = self.src.get(&format!("pkg/{key}/{version}.sigs/{name}"))?;
            write(
                &self
                    .cache_dir()
                    .join(key)
                    .join(format!("{version}.sigs"))
                    .join(name),
                &sig,
            )?;
        }
        Ok(())
    }

    /// Check every package of a lock against the log. `previous` is the lock before this one, whose pinned
    /// signers are warned about when revoked. Returns notes to show.
    pub fn check_lock(
        &self,
        head: &SignedHead,
        audit: &Audit,
        lock: &Lock,
        previous: Option<&Lock>,
    ) -> Result<Vec<String>, String> {
        let mut errors = Vec::new();
        for p in &lock.packages {
            let id = format!("{}@{}", p.name, p.version);
            let version = p.version.to_string();
            let entry = match self.release_entry(head, &p.name, &version) {
                Ok(Entry::Release {
                    package,
                    version: v,
                    tree,
                    signer,
                }) if package == p.name && v == version => (tree, signer),
                Ok(_) => {
                    errors.push(format!(
                        "{id}: the lookup leads to an entry about something else"
                    ));
                    continue;
                }
                Err(e) => {
                    errors.push(format!("{id}: {e}"));
                    continue;
                }
            };
            let (tree, signer) = entry;
            if tree != p.hash {
                errors.push(format!(
                    "{id}: the log records tree {tree}, but the package here is {}",
                    p.hash
                ));
            }
            if audit
                .releases
                .get(&(p.name.clone(), version.clone()))
                .is_some_and(|v| v.len() > 1)
            {
                errors.push(format!(
                    "{id}: the log holds two different releases of this version"
                ));
            }
            if audit.yanked.contains(&(p.name.clone(), version.clone())) {
                errors.push(format!("{id}: was yanked by its publisher"));
            }
            if audit.revoked.contains(&signer) {
                errors.push(format!("{id}: its signer {signer} has been revoked"));
            }
            match &p.signer {
                Some(s) if *s == signer => {}
                Some(s) => errors.push(format!(
                    "{id}: the lock pins {s}, but the log says {signer} signed it"
                )),
                None => errors.push(format!(
                    "{id}: the log says {signer} signed it, but no valid signature by it was found"
                )),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        let mut notes = vec![format!(
            "checked {} packages against the log of {} entries (head of {} signed by {})",
            lock.packages.len(),
            audit.entries,
            self.trust.origin,
            self.trust.log_key
        )];
        if let Some(prev) = previous {
            let mut seen = BTreeSet::new();
            for p in &prev.packages {
                if let Some(s) = &p.signer {
                    if audit.revoked.contains(s) && seen.insert((p.name.clone(), s.clone())) {
                        notes.push(format!(
                            "warning: {} is still pinned to {s}, which has been revoked",
                            p.name
                        ));
                    }
                }
            }
        }
        Ok(notes)
    }
}

/// The resolver's view of a registry: each release it asks about is looked up in the log (with its proof),
/// downloaded into the cache and checked there, and then read as an ordinary folder index.
pub struct RemoteIndex<'a> {
    pub client: &'a Client,
    pub head: &'a SignedHead,
    pub cache: crate::resolve::DirIndex,
}

impl<'a> RemoteIndex<'a> {
    pub fn new(client: &'a Client, head: &'a SignedHead) -> RemoteIndex<'a> {
        RemoteIndex {
            client,
            head,
            cache: crate::resolve::DirIndex {
                root: client.cache_dir(),
            },
        }
    }
}

impl crate::resolve::Index for RemoteIndex<'_> {
    fn entry(
        &self,
        key: &str,
        version: &crate::manifest::Version,
    ) -> Result<crate::resolve::Entry, String> {
        let v = version.to_string();
        match self.client.release_entry(self.head, key, &v)? {
            Entry::Release {
                package,
                version: ev,
                tree,
                signer,
            } if package == key && ev == v => {
                self.client.fetch_package(key, &v, &tree, Some(&signer))?;
            }
            _ => return Err(format!("the log has no release of {key}@{v}")),
        }
        self.cache.entry(key, version)
    }
}

/// The key to load for a command: its secret from the key store.
pub fn secret(store_root: &Path, id: &str) -> Result<[u8; 32], String> {
    keys::load(store_root, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::Locked;
    use crate::manifest::Version;

    struct Env {
        root: PathBuf,
        reg: Registry,
        store: PathBuf,
        log_secret: [u8; 32],
        log_id: String,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn id(secret: &[u8; 32]) -> String {
        sign::key_id(&sign::public_of(secret))
    }

    fn env(name: &str) -> Env {
        let root =
            std::env::temp_dir().join(format!("islet-registry-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let log_secret = sign::generate().unwrap();
        let reg = Registry::init(&root.join("reg"), "registry.test", &log_secret).unwrap();
        Env {
            store: root.join("store"),
            log_id: id(&log_secret),
            log_secret,
            reg,
            root,
        }
    }

    impl Env {
        fn package(&self, name: &str, version: &str, body: &str) -> PathBuf {
            let d = self.root.join("src").join(format!("{name}-{version}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("islet.yaml"),
                format!("format: 1\npackage:\n  name: {name}\n  version: {version}\n"),
            )
            .unwrap();
            fs::write(d.join("a.pel"), body).unwrap();
            d
        }

        fn publish(
            &self,
            name: &str,
            version: &str,
            body: &str,
            by: &[u8; 32],
        ) -> Result<u64, String> {
            let key = if version.starts_with("2.") || version.starts_with('3') {
                format!("{name}/v{}", &version[..1])
            } else {
                name.to_string()
            };
            self.reg.publish(
                &self.package(name, version, body),
                &key,
                version,
                by,
                &self.log_secret,
            )
        }

        /// The tree hash a published release has: the hash of its file list.
        fn tree_of(&self, key: &str, version: &str) -> String {
            let list = fs::read(
                self.reg
                    .pkg_root()
                    .join(key)
                    .join(format!("{version}.tree")),
            )
            .unwrap();
            blake3::to_hex(&blake3::hash(&list))
        }

        fn trust(&self, witnesses: &[String], need: usize) {
            let t = Trust {
                origin: "registry.test".into(),
                log_key: self.log_id.clone(),
                witnesses: witnesses.to_vec(),
                need,
            };
            let p = Trust::path(&self.store, "registry.test");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, t.to_text()).unwrap();
        }

        fn client(&self) -> Result<Client, String> {
            Client::open(self.reg.dir.to_str().unwrap(), &self.store)
        }

        /// What `islet lock` would hold for these releases.
        fn lock(&self, releases: &[(&str, &str, Option<String>)]) -> Lock {
            Lock {
                packages: releases
                    .iter()
                    .map(|(k, v, signer)| Locked {
                        name: k.to_string(),
                        version: Version::parse(v).unwrap(),
                        hash: self.tree_of(k, v),
                        signer: signer.clone(),
                        dependencies: vec![],
                    })
                    .collect(),
                ..Lock::default()
            }
        }
    }

    #[test]
    fn a_client_checks_what_was_published() {
        let e = env("roundtrip");
        let (a, b) = (sign::generate().unwrap(), sign::generate().unwrap());
        assert!(e.client().err().unwrap().contains("islet registry add"));
        e.trust(&[], 0);
        assert_eq!(e.publish("json", "1.5.0", "one", &a), Ok(0));
        assert_eq!(e.publish("units", "0.2.4", "two", &b), Ok(1));
        assert_eq!(e.publish("json", "2.0.1", "three", &a), Ok(2));
        let c = e.client().unwrap();
        let head = c.check_head().unwrap();
        assert_eq!(head.head.size, 3);
        let audit = c.audit(&head).unwrap();
        assert_eq!(audit.releases.len(), 3);
        let lock = e.lock(&[
            ("json", "1.5.0", Some(id(&a))),
            ("units", "0.2.4", Some(id(&b))),
            ("json/v2", "2.0.1", Some(id(&a))),
        ]);
        let notes = c.check_lock(&head, &audit, &lock, None).unwrap();
        assert!(notes[0].contains("checked 3 packages"), "{notes:?}");
        // The signature sits where `islet lock` looks for it, and a release never changes.
        assert!(e.reg.pkg_root().join("json/1.5.0.sigs").is_dir());
        assert!(e
            .publish("json", "1.5.0", "other", &a)
            .unwrap_err()
            .contains("already published"));
        // A package that says it is published by one key is not published by another.
        let d = e.package("wire", "0.7.2", "x");
        fs::write(
            d.join("islet.yaml"),
            format!(
                "format: 1\npackage:\n  name: wire\n  version: 0.7.2\n  publisher: \"{}\"\n",
                id(&b)
            ),
        )
        .unwrap();
        let err = e
            .reg
            .publish(&d, "wire", "0.7.2", &a, &e.log_secret)
            .unwrap_err();
        assert!(err.contains("names the publisher"), "{err}");
    }

    #[test]
    fn a_wrong_tree_signer_or_lookup_is_caught() {
        let e = env("wrong");
        let (a, b) = (sign::generate().unwrap(), sign::generate().unwrap());
        e.trust(&[], 0);
        e.publish("json", "1.5.0", "one", &a).unwrap();
        e.publish("units", "0.2.4", "two", &b).unwrap();
        let c = e.client().unwrap();
        let head = c.check_head().unwrap();
        let audit = c.audit(&head).unwrap();
        // A lock that holds another tree than the log records for the release.
        let mut bad = e.lock(&[("json", "1.5.0", Some(id(&a)))]);
        bad.packages[0].hash = e.tree_of("units", "0.2.4");
        let err = c.check_lock(&head, &audit, &bad, None).unwrap_err();
        assert!(err.contains("the log records tree"), "{err}");
        // Another signer in the lock than in the log, or none found.
        let err = c
            .check_lock(
                &head,
                &audit,
                &e.lock(&[("units", "0.2.4", Some(id(&a)))]),
                None,
            )
            .unwrap_err();
        assert!(err.contains("the log says"), "{err}");
        let err = c
            .check_lock(&head, &audit, &e.lock(&[("units", "0.2.4", None)]), None)
            .unwrap_err();
        assert!(err.contains("no valid signature"), "{err}");
        // A lookup file that points at another release cannot make that release pass.
        fs::write(lookup_file(&e.reg.dir, "units", "0.2.4"), "index: 0\n").unwrap();
        let err = c
            .check_lock(
                &head,
                &audit,
                &e.lock(&[("units", "0.2.4", Some(id(&b)))]),
                None,
            )
            .unwrap_err();
        assert!(err.contains("something else"), "{err}");
        // Past the end, or not in the log at all.
        fs::write(lookup_file(&e.reg.dir, "units", "0.2.4"), "index: 9\n").unwrap();
        assert!(c
            .check_lock(
                &head,
                &audit,
                &e.lock(&[("units", "0.2.4", Some(id(&b)))]),
                None
            )
            .is_err());
        fs::remove_file(lookup_file(&e.reg.dir, "units", "0.2.4")).unwrap();
        assert!(c
            .check_lock(
                &head,
                &audit,
                &e.lock(&[("units", "0.2.4", Some(id(&b)))]),
                None
            )
            .is_err());
    }

    #[test]
    fn tampered_entries_and_tiles_are_caught() {
        let e = env("tamper");
        let a = sign::generate().unwrap();
        e.trust(&[], 0);
        for i in 0..5 {
            e.publish("json", &format!("1.5.{i}"), "x", &a).unwrap();
        }
        let c = e.client().unwrap();
        let head = c.check_head().unwrap();
        assert!(c.audit(&head).is_ok());
        // An entry rewritten in place: the entries no longer hash to the signed root.
        let tile = file_of(&e.reg.dir, &tlog::entry_tile_path(0, 5));
        let good = fs::read(&tile).unwrap();
        let mut bad = good.clone();
        let at = bad.len() - 5;
        bad[at] ^= 1;
        fs::write(&tile, &bad).unwrap();
        assert!(c.audit(&head).is_err());
        fs::write(&tile, &good).unwrap();
        // A hash tile rewritten: the inclusion proof computed from it no longer leads to the root.
        let htile = file_of(&e.reg.dir, &tlog::tile_path(0, 0, 5));
        let hgood = fs::read(&htile).unwrap();
        let mut hbad = hgood.clone();
        hbad[3 * 32] ^= 1;
        fs::write(&htile, &hbad).unwrap();
        let lock = e.lock(&[("json", "1.5.0", Some(id(&a)))]);
        let audit = c.audit(&head).unwrap();
        let err = c.check_lock(&head, &audit, &lock, None).unwrap_err();
        assert!(err.contains("not in the log the head vouches for"), "{err}");
        fs::write(&htile, &hgood).unwrap();
        assert!(c.check_lock(&head, &audit, &lock, None).is_ok());
    }

    #[test]
    fn the_head_needs_the_trusted_key_and_the_witnesses() {
        let e = env("witness");
        let a = sign::generate().unwrap();
        let (w1, w2) = (sign::generate().unwrap(), sign::generate().unwrap());
        e.trust(&[id(&w1), id(&w2)], 2);
        e.publish("json", "1.5.0", "x", &a).unwrap();
        let c = e.client().unwrap();
        assert!(c.check_head().unwrap_err().contains("0 of the required 2"));
        e.reg.cosign(&e.store, &w1, &e.log_id).unwrap();
        assert!(c.check_head().unwrap_err().contains("1 of the required 2"));
        // A witness that does not know the log's key refuses to cosign.
        assert!(e
            .reg
            .cosign(&e.store, &w2, &id(&sign::generate().unwrap()))
            .is_err());
        e.reg.cosign(&e.store, &w2, &e.log_id).unwrap();
        assert_eq!(c.check_head().unwrap().witnesses.len(), 2);
        // The next head has to be cosigned again; the witnesses prove it grew first.
        e.publish("json", "1.5.1", "y", &a).unwrap();
        assert!(c.check_head().is_err());
        e.reg.cosign(&e.store, &w1, &e.log_id).unwrap();
        e.reg.cosign(&e.store, &w2, &e.log_id).unwrap();
        assert_eq!(c.check_head().unwrap().head.size, 2);
        // A client that trusts another log key does not accept this registry at all.
        let t = Trust {
            origin: "registry.test".into(),
            log_key: id(&sign::generate().unwrap()),
            witnesses: vec![],
            need: 0,
        };
        fs::write(Trust::path(&e.store, "registry.test"), t.to_text()).unwrap();
        assert!(e
            .client()
            .unwrap()
            .check_head()
            .unwrap_err()
            .contains("not the log key"));
    }

    #[test]
    fn a_rollback_or_a_rewritten_log_is_refused() {
        let e = env("rollback");
        let a = sign::generate().unwrap();
        e.trust(&[], 0);
        e.publish("json", "1.5.0", "x", &a).unwrap();
        let old_head = fs::read(e.reg.dir.join("head.yaml")).unwrap();
        e.publish("json", "1.5.1", "y", &a).unwrap();
        let c = e.client().unwrap();
        assert_eq!(c.check_head().unwrap().head.size, 2);
        // The registry serves its older, validly signed head again: the log shrank.
        fs::write(e.reg.dir.join("head.yaml"), &old_head).unwrap();
        let err = c.check_head().unwrap_err();
        assert!(err.contains("not an extension"), "{err}");
        // A log with the same key and origin but another history that has grown past what was seen.
        let forked = Registry::init(&e.root.join("fork"), "registry.test", &e.log_secret).unwrap();
        for (i, body) in ["z", "z", "z"].iter().enumerate() {
            let d = e.package("json", &format!("1.9.{i}"), body);
            forked
                .publish(&d, "json", &format!("1.9.{i}"), &a, &e.log_secret)
                .unwrap();
        }
        let c2 = Client::open(e.root.join("fork").to_str().unwrap(), &e.store).unwrap();
        let err = c2.check_head().unwrap_err();
        assert!(err.contains("not an extension"), "{err}");
    }

    #[test]
    fn a_yanked_release_and_a_revoked_key_are_refused() {
        let e = env("yank");
        let (a, b) = (sign::generate().unwrap(), sign::generate().unwrap());
        e.trust(&[], 0);
        e.publish("json", "1.5.0", "x", &a).unwrap();
        e.publish("units", "0.2.4", "y", &b).unwrap();
        // Only the release's own signer can yank it.
        assert!(e
            .reg
            .yank("json", "1.5.0", &b, &e.log_secret)
            .unwrap_err()
            .contains("not"));
        assert!(e.reg.yank("json", "9.9.9", &a, &e.log_secret).is_err());
        e.reg.yank("json", "1.5.0", &a, &e.log_secret).unwrap();
        let c = e.client().unwrap();
        let head = c.check_head().unwrap();
        let audit = c.audit(&head).unwrap();
        let err = c
            .check_lock(
                &head,
                &audit,
                &e.lock(&[("json", "1.5.0", Some(id(&a)))]),
                None,
            )
            .unwrap_err();
        assert!(err.contains("yanked"), "{err}");
        // Nothing was deleted: the entry is still in the log and the package is still there.
        assert_eq!(head.head.size, 3);
        assert!(e.reg.pkg_root().join("json/1.5.0.tree").is_file());
        // A revoked key: refused for a new pin, warned about where it is pinned already.
        e.reg.revoke(&id(&b), &e.log_secret).unwrap();
        let head = c.check_head().unwrap();
        let audit = c.audit(&head).unwrap();
        let units = e.lock(&[("units", "0.2.4", Some(id(&b)))]);
        let err = c.check_lock(&head, &audit, &units, None).unwrap_err();
        assert!(err.contains("revoked"), "{err}");
        let other = e.lock(&[]);
        let notes = c.check_lock(&head, &audit, &other, Some(&units)).unwrap();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("warning") && n.contains(&id(&b))),
            "{notes:?}"
        );
    }

    #[test]
    fn the_trust_file_round_trips_and_refuses_nonsense() {
        let k = |c: char| format!("key:{}", c.to_string().repeat(64));
        let t = Trust {
            origin: "registry.example/shard-3".into(),
            log_key: k('a'),
            witnesses: vec![k('b'), k('c')],
            need: 1,
        };
        assert_eq!(Trust::parse("t", &t.to_text()).unwrap(), t);
        let text = t.to_text();
        assert!(Trust::parse("t", &text.replace("need: 1", "need: 3")).is_err());
        assert!(Trust::parse("t", &text.replace(&k('a'), "key:abc")).is_err());
        assert!(Trust::parse("t", &text.replace("format: 1", "format: 2")).is_err());
        assert!(Trust::parse("t", &text.replace("log_key", "logkey")).is_err());
    }

    #[test]
    fn registry_paths_cannot_leave_the_folder() {
        for ok in [
            "head.yaml",
            "tile/0/x001/000",
            "pkg/json/v2/2.0.1.tree",
            "blob/ab12",
        ] {
            assert!(safe_rel(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\b",
            "c:x",
            "a b",
            "a%2e",
            "a?b",
            "a#b",
        ] {
            assert!(!safe_rel(bad), "{bad}");
        }
        let d = std::env::temp_dir().join(format!("islet-safe-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        assert!(DirFetch(d.clone()).get("../etc/passwd").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_package_is_fetched_by_its_file_list_and_every_byte_is_checked() {
        let e = env("fetch");
        let a = sign::generate().unwrap();
        e.trust(&[], 0);
        e.publish("units", "0.2.4", "two", &a).unwrap();
        let c = e.client().unwrap();
        let tree = e.tree_of("units", "0.2.4");
        c.fetch_package("units", "0.2.4", &tree, Some(&id(&a)))
            .unwrap();
        let got = c.cache_dir().join("units/0.2.4");
        assert_eq!(store::tree_hash(&got).unwrap(), tree);
        assert_eq!(fs::read_to_string(got.join("a.pel")).unwrap(), "two");
        assert!(c.cache_dir().join("units/0.2.4.sigs").is_dir());
        // The cache is not trusted either: a changed file is fetched again.
        fs::write(got.join("a.pel"), "evil").unwrap();
        c.fetch_package("units", "0.2.4", &tree, None).unwrap();
        assert_eq!(fs::read_to_string(got.join("a.pel")).unwrap(), "two");
        // A registry that changes a file's bytes, or the list, or its length: refused, nothing kept.
        let _ = fs::remove_dir_all(c.cache_dir());
        let blob = e
            .reg
            .dir
            .join("blob")
            .join(blake3::to_hex(&blake3::hash(b"two")));
        fs::write(&blob, "twO").unwrap();
        let err = c.fetch_package("units", "0.2.4", &tree, None).unwrap_err();
        assert!(err.contains("is not the file the list names"), "{err}");
        assert!(!c.cache_dir().join("units/0.2.4").exists());
        fs::write(&blob, "two").unwrap();
        let list = e.reg.pkg_root().join("units/0.2.4.tree");
        let good = fs::read_to_string(&list).unwrap();
        fs::write(&list, good.replace("a.pel", "b.pel")).unwrap();
        let err = c.fetch_package("units", "0.2.4", &tree, None).unwrap_err();
        assert!(err.contains("does not hash to"), "{err}");
        // A list that names a path outside the package.
        let evil = good.replace("a.pel", "../x");
        fs::write(&list, &evil).unwrap();
        let evil_tree = blake3::to_hex(&blake3::hash(evil.as_bytes()));
        let err = c
            .fetch_package("units", "0.2.4", &evil_tree, None)
            .unwrap_err();
        assert!(err.contains("unacceptable"), "{err}");
    }

    #[test]
    fn the_resolver_reads_dependencies_through_the_log_and_the_cache() {
        use crate::resolve::Index;
        let e = env("remote");
        let a = sign::generate().unwrap();
        e.trust(&[], 0);
        let d = e.package("json", "1.5.0", "x");
        fs::write(
            d.join("islet.yaml"),
            "format: 1\npackage:\n  name: json\n  version: 1.5.0\ndependencies:\n  units: 0.2.4\n",
        )
        .unwrap();
        e.publish("units", "0.2.4", "y", &a).unwrap();
        e.reg
            .publish(&d, "json", "1.5.0", &a, &e.log_secret)
            .unwrap();
        let c = e.client().unwrap();
        let head = c.check_head().unwrap();
        let idx = RemoteIndex::new(&c, &head);
        let entry = idx
            .entry("json", &Version::parse("1.5.0").unwrap())
            .unwrap();
        assert_eq!(entry.hash, e.tree_of("json", "1.5.0"));
        assert_eq!(
            entry.deps,
            vec![("units".to_string(), Version::parse("0.2.4").unwrap())]
        );
        // The signature came along, where `islet lock` looks for it.
        assert!(c
            .cache_dir()
            .join("json/1.5.0.sigs")
            .join(crate::trust::sig_file_name(&id(&a)))
            .is_file());
        // A release that is not in the log is not offered, even if its files were slipped in.
        assert!(idx
            .entry("json", &Version::parse("1.9.9").unwrap())
            .is_err());
    }

    /// The same registry over real HTTP: served by `serve`, read by `curl`.
    #[test]
    fn a_registry_works_over_http() {
        if std::process::Command::new("curl")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("curl is not installed; skipping");
            return;
        }
        let e = env("http");
        let a = sign::generate().unwrap();
        e.trust(&[], 0);
        e.publish("units", "0.2.4", "two", &a).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let dir = e.reg.dir.clone();
        let server = std::thread::spawn(move || serve(&dir, listener, Some(400)));
        let c = Client::open(&url, &e.store).unwrap();
        let head = c.check_head().unwrap();
        assert_eq!(head.head.size, 1);
        let audit = c.audit(&head).unwrap();
        let lock = e.lock(&[("units", "0.2.4", Some(id(&a)))]);
        // Fetch with the signer from the lock, then check the lock against the log.
        c.fetch_package("units", "0.2.4", &lock.packages[0].hash, Some(&id(&a)))
            .unwrap();
        assert!(c.check_lock(&head, &audit, &lock, None).is_ok());
        // Not found, a path that tries to leave, and a method that is not GET.
        assert!(c.src.get("nothing/here").is_err());
        assert!(HttpFetch::new(&url).unwrap().get("../head.yaml").is_err());
        let raw = std::process::Command::new("curl")
            .args([
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                &format!("{url}/head.yaml"),
            ])
            .output();
        if let Ok(o) = raw {
            let code = String::from_utf8_lossy(&o.stdout).to_string();
            assert!(code == "405" || code.is_empty() || code == "000", "{code}");
        }
        drop(server); // the thread ends with the test process
    }
}
