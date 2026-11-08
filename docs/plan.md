# Islet milestone plan (proposal)

Follows the Pelago workflow (`docs/process/workflow.md` in pelagolang/pelago): Build phase is open, at most three
cards in "Doing" across the whole project, so none of these start in parallel with other work without a free slot.
Each card has a one-line "done when". Order is strict: take from the top.

| Card | What | Done when |
|---|---|---|
| I-0 | Repo setup: README, licences, PR template, CI, schema, design record. | Merged. |
| I-1 | Store and hashing (merged). BLAKE3, per-volume `.islet` layout, put and get by hash, link into a project (symlink, junction on Windows), `islet store` info and verify. No network. | A directory of source can be stored, linked and verified on Linux and Windows CI. |
| I-2 | Manifest and lock (in progress, PR claude/feat/manifest). A TOML reader, `islet.toml` checked against the schema with line-numbered errors, the canonical `islet.lock` writer, `{ from = "..." }` references. | Round-trip: parse, write, parse gives identical bytes; bad files give clear errors. |
| I-3 | Resolution and compiler hook. Minimal version selection over local and path sources, workspaces, major paths, `islet add`, `remove`, `tree`. Agree with the compiler how a build finds linked packages (extend `package.rs`). | `pelago build` compiles a project with two linked dependencies, including two majors of one package. |
| I-4 | Registry design record (a design doc and the owner's answers) before any code: log, signing, index growth, search. | Doc merged with the owner's decisions recorded. |
| I-5 | Registry client and a local test registry. Fetch by hash, verify signatures and log inclusion, yank handling. | A package is published to the local registry, fetched, verified and rejected when tampered with. |
| I-6 | Public-API summary and `islet update`. The compiler gets a command that emits a package's exported names, types and access modes; Islet diffs two summaries. | `islet update` offers a compatible bump and refuses an incompatible one, with the exact differences shown. |
| I-7 | `islet regen`, generators and declared C link inputs. Generator contract settled first (card I-4b, a design doc). | A root project regenerates a file, the result is stored and pinned, and a normal build runs no foreign code. |
| I-8 | `islet publish`, `audit`, `search`. | End to end against the test registry. |
| I-9 | Monorepo support: `monorepo.toml`, shared table and exceptions, unify-then-prove, source and API hashes, the task cache, `islet affected`. Needs I-6 first. | A workspace with several members builds with one `deps/`; a body-only change leaves dependents' `check` cached. |

## Notes

- **Language.** Recommendation: write stage 0 of Islet in Rust, like the compiler, so it builds from the same toolchain
  and CI, then move it to Pelago once the compiler can carry it. The Pelago compiler has a no-dependencies rule. For
  Islet that rule would mean writing TOML, BLAKE3 and signature code ourselves. I recommend our own small TOML subset
  and BLAKE3 (both small), and an audited crypto crate or vendored reference code for signatures only, because
  hand-written crypto is a risk we should not take. This needs the owner's decision (question 1).
- **Windows first-class.** Junctions, drive-root stores and path length rules are tested on Windows CI from I-1.
- **Reports** say which parts are conventional and which are Pelago-specific.
- **Cost.** CI stays light: no benchmarks, and the schema check runs in seconds.

## Questions for the owner

1. Rust for stage 0, with our own TOML and BLAKE3 and one audited signature dependency, or no dependencies at all?
2. May I start I-1 when a "Doing" slot is free (the project cap is three)?
3. Should the compiler hook in I-3 be a new `pelago` subcommand, or should `islet` write a resolved map that `pelago build` reads?
