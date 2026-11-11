//! Signing a package tree (`docs/signing.md`, card S-1).
//!
//! A signature is over a short canonical statement, never an archive:
//!
//! ```text
//! islet-sign-v1
//! package: json/v2
//! version: 2.0.1
//! tree: <64 hex, the tree hash>
//! ```
//!
//! The first line separates this use of the key from every other. The tree hash already pins every file, length
//! and path, so the statement pins the whole package. The signature is Ed25519 (the audited `ed25519-dalek`; this
//! file is the only place that names it). The key's identity is `key:` and the BLAKE3 hash of its public key.
//!
//! A signature file is strict YAML like every other file, and carries the public key so it can be checked alone;
//! whether that key is *trusted* is decided by the pin in `islet.lock` (card S-2), not here.

use crate::blake3;
use crate::manifest::{is_dep_key, Version};
use crate::yaml::{self, Diag, PErr, Val};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

const DOMAIN: &str = "islet-sign-v1";

pub fn key_id(public: &[u8; 32]) -> String {
    format!("key:{}", blake3::to_hex(&blake3::hash(public)))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// A fresh secret key from the operating system's random source.
pub fn generate() -> Result<[u8; 32], String> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| format!("no system randomness: {e}"))?;
    Ok(secret)
}

pub fn public_of(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

/// Sign arbitrary bytes (the log's heads and witness cosignatures use this). Returns the public key and the
/// signature; callers add their own domain line to `msg` first.
pub fn sign_bytes(secret: &[u8; 32], msg: &[u8]) -> ([u8; 32], [u8; 64]) {
    let key = SigningKey::from_bytes(secret);
    (key.verifying_key().to_bytes(), key.sign(msg).to_bytes())
}

/// Check a signature over `msg` (strict: no malleable encodings, no small-order keys).
pub fn verify_bytes(public: &[u8; 32], msg: &[u8], signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(public)
        .and_then(|k| k.verify_strict(msg, &Signature::from_bytes(signature)))
        .is_ok()
}

/// The text that is signed. The fields are checked first so none can carry a newline or a different meaning.
pub fn statement(package: &str, version: &str, tree: &str) -> Result<String, String> {
    if !is_dep_key(package) {
        return Err(format!("`{package}` is not a package name"));
    }
    if Version::parse(version).is_none() {
        return Err(format!("`{version}` is not a version like 1.2.3"));
    }
    if blake3::from_hex(tree).is_none() || tree.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err("the tree hash must be 64 lowercase hex digits".into());
    }
    Ok(format!(
        "{DOMAIN}\npackage: {package}\nversion: {version}\ntree: {tree}\n"
    ))
}

/// One signature, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sig {
    pub package: String,
    pub version: String,
    pub tree: String,
    pub public: [u8; 32],
    pub signature: [u8; 64],
}

impl Sig {
    pub fn signer(&self) -> String {
        key_id(&self.public)
    }

    pub fn make(
        secret: &[u8; 32],
        package: &str,
        version: &str,
        tree: &str,
    ) -> Result<Sig, String> {
        let msg = statement(package, version, tree)?;
        let key = SigningKey::from_bytes(secret);
        let signature = key.sign(msg.as_bytes()).to_bytes();
        Ok(Sig {
            package: package.into(),
            version: version.into(),
            tree: tree.into(),
            public: key.verifying_key().to_bytes(),
            signature,
        })
    }

    /// Check the signature against its own statement (strict: no malleable encodings, no small-order keys).
    pub fn verify(&self) -> Result<(), String> {
        let msg = statement(&self.package, &self.version, &self.tree)?;
        let key =
            VerifyingKey::from_bytes(&self.public).map_err(|_| "the public key is not valid")?;
        key.verify_strict(msg.as_bytes(), &Signature::from_bytes(&self.signature))
            .map_err(|_| "the signature does not match".to_string())
    }

