<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="brand/logo/islet-horizontal-dark.svg">
    <img alt="islet" src="brand/logo/islet-horizontal.svg" width="320">
  </picture>
</p>

# Islet

The package manager for [Pelago](https://github.com/pelagolang/pelago). The command is `islet`.

Status: design stage. Nothing here installs packages yet. See [docs/design.md](docs/design.md) for what is decided
and [docs/plan.md](docs/plan.md) for the milestones.

## What it is

- **Source packages.** Pelago builds whole programs and has no general linker, so a package is source the compiler
  reads next to yours.
- **One global store.** Every package version is stored once, named by its BLAKE3 hash. Projects link into the store
  (junctions on Windows). One store per volume, in `.islet` at the drive root.
- **Two manifests, one owner per key.** `islet.yaml` holds package identity, dependencies, workspace, root-only
  generators and C link inputs. `pelago.yaml` stays the compiler's file. Every file is YAML in one strict subset, and `islet.lock` is committed.
- **Majors in the import path** (`"json/v2"`), minimal version selection, and a compiler-checked public-API diff for
  `islet update`.
- **No build scripts.** Generators run only through `islet regen`, for the root project; their output is hashed,
  stored and shipped with the package.
- **Signing from day one**, designed for a very large index.

## Repository layout

- `schema/`: JSON Schemas for `islet.yaml` and `monorepo.yaml`. Published from the website repo to
  <https://pelagolang.github.io/website/schema/>; the tool embeds the same files.
- `docs/`: design and plan; [`docs/brand.md`](docs/brand.md) is the brand record.
- `brand/`: logo, icons, file icons and social images, with the script that builds them.

## License

MIT OR Apache-2.0, at your option. See `LICENSE-MIT` and `LICENSE-APACHE`.
