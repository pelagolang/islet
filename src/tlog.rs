//! The transparency log: formats and proofs (`docs/signing.md` section 4, card S-3). A library with no network and
//! no registry in it; the local test registry (S-4) and, later, the real one are built on top.
//!
//! **Conventional parts.** The Merkle tree and its inclusion and consistency proofs are RFC 6962 / RFC 9162
//! (leaf hashes start with byte 0, interior hashes with byte 1, the tree splits at the largest power of two below
//! its size). The tile layout (`tile/<level>/<n>`, three-digit path groups, partial tiles `.p/<width>`) is the
//! C2SP `tlog-tiles` layout that Go's checksum database uses, so a CDN can serve it as static files. Signed tree
//! heads and witness cosignatures are the same idea as C2SP checkpoints and witnesses.
//!
//! **Pelago's own parts.** The hash is BLAKE3 (one hash in the whole system), so these logs do not interoperate
//! byte for byte with Go's SHA-256 ones. Entries are a short canonical text (`islet-log-entry-v1`) naming a
//! release, a yank or a key revocation. Heads and cosignatures use strict YAML like every other Islet file and
//! carry the signer's public key. Sharding is by the first bits of `BLAKE3(package key)`.
//!
//! A client that holds one signed head can check that an entry is in the log everyone sees (an inclusion proof
//! against the head's root) and that the log only ever grew (a consistency proof from the head it saw last), with
//! a few kilobytes of tiles, and can insist that N of M witnesses cosigned the head.

use crate::blake3;
use crate::lock::is_key_id;
use crate::manifest::{is_dep_key, Version};
use crate::sign;
use crate::yaml::{self, Diag, PErr, Val};
use std::collections::BTreeMap;

pub type Hash = [u8; 32];

pub fn hex(h: &Hash) -> String {
    blake3::to_hex(h)
}

/// The root of a tree with no leaves.
pub fn empty_root() -> Hash {
    blake3::hash(&[])
}

/// `H(0x00 || data)`: a leaf. The prefix keeps a leaf from ever being read as an interior node.
pub fn leaf_hash(data: &[u8]) -> Hash {
    let mut v = Vec::with_capacity(data.len() + 1);
    v.push(0);
    v.extend_from_slice(data);
    blake3::hash(&v)
}

/// `H(0x01 || left || right)`: an interior node.
/// The roots of the perfect subtrees that make up a tree, left to right: all that is needed to keep adding leaves
/// and to know the root, without the leaves themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontier {
    pub size: u64,
    pub nodes: Vec<Hash>,
}

impl Frontier {
    pub fn push(&mut self, leaf: Hash) {
        let mut h = leaf;
        let mut s = self.size;
        while s & 1 == 1 {
            let left = self
                .nodes
                .pop()
                .expect("a frontier has one node per set bit");
            h = node_hash(&left, &h);
            s >>= 1;
        }
        self.nodes.push(h);
        self.size += 1;
    }

    pub fn root(&self) -> Hash {
        let mut it = self.nodes.iter().rev();
        let Some(last) = it.next() else {
            return empty_root();
        };
        it.fold(*last, |r, left| node_hash(left, &r))
    }

    /// Does the list of nodes fit the size? (One per set bit.)
    pub fn fits(&self) -> bool {
        self.nodes.len() as u32 == self.size.count_ones()
    }
}

pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut v = [0u8; 65];
    v[0] = 1;
    v[1..33].copy_from_slice(left);
    v[33..].copy_from_slice(right);
    blake3::hash(&v)
}

/// The largest power of two strictly below `n` (n >= 2).
fn pow2_below(n: u64) -> u64 {
    1u64 << (63 - (n - 1).leading_zeros())
}

// ---- hashes of complete subtrees ------------------------------------------------------------------------------

/// Where the hashes of complete subtrees come from: memory (`Log`), or tiles fetched on demand (`TileHashes`).
pub trait HashSource {
    /// The hash of the subtree of `2^level` leaves that starts at leaf `index << level`. It must lie inside the tree.
    fn hash(&self, level: u32, index: u64) -> Result<Hash, String>;
}

/// The root of leaves `lo..hi` (RFC 9162 `MTH`).
fn mth(src: &dyn HashSource, lo: u64, hi: u64) -> Result<Hash, String> {
    let n = hi - lo;
    if n == 0 {
        return Ok(empty_root());
    }
    if n.is_power_of_two() && lo.is_multiple_of(n) {
        return src.hash(n.trailing_zeros(), lo / n);
    }
    let k = pow2_below(n);
    Ok(node_hash(&mth(src, lo, lo + k)?, &mth(src, lo + k, hi)?))
}

pub fn root(src: &dyn HashSource, size: u64) -> Result<Hash, String> {
    mth(src, 0, size)
}

/// The inclusion proof for leaf `index` in the tree of `size` leaves (RFC 9162 `PATH`).
pub fn inclusion_proof(src: &dyn HashSource, size: u64, index: u64) -> Result<Vec<Hash>, String> {
    if index >= size {
        return Err(format!("leaf {index} is not in a tree of {size} leaves"));
    }
    fn path(
        src: &dyn HashSource,
        m: u64,
        lo: u64,
        hi: u64,
        out: &mut Vec<Hash>,
    ) -> Result<(), String> {
        let n = hi - lo;
        if n == 1 {
            return Ok(());
        }
        let k = pow2_below(n);
        if m < k {
            path(src, m, lo, lo + k, out)?;
            out.push(mth(src, lo + k, hi)?);
        } else {
            path(src, m - k, lo + k, hi, out)?;
            out.push(mth(src, lo, lo + k)?);
        }
        Ok(())
    }
    let mut out = Vec::new();
    path(src, index, 0, size, &mut out)?;
    Ok(out)
}

/// Check an inclusion proof (RFC 9162 section 2.1.3.2).
pub fn verify_inclusion(
    leaf: &Hash,
    index: u64,
    size: u64,
    proof: &[Hash],
    root: &Hash,
) -> Result<(), String> {
    if index >= size {
        return Err(format!("leaf {index} is not in a tree of {size} leaves"));
    }
    let (mut f, mut s) = (index, size - 1);
    let mut r = *leaf;
    for p in proof {
        if s == 0 {
            return Err("the proof is longer than the tree is deep".into());
        }
        if f & 1 == 1 || f == s {
            r = node_hash(p, &r);
            if f & 1 == 0 {
                while f & 1 == 0 && f != 0 {
                    f >>= 1;
                    s >>= 1;
                }
            }
        } else {
            r = node_hash(&r, p);
        }
        f >>= 1;
        s >>= 1;
    }
    if s != 0 {
        return Err("the proof is too short".into());
    }
    if &r != root {
        return Err("the proof does not lead to the root".into());
    }
    Ok(())
}

