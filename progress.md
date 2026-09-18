# progress.md — running log

Newest first. Each entry: what, why, checks run, risks, follow-ups.
Linear: project *pgrust-drop* (team NAT). GitHub: `scull7/pgrust-drop`.

## 2026-09-17 — Target matrix: musl + Darwin primary (ADR-0002, 0006, 0007)

**What**
- ADR-0007 (new): `*-unknown-linux-musl` (Omen devices) and `aarch64-apple-darwin`
  (developer laptops) are the primary targets; glibc is a pull-request gate only.
  A byte-diff gate is only valid when both sides link the same C library, so
  `testkit::reference` derives `Libc` from `cfg!` at compile time and each lane
  has its own variable (`PGDROP_REF_BIN_{GNU,MUSL,APPLE}`).
- ADR-0002 revised: the embedded template carries **bootstrap catalogs only**.
  Locale is stamped at run time in the `postgres --single` phase from the host's
  libc, so one image serves every libc and the locale matrix is not frozen at
  mint time. Fallback if pgrust's `--single` cannot import collations: a single
  image minted with `builtin` + `C.UTF-8`.
- ADR-0006 completed: crypto provider is `ring` (owner decision), rustls with
  `default-features = false`. Root store still undecided.
- `scripts/fetch-ref-binaries.sh`: PostgreSQL 18.6.0 reference binaries per lane
  and architecture from Maven Central, the only source covering musl and Darwin.
- CI restructured into lanes: musl (`container: alpine:3.21`) on every push, gnu
  and apple gated behind `needs: musl` and pull-request-only.

**Why**
The product runs on Alpine and on Apple silicon; neither has glibc. Measured on
one host, PostgreSQL 18.6 both sides:

| mint recipe (glibc initdb) | `datcollversion` | musl server on it |
| --- | --- | --- |
| `--no-locale` | `NULL` | clean |
| `--locale-provider=builtin --builtin-locale=C.UTF-8` | `1` | clean |
| `--locale=en_US.UTF-8` | `2.39` | warns on every connection |

`2.39` is the *mint host's glibc version*, so per-libc images would not have
fixed it — it would take one image per libc version. Hence: bake no locale.

Also measured: `initdb --help`/`--version` are byte-identical across libcs, but
`initdb -D data` differs in the locale block, and `--locale=xx_ZZ.UTF-8` exits 1
on glibc and **0 on musl** (musl's `setlocale` accepts any name). Both are now
entries in `docs/divergences.md`.

**Checks run** (this container, Rust 1.96.0)
- gnu lane: `cargo fmt --all --check`, pedantic clippy, `cargo test --all-features`
  — all clean, 49 tests.
- musl lane: `rustup target add x86_64-unknown-linux-musl`, pedantic clippy and
  `cargo test --all-features --target x86_64-unknown-linux-musl` — clean.
- **The initdb byte-diff gate ran for real for the first time**, in both lanes,
  against Maven's 18.6.0 builds: `rinitdb --help`/`--version` are byte-identical
  to C initdb. Previously it always printed SKIP.
- `ring` on musl: verified it fails without a musl C toolchain
  (`failed to find tool "x86_64-linux-musl-gcc"`) and builds with `musl-tools`
  plus `CC_x86_64_unknown_linux_musl=musl-gcc`. Recorded in ADR-0006.
- Not run: the CI workflow itself. The Alpine container job (rustup on a musl
  host, `actions/checkout` in a container) has never executed; first PR run is
  its real test.

**Resolved same day (owner, 2026-09-17)**
- `webpki-roots` approved as rlibpq's root store (ADR-0006).
- pgrust's `--single` **does** support `pg_import_system_collations()` (foid
  3445) and `pg_collation_actual_version()` (foid 3448), and stamps
  `collversion` as rows are created, so M1 commits to run-time stamping on the
  gnu lane. A NULL libc `collversion` on macOS or musl is correct, not a bug.
  NAT-376 only pins a rev containing the port; the `--single` script is NAT-383,
  whose description now carries the statements, the provider table and the
  failure modes.
- Linear: NAT-383 and NAT-376 updated; NAT-425 (psql reference per lane, M3),
  NAT-426 (arm image portability, v2), NAT-427 (Docker fixtures, v2) created.

