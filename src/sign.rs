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
}