/// The consistency proof between the tree of `old` leaves and the tree of `new` leaves (RFC 9162 `PROOF`).
pub fn consistency_proof(src: &dyn HashSource, old: u64, new: u64) -> Result<Vec<Hash>, String> {
    if old > new {
        return Err(format!("{old} leaves cannot grow into {new}"));
    }
    if old == 0 || old == new {
        return Ok(Vec::new());
    }
    fn sub(
        src: &dyn HashSource,
        m: u64,
        lo: u64,
        hi: u64,
        complete: bool,
        out: &mut Vec<Hash>,
    ) -> Result<(), String> {
        let n = hi - lo;
        if m == n {
            if !complete {
                out.push(mth(src, lo, hi)?);
            }
            return Ok(());
        }
        let k = pow2_below(n);
        if m <= k {
            sub(src, m, lo, lo + k, complete, out)?;
            out.push(mth(src, lo + k, hi)?);
        } else {
            sub(src, m - k, lo + k, hi, false, out)?;
            out.push(mth(src, lo, lo + k)?);
        }
        Ok(())
    }
    let mut out = Vec::new();
    sub(src, old, 0, new, true, &mut out)?;
    Ok(out)
}

/// Check that the tree with `new_root` over `new` leaves extends the tree with `old_root` over `old` leaves
/// (RFC 9162 section 2.1.4.2).
pub fn verify_consistency(
    old: u64,
    new: u64,
    old_root: &Hash,
    new_root: &Hash,
    proof: &[Hash],
) -> Result<(), String> {
    if old > new {
        return Err(format!("the log shrank from {old} to {new} leaves"));
    }
    if old == new {
        return if proof.is_empty() && old_root == new_root {
            Ok(())
        } else {
            Err("the same size has two different roots".into())
        };
    }
    if old == 0 {
        return if proof.is_empty() {
            Ok(())
        } else {
            Err("an empty tree needs no proof".into())
        };
    }
    let mut path: Vec<Hash> = Vec::with_capacity(proof.len() + 1);
    if old.is_power_of_two() {
        path.push(*old_root);
    }
    path.extend_from_slice(proof);
    if path.is_empty() {
        return Err("the proof is empty".into());
    }
    let (mut f, mut s) = (old - 1, new - 1);
    while f & 1 == 1 {
        f >>= 1;
        s >>= 1;
    }
    let (mut fr, mut sr) = (path[0], path[0]);
    for c in &path[1..] {
        if s == 0 {
            return Err("the proof is longer than the tree is deep".into());
        }
        if f & 1 == 1 || f == s {
            fr = node_hash(c, &fr);
            sr = node_hash(c, &sr);
            if f & 1 == 0 {
                while f & 1 == 0 && f != 0 {
                    f >>= 1;
                    s >>= 1;
                }
            }
        } else {
            sr = node_hash(&sr, c);
        }
        f >>= 1;
        s >>= 1;
    }
    if s != 0 {
        return Err("the proof is too short".into());
    }
    if &fr != old_root {
        return Err("the proof does not lead to the old root".into());
    }
    if &sr != new_root {
        return Err("the proof does not lead to the new root".into());
    }
    Ok(())
}

// ---- tiles ------------------------------------------------------------------------------------------------------

/// A tile holds 2^8 = 256 hashes of one row of the tree: 8 KiB, so a lookup in a log of a billion entries needs
/// about four of them.
pub const TILE_HEIGHT: u32 = 8;
pub const TILE_WIDTH: u64 = 1 << TILE_HEIGHT;

/// `n` as the path of `tlog-tiles`: zero-padded to groups of three digits, every group but the last prefixed `x`
/// (1234067 is `x001/x234/067`).
fn index_path(n: u64) -> String {
    let mut groups = Vec::new();
    let mut rest = n;
    loop {
        groups.push(rest % 1000);
        rest /= 1000;
        if rest == 0 {
            break;
        }
    }
    groups.reverse();
    let last = groups.len() - 1;
    groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            if i < last {
                format!("x{g:03}")
            } else {
                format!("{g:03}")
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a hash tile lives: `tile/<level>/<n>`, or `tile/<level>/<n>.p/<width>` while it is not yet full.
pub fn tile_path(level: u32, n: u64, width: u64) -> String {
    let base = format!("tile/{level}/{}", index_path(n));
    if width == TILE_WIDTH {
        base
    } else {
        format!("{base}.p/{width}")
    }
}

/// Where a tile of entries lives: `tile/entries/<n>`, `.p/<width>` while partial.
pub fn entry_tile_path(n: u64, width: u64) -> String {
    let base = format!("tile/entries/{}", index_path(n));
    if width == TILE_WIDTH {
        base
    } else {
        format!("{base}.p/{width}")
    }
}

/// Entries in a tile are each preceded by their length as two bytes, big endian.
pub fn encode_entry_tile(entries: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for e in entries {
        let n = u16::try_from(e.len()).map_err(|_| "an entry is longer than 65535 bytes")?;
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(e);
    }
    Ok(out)
}

pub fn decode_entry_tile(mut data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if data.len() < 2 {
            return Err("an entry tile ends inside a length".into());
        }
        let n = u16::from_be_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + n {
            return Err("an entry tile ends inside an entry".into());
        }
        out.push(data[2..2 + n].to_vec());
        data = &data[2 + n..];
    }
    Ok(out)
}

/// Where tiles come from: a directory of static files, a CDN, or memory.
pub trait TileSource {
    /// The `width` hashes of tile `n` at `level`. A tile that is not full is asked for by its current width.
    fn tile(&self, level: u32, n: u64, width: u64) -> Result<Vec<Hash>, String>;
}

/// Hashes of complete subtrees of a tree of `size` leaves, read from tiles.
pub struct TileHashes<'a> {
    pub tiles: &'a dyn TileSource,
    pub size: u64,
}

impl HashSource for TileHashes<'_> {
    fn hash(&self, level: u32, index: u64) -> Result<Hash, String> {
        let row_level = level / TILE_HEIGHT * TILE_HEIGHT;
        let d = level - row_level;
        if row_level >= 64 || d >= 64 {
            return Err("no such level".into());
        }
        let first = index << d; // first node of the row (level `row_level`) under this node
        let count = 1u64 << d;
        let row_len = self.size >> row_level;
        if first + count > row_len {
            return Err(format!(
                "the subtree ({level}, {index}) is not complete in a tree of {} leaves",
                self.size
            ));
        }
        let n = first / TILE_WIDTH;
        let width = (row_len - n * TILE_WIDTH).min(TILE_WIDTH);
        let tile = self.tiles.tile(row_level / TILE_HEIGHT, n, width)?;
        if tile.len() as u64 != width {
            return Err(format!(
                "tile {n} at level {} has {} hashes, not {width}",
                row_level / TILE_HEIGHT,
                tile.len()
            ));
        }
        let at = (first - n * TILE_WIDTH) as usize;
        let mut row: Vec<Hash> = tile[at..at + count as usize].to_vec();
        while row.len() > 1 {
            row = row.chunks(2).map(|p| node_hash(&p[0], &p[1])).collect();
        }
        Ok(row[0])
    }
}

/// The `width` hashes in the bytes of a tile file.
pub fn parse_tile(data: &[u8], width: u64) -> Result<Vec<Hash>, String> {
    if data.len() as u64 != width * 32 {
        return Err(format!(
            "a tile of {width} hashes is {} bytes, not {}",
            width * 32,
            data.len()
        ));
    }
    Ok(data
        .chunks(32)
        .map(|c| <[u8; 32]>::try_from(c).expect("chunks of 32"))
        .collect())
}