    pub fn to_text(&self) -> String {
        format!(
            "# islet signature: written by islet, do not edit.\nformat: 1\npackage: {}\nversion: {}\ntree: {}\nsigner: {}\npublic: {}\nsignature: {}\n",
            yaml::string(&self.package),
            yaml::string(&self.version),
            yaml::quoted(&self.tree),
            yaml::quoted(&self.signer()),
            yaml::quoted(&hex(&self.public)),
            yaml::quoted(&hex(&self.signature)),
        )
    }

    pub fn parse(file: &str, src: &str) -> Result<Sig, Diag> {
        Sig::read(src).map_err(|e| Diag::new(file, src, e))
    }

    fn read(src: &str) -> Result<Sig, PErr> {
        let root = yaml::parse(src)?;
        const KEYS: [&str; 7] = [
            "format",
            "package",
            "version",
            "tree",
            "signer",
            "public",
            "signature",
        ];
        for e in &root.entries {
            if !KEYS.contains(&e.key.as_str()) {
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
            None => return Err(PErr::new(0, "a signature needs `format: 1`")),
        }
        let text = |k: &str| -> Result<(String, usize), PErr> {
            match root.get(k) {
                Some(n) => match &n.v {
                    Val::Str(s) => Ok((s.clone(), n.at)),
                    _ => Err(PErr::new(n.at, format!("`{k}` must be text"))),
                },
                None => Err(PErr::new(0, format!("`{k}` is missing"))),
            }
        };
        let (package, _) = text("package")?;
        let (version, _) = text("version")?;
        let (tree, _) = text("tree")?;
        let (signer, signer_at) = text("signer")?;
        let (public, public_at) = text("public")?;
        let (signature, sig_at) = text("signature")?;
        let public: [u8; 32] = unhex(&public)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| PErr::new(public_at, "`public` must be 64 lowercase hex digits"))?;
        let signature: [u8; 64] = unhex(&signature)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| PErr::new(sig_at, "`signature` must be 128 lowercase hex digits"))?;
        if signer != key_id(&public) {
            return Err(PErr::new(signer_at, "`signer` is not the hash of `public`"));
        }
        Ok(Sig {
            package,
            version,
            tree,
            public,
            signature,
        })
    }
}

const ROTATE_DOMAIN: &str = "islet-rotate-v1";

/// A key rotation: the old key says "from now on this package is published by the new key". Signed by the old
/// key only, so a client that pinned the old key follows it without a prompt; losing the old key means a manual
/// `islet trust`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    pub package: String,
    pub old: String,
    pub new: String,
    /// The old key's public half, so the file can be checked alone.
    pub public: [u8; 32],
    pub signature: [u8; 64],
    /// The new key's co-signature of the same statement: proof that whoever rotates to a key holds it, so a typo
    /// or a stranger's key cannot be pinned by accident. A rotation without it is not followed.
    pub new_public: Option<[u8; 32]>,
    pub new_signature: Option<[u8; 64]>,
}

fn is_key_id(s: &str) -> bool {
    s.strip_prefix("key:").is_some_and(|h| {
        blake3::from_hex(h).is_some() && !h.bytes().any(|b| b.is_ascii_uppercase())
    })
}

pub fn rotation_statement(package: &str, old: &str, new: &str) -> Result<String, String> {
    if !is_dep_key(package) {
        return Err(format!("`{package}` is not a package name"));
    }
    if !is_key_id(old) || !is_key_id(new) {
        return Err("a key id is `key:` and 64 lowercase hex digits".into());
    }
    Ok(format!(
        "{ROTATE_DOMAIN}\npackage: {package}\nold: {old}\nnew: {new}\n"
    ))
}

