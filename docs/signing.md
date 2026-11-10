# Signing and the transparency log

Status: draft for the owner. It builds on the decisions in `docs/design.md` (signing required from day one, designed
for a very large index, BLAKE3). Nothing here is implemented yet. Questions are at the end, with a recommendation on
each.

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
- **Where private keys live:** `<store root>/keys/<id>.key`, owner-only, with an optional passphrase. The system
  keychain (Windows Credential Manager, macOS Keychain, Secret Service) comes later as an option, not a rule.
- **Publisher:** a package's `islet.yaml` may name its signing key (`publisher: key:<hex>`); the first release fixes it.

## 3. Trust: pin the publisher, do not trust a name

`islet.lock` gets one more field per package, `signer: key:<hex>`. The rules:

1. **First use pins.** When `islet add` or `islet lock` first sees a package, it verifies the signature, records the
   signer in the lock, and shows the key id so a human can compare it.
2. **A different signer is an error.** A new release signed by another key is refused until `islet trust <package>
   <key id>` is run, and the lock diff shows it in review. This catches a hijacked account (the attacker does not
   hold the old key) the way a changed SSH host key does.
3. **Rotation is signed by the old key.** A rotation statement (`islet-rotate-v1`, old id, new id) signed with the
   old key is accepted without a prompt. Loss of the old key means a manual `islet trust`.
4. **Offline is the normal case.** Verification needs only the public key and the signature. `install` re-verifies
   from the store and needs no network.

A signature alone does not show that everyone sees the same package list; that is the log's job.

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
| S-1 | `islet key gen`, `islet sign <dir>`, `islet verify <dir>`; the statement format; `.sig` in the store | A tree is signed, verified, and a changed file or a changed version makes verification fail. |
| S-2 | The signer pin in `islet.lock` (with schema and example), `islet trust`, rotation statements | A lock records the signer; a different signer is refused; rotation by the old key is accepted. |
| S-3 | Log formats: tiles, signed tree heads, inclusion and consistency proofs, witness cosignatures, as a library with test vectors | Proofs verify against a log built in the tests, and tampering with any tile or head is caught. |
| S-4 | A local test registry that serves tiles, and `islet` fetching with proof checks (this is card I-5) | A package is published, fetched, verified and rejected when tampered with. |

S-1 and S-2 do not need a network or a registry, so they come first and are useful on their own (a monorepo can sign
internal packages today).

## 6. Questions

1. **Implementation of Ed25519** (card posted in the thread): recommended `ed25519-dalek`, one audited crate, isolated
   in `src/sign.rs`. The signing key is read from a file, so the only other need is random bytes at key generation,
   for which I would use the operating system's generator through the `getrandom` crate (a second tiny dependency)
   or, if you prefer, read the system source directly with a few lines per operating system.
2. **Trust model:** pin the signer in the lock on first use, with signed rotation (section 3). Recommended. The
   alternative is a central list of trusted publishers, which does not scale to a billion users.
3. **Key id format:** the BLAKE3 fingerprint of the public key (section 2). Recommended over a name or an email.
4. **Passphrase on private keys:** optional in S-1, with the system keychain as a later option. Recommended; a
   mandatory passphrase breaks CI.
5. **Witnesses:** build the cosignature check into S-3 now, so the log does not have a single point of trust later.
   Recommended, since retrofitting it changes the head format.
6. **Where the registry lives:** a name for the service and who runs the first witnesses are not decided here; they
   belong to the registry card.
