# Signing and the transparency log

Status: decided by the owner on 2026-11-10 ("go with your suggestions"; private keys in the OS keychain with a
file fallback). Built in stages, see section 5. It builds on the decisions in `docs/design.md` (signing required from
day one, designed for a very large index, BLAKE3).

Conventional or Pelago-specific, as the standing rule asks: the primitives (Ed25519, an append-only Merkle log, signed
tree heads, witnesses) are conventional and come from the same family as Go's checksum database and Sigstore. What
is Pelago's own: signing the *tree hash* (so a signature covers exactly what the store addresses, with no archive
format to attack), pinning a publisher per package in `islet.lock`, and the stage plan that lets all of it work offline.

## 1. What a signature says

A signature is over a short canonical statement, never over an archive:

```
islet-sign-v1
package: json/v2
version: 2.0.1
tree: <64 hex, the tree hash of the package>
```

The first line is a domain separator, so a signature made here can never be replayed as something else. The tree hash
already pins every file, its length and its path (and, since 2026-11-10, normalised line endings), so the statement
pins the whole package. There is no tarball and no compression to get wrong: what is signed is what the store
addresses and what the compiler reads.

The statement is written in the same strict YAML subset as every other file. It is stored as `<tree hash>.sig`
beside the tree in the store, and published next to the package.

## 2. Keys and identity

- **Algorithm:** Ed25519 (fast, small, no parameter choices, widely implemented). Keys are 32 bytes.
- **Key id:** `BLAKE3(public key)` in 64 hex digits, written `key:<hex>`. An id is a fingerprint, so there is no
  name registry to trust and nothing to squat.