impl Rotation {
    pub fn make(secret: &[u8; 32], package: &str, new: &str) -> Result<Rotation, String> {
        let key = SigningKey::from_bytes(secret);
        let public = key.verifying_key().to_bytes();
        let old = key_id(&public);
        let msg = rotation_statement(package, &old, new)?;
        Ok(Rotation {
            package: package.into(),
            old,
            new: new.into(),
            public,
            signature: key.sign(msg.as_bytes()).to_bytes(),
            new_public: None,
            new_signature: None,
        })
    }

    /// Add the new key's co-signature. The secret must belong to the key named in `new`.
    pub fn cosign(&mut self, new_secret: &[u8; 32]) -> Result<(), String> {
        let key = SigningKey::from_bytes(new_secret);
        let public = key.verifying_key().to_bytes();
        if key_id(&public) != self.new {
            return Err(format!(
                "that key is {}, not the new key {}",
                key_id(&public),
                self.new
            ));
        }
        let msg = rotation_statement(&self.package, &self.old, &self.new)?;
        self.new_public = Some(public);
        self.new_signature = Some(key.sign(msg.as_bytes()).to_bytes());
        Ok(())
    }

    /// Is the old key's signature good, and the new key's too if it is there?
    pub fn verify(&self) -> Result<(), String> {
        if self.old != key_id(&self.public) {
            return Err("`old` is not the hash of `public`".into());
        }
        let msg = rotation_statement(&self.package, &self.old, &self.new)?;
        let key =
            VerifyingKey::from_bytes(&self.public).map_err(|_| "the public key is not valid")?;
        key.verify_strict(msg.as_bytes(), &Signature::from_bytes(&self.signature))
            .map_err(|_| "the signature does not match".to_string())?;
        if let (Some(public), Some(signature)) = (&self.new_public, &self.new_signature) {
            if self.new != key_id(public) {
                return Err("`new` is not the hash of `new_public`".into());
            }
            let nk =
                VerifyingKey::from_bytes(public).map_err(|_| "the new public key is not valid")?;
            nk.verify_strict(msg.as_bytes(), &Signature::from_bytes(signature))
                .map_err(|_| "the new key's signature does not match".to_string())?;
        }
        Ok(())
    }