/// Tiles as files in a map (the tests and the local registry use this; a CDN client would fetch the same paths).
pub struct TileFiles(pub BTreeMap<String, Vec<u8>>);

impl TileSource for TileFiles {
    fn tile(&self, level: u32, n: u64, width: u64) -> Result<Vec<Hash>, String> {
        let path = tile_path(level, n, width);
        let data = self
            .0
            .get(&path)
            .ok_or_else(|| format!("{path}: not found"))?;
        parse_tile(data, width).map_err(|e| format!("{path}: {e}"))
    }
}

// ---- an in-memory log -----------------------------------------------------------------------------------------

/// A log held in memory: the builder for tests and for the local test registry.
#[derive(Debug, Clone, Default)]
pub struct Log {
    entries: Vec<Vec<u8>>,
    leaves: Vec<Hash>,
}

impl Log {
    pub fn new() -> Log {
        Log::default()
    }

    /// A log with these entries, in order.
    pub fn from_entries(entries: Vec<Vec<u8>>) -> Log {
        let leaves = entries.iter().map(|e| leaf_hash(e)).collect();
        Log { entries, leaves }
    }

    pub fn append(&mut self, entry: &[u8]) -> u64 {
        self.entries.push(entry.to_vec());
        self.leaves.push(leaf_hash(entry));
        self.leaves.len() as u64 - 1
    }

    pub fn size(&self) -> u64 {
        self.leaves.len() as u64
    }

    pub fn entry(&self, index: u64) -> Option<&[u8]> {
        self.entries.get(index as usize).map(|e| e.as_slice())
    }

    pub fn leaf(&self, index: u64) -> Option<Hash> {
        self.leaves.get(index as usize).copied()
    }

    /// The root of the first `size` leaves (an older head's root, for example).
    pub fn root_at(&self, size: u64) -> Result<Hash, String> {
        if size > self.size() {
            return Err(format!("the log has {} entries, not {size}", self.size()));
        }
        mth(self, 0, size)
    }

    pub fn root(&self) -> Hash {
        self.root_at(self.size()).expect("own size")
    }

    /// Every file a static server needs to serve the log at its current size.
    pub fn tile_files(&self) -> TileFiles {
        let mut files = BTreeMap::new();
        let size = self.size();
        let mut level = 0u32;
        while level * TILE_HEIGHT < 64 && (size >> (level * TILE_HEIGHT)) > 0 {
            let row_len = size >> (level * TILE_HEIGHT);
            let tiles = row_len.div_ceil(TILE_WIDTH);
            for n in 0..tiles {
                let width = (row_len - n * TILE_WIDTH).min(TILE_WIDTH);
                let mut data = Vec::with_capacity(width as usize * 32);
                for j in n * TILE_WIDTH..n * TILE_WIDTH + width {
                    data.extend_from_slice(
                        &self.hash(level * TILE_HEIGHT, j).expect("inside the tree"),
                    );
                }
                files.insert(tile_path(level, n, width), data);
            }
            level += 1;
        }
        let tiles = size.div_ceil(TILE_WIDTH);
        for n in 0..tiles {
            let width = (size - n * TILE_WIDTH).min(TILE_WIDTH);
            let es: Vec<&[u8]> = (n * TILE_WIDTH..n * TILE_WIDTH + width)
                .map(|i| self.entries[i as usize].as_slice())
                .collect();
            if let Ok(data) = encode_entry_tile(&es) {
                files.insert(entry_tile_path(n, width), data);
            }
        }
        TileFiles(files)
    }
}

impl HashSource for Log {
    fn hash(&self, level: u32, index: u64) -> Result<Hash, String> {
        if level >= 64 {
            return Err("no such level".into());
        }
        let lo = index.checked_shl(level).ok_or("no such subtree")?;
        let hi = lo + (1u64 << level);
        if hi > self.size() {
            return Err(format!("the subtree ({level}, {index}) is not complete"));
        }
        fn go(leaves: &[Hash]) -> Hash {
            if leaves.len() == 1 {
                return leaves[0];
            }
            let k = leaves.len() / 2; // aligned power-of-two ranges split in half
            node_hash(&go(&leaves[..k]), &go(&leaves[k..]))
        }
        Ok(go(&self.leaves[lo as usize..hi as usize]))
    }
}

// ---- entries ----------------------------------------------------------------------------------------------------

const ENTRY_DOMAIN: &str = "islet-log-entry-v1";

/// What the log records. Nothing is ever deleted: a yank or a revocation is one more entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A package release: the tree hash and the key that signed it.
    Release {
        package: String,
        version: String,
        tree: String,
        signer: String,
    },
    /// The release is withdrawn; `signer` is the key that asks for it.
    Yank {
        package: String,
        version: String,
        tree: String,
        signer: String,
    },
    /// The key is compromised: clients refuse to pin it and warn about it where it is pinned. `proof` is the
    /// key holder's own signature over `revoke_statement(key)` (128 hex digits), or `none` when the operator
    /// revoked it on their own say.
    Revoke { key: String, proof: String },
}

/// What a key holder signs to revoke their own key.
pub fn revoke_statement(key: &str) -> String {
    format!("islet-revoke-v1\nkey: {key}\n")
}

/// Does a revoke entry's proof hold? `none` is the operator's word and needs nothing; anything else must be
/// the revoked key's own signature.
pub fn revoke_proof_ok(key: &str, proof: &str) -> bool {
    if proof == "none" {
        return true;
    }
    let Some(sig) = crate::sign::unhex(proof) else {
        return false;
    };
    // The key id is a hash of the public key, so the public key travels in the proof: 64 bytes of signature
    // after 32 bytes of public key.
    if sig.len() != 96 {
        return false;
    }
    let public: [u8; 32] = sig[..32].try_into().unwrap();
    let signature: [u8; 64] = sig[32..].try_into().unwrap();
    crate::sign::key_id(&public) == key
        && crate::sign::verify_bytes(&public, revoke_statement(key).as_bytes(), &signature)
}

fn tree_ok(t: &str) -> bool {
    blake3::from_hex(t).is_some() && !t.bytes().any(|b| b.is_ascii_uppercase())
}