- **Where private keys live (owner's decision):** the operating system's key store first, a key file when there is
  none. macOS: the Keychain (`security`). Linux: the Secret Service (`secret-tool`). Windows: the key is encrypted with
  the user's DPAPI key (PowerShell) and the ciphertext is kept in `<store root>/keys/`. If the tool is missing or
  refuses, the key goes to `<store root>/keys/<id>.key`, readable by its owner only, and `islet key gen` says which
  one it used. Islet calls these system tools; it links no keychain library. `ISLET_KEY_BACKEND=file` forces the file. Known limit: macOS's `security` tool takes the secret as an argument, so
  for a moment it is visible to other processes of the same user; the other systems pass it on standard input.
- **Publisher:** a package's `islet.yaml` may name its signing key (`publisher: key:<hex>`); the first release fixes it.

## 3. Trust: pin the publisher, do not trust a name

`islet.lock` gets one more field per package, `signer: key:<hex>`. The rules:

1. **First use pins.** When `islet add` or `islet lock` first sees a package, it verifies the signature, records the
   signer in the lock, and shows the key id so a human can compare it.
2. **A different signer is an error.** A new release signed by another key is refused until `islet trust <package>
   <key id>` is run, and the lock diff shows it in review. This catches a hijacked account (the attacker does not
   hold the old key) the way a changed SSH host key does.
3. **Rotation is signed by both keys.** A rotation statement (`islet-rotate-v1`, package, old id, new id) signed with
   the old key and co-signed by the new one is accepted without a prompt. Loss of the old key means a manual `islet trust`.
4. **Offline is the normal case.** Verification needs only the public key and the signature. `install` re-verifies
   from the store and needs no network.

A signature alone does not show that everyone sees the same package list; that is the log's job.

### 3a. How S-2 works on disk

- `islet lock` reads the existing `islet.lock` as the previous pins, then for each package looks for valid
  signatures in `<store>/sigs/<tree hash>/` and in the folder index next to the release
  (`<index>/<key>/<version>.sigs/`). A file that does not parse, names another package, version or tree, or does not
  verify is ignored.
- One valid signer and no pin: pinned, and the key id is printed. Pinned and the pin signed it: kept. Pinned and only
  another key signed it, or nobody did: an error naming both keys. Two valid signers and no pin: an error asking for
  `islet trust`. `--require-signed` makes an unsigned package an error too. Unsigned packages are allowed by default
  until a registry exists to enforce it.
- `islet trust <dir> <package> <key id>` rewrites the pin for every locked version of the package; the next
  `islet lock` checks that the key really signs them.
- `islet rotate <package> --old <id> --new <id>` signs the statement
  `islet-rotate-v1 / package / old / new` with the old key and stores it in `<store>/rotations/<key>/` (or `--out`);
  rotations are also read from `<index>/<key>/rotations/`. `lock` follows a chain of them from the pin.
- `islet install` requires a valid signature by the pinned signer, copies the index's signatures into the store, and
  from then on checks the store alone, so it works offline.
- `publisher: key:<hex>` in the `package` table of `islet.yaml` names the key that signs the release. When it is
  there, only that key's signature counts: it must have signed this release, it is what first use pins, and a later
  release that names another key is refused unless a rotation from the pinned key reaches it.
- A rotation needs both keys. `islet rotate` has the old key sign and, when the new key is on the same machine, the
  new key co-sign. Otherwise it says so, and `islet rotate --cosign <file> --key <new key id>` finishes it where the
  new key lives. `islet lock` follows only complete rotations, so a typo or a stranger's key cannot be pinned by a
  rotation. (A key lost entirely still means a manual `islet trust`.)

## 4. The transparency log

The registry publishes an append-only Merkle log of every `(package, version, tree hash, signer)` entry. The design
goal is that a client with a hash and a signature can prove, without trusting the registry, that the entry is in the
log everyone sees, and that the log only ever grew.

- **Tile layout.** The log is served as static, cacheable *tiles* (the layout of Go's checksum database and the
  C2SP `tlog-tiles` format), so a CDN serves a billion clients and a client downloads a few kilobytes per lookup.
- **Signed tree head.** The registry signs the size and root hash of the log. A client keeps the last head it saw
  and asks for a *consistency proof* each time it sees a newer one; a log that rewrites history is caught.
- **Inclusion proof.** `islet lock` fetches the proof for the entry it is about to pin and stores nothing in the lock
  except the signer; the proof is checked once, at lock time, then the store holds the signed statement.
- **Witnesses.** Independent parties cosign tree heads. A client may require N of M cosignatures before it accepts
  a head. This stops a registry from showing two clients two different logs (a split view). The first witnesses can
  be run by us and by volunteers; the protocol does not care who.
- **Sharding for scale.** The log is sharded by the first bits of `BLAKE3(package name)`, each shard its own log, so
  no single log or machine has to hold or serve the whole index. The tile format already supports this.
- **Yanks and revocation.** A yank is a new log entry, never a deletion. A compromised key is revoked by an entry
  naming the key id; clients refuse to pin a revoked key and warn on one already pinned.

## 5. Stages

| Card | What | Done when |
|---|---|---|
| S-1 | `islet key gen`, `islet sign <dir>`, `islet verify <dir>`; the statement format; `.sig` in the store | A tree is signed, verified, and a changed file or a changed version makes verification fail. **Built** (signatures live in `<store>/sigs/<tree hash>/<signer>.sig`, outside the tree, since a signature inside it would change its hash). |
| S-2 | The signer pin in `islet.lock` (with schema and example), `islet trust`, rotation statements | A lock records the signer; a different signer is refused; rotation by the old key is accepted. **Built** (see section 3a). |
| S-3 | Log formats: tiles, signed tree heads, inclusion and consistency proofs, witness cosignatures, as a library with test vectors | Proofs verify against a log built in the tests, and tampering with any tile or head is caught. |
| S-4 | A local test registry that serves tiles, and `islet` fetching with proof checks (this is card I-5) | A package is published, fetched, verified and rejected when tampered with. |

S-1 and S-2 do not need a network or a registry, so they come first and are useful on their own (a monorepo can sign
internal packages today).

## 6. Decisions (2026-11-10)

1. **Ed25519:** `ed25519-dalek`, one audited crate, isolated in `src/sign.rs`. Random bytes for key generation come
   from the operating system through the `getrandom` crate.
2. **Trust model:** pin the signer in the lock on first use, with signed rotation.
3. **Key id:** the BLAKE3 fingerprint of the public key.
4. **Private keys:** the OS key store, falling back to a file (section 2). No passphrase in S-1.
5. **Witnesses:** the cosignature check is part of S-3, so the head format has it from the start.
6. **The registry's name and the first witnesses** belong to the registry card, not decided here.
