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
| Manifests | `islet.toml` (identity, dependencies, root-only generators, C link inputs, publish metadata), `monorepo.toml` (see Monorepos) and `pelago.toml` (compiler: opt, target, `[fmt]`, lints, tests, editor). One owner per key. Another file may reference a key with `{ from = "islet" }`, never copy it. | Pelago-specific |
| Format key | Each file has its own `format = 1`. | Conventional |
| Lock | `islet.lock`, TOML, committed, one canonical sorted form: `format = 1`, then one `[[package]]` per resolved package with `name` (the key as written, so `json/v2` for a second major), `version`, `hash` (the store address) and, when not empty, `dependencies` (sorted `name@version` strings, each a package of the same lock). Packages sort by name then version. `islet lock fmt` rewrites a file into this form and `islet check` refuses any other. Proposed in I-2; not yet confirmed by the owner. | Conventional shape |
| Versions | Major version in the import path (`"json/v2"`). Minimal version selection. `islet update` diffs the public API with the compiler's checker and says whether a bump is safe. | Path majors and MVS are Go's; the compiler-checked API diff is ours |
| Build scripts | None. Generators run only through `islet regen` in the root project; output is hashed, stored by generator hash plus input hashes, and shipped by packages. Dependencies never declare generators. | Pelago-specific; the generator contract is still open (see plan) |
| Signing | Required from day one. A transparency-log design built for a very large index. | Conventional (same family as Go's checksum database and Sigstore); say so in every report |
| Finding packages | `deps/` at the project root (visible) holds one link per dependency into the store; the compiler follows the links with no resolver or network. Each link targets a store folder named by its BLAKE3 hash, and store folders are read-only. `deps/` is only a cache of `islet.lock`: if it is missing or stale and the store has every hash in the lock, the compiler recreates the links itself (read the lock, link; no resolving, no fetching). A missing hash sends the user to `islet install` and names it. A build refuses to run when a link and the lock disagree. `.islet` is only the per-volume store. | Pelago-specific combination (link folder like pnpm's `node_modules`; self-verifying and self-healing from the lock are ours) |
| Schemas | One JSON Schema per file, published on GitHub Pages from the website repo (`https://pelagolang.github.io/website/schema/...`), embedded in the tools, referenced by a `#:schema` line. | Conventional |

## Monorepos

Decided with Dennis on 2026-11-07. There is no special name: it is a monorepo.

| Area | Decision | Conventional or Pelago-specific |
|---|---|---|
| Root file | `monorepo.toml` at the repo root, with its own `format = 1` and schema. It owns the member list, the shared dependency table (our catalogs), the exceptions and the cache settings. Members keep their own `islet.toml` and point at the shared table with `{ from = "monorepo" }`, never copying. The root need not be a package itself. | Separate workspace file as in `pnpm-workspace.yaml`; reference-not-copy is ours |
| One `deps/` | One `deps/` and one `islet.lock` at the root. | Conventional |
| One version | The workspace resolves one version per package and major (minimal version selection already gives one answer). When `install` or `update` unifies a version, the compiler type-checks every member against it. A failure names the member and symbol; the fix is to pin the workspace lower or add an `[[exception]]` with a reason. An exception keeps an exact version in the same root `deps/` (`deps/json@1.2.0` next to `deps/json`), and the lock records which member uses which. | Unify-then-prove is ours; it rests on the compiler-checked API diff |
| Two hashes | Each member has a source hash (its files plus its dependencies' source hashes) and an API hash (everything downstream code can observe at compile time: exported names, signatures, type layouts, access modes, public constants, bodies of generic functions and `meta` code; anything uncertain is included). `check`, `doc` and lints are keyed by own source hash plus dependencies' API hashes, compiler version and flags. `test` and `build` are keyed by the source hashes of the whole closure. | API-hash invalidation is ours; turborepo only sees files |
| Task cache | Results live in the per-volume store, keyed by those hashes, compiler version, target and flags. A shared remote cache uses the same content-addressed protocol and the same signing as packages. Trust signatures; `islet cache verify` recomputes a sample, and CI can run with the API shortcut off. | Remote cache is conventional; one trust model with packages is ours |
| One compiler process | A monorepo is checked in one compiler process that shares parsed and checked files across members, not one process per package. Needs the compiler to check several packages in one run (agree with the compiler owner before I-3). | Pelago-specific |
| `islet affected` | From a git diff, names the members to re-check (API hashes) and re-test (source hashes). Later: re-run only tests that use a changed symbol, which needs a per-test symbol list from the compiler. | Affected-by-diff is conventional; by symbol is ours |
| Large repos | Each member keeps a small stamp (hashes plus file timestamps) so only changed files are re-hashed. | Conventional |

## Still open

- Registry and index design at scale (index growth, search as a rebuildable derived service).
- Version resolution beyond minimal version selection.
- A codegen approach that keeps working without turning into a build-script system. Dennis rejected a sandbox as
  uncreative. Current direction: generators are Pelago functions with no ambient authority, and foreign tools run only
  on request through `islet regen`, with output pinned in the lock file.
- Implementation language and dependencies (see `docs/plan.md`).