impl Entry {
    /// The canonical bytes, which are the leaf of the log. Fields are checked first, so none can carry a newline.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let text = match self {
            Entry::Release {
                package,
                version,
                tree,
                signer,
            }
            | Entry::Yank {
                package,
                version,
                tree,
                signer,
            } => {
                if !is_dep_key(package) {
                    return Err(format!("`{package}` is not a package name"));
                }
                if Version::parse(version).is_none() {
                    return Err(format!("`{version}` is not a version like 1.2.3"));
                }
                if !tree_ok(tree) {
                    return Err("the tree hash must be 64 lowercase hex digits".into());
                }
                if !is_key_id(signer) {
                    return Err("a key id is `key:` and 64 lowercase hex digits".into());
                }
                let kind = if matches!(self, Entry::Release { .. }) {
                    "release"
                } else {
                    "yank"
                };
                format!(
                    "{ENTRY_DOMAIN}\nkind: {kind}\npackage: {package}\nversion: {version}\ntree: {tree}\nsigner: {signer}\n"
                )
            }
            Entry::Revoke { key, proof } => {
                if !is_key_id(key) {
                    return Err("a key id is `key:` and 64 lowercase hex digits".into());
                }
                let hexish = proof.len() == 192
                    && proof
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                if proof != "none" && !hexish {
                    return Err("a revoke proof is `none` or 192 lowercase hex digits".into());
                }
                format!("{ENTRY_DOMAIN}\nkind: revoke\nkey: {key}\nproof: {proof}\n")
            }
        };
        Ok(text.into_bytes())
    }

    /// Read an entry. Only the canonical bytes are accepted, so one entry has exactly one leaf.
    pub fn parse(bytes: &[u8]) -> Result<Entry, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "an entry is not text")?;
        let mut lines = text.split('\n');
        if lines.next() != Some(ENTRY_DOMAIN) {
            return Err(format!("an entry starts with `{ENTRY_DOMAIN}`"));
        }
        let mut field = |name: &str| -> Result<String, String> {
            let line = lines.next().ok_or_else(|| format!("`{name}` is missing"))?;
            line.strip_prefix(&format!("{name}: "))
                .map(str::to_string)
                .ok_or_else(|| format!("expected `{name}: ...`, found `{line}`"))
        };
        let kind = field("kind")?;
        let entry = match kind.as_str() {
            "release" | "yank" => {
                let (package, version, tree, signer) = (
                    field("package")?,
                    field("version")?,
                    field("tree")?,
                    field("signer")?,
                );
                if kind == "release" {
                    Entry::Release {
                        package,
                        version,
                        tree,
                        signer,
                    }
                } else {
                    Entry::Yank {
                        package,
                        version,
                        tree,
                        signer,
                    }
                }
            }
            "revoke" => Entry::Revoke {
                key: field("key")?,
                proof: field("proof")?,
            },
            other => return Err(format!("unknown entry kind `{other}`")),
        };
        if entry.to_bytes()? != bytes {
            return Err("the entry is not in canonical form".into());
        }
        Ok(entry)
    }
}

// ---- signed tree heads and witnesses ---------------------------------------------------------------------------

const HEAD_DOMAIN: &str = "islet-log-head-v1";
const WITNESS_DOMAIN: &str = "islet-witness-v1";

/// The size and root of a log at one moment: what a head says, and what the log key signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The log's name, so a head of one log (or one shard) cannot pass for another's.
    pub origin: String,
    pub size: u64,
    pub root: Hash,
}

fn origin_ok(o: &str) -> bool {
    !o.is_empty()
        && o.len() <= 100
        && o.bytes().all(|b| b.is_ascii_graphic())
        && !o.starts_with('#')
}

impl Head {
    fn body(&self) -> String {
        format!(
            "origin: {}\nsize: {}\nroot: {}\n",
            self.origin,
            self.size,
            hex(&self.root)
        )
    }

    /// What the log signs. The domain line keeps it from being read as anything else, including a cosignature.
    pub fn log_statement(&self) -> Result<String, String> {
        if !origin_ok(&self.origin) {
            return Err("an origin is 1 to 100 visible characters".into());
        }
        Ok(format!("{HEAD_DOMAIN}\n{}", self.body()))
    }

    /// What a witness signs: the same head under its own domain line.
    pub fn witness_statement(&self) -> Result<String, String> {
        if !origin_ok(&self.origin) {
            return Err("an origin is 1 to 100 visible characters".into());
        }
        Ok(format!("{WITNESS_DOMAIN}\n{}", self.body()))
    }
}

/// A public key and its signature over a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySig {
    pub public: [u8; 32],
    pub signature: [u8; 64],
}

impl KeySig {
    pub fn id(&self) -> String {
        sign::key_id(&self.public)
    }

    fn check(&self, statement: &str) -> bool {
        sign::verify_bytes(&self.public, statement.as_bytes(), &self.signature)
    }
}

/// A head with the log's signature and any witness cosignatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedHead {
    pub head: Head,
    pub log: KeySig,
    pub witnesses: Vec<KeySig>,
}

impl SignedHead {
    pub fn sign(head: Head, log_secret: &[u8; 32]) -> Result<SignedHead, String> {
        let (public, signature) = sign::sign_bytes(log_secret, head.log_statement()?.as_bytes());
        Ok(SignedHead {
            head,
            log: KeySig { public, signature },
            witnesses: Vec::new(),
        })
    }

    /// A witness's cosignature of this head. Use [`witness_cosign`] to check the log first, as a witness must.
    pub fn add_witness(&mut self, witness_secret: &[u8; 32]) -> Result<(), String> {
        let (public, signature) =
            sign::sign_bytes(witness_secret, self.head.witness_statement()?.as_bytes());
        let w = KeySig { public, signature };
        if !self.witnesses.iter().any(|x| x.public == w.public) {
            self.witnesses.push(w);
        }
        Ok(())
    }

    /// Accept the head only if it is this log's (`origin`, signed by `log_key`) and at least `need` distinct
    /// witnesses out of `witnesses` (key ids) cosigned it. Returns the ids that did. A cosignature by a key that
    /// is not in `witnesses`, or twice by one key, counts for nothing.
    pub fn verify(
        &self,
        origin: &str,
        log_key: &str,
        witnesses: &[String],
        need: usize,
    ) -> Result<Vec<String>, String> {
        if self.head.origin != origin {
            return Err(format!(
                "the head is for `{}`, not `{origin}`",
                self.head.origin
            ));
        }
        if self.log.id() != log_key {
            return Err(format!(
                "the head is signed by {}, not the log key {log_key}",
                self.log.id()
            ));
        }
        if !self.log.check(&self.head.log_statement()?) {
            return Err("the log's signature on the head does not verify".into());
        }
        let statement = self.head.witness_statement()?;
        let mut good: Vec<String> = Vec::new();
        for w in &self.witnesses {
            let id = w.id();
            if witnesses.contains(&id) && !good.contains(&id) && w.check(&statement) {
                good.push(id);
            }
        }
        if good.len() < need {
            return Err(format!(
                "{} of the required {need} witnesses cosigned the head",
                good.len()
            ));
        }
        Ok(good)
    }

    /// The strict YAML form, for a file next to the tiles.
    pub fn to_text(&self) -> Result<String, String> {
        self.head.log_statement()?;
        let mut out = format!(
            "# islet signed tree head: written by islet, do not edit.\nformat: 1\norigin: {}\nsize: {}\nroot: {}\nsignature:\n  public: {}\n  signature: {}\n",
            yaml::string(&self.head.origin),
            self.head.size,
            yaml::quoted(&hex(&self.head.root)),
            yaml::quoted(&sign::hex(&self.log.public)),
            yaml::quoted(&sign::hex(&self.log.signature)),
        );
        if !self.witnesses.is_empty() {
            out.push_str("witnesses:\n");
            for w in &self.witnesses {
                out.push_str(&format!(
                    "  - public: {}\n    signature: {}\n",
                    yaml::quoted(&sign::hex(&w.public)),
                    yaml::quoted(&sign::hex(&w.signature))
                ));
            }
        }
        Ok(out)
    }

