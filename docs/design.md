# Islet design decisions

Decided by Dennis in the ecosystem brainstorm (2026-11-07). Do not reopen without the owner. Where a decision is
conventional we say so, because Islet is meant to have Pelago's own voice.

## Decisions

| Area | Decision | Conventional or Pelago-specific |
|---|---|---|
| Name | Islet, command `islet`. Sub-tools take sea and island names, not ship parts. | Pelago-specific |
| Packages | Source, read by the compiler next to the root project. No linker. | Follows from whole-program builds |
| Hash | BLAKE3 for all content addresses. | Conventional choice, fast |
| Line endings | Before hashing, a text file (valid UTF-8, no NUL byte) has `\r\n` turned into `\n`; the store keeps the LF form, so it still re-hashes to itself. Binary files are hashed as they are. Owner's decision, 2026-11-10; it must hold before `islet install` so a Windows and a Linux checkout agree. | Conventional idea, applied to the address |
| Store | pnpm-style global content-addressed store. One store per volume, in `.islet` at the drive root. Nothing on C: by default. A project uses the store on its own volume. Projects link into it (symlinks, junctions on Windows). | Store-and-link is pnpm's idea; the per-volume rule and the whole-program source model are ours |
| Manifests | `islet.yaml` (identity, dependencies, root-only generators, C link inputs, publish metadata), `monorepo.yaml` (see Monorepos) and `pelago.yaml` (compiler: opt, target, `[fmt]`, lints, tests, editor). One owner per key. Another file may reference a key with `from: islet`, never copy it. | Pelago-specific |
| File format | Every file is YAML in one strict subset (owner's decision, 2026-11-08, because it reads better than TOML: nesting by indentation, no repeated table headers). Block mappings and lists, comments, plain or quoted strings, `true`, `false` and whole numbers, `{}` and `[]` only for empty. Refused with a line and column: tabs, flow style with content, anchors, aliases, tags, block scalars, several documents, `null`, empty values, duplicate keys, and plain words that YAML tools read differently (`yes`, `no`, `1.5`, `0x1F`, dates), which must be quoted. A file in the subset is plain YAML, so any YAML tool reads it the same way (CI checks the examples with a full YAML reader). Names: `islet.yaml`, `monorepo.yaml`, `pelago.yaml`, `islet.lock` (YAML inside). Replaces the earlier TOML choice. | Conventional language; the strict subset and the writer that always emits one canonical form are ours |
| Format key | Each file has its own `format: 1`. | Conventional |
| Lock | `islet.lock`, committed, one canonical form, sorted, in the same layout as `pnpm-lock.yaml` (the owner's reference). `format: 1`, then three sections, each left out when empty. `catalog:` is what the shared `dependencies` of `monorepo.yaml` resolved to. `members:` is what each member resolved to (pnpm's importers): a bare version, or a mapping with `version`, `from: monorepo`, `target`, `exception: true`, or just `path` for a sibling member. `packages:` lists every package in the closure as `name@version:` with a quoted `hash` (its store address) and, if it has dependencies, a sorted `needs` list. Member source and API hashes are not in the lock: they are cache stamps. No peer-dependency variants, because packages are source compiled together; the hash is the integrity check. `islet lock fmt` rewrites a file into this form and `islet check` refuses any other. The layout and the exact keys (`catalog`, `members`, `packages`, `hash`, `needs`, `from`, `target`, `exception`, `path`) were confirmed by the owner on 2026-11-08. | Layout follows pnpm's; the hash-as-integrity and `from`/`exception` markers are ours |
| Versions | Major version in the import path (`"json/v2"`). Minimal version selection. `islet update` diffs the public API with the compiler's checker and says whether a bump is safe. | Path majors and MVS are Go's; the compiler-checked API diff is ours |
| Build scripts | None. Generators run only through `islet regen` in the root project; output is hashed, stored by generator hash plus input hashes, and shipped by packages. Dependencies never declare generators. | Pelago-specific; the generator contract is still open (see plan) |
| Signing | Required from day one. A transparency-log design built for a very large index. | Conventional (same family as Go's checksum database and Sigstore); say so in every report |
| Finding packages | `deps/` at the project root (visible) holds one link per dependency into the store; the compiler follows the links with no resolver or network. Each link targets a store folder named by its BLAKE3 hash, and store folders are read-only. `deps/` is only a cache of `islet.lock`: if it is missing or stale and the store has every hash in the lock, the compiler recreates the links itself (read the lock, link; no resolving, no fetching). A missing hash sends the user to `islet install` and names it. A build refuses to run when a link and the lock disagree. `.islet` is only the per-volume store. | Pelago-specific combination (link folder like pnpm's `node_modules`; self-verifying and self-healing from the lock are ours) |
| Compiler hook | `islet` writes a resolved map (package, major, store path, source hash); `pelago build` reads it and never calls `islet`. Owner's decision, 2026-11-10. The format is proposed in `docs/resolved-map.md`. | Pelago-specific |
| Schemas | One JSON Schema per file, published on GitHub Pages from the website repo (`https://pelagolang.github.io/website/schema/islet/1.json` and `.../monorepo/1.json`), embedded in the tools, referenced by a `# yaml-language-server: $schema=` comment. The schema checks the YAML data model, which matches JSON. | Conventional |

## Monorepos

Decided with Dennis on 2026-11-07. There is no special name: it is a monorepo.

| Area | Decision | Conventional or Pelago-specific |
|---|---|---|
| Root file | `monorepo.yaml` at the repo root, with its own `format: 1` and schema. It owns the member list, the shared dependency table (our catalogs), the exceptions and the cache settings. Members keep their own `islet.yaml` and point at the shared table with `from: monorepo`, never copying. The root need not be a package itself. | Separate workspace file as in `pnpm-workspace.yaml`; reference-not-copy is ours |
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
