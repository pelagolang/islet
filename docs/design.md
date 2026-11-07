# Islet design decisions

Decided by Dennis in the ecosystem brainstorm (2026-11-07). Do not reopen without the owner. Where a decision is
conventional we say so, because Islet is meant to have Pelago's own voice.

## Decisions

| Area | Decision | Conventional or Pelago-specific |
|---|---|---|
| Name | Islet, command `islet`. Sub-tools take sea and island names, not ship parts. | Pelago-specific |
| Packages | Source, read by the compiler next to the root project. No linker. | Follows from whole-program builds |
| Hash | BLAKE3 for all content addresses. | Conventional choice, fast |
| Store | pnpm-style global content-addressed store. One store per volume, in `.islet` at the drive root. Nothing on C: by default. A project uses the store on its own volume. Projects link into it (symlinks, junctions on Windows). | Store-and-link is pnpm's idea; the per-volume rule and the whole-program source model are ours |
| Manifests | `islet.toml` (identity, dependencies, workspace, root-only generators, C link inputs, publish metadata) and `pelago.toml` (compiler: opt, target, `[fmt]`, lints, tests, editor). One owner per key. Another file may reference a key with `{ from = "islet" }`, never copy it. | Pelago-specific |
| Format key | Each file has its own `format = 1`. | Conventional |
| Lock | `islet.lock`, TOML, committed, one canonical sorted form. | Conventional shape |
| Versions | Major version in the import path (`"json/v2"`). Minimal version selection. `islet update` diffs the public API with the compiler's checker and says whether a bump is safe. | Path majors and MVS are Go's; the compiler-checked API diff is ours |
| Build scripts | None. Generators run only through `islet regen` in the root project; output is hashed, stored by generator hash plus input hashes, and shipped by packages. Dependencies never declare generators. | Pelago-specific; the generator contract is still open (see plan) |
| Signing | Required from day one. A transparency-log design built for a very large index. | Conventional (same family as Go's checksum database and Sigstore); say so in every report |
| Finding packages | `deps/` at the project root (visible) holds one link per dependency into the store; the compiler follows the links with no resolver or network. Each link targets a store folder named by its BLAKE3 hash, and store folders are read-only. `deps/` is only a cache of `islet.lock`: if it is missing or stale and the store has every hash in the lock, the compiler recreates the links itself (read the lock, link; no resolving, no fetching). A missing hash sends the user to `islet install` and names it. A build refuses to run when a link and the lock disagree. `.islet` is only the per-volume store. | Pelago-specific combination (link folder like pnpm's `node_modules`; self-verifying and self-healing from the lock are ours) |
| Schemas | One JSON Schema per file, published on GitHub Pages from the website repo (`https://pelagolang.github.io/website/schema/...`), embedded in the tools, referenced by a `#:schema` line. | Conventional |

## Still open

- Registry and index design at scale (index growth, search as a rebuildable derived service).
- Version resolution beyond minimal version selection.
- A codegen approach that keeps working without turning into a build-script system. Dennis rejected a sandbox as
  uncreative. Current direction: generators are Pelago functions with no ambient authority, and foreign tools run only
  on request through `islet regen`, with output pinned in the lock file.
- Implementation language and dependencies (see `docs/plan.md`).
