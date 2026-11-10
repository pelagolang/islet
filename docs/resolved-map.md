# The resolved map

Status: proposal for the owner and the compiler thread. Nothing reads or writes it yet.

Decision already taken (2026-11-10): `islet` writes a resolved map and `pelago build` reads it. The compiler never
runs `islet` and never parses `islet.lock`. This page proposes what the map holds.

## Why a second file

`islet.lock` is the record of what was chosen: hashes, versions, who needs whom. It is committed and reviewed.
The map is what the compiler needs to *find source*: for each importing package, where each import lives. It
holds machine-local paths into the `deps/` folder, so it is generated, git-ignored, and rewritten by
`islet install` whenever the lock or the store changes. The compiler can therefore stay ignorant of hashes,
versions, majors and monorepo rules.

## Proposed file: `deps/map.yaml`

```yaml
# deps/map.yaml: written by islet, read by pelago, do not edit.
format: 1

packages:
  json@1.5.0:
    dir: json@1.5.0
    imports:
      units: units@0.2.4
  units@0.2.4:
    dir: units@0.2.4

members:
  libs/core:
    imports:
      json: json@1.5.0
  apps/cli:
    imports:
      core: member:libs/core
```

- `dir` is relative to the `deps/` folder. `islet install` makes `deps/<name>@<version>` a link into the store
  (`deps/json/v2@2.0.1` for a second major). The compiler reads source through the link, so the store stays
  read-only and shared.
- `imports` is the scope of one package: the key it writes in its source (`json`, `json/v2`) to the package it
  gets. Two packages may import the same key and get different versions; two majors of one package are just two
  keys. This is what makes "two linked dependencies, including two majors of one package" work with no global
  name table.
- A member import of another member is written `member:<path>`; the compiler resolves it to that member's folder
  in the workspace, so editing it needs no reinstall.
- Only `format`, `packages`, `members`; strict YAML like every other file, sorted, one canonical form.

## What the compiler has to do

Extend `package.rs`: given the folder it was asked to build, walk up to find `deps/map.yaml`; when an import is not
inside the package itself, look it up in the scope of the importing package or member. No map means no external
imports (a project without dependencies builds as today).

## Open questions

1. Name and place: `deps/map.yaml` (recommended; `deps/` already belongs to islet and is git-ignored), or a
   file at the project root?
2. Should `member:` entries be relative paths instead (`../core`)? Recommended: keep `member:<path>`, so the
   map never depends on where the member sits relative to the importer.
3. Should the map also carry each package's `target` and C link inputs now, or wait for the C card (I-8)?
   Recommended: wait; the format key lets us add them.
