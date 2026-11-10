//! Where private signing keys are kept (owner's decision, 2026-11-10): the operating system's key store first, a
//! key file when there is none.
//!
//! - macOS: the Keychain, through the `security` tool.
//! - Linux: the Secret Service, through `secret-tool` (libsecret).
//! - Windows: the key is encrypted with the user's DPAPI key (PowerShell) and the ciphertext is kept in
//!   `<root>/keys/<id>.dpapi`; only that user on that machine can decrypt it.
//! - Anything else, or a tool that is missing or refuses: `<root>/keys/<id>.key`, readable by its owner only.
//!
//! Islet runs these system tools and links no keychain library. The secret goes to the tool on standard input
//! wherever the tool allows it. The exception is `security` on macOS, which only takes the secret as an argument,
//! so it is visible to other processes of the same user for a moment while it runs.
//!
//! The public key is always kept in `<root>/keys/<id>.pub`, so keys can be listed on every system.

use crate::sign::{hex, key_id, public_of, unhex};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefer {
    /// The system key store, then the file.
    Auto,
    /// Only the file.
    File,
}

/// Each system store is built only on its own platform, so some variants are never made on another one.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    Secret,
    Dpapi,
    File,
}

impl Backend {
    pub fn describe(self) -> &'static str {
        match self {
            Backend::Keychain => "the macOS Keychain",
            Backend::Secret => "the Secret Service (secret-tool)",
            Backend::Dpapi => "Windows DPAPI (encrypted for this user)",
            Backend::File => "a key file (owner-only)",
        }
    }
}

fn dir(root: &Path) -> PathBuf {
    root.join("keys")
}

fn run(mut cmd: Command, stdin: Option<&str>) -> Option<String> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let Some(text) = stdin {
        child.stdin.take()?.write_all(text.as_bytes()).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

// ---- native stores ---------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn native_store(_root: &Path, id: &str, secret_hex: &str) -> Option<Backend> {
    let mut c = Command::new("security");
    c.args([
        "add-generic-password",
        "-U",
        "-a",
        id,
        "-s",
        "islet",
        "-w",
        secret_hex,
    ]);
    run(c, None).map(|_| Backend::Keychain)
}