    /// Both keys have signed. Only a complete rotation is followed.
    pub fn verify_complete(&self) -> Result<(), String> {
        if self.new_public.is_none() || self.new_signature.is_none() {
            return Err("the new key has not co-signed (`islet rotate --cosign`)".into());
        }
        self.verify()
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "# islet key rotation: written by islet, do not edit.\nformat: 1\npackage: {}\nold: {}\nnew: {}\npublic: {}\nsignature: {}\n",
            yaml::string(&self.package),
            yaml::quoted(&self.old),
            yaml::quoted(&self.new),
            yaml::quoted(&hex(&self.public)),
            yaml::quoted(&hex(&self.signature)),
        );
        if let (Some(p), Some(s)) = (&self.new_public, &self.new_signature) {
            out.push_str(&format!(
                "new_public: {}\nnew_signature: {}\n",
                yaml::quoted(&hex(p)),
                yaml::quoted(&hex(s))
            ));
        }
        out
    }

    pub fn parse(file: &str, src: &str) -> Result<Rotation, Diag> {
        Rotation::read(src).map_err(|e| Diag::new(file, src, e))
    }

    fn read(src: &str) -> Result<Rotation, PErr> {
        let root = yaml::parse(src)?;
        const KEYS: [&str; 8] = [
            "format",
            "package",
            "old",
            "new",
            "public",
            "signature",
            "new_public",
            "new_signature",
        ];
        for e in &root.entries {
            if !KEYS.contains(&e.key.as_str()) {
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
            None => return Err(PErr::new(0, "a rotation needs `format: 1`")),
        }
        let text = |k: &str| -> Result<(String, usize), PErr> {
            match root.get(k) {
                Some(n) => match &n.v {
                    Val::Str(s) => Ok((s.clone(), n.at)),
                    _ => Err(PErr::new(n.at, format!("`{k}` must be text"))),
                },
                None => Err(PErr::new(0, format!("`{k}` is missing"))),
            }
        };
        let (package, _) = text("package")?;
        let (old, old_at) = text("old")?;
        let (new, new_at) = text("new")?;
        let (public, public_at) = text("public")?;
        let (signature, sig_at) = text("signature")?;
        if !is_key_id(&old) {
            return Err(PErr::new(old_at, "`old` must be a key id"));
        }
        if !is_key_id(&new) {
            return Err(PErr::new(new_at, "`new` must be a key id"));
        }
        let public: [u8; 32] = unhex(&public)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| PErr::new(public_at, "`public` must be 64 lowercase hex digits"))?;
        let signature: [u8; 64] = unhex(&signature)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| PErr::new(sig_at, "`signature` must be 128 lowercase hex digits"))?;
        let optional = |k: &str, n: usize| -> Result<Option<Vec<u8>>, PErr> {
            match root.get(k) {
                None => Ok(None),
                Some(node) => match &node.v {
                    Val::Str(s) => unhex(s).filter(|v| v.len() == n).map(Some).ok_or_else(|| {
                        PErr::new(
                            node.at,
                            format!("`{k}` must be {} lowercase hex digits", 2 * n),
                        )
                    }),
                    _ => Err(PErr::new(node.at, format!("`{k}` must be text"))),
                },
            }
        };
        let new_public = optional("new_public", 32)?;
        let new_signature = optional("new_signature", 64)?;
        if new_public.is_some() != new_signature.is_some() {
            return Err(PErr::new(0, "`new_public` and `new_signature` go together"));
        }
        Ok(Rotation {
            package,
            old,
            new,
            public,
            signature,
            new_public: new_public.map(|v| v.try_into().unwrap()),
            new_signature: new_signature.map(|v| v.try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "2708a2eef28076c39dd92a656d62e0259a7011ad7d7e4e6dcddff60474e936aa";

    /// RFC 8032, section 7.1, test 1: the empty message.
    #[test]
    fn the_crate_matches_rfc_8032() {
        let secret: [u8; 32] =
            unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
                .unwrap()
                .try_into()
                .unwrap();
        let key = SigningKey::from_bytes(&secret);
        assert_eq!(
            hex(&key.verifying_key().to_bytes()),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(
            hex(&key.sign(b"").to_bytes()),
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        );
    }

    #[test]
    fn the_statement_is_exact_and_refuses_odd_fields() {
        assert_eq!(
            statement("json/v2", "2.0.1", TREE).unwrap(),
            format!("islet-sign-v1\npackage: json/v2\nversion: 2.0.1\ntree: {TREE}\n")
        );
        assert!(statement("a\nb", "1.0.0", TREE).is_err());
        assert!(statement("json", "1.0", TREE).is_err());
        assert!(statement("json", "1.0.0", "abc").is_err());
        assert!(statement("json", "1.0.0", &TREE.to_uppercase()).is_err());
    }

    #[test]
    fn sign_verify_and_text_round_trip() {
        let secret = generate().unwrap();
        let sig = Sig::make(&secret, "json", "1.5.0", TREE).unwrap();
        assert_eq!(sig.verify(), Ok(()));
        assert_eq!(sig.signer(), key_id(&public_of(&secret)));
        let text = sig.to_text();
        let back = Sig::parse("x", &text).unwrap();
        assert_eq!(back, sig);
        assert_eq!(back.verify(), Ok(()));
    }

    #[test]
    fn any_change_breaks_the_signature() {
        let secret = generate().unwrap();
        let sig = Sig::make(&secret, "json", "1.5.0", TREE).unwrap();
        let mut a = sig.clone();
        a.version = "1.5.1".into();
        assert!(a.verify().is_err());
        let mut b = sig.clone();
        b.package = "jsom".into();
        assert!(b.verify().is_err());
        let mut c = sig.clone();
        c.tree = "0".repeat(64);
        assert!(c.verify().is_err());
        let mut d = sig.clone();
        d.signature[0] ^= 1;
        assert!(d.verify().is_err());
        // Another key's signature on the same statement does not verify under this key.
        let other = Sig::make(&generate().unwrap(), "json", "1.5.0", TREE).unwrap();
        let mut e = sig.clone();
        e.signature = other.signature;
        assert!(e.verify().is_err());
    }

    #[test]
    fn a_bad_file_says_where() {
        let secret = generate().unwrap();
        let text = Sig::make(&secret, "json", "1.5.0", TREE).unwrap().to_text();
        let e = Sig::parse("s", &text.replace("format: 1", "format: 2")).unwrap_err();
        assert!(e.msg.contains("unsupported"), "{e}");
        let e = Sig::parse("s", &text.replace("signer: \"key:", "signer: \"key:0")).unwrap_err();
        assert!(e.msg.contains("not the hash"), "{e}");
        let e = Sig::parse("s", &format!("{text}extra: 1\n")).unwrap_err();
        assert!(e.msg.contains("unknown key"), "{e}");
    }

    #[test]
    fn a_changed_file_changes_the_tree_and_so_the_statement() {
        let d = std::env::temp_dir().join(format!("islet-sign-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.pel"), "one").unwrap();
        let before = crate::store::tree_hash(&d).unwrap();
        let sig = Sig::make(&generate().unwrap(), "json", "1.0.0", &before).unwrap();
        std::fs::write(d.join("a.pel"), "two").unwrap();
        let after = crate::store::tree_hash(&d).unwrap();
        assert_ne!(before, after);
        // The signature still holds for the old tree and says nothing about the new one.
        assert_eq!(sig.verify(), Ok(()));
        assert_ne!(sig.tree, after);
    }

    #[test]
    fn a_rotation_is_signed_by_the_old_key_and_holds_only_for_its_statement() {
        let old = generate().unwrap();
        let new_id = key_id(&public_of(&generate().unwrap()));
        let r = Rotation::make(&old, "json", &new_id).unwrap();
        assert_eq!(r.old, key_id(&public_of(&old)));
        assert_eq!(r.verify(), Ok(()));
        assert_eq!(Rotation::parse("r", &r.to_text()).unwrap(), r);
        let mut other_new = r.clone();
        other_new.new = key_id(&public_of(&generate().unwrap()));
        assert!(other_new.verify().is_err());
        let mut other_pkg = r.clone();
        other_pkg.package = "yaml".into();
        assert!(other_pkg.verify().is_err());
        // A package signature never passes as a rotation: the domain lines differ.
        let sig = Sig::make(&old, "json", "1.0.0", TREE).unwrap();
        let mut forged = r.clone();
        forged.signature = sig.signature;
        assert!(forged.verify().is_err());
        assert!(Rotation::make(&old, "json", "key:zz").is_err());
    }

    #[test]
    fn the_new_key_cosigns_and_a_forged_cosignature_fails() {
        let (old, new) = (generate().unwrap(), generate().unwrap());
        let new_id = key_id(&public_of(&new));
        let mut r = Rotation::make(&old, "json", &new_id).unwrap();
        assert!(r.verify_complete().is_err());
        r.cosign(&new).unwrap();
        assert_eq!(r.verify_complete(), Ok(()));
        let back = Rotation::parse("r", &r.to_text()).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.verify_complete(), Ok(()));
        // A co-signature by some other key, or over some other package, does not hold.
        let mut bad = r.clone();
        bad.new_signature = Some(Rotation::make(&old, "json", &new_id).unwrap().signature);
        assert!(bad.verify().is_err());
        let mut other_pkg = r.clone();
        other_pkg.package = "yaml".into();
        assert!(other_pkg.verify().is_err());
        // One half of the pair is a file error.
        let text = r.to_text().replace("new_public:", "x_public:");
        assert!(Rotation::parse("r", &text).is_err());
    }
}