**CI, first real run (PR #15)**
- All three lanes green on the first attempt, including the Alpine container
  job: `actions/checkout` and rustup both work on a musl host, which was the
  part nothing had ever exercised.
- Green did not prove the gate *ran*, though: a missing reference binary makes
  the byte-diff test print SKIP and pass, and the println is captured, so a lane
  that silently stopped proving conformance looks identical to one that proved
  it. `PGDROP_REQUIRE_REF=1` (set in CI) now turns that skip into a failure, via
  `reference::find_or_skip`. A laptop without the binaries still skips.

**Risks / open questions**
- Alpine's `postgresql18` package layout in `Libc::Musl::default_dirs` is a
  guess; the Alpine lane fetches from Maven, so nothing depends on it yet.
- The `main` ruleset is **not** applied: the classic branch-protection API
  returns 403 `Resource not accessible by integration` for this session, and the
  rulesets API (which does read back 200) could not be written to from here.
  Owner action, see below.
- Template image portability across architectures is untested → v2, via a
  `pg_controldata` comparison on an arm runner.
- `PGDROP_REF_BIN` (lane-agnostic) is still accepted and is an unchecked
  assertion that the directory matches the compiled lane. A follow-up could read
  the reference's ELF interpreter and refuse a cross-libc pairing outright.
- No `psql` in the Maven bundles; the M3 rpsql gate needs its own reference.

**Follow-ups**
- NAT-374 gate runner: now partly done (the fetch script and both lanes run).
- **Owner action**: run `scripts/setup-branch-ruleset.sh` locally with an
  admin-authenticated `gh` (`--dry-run` first). It creates or updates the `main`
  ruleset requiring the `musl`, `apple` and `gnu` checks, deriving those names
  from `ci.yml` so the two cannot drift. The equivalent UI path is
  Settings > Rulesets > New branch ruleset, where the enforcement status
  defaults to Disabled and must be set to Active. Until it is applied the lane
  ordering is advisory: `needs: musl` gates execution, but nothing blocks a
  merge.
- Requiring all three is not belt and braces. GitHub treats `skipped` and
  `neutral` as successful, and its own troubleshooting docs say a job skipped
  because a `needs:` dependency failed "may not block merging". With only `gnu`
  required, a red musl lane would skip `gnu` and the merge would be allowed.
  CI job names were flattened to `musl`, `apple`, `gnu` for the same reason: the
  required-check string has to match the check name exactly.
- ELF-interpreter check on the reference binary, to turn the lane-agnostic
  `PGDROP_REF_BIN` from a trusted assertion into an enforced one.

## 2026-09-16 — Owner decisions applied (NAT-375, NAT-392, NAT-398, NAT-405)

**What**
- ADR-0001 accepted: pgrust as a rev-pinned Cargo git dependency.
- ADR-0003 accepted: per-crate licensing. `testkit`/`rinitdb`/`rlibpq`/`rpsql`
  MIT, ported from PostgreSQL C only (no pgrust code copied); `pgdrop`
  AGPL-3.0-only (`crates/pgdrop/LICENSE`, `NOTICE.md`, `license` fields).
  rpsql will be written fresh from `src/bin/psql` C sources.
- ADR-0005: rpsql line editing with `redox_liner` (`noline` rejected: no
  completion hook, no history file, MPL-2.0).
- ADR-0006: rlibpq TLS backend is `rustls` behind a `tls` feature with a
  `NoTls` fallback; crypto provider chosen in NAT-392.
- PR opened for the branch so CI runs.

**Checks run**: `cargo fmt --check`, pedantic clippy, `cargo test` (46 tests)
still green; the license text for pgdrop is the AGPL-3.0 file from pgrust's
repository verbatim.

**Follow-ups**: NAT-398 rewritten (port from C, not import); NAT-389/388/391
now say "read pgrust for how, port from C"; M3 gains no new issue yet, the
`describe.c`/`print.c` ports were already scoped as fresh work.

## 2026-09-16 — Project bootstrap (NAT-372, NAT-373, NAT-377, NAT-406)

**What**
- Created the Linear project *pgrust-drop* with milestones M0 Foundation, M1
  rinitdb, M2 rlibpq, M3 rpsql, M4 pgdrop binary, M5 Faithful bootstrap and
  44 issues (NAT-372 … NAT-415), each a ~10-minute reviewable chunk naming the
  upstream test it steals.
- Scaffolded the Cargo workspace (`testkit`, `rinitdb`, `rlibpq`, `rpsql`,
  `pgdrop`), pinned Rust 1.96.0 (pgrust's pin), CI (fmt, clippy pedantic,
  test), `AGENTS.md`, ADR-0001 … ADR-0004, `docs/test-stealing.md`,
  `docs/divergences.md`.
- `testkit`: ports of `program_help_ok`, `program_version_ok`,
  `program_options_handling_ok` as pure checks over a `CommandOutcome`.
- `rinitdb`: usage-rs CLI mirroring `initdb.c`'s 41 long options and
  `"A:c:dD:E:gkL:nNsST:U:WX:"` shorts; `--help`/`-?`/`--version`/`-V` fast path
  printing the upstream text verbatim; everything else exits with
  "not implemented yet" naming the Linear issue.
- `pgdrop`: multicall dispatcher skeleton (`pgdrop initdb …` and symlink
  `initdb`), other applets stubbed.

**Why**
pgrust ships no `initdb`/`psql`; its README makes users install PostgreSQL 18
client tools first and test suites reach for Docker. The plan removes that.

**Findings that shaped the plan**
- pgrust's `postgres` refuses `--boot` (`main_main/src/lib.rs:328`), so a
  faithful initdb port cannot run today → ADR-0002 embedded template image;
  M5 tracks the upstream `--boot` + `genbki` work.
- pgrust already has a Rust `psql` (`crates/bin/psql`, ~5k lines, gate script
  diffing vs PGDG psql) and an in-server client (`crates/interfaces/pgclient`)
  with conninfo/URI tests → rpsql seeds from it, rlibpq lifts auth/framing.
- pgrust #40 asks for a pure-Rust libpq: native crate + C-ABI `libpq.so`,
  GSSAPI feature-gated, cross-compilation friendly → M2 scope.
- pgrust reads share assets (timezone, tsearch) from `PGRUST_PGSHAREDIR` at
  runtime → M4 embeds them.
- Owner decision (this session): all CLIs use usage-rs → ADR-0004. Probe:
  `usage-rs` 6.9.1, MIT, 10 crates, builds in ~9 s on 1.96; needs
  `unknown_flags = "error"` (default swallows unknown flags into positionals)
  and `disable_help_flag`/`disable_version_flag` to keep upstream spellings.

**Checks run** (Rust 1.96.0, this container, no PostgreSQL installed)
- `cargo fmt --all --check` clean.
- `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic` clean.
  Two style lints are allowed at crate level with a comment (`doc_markdown`,
  `module_name_repetitions`); `struct_excessive_bools` is allowed on the one
  struct that mirrors C's option table. Note: crate attributes are needed because
  the CLI's `-W clippy::pedantic` outranks the workspace `[lints]` table.
- `cargo test --all-features`: 46 tests pass (testkit 13, rinitdb 17 unit +
  4 integration, pgdrop 6 unit + 4 integration, rpsql 2). The byte-diff gate
  against C `initdb --help/--version` printed `SKIP (flagged, not silent)`
  here; it runs for real once CI installs the PGDG 18 packages (NAT-374).
- Not run: anything needing a pgrust build (nothing links pgrust yet).
- Correction: the first scaffold commit was pushed while clippy still had four
  findings (a grep in the check script hid the exit code). The follow-up commit
  fixes them; the numbers above are for the branch head. Lesson recorded in
  AGENTS.md: gate on the command's exit status, never on filtered output.
- CI note: `ci.yml` runs on pushes to `main` and on pull requests, so this
  branch gets its first CI run when a PR is opened.

**Risks / open questions**
- Licensing (ADR-0003): repo is MIT, pgrust is AGPL-3.0; `pgdrop` and `rpsql`
  are AGPL-derived. Owner decision needed.
- Embedded template limits the locale/encoding matrix until M5.
- TLS backend for rlibpq (NAT-392) and line editing for rpsql (NAT-405) are
  dependency decisions awaiting approval.
- GitHub API access to `malisper/pgrust` is not available from this
  environment (read-only clone works); upstream issue filing (NAT-414) needs
  the owner or a session with that repo attached.

**Follow-ups**
- NAT-374 gate runner, NAT-375 ADR sign-off, NAT-376 vendor pgrust.