#[cfg(target_os = "macos")]
fn native_load(_root: &Path, id: &str) -> Option<String> {
    let mut c = Command::new("security");
    c.args(["find-generic-password", "-a", id, "-s", "islet", "-w"]);
    run(c, None)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_store(_root: &Path, id: &str, secret_hex: &str) -> Option<Backend> {
    let mut c = Command::new("secret-tool");
    c.args([
        "store",
        "--label",
        &format!("islet signing key {id}"),
        "service",
        "islet",
        "key",
        id,
    ]);
    run(c, Some(secret_hex)).map(|_| Backend::Secret)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_load(_root: &Path, id: &str) -> Option<String> {
    let mut c = Command::new("secret-tool");
    c.args(["lookup", "service", "islet", "key", id]);
    run(c, None)
}

#[cfg(windows)]
const PS_PROTECT: &str = "Add-Type -AssemblyName System.Security; $s=[Console]::In.ReadToEnd().Trim(); \
    $p=[Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($s),$null,'CurrentUser'); \
    [Convert]::ToBase64String($p)";

#[cfg(windows)]
const PS_UNPROTECT: &str = "Add-Type -AssemblyName System.Security; $b=[Convert]::FromBase64String([Console]::In.ReadToEnd().Trim()); \
    $u=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,'CurrentUser'); \
    [Text.Encoding]::UTF8.GetString($u)";

#[cfg(windows)]
fn powershell(script: &str, stdin: &str) -> Option<String> {
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    run(c, Some(stdin))
}

#[cfg(windows)]
fn native_store(root: &Path, id: &str, secret_hex: &str) -> Option<Backend> {
    let blob = powershell(PS_PROTECT, secret_hex)?;
    let blob = blob.trim();
    if blob.is_empty() {
        return None;
    }
    fs::write(dir(root).join(format!("{}.dpapi", file_stem(id))), blob).ok()?;
    Some(Backend::Dpapi)
}

#[cfg(windows)]
fn native_load(root: &Path, id: &str) -> Option<String> {
    let blob = fs::read_to_string(dir(root).join(format!("{}.dpapi", file_stem(id)))).ok()?;
    powershell(PS_UNPROTECT, &blob)
}

#[cfg(not(any(unix, windows)))]
fn native_store(_root: &Path, _id: &str, _secret_hex: &str) -> Option<Backend> {
    None
}

#[cfg(not(any(unix, windows)))]
fn native_load(_root: &Path, _id: &str) -> Option<String> {
    None
}

// ---- files -----------------------------------------------------------------------------------------------

/// `key:<hex>` as a file name part (a colon is not allowed in a Windows file name).
fn file_stem(id: &str) -> String {
    id.replace(':', "-")
}

#[cfg(unix)]
fn owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn owner_only(_path: &Path) -> std::io::Result<()> {
    // The store lives in the user's own profile, whose folder permissions already limit it to that user.
    Ok(())
}

fn file_store(root: &Path, id: &str, secret_hex: &str) -> Result<Backend, String> {
    let path = dir(root).join(format!("{}.key", file_stem(id)));
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
    };
    #[cfg(not(unix))]
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    let mut f = created.map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(format!("{secret_hex}\n").as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    owner_only(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Backend::File)
}

fn parse_secret(text: &str) -> Option<[u8; 32]> {
    unhex(text.trim())?.try_into().ok()
}

// ---- the interface ---------------------------------------------------------------------------------------

/// Keep `secret` and its public key; returns the id and where the secret went.
pub fn store(root: &Path, secret: &[u8; 32], prefer: Prefer) -> Result<(String, Backend), String> {
    let public = public_of(secret);
    let id = key_id(&public);
    let d = dir(root);
    fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let pub_path = d.join(format!("{}.pub", file_stem(&id)));
    if pub_path.exists() {
        return Err(format!("{id} already exists"));
    }
    let secret_hex = hex(secret);
    let backend = match prefer {
        Prefer::Auto => match native_store(root, &id, &secret_hex) {
            Some(b) => b,
            None => file_store(root, &id, &secret_hex)?,
        },
        Prefer::File => file_store(root, &id, &secret_hex)?,
    };
    fs::write(&pub_path, format!("{}\n", hex(&public)))
        .map_err(|e| format!("{}: {e}", pub_path.display()))?;
    Ok((id, backend))
}

/// The secret for `id`: the system store if it has it, then the file.
pub fn load(root: &Path, id: &str) -> Result<[u8; 32], String> {
    let secret = native_load(root, id)
        .and_then(|t| parse_secret(&t))
        .or_else(|| {
            fs::read_to_string(dir(root).join(format!("{}.key", file_stem(id))))
                .ok()
                .and_then(|t| parse_secret(&t))
        })
        .ok_or_else(|| {
            format!(
                "no private key for {id} (the system key store and {} were tried)",
                dir(root).display()
            )
        })?;
    if key_id(&public_of(&secret)) != id {
        return Err(format!("the key stored for {id} does not match it"));
    }
    Ok(secret)
}

/// Every known key id with its public key, sorted.
pub fn list(root: &Path) -> Result<Vec<(String, [u8; 32])>, String> {
    let d = dir(root);
    let mut out = Vec::new();
    if !d.is_dir() {
        return Ok(out);
    }
    for e in fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".pub") {
            if let Some(public) = fs::read_to_string(e.path())
                .ok()
                .and_then(|t| parse_secret(&t))
            {
                let id = stem.replacen('-', ":", 1);
                if key_id(&public) == id {
                    out.push((id, public));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::generate;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("islet-keys-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_key_file_round_trips_and_is_owner_only() {
        let root = scratch("file");
        let secret = generate().unwrap();
        let (id, backend) = store(&root, &secret, Prefer::File).unwrap();
        assert_eq!(backend, Backend::File);
        assert_eq!(load(&root, &id).unwrap(), secret);
        assert_eq!(list(&root).unwrap(), vec![(id.clone(), public_of(&secret))]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir(&root).join(format!("{}.key", file_stem(&id))))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The same key is never stored twice.
        assert!(store(&root, &secret, Prefer::File).is_err());
        // An unknown key says what was tried.
        let e = load(&root, &key_id(&[7u8; 32])).unwrap_err();
        assert!(e.contains("no private key"), "{e}");
    }

    #[test]
    fn a_swapped_key_file_is_refused() {
        let root = scratch("swap");
        let a = generate().unwrap();
        let b = generate().unwrap();
        let (id_a, _) = store(&root, &a, Prefer::File).unwrap();
        let path = dir(&root).join(format!("{}.key", file_stem(&id_a)));
        fs::write(&path, format!("{}\n", hex(&b))).unwrap();
        // Auto may find a system-store entry for nothing; the file holds the wrong key either way.
        assert!(load(&root, &id_a).unwrap_err().contains("does not match"));
    }

    /// Windows has a system store that needs no setup, so it can run here for real.
    #[cfg(windows)]
    #[test]
    fn dpapi_round_trips() {
        let root = scratch("dpapi");
        let secret = generate().unwrap();
        let (id, backend) = store(&root, &secret, Prefer::Auto).unwrap();
        assert_eq!(backend, Backend::Dpapi);
        assert_eq!(load(&root, &id).unwrap(), secret);
    }
}