    pub fn parse(file: &str, src: &str) -> Result<SignedHead, Diag> {
        SignedHead::read(src).map_err(|e| Diag::new(file, src, e))
    }

    fn read(src: &str) -> Result<SignedHead, PErr> {
        let root = yaml::parse(src)?;
        for e in &root.entries {
            if !["format", "origin", "size", "root", "signature", "witnesses"]
                .contains(&e.key.as_str())
            {
                return Err(PErr::new(e.key_at, format!("unknown key `{}`", e.key)));
            }
        }
        match root.get("format") {
            Some(n) if n.v == Val::Int(1) => {}
            Some(n) => {
                return Err(PErr::new(
                    n.at,
                    "unsupported `format` (this islet reads `format: 1`)",
                ))
            }
            None => return Err(PErr::new(0, "a head needs `format: 1`")),
        }
        let need = |k: &str| {
            root.get(k)
                .ok_or_else(|| PErr::new(0, format!("`{k}` is missing")))
        };
        let origin = match &need("origin")?.v {
            Val::Str(s) if origin_ok(s) => s.clone(),
            _ => {
                return Err(PErr::new(
                    need("origin")?.at,
                    "`origin` is 1 to 100 visible characters",
                ))
            }
        };
        let size = match &need("size")?.v {
            Val::Int(n) if *n >= 0 => *n as u64,
            _ => return Err(PErr::new(need("size")?.at, "`size` is a whole number")),
        };
        let root_hash = match &need("root")?.v {
            Val::Str(s) if tree_ok(s) => blake3::from_hex(s).expect("checked"),
            _ => {
                return Err(PErr::new(
                    need("root")?.at,
                    "`root` must be 64 lowercase hex digits",
                ))
            }
        };
        fn keysig(n: &yaml::Node) -> Result<KeySig, PErr> {
            let Val::Table(t) = &n.v else {
                return Err(PErr::new(n.at, "a signature is a mapping"));
            };
            for e in &t.entries {
                if e.key != "public" && e.key != "signature" {
                    return Err(PErr::new(e.key_at, format!("unknown key `{}`", e.key)));
                }
            }
            let get = |k: &str, len: usize| -> Result<Vec<u8>, PErr> {
                match t.get(k) {
                    Some(x) => match &x.v {
                        Val::Str(s) => sign::unhex(s).filter(|v| v.len() == len).ok_or_else(|| {
                            PErr::new(
                                x.at,
                                format!("`{k}` must be {} lowercase hex digits", 2 * len),
                            )
                        }),
                        _ => Err(PErr::new(x.at, format!("`{k}` must be text"))),
                    },
                    None => Err(PErr::new(n.at, format!("`{k}` is missing"))),
                }
            };
            Ok(KeySig {
                public: get("public", 32)?.try_into().expect("32"),
                signature: get("signature", 64)?.try_into().expect("64"),
            })
        }
        let log = keysig(need("signature")?)?;
        let mut witnesses = Vec::new();
        if let Some(w) = root.get("witnesses") {
            match &w.v {
                Val::Arr(items) => {
                    for i in items {
                        witnesses.push(keysig(i)?);
                    }
                }
                _ => return Err(PErr::new(w.at, "`witnesses` must be a list")),
            }
        }
        Ok(SignedHead {
            head: Head {
                origin,
                size,
                root: root_hash,
            },
            log,
            witnesses,
        })
    }
}

/// A client that kept `old` accepts `new` only if the log grew: the same root at the same size, or a consistency
/// proof from the old head to the new one. A log that rewrites history cannot produce the proof.
pub fn check_update(old: &Head, new: &Head, proof: &[Hash]) -> Result<(), String> {
    if old.origin != new.origin {
        return Err("the two heads are of different logs".into());
    }
    verify_consistency(old.size, new.size, &old.root, &new.root, proof)
}

/// What a witness does before it cosigns: the log's signature must verify, and the head must extend the last one
/// the witness cosigned (`previous`, with the proof). Returns the witness's signature to add to the head.
pub fn witness_cosign(
    witness_secret: &[u8; 32],
    log_key: &str,
    previous: Option<&Head>,
    proof: &[Hash],
    signed: &SignedHead,
) -> Result<KeySig, String> {
    signed.verify(&signed.head.origin, log_key, &[], 0)?;
    if let Some(prev) = previous {
        check_update(prev, &signed.head, proof)?;
    }
    let (public, signature) =
        sign::sign_bytes(witness_secret, signed.head.witness_statement()?.as_bytes());
    Ok(KeySig { public, signature })
}

// ---- sharding ---------------------------------------------------------------------------------------------------

/// The most bits of a shard number (65536 shards).
pub const MAX_SHARD_BITS: u32 = 16;

/// Which shard holds a package: the first `bits` bits of `BLAKE3(package key)`. Entries about a package, its
/// releases and yanks, always land in the same shard, so a client reads one small log.
pub fn shard_of(package_key: &str, bits: u32) -> u32 {
    assert!(
        bits <= MAX_SHARD_BITS,
        "at most {MAX_SHARD_BITS} shard bits"
    );
    if bits == 0 {
        return 0;
    }
    let h = blake3::hash(package_key.as_bytes());
    let first = u32::from(h[0]) << 8 | u32::from(h[1]);
    first >> (16 - bits)
}

