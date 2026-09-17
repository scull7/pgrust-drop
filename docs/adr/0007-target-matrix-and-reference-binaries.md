# ADR-0007: target matrix, libc lanes and reference binaries

Status: accepted (owner decision 2026-09-17).

## Context

pgrust-drop exists so a PostgreSQL test cluster needs neither Docker nor the
PGDG client packages. Two deployments make that concrete:

- **`aarch64-apple-darwin`** — developer laptops (7 of 8 engineers on Apple
  silicon). No container runtime in the loop; `pgdrop` runs natively.
- **`*-unknown-linux-musl`** — Alpine Linux on Omen's remote devices, where the
  product ships.

Neither has glibc. That makes musl and Darwin the primary targets and glibc the
odd one out, which inverts the usual Rust CI shape.

The conformance method (`docs/test-stealing.md`) runs the upstream test against
the C tool and against ours, then diffs byte-for-byte. That only proves
something when both sides link the *same* C library. Measured on one host,
PostgreSQL 18.6 both sides, same environment and flags:

```
initdb --help, initdb --version          → byte-identical, glibc vs musl
initdb -D data (no locale flags)         → differs: glibc resolved every
                                            category to C.UTF-8 and printed the
                                            one-line form; musl resolved
                                            LC_COLLATE to C and printed the
                                            eight-line "locale configuration"
                                            block
initdb --locale=xx_ZZ.UTF-8              → glibc exits 1 with initdb's
                                            "invalid locale" error and its ICU
                                            hint; musl exits 0 and creates the
                                            cluster
```

A glibc reference would therefore report our musl build as wrong where neither
implementation is. The libc is part of the fixture, not an incidental detail.

Reference binaries have to come from somewhere that covers musl and Darwin.
PGDG publishes neither. Maven Central's `io.zonky.test.postgres` artifacts are
vanilla PostgreSQL builds published for every lane we need, all at 18.6.0:
`linux-amd64`, `linux-arm64v8`, `linux-amd64-alpine`, `linux-arm64v8-alpine`,
`darwin-amd64`, `darwin-arm64v8`.

## Decision

**Lanes.** A gate compares like with like. `testkit::reference` derives
[`Libc`] from `cfg!` at compile time and looks up a per-lane environment
variable (`PGDROP_REF_BIN_{GNU,MUSL,APPLE}`), so a reference directory exported
for one lane cannot be picked up by another.

| lane | target | role |
| ---- | ------ | ---- |
| `musl` | `x86_64-unknown-linux-musl` | primary; runs on every push |
| `apple` | `aarch64-apple-darwin` | developer platform; runs on pull requests |
| `gnu` | `x86_64-unknown-linux-gnu` | final pull-request gate only |

**CI.** The musl lane runs in `container: alpine:3.21` on `ubuntu-latest` —
GitHub offers no Alpine runner image, and a container is the supported way to
get a musl userland. The `gnu` and `apple` lanes declare `needs: musl`, so they
never start until musl is green, and carry
`if: github.event_name == 'pull_request'`. Branch protection requires `gnu`,
which makes it the last gate rather than a parallel one.

**Reference binaries.** `scripts/fetch-ref-binaries.sh` picks the Maven
classifier for the running lane and architecture, unpacks it under `.ref/`, and
prints the `export` line. Version is pinned to 18.6.0 to match the tree
vendored in pgrust. The same script serves a laptop and CI, so a developer's
gate and CI's gate use identical builds.

**Docker stays out of v1.** A GitHub Actions `container:` is CI configuration,
not something an engineer installs; nothing in the product, the test harness or
the developer workflow may require a container runtime. Docker-based fixtures
are a v2 question.

## Consequences

- Three lanes mean three reference downloads (~15 MB jar each) and three sets of
  expectations wherever libc shows through. Each such place is an entry in
  `docs/divergences.md` naming the lane that pins it.
- The Maven bundles ship `initdb`, `postgres`, `pg_ctl` and `libpq` but **no
  `psql`**. The M3 rpsql gate needs its own reference per lane: a source build
  (`git clone -b REL_18_6 postgres/postgres`, `musl-tools` on the musl lane) or
  Alpine's own `postgresql18-client` package inside the container. Decided when
  M3 starts.
- Locale-sensitive behaviour must be exercised on a real Alpine userland. Running
  musl PostgreSQL binaries on a glibc distribution works (the loader comes from
  Ubuntu's `musl` package and RPATH resolves the rest) and is fine for a quick
  local check, but `initdb` shells out to `locale`, which would be glibc's. Such
  a hybrid is not a lane.
- Architecture portability of the ADR-0002 template image is untested: x86\_64
  and aarch64 are both 64-bit little-endian with matching alignment, so a
  datadir should move between them, but nothing proves it yet. Deferred to v2,
  tracked with a `pg_controldata` comparison on an arm runner.
- Adding `ring` (ADR-0006) puts a C compiler back in the musl build: `musl-tools`
  plus `CC_x86_64_unknown_linux_musl=musl-gcc`. Without it the workspace
  cross-builds to a static-pie musl binary with no C toolchain at all.