/// The origin of one shard's log: `<origin>/shard-<hex>`, the hex padded to the number of bits. No shards, no
/// suffix.
pub fn shard_origin(origin: &str, shard: u32, bits: u32) -> String {
    if bits == 0 {
        return origin.to_string();
    }
    format!(
        "{origin}/shard-{shard:0width$x}",
        width = bits.div_ceil(4) as usize
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn log_of(n: u64) -> Log {
        let mut l = Log::new();
        for i in 0..n {
            l.append(format!("entry {i}").as_bytes());
        }
        l
    }

    /// An independent way to the same root: pair neighbours level by level, carrying an odd one up unchanged.
    fn naive_root(leaves: &[Hash]) -> Hash {
        if leaves.is_empty() {
            return empty_root();
        }
        let mut row = leaves.to_vec();
        while row.len() > 1 {
            let mut next = Vec::new();
            for p in row.chunks(2) {
                next.push(if p.len() == 2 {
                    node_hash(&p[0], &p[1])
                } else {
                    p[0]
                });
            }
            row = next;
        }
        row[0]
    }

    #[test]
    fn leaves_and_nodes_are_domain_separated() {
        let a = leaf_hash(b"x");
        assert_ne!(a, blake3::hash(b"x"));
        // A leaf whose data looks like a node's input does not collide with that node.
        let n = node_hash(&a, &a);
        let mut fake = Vec::new();
        fake.extend_from_slice(&a);
        fake.extend_from_slice(&a);
        assert_ne!(leaf_hash(&fake), n);
        assert_eq!(root(&log_of(0), 0).unwrap(), empty_root());
    }

    /// A regression vector: the root of the log `entry 0` .. `entry 4`. It was produced by this code and
    /// checked against `naive_root`, so it pins the leaf prefix, the node prefix, BLAKE3 and the tree shape.
    #[test]
    fn a_pinned_root() {
        let l = log_of(5);
        assert_eq!(l.root(), naive_root(&l.leaves));
        assert_eq!(hex(&l.root()), PINNED_ROOT_OF_5);
    }
    const PINNED_ROOT_OF_5: &str =
        "e4823ba3733207de0977808438342e10e66d8989b540fec5a47d90226213dbda";

    #[test]
    fn roots_match_the_independent_construction() {
        for n in 0..=70u64 {
            let l = log_of(n);
            assert_eq!(l.root(), naive_root(&l.leaves), "size {n}");
            for m in 0..=n {
                assert_eq!(l.root_at(m).unwrap(), naive_root(&l.leaves[..m as usize]));
            }
        }
    }

    #[test]
    fn every_inclusion_proof_verifies_and_nothing_else_does() {
        for size in 1..=40u64 {
            let l = log_of(size);
            let r = l.root();
            for i in 0..size {
                let proof = inclusion_proof(&l, size, i).unwrap();
                let leaf = l.leaf(i).unwrap();
                assert!(
                    proof.len() as u32 <= 64 - (size - 1).leading_zeros(),
                    "short proofs"
                );
                assert_eq!(
                    verify_inclusion(&leaf, i, size, &proof, &r),
                    Ok(()),
                    "{size}/{i}"
                );
                // Wrong leaf, wrong root, wrong index. (The size is not checked on its own: a head always carries size
                // and root together, and a proof for one size can be valid against that size's root only.)
                assert!(verify_inclusion(&leaf_hash(b"other"), i, size, &proof, &r).is_err());
                assert!(verify_inclusion(&leaf, i, size, &proof, &leaf_hash(b"x")).is_err());
                if size > 1 {
                    assert!(verify_inclusion(&leaf, (i + 1) % size, size, &proof, &r).is_err());
                }
                // Any flipped bit in any proof hash, a dropped hash, an extra hash.
                for k in 0..proof.len() {
                    let mut bad = proof.clone();
                    bad[k][0] ^= 1;
                    assert!(verify_inclusion(&leaf, i, size, &bad, &r).is_err());
                }
                if !proof.is_empty() {
                    assert!(verify_inclusion(&leaf, i, size, &proof[1..], &r).is_err());
                }
                let mut longer = proof.clone();
                longer.push(leaf_hash(b"extra"));
                assert!(verify_inclusion(&leaf, i, size, &longer, &r).is_err());
            }
            assert!(inclusion_proof(&l, size, size).is_err());
        }
    }

    #[test]
    fn every_consistency_proof_verifies_and_nothing_else_does() {
        for new in 1..=40u64 {
            let l = log_of(new);
            let nr = l.root();
            for old in 0..=new {
                let or = l.root_at(old).unwrap();
                let proof = consistency_proof(&l, old, new).unwrap();
                assert_eq!(
                    verify_consistency(old, new, &or, &nr, &proof),
                    Ok(()),
                    "{old}->{new}"
                );
                if old == 0 || old == new {
                    assert!(proof.is_empty());
                    continue;
                }
                // A history that was rewritten: another old root, another new root.
                assert!(verify_consistency(old, new, &leaf_hash(b"x"), &nr, &proof).is_err());
                assert!(verify_consistency(old, new, &or, &leaf_hash(b"x"), &proof).is_err());
                for k in 0..proof.len() {
                    let mut bad = proof.clone();
                    bad[k][31] ^= 0x80;
                    assert!(
                        verify_consistency(old, new, &or, &nr, &bad).is_err(),
                        "{old}->{new}"
                    );
                }
                let mut longer = proof.clone();
                longer.push(leaf_hash(b"extra"));
                assert!(verify_consistency(old, new, &or, &nr, &longer).is_err());
                if proof.len() > 1 {
                    assert!(
                        verify_consistency(old, new, &or, &nr, &proof[..proof.len() - 1]).is_err()
                    );
                }
            }
        }
        assert!(verify_consistency(5, 3, &empty_root(), &empty_root(), &[]).is_err());
        assert!(consistency_proof(&log_of(3), 5, 3).is_err());
    }

    #[test]
    fn a_forked_log_has_no_consistency_proof_that_verifies() {
        let a = log_of(20);
        let mut b = Log::new();
        for i in 0..30u64 {
            // Same first 10 entries, then a different history.
            b.append(
                if i < 10 {
                    format!("entry {i}")
                } else {
                    format!("fork {i}")
                }
                .as_bytes(),
            );
        }
        let old_root = a.root_at(20).unwrap();
        let proof = consistency_proof(&b, 20, 30).unwrap();
        assert!(verify_consistency(20, 30, &old_root, &b.root(), &proof).is_err());
        // But the shared prefix is consistent.
        let p10 = consistency_proof(&b, 10, 30).unwrap();
        assert_eq!(
            verify_consistency(10, 30, &a.root_at(10).unwrap(), &b.root(), &p10),
            Ok(())
        );
    }

    #[test]
    fn tile_paths_follow_tlog_tiles() {
        assert_eq!(tile_path(0, 0, 256), "tile/0/000");
        assert_eq!(tile_path(1, 7, 256), "tile/1/007");
        assert_eq!(tile_path(0, 1234067, 256), "tile/0/x001/x234/067");
        assert_eq!(tile_path(2, 12, 3), "tile/2/012.p/3");
        assert_eq!(entry_tile_path(1000, 256), "tile/entries/x001/000");
        assert_eq!(entry_tile_path(5, 17), "tile/entries/005.p/17");
    }

    #[test]
    fn entry_tiles_round_trip() {
        let t = encode_entry_tile(&[b"one", b"", b"three"]).unwrap();
        assert_eq!(
            decode_entry_tile(&t).unwrap(),
            vec![b"one".to_vec(), vec![], b"three".to_vec()]
        );
        assert!(decode_entry_tile(&t[..t.len() - 1]).is_err());
        assert!(decode_entry_tile(&[0]).is_err());
        assert!(encode_entry_tile(&[&vec![0u8; 70000]]).is_err());
    }

    /// Counts the tiles a client downloads.
    struct Counting<'a> {
        inner: &'a TileFiles,
        fetched: Cell<usize>,
    }
    impl TileSource for Counting<'_> {
        fn tile(&self, level: u32, n: u64, width: u64) -> Result<Vec<Hash>, String> {
            self.fetched.set(self.fetched.get() + 1);
            self.inner.tile(level, n, width)
        }
    }

    #[test]
    fn proofs_from_tiles_match_proofs_from_memory_and_need_few_tiles() {
        // 66000 entries: two tile levels full, a third started, and partial tiles at every level.
        let l = log_of(66_000);
        let files = l.tile_files();
        let want = [
            tile_path(0, 0, 256),
            tile_path(0, 257, 66_000 - 257 * 256),
            tile_path(1, 0, 256),
            tile_path(1, 1, 1),
            tile_path(2, 0, 1),
            entry_tile_path(257, 66_000 - 257 * 256),
        ];
        for w in &want {
            assert!(files.0.contains_key(w), "{w}");
        }
        let size = l.size();
        let counting = Counting {
            inner: &files,
            fetched: Cell::new(0),
        };
        let th = TileHashes {
            tiles: &counting,
            size,
        };
        assert_eq!(root(&th, size).unwrap(), l.root());
        for i in [0u64, 1, 255, 256, 65_535, 65_536, 65_999] {
            counting.fetched.set(0);
            let proof = inclusion_proof(&th, size, i).unwrap();
            assert_eq!(proof, inclusion_proof(&l, size, i).unwrap(), "leaf {i}");
            assert_eq!(
                verify_inclusion(&l.leaf(i).unwrap(), i, size, &proof, &l.root()),
                Ok(())
            );
            // Memory-free: a proof needs a handful of tiles, never the log.
            assert!(
                counting.fetched.get() <= 40,
                "{} tiles for leaf {i}",
                counting.fetched.get()
            );
        }
        let old = 40_000u64;
        let proof = consistency_proof(&th, old, size).unwrap();
        assert_eq!(proof, consistency_proof(&l, old, size).unwrap());
        assert_eq!(
            verify_consistency(old, size, &l.root_at(old).unwrap(), &l.root(), &proof),
            Ok(())
        );
        // The entries come back out of their tiles.
        let tile = decode_entry_tile(&files.0[&entry_tile_path(0, 256)]).unwrap();
        assert_eq!(tile.len(), 256);
        assert_eq!(tile[5], b"entry 5");
    }

    #[test]
    fn a_tampered_tile_is_caught_by_the_signed_root() {
        let l = log_of(700);
        let good_root = l.root();
        let mut files = l.tile_files();
        let path = tile_path(0, 1, 256);
        files.0.get_mut(&path).unwrap()[40] ^= 1;
        let leaf = l.leaf(300).unwrap();
        {
            let th = TileHashes {
                tiles: &files,
                size: 700,
            };
            // The proof for a leaf in that tile no longer leads to the root the head vouches for.
            let proof = inclusion_proof(&th, 700, 300).unwrap();
            assert!(verify_inclusion(&leaf, 300, 700, &proof, &good_root).is_err());
            // Asking for a subtree that is not complete is refused.
            assert!(th.hash(8, 2).is_err());
        }
        // A tile of the wrong length or one that is missing is an error, not a wrong answer.
        files.0.get_mut(&path).unwrap().pop();
        let th = TileHashes {
            tiles: &files,
            size: 700,
        };
        assert!(inclusion_proof(&th, 700, 300).is_err());
        files.0.remove(&path);
        let th = TileHashes {
            tiles: &files,
            size: 700,
        };
        assert!(inclusion_proof(&th, 700, 300).is_err());
    }

    const TREE: &str = "2708a2eef28076c39dd92a656d62e0259a7011ad7d7e4e6dcddff60474e936aa";

    fn key() -> ([u8; 32], String) {
        let s = sign::generate().unwrap();
        let id = sign::key_id(&sign::public_of(&s));
        (s, id)
    }

    #[test]
    fn entries_have_exactly_one_spelling() {
        let (_, k) = key();
        let r = Entry::Release {
            package: "json/v2".into(),
            version: "2.0.1".into(),
            tree: TREE.into(),
            signer: k.clone(),
        };
        let bytes = r.to_bytes().unwrap();
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            format!("islet-log-entry-v1\nkind: release\npackage: json/v2\nversion: 2.0.1\ntree: {TREE}\nsigner: {k}\n")
        );
        assert_eq!(Entry::parse(&bytes).unwrap(), r);
        let y = Entry::Yank {
            package: "json/v2".into(),
            version: "2.0.1".into(),
            tree: TREE.into(),
            signer: k.clone(),
        };
        assert_eq!(Entry::parse(&y.to_bytes().unwrap()).unwrap(), y);
        assert_ne!(y.to_bytes().unwrap(), bytes);
        let v = Entry::Revoke {
            key: k.clone(),
            proof: "none".into(),
        };
        assert_eq!(Entry::parse(&v.to_bytes().unwrap()).unwrap(), v);
        // Anything not canonical is refused: extra text, a missing newline, a reordered field, a bad field.
        let text = String::from_utf8(bytes).unwrap();
        assert!(Entry::parse(format!("{text}x").as_bytes()).is_err());
        assert!(Entry::parse(text.trim_end().as_bytes()).is_err());
        assert!(Entry::parse(text.replace("kind: release", "kind: yankk").as_bytes()).is_err());
        assert!(Entry::parse(text.replace("json/v2", "Json").as_bytes()).is_err());
        assert!(Entry::parse(
            text.replace("islet-log-entry-v1", "islet-sign-v1")
                .as_bytes()
        )
        .is_err());
        assert!(Entry::parse(&[0xff, 0xfe]).is_err());
        assert!(Entry::Revoke {
            key: "key:zz".into(),
            proof: "none".into()
        }
        .to_bytes()
        .is_err());
        assert!(Entry::Release {
            package: "a\nb".into(),
            version: "1.0.0".into(),
            tree: TREE.into(),
            signer: k,
        }
        .to_bytes()
        .is_err());
    }

    #[test]
    fn a_frontier_keeps_the_root_while_leaves_are_added() {
        let mut l = Log::new();
        let mut f = Frontier::default();
        assert_eq!(f.root(), empty_root());
        for i in 0..40u32 {
            let e = format!("entry {i}");
            l.append(e.as_bytes());
            f.push(leaf_hash(e.as_bytes()));
            assert_eq!(f.root(), l.root(), "size {}", i + 1);
            assert!(f.fits());
        }
    }

    fn signed(l: &Log, origin: &str, secret: &[u8; 32]) -> SignedHead {
        SignedHead::sign(
            Head {
                origin: origin.into(),
                size: l.size(),
                root: l.root(),
            },
            secret,
        )
        .unwrap()
    }

    #[test]
    fn a_head_needs_the_log_key_and_enough_distinct_witnesses() {
        let l = log_of(12);
        let (log_s, log_k) = key();
        let ((w1, k1), (w2, k2), (w3, k3), (_, kx)) = (key(), key(), key(), key());
        let ws = vec![k1.clone(), k2.clone(), k3.clone()];
        let mut h = signed(&l, "islet.example", &log_s);
        assert_eq!(h.verify("islet.example", &log_k, &ws, 0).unwrap().len(), 0);
        assert!(h.verify("islet.example", &log_k, &ws, 1).is_err());
        h.add_witness(&w1).unwrap();
        h.add_witness(&w1).unwrap(); // twice by one key: still one
        assert!(h.verify("islet.example", &log_k, &ws, 2).is_err());
        h.add_witness(&w2).unwrap();
        assert_eq!(
            h.verify("islet.example", &log_k, &ws, 2).unwrap(),
            vec![k1.clone(), k2.clone()]
        );
        assert!(h.verify("islet.example", &log_k, &ws, 3).is_err());
        // A cosignature from a key that is not a known witness does not count.
        let (stranger, _) = key();
        h.add_witness(&stranger).unwrap();
        assert!(h.verify("islet.example", &log_k, &ws, 3).is_err());
        h.add_witness(&w3).unwrap();
        assert_eq!(h.verify("islet.example", &log_k, &ws, 3).unwrap().len(), 3);
        // The wrong log key, the wrong origin.
        assert!(h.verify("islet.example", &kx, &ws, 0).is_err());
        assert!(h.verify("other.example", &log_k, &ws, 0).is_err());
        // Any change to the head breaks both the log's signature and the cosignatures.
        let mut t = h.clone();
        t.head.size += 1;
        assert!(t.verify("islet.example", &log_k, &ws, 0).is_err());
        let mut t = h.clone();
        t.head.root[0] ^= 1;
        assert!(t.verify("islet.example", &log_k, &ws, 0).is_err());
        let mut t = h.clone();
        t.witnesses[0].signature[0] ^= 1;
        assert_eq!(t.verify("islet.example", &log_k, &ws, 0).unwrap().len(), 2);
    }

    #[test]
    fn a_cosignature_is_not_a_log_signature_and_the_other_way_round() {
        let l = log_of(3);
        let (log_s, log_k) = key();
        let mut h = signed(&l, "o", &log_s);
        // The log's key signing as a witness gives a signature that is not accepted as the log's.
        h.add_witness(&log_s).unwrap();
        let mut swapped = h.clone();
        swapped.log = h.witnesses[0].clone();
        assert!(swapped.verify("o", &log_k, &[], 0).is_err());
        // And the log's signature does not pass as a witness's.
        let mut other = h.clone();
        other.witnesses = vec![h.log.clone()];
        assert!(other
            .verify("o", &log_k, std::slice::from_ref(&log_k), 1)
            .is_err());
    }

    #[test]
    fn heads_round_trip_through_text() {
        let l = log_of(9);
        let (log_s, _) = key();
        let (w, _) = key();
        let mut h = signed(&l, "islet.example/shard-3", &log_s);
        let bare = SignedHead::parse("h", &h.to_text().unwrap()).unwrap();
        assert_eq!(bare, h);
        h.add_witness(&w).unwrap();
        let text = h.to_text().unwrap();
        assert_eq!(SignedHead::parse("h", &text).unwrap(), h);
        for (from, to) in [
            ("format: 1", "format: 2"),
            ("size: 9", "size: -1"),
            ("origin:", "oragin:"),
            ("root: \"", "root: \"0"),
            ("  - public", "  - pubic"),
        ] {
            assert!(
                SignedHead::parse("h", &text.replace(from, to)).is_err(),
                "{from}"
            );
        }
        assert!(SignedHead::parse("h", &format!("{text}extra: 1\n")).is_err());
    }

    #[test]
    fn a_client_follows_the_log_forward_and_catches_a_rewrite() {
        let mut l = log_of(30);
        let old = Head {
            origin: "o".into(),
            size: l.size(),
            root: l.root(),
        };
        for i in 30..75 {
            l.append(format!("entry {i}").as_bytes());
        }
        let new = Head {
            origin: "o".into(),
            size: l.size(),
            root: l.root(),
        };
        let proof = consistency_proof(&l, old.size, new.size).unwrap();
        assert_eq!(check_update(&old, &new, &proof), Ok(()));
        assert_eq!(check_update(&new, &new, &[]), Ok(()));
        // The log shrank.
        assert!(check_update(&new, &old, &[]).is_err());
        // The log rewrote entry 7 and grew: no proof it can give verifies against the head the client holds.
        let mut forked = Log::new();
        for i in 0..75u64 {
            forked.append(
                if i == 7 {
                    "rewritten".to_string()
                } else {
                    format!("entry {i}")
                }
                .as_bytes(),
            );
        }
        let fh = Head {
            origin: "o".into(),
            size: forked.size(),
            root: forked.root(),
        };
        let fproof = consistency_proof(&forked, old.size, fh.size).unwrap();
        assert!(check_update(&old, &fh, &fproof).is_err());
        assert!(check_update(&old, &fh, &proof).is_err());
        // Another log's head is not an update.
        let other = Head {
            origin: "p".into(),
            ..new.clone()
        };
        assert!(check_update(&old, &other, &proof).is_err());
    }

    #[test]
    fn a_witness_cosigns_only_a_log_that_has_grown_honestly() {
        let mut l = log_of(10);
        let (log_s, log_k) = key();
        let (w, wk) = key();
        let first = signed(&l, "o", &log_s);
        let c = witness_cosign(&w, &log_k, None, &[], &first).unwrap();
        assert_eq!(c.id(), wk);
        let mut cosigned = first.clone();
        cosigned.witnesses.push(c);
        assert_eq!(
            cosigned
                .verify("o", &log_k, std::slice::from_ref(&wk), 1)
                .unwrap()
                .len(),
            1
        );
        for i in 10..25 {
            l.append(format!("entry {i}").as_bytes());
        }
        let second = signed(&l, "o", &log_s);
        let proof = consistency_proof(&l, 10, 25).unwrap();
        assert!(witness_cosign(&w, &log_k, Some(&first.head), &proof, &second).is_ok());
        // Without the proof, with a bad proof, or for a head the log did not sign: no cosignature.
        assert!(witness_cosign(&w, &log_k, Some(&first.head), &[], &second).is_err());
        let mut forked = Log::new();
        for i in 0..25u64 {
            forked.append(
                if i == 3 {
                    "rewritten".to_string()
                } else {
                    format!("entry {i}")
                }
                .as_bytes(),
            );
        }
        let evil = signed(&forked, "o", &log_s);
        let eproof = consistency_proof(&forked, 10, 25).unwrap();
        assert!(witness_cosign(&w, &log_k, Some(&first.head), &eproof, &evil).is_err());
        let (other_log, _) = key();
        assert!(witness_cosign(&w, &log_k, None, &[], &signed(&l, "o", &other_log)).is_err());
    }

    #[test]
    fn shards_are_stable_in_range_and_even() {
        assert_eq!(shard_of("json", 0), 0);
        assert_eq!(shard_of("json/v2", 6), shard_of("json/v2", 6));
        // Fewer bits is a prefix of more bits.
        for k in ["json", "json/v2", "uart", "wire"] {
            assert_eq!(shard_of(k, 3), shard_of(k, 8) >> 5);
        }
        let mut counts = [0u32; 16];
        for i in 0..3200 {
            let s = shard_of(&format!("pkg-{i}"), 4);
            assert!(s < 16);
            counts[s as usize] += 1;
        }
        assert!(counts.iter().all(|c| (120..=280).contains(c)), "{counts:?}");
        assert_eq!(shard_origin("islet.example", 5, 0), "islet.example");
        assert_eq!(shard_origin("islet.example", 5, 4), "islet.example/shard-5");
        assert_eq!(
            shard_origin("islet.example", 0xab, 8),
            "islet.example/shard-ab"
        );
        assert_eq!(
            shard_origin("islet.example", 5, 10),
            "islet.example/shard-005"
        );
    }
}
