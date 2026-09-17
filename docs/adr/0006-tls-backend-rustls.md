# ADR-0006: rlibpq's TLS backend is rustls

Status: accepted (owner decision 2026-09-16); crypto provider settled
2026-09-17.

## Context

pgrust #40 wants a pure-Rust libpq that removes the libc/OpenSSL
cross-compilation pain. libpq's negotiation (`sslmode`, `sslnegotiation`,
`t/005_negotiate_encryption.pl`) is independent of the TLS implementation;
only the handshake and certificate verification are.

## Decision

`rustls` behind a `tls` Cargo feature (on by default), with a `NoTls` backend
always compiled so the negotiation state machine and its stolen tests run
without it. Use rustls wherever a choice exists rather than reaching past it.

The crypto provider is **`ring`**, not rustls' default `aws-lc-rs` (owner
decision, 2026-09-17). ADR-0007 makes `*-unknown-linux-musl` and
`aarch64-apple-darwin` the primary targets, and `ring` is the provider that
cross-builds to both without cmake or bindgen. `rustls` is therefore declared
with `default-features = false` and the `ring` feature, so the default provider
is never pulled in alongside it.

## Consequences

- `ring` compiles C and assembly, so the musl target gains a build prerequisite
  that the workspace does not have today. Measured: with no musl C toolchain,
  `cargo build --target x86_64-unknown-linux-musl` fails in `cc-rs` with
  `failed to find tool "x86_64-linux-musl-gcc"`; with Ubuntu's `musl-tools`
  installed and `CC_x86_64_unknown_linux_musl=musl-gcc` it builds. Until `ring`
  lands, the workspace cross-builds to a static-pie musl binary with no C
  compiler at all, so this is a real cost, accepted for the cross-platform
  build story. CI's musl lane and `AGENTS.md` must state the prerequisite.
- `sslmode=verify-ca|verify-full` use **`webpki-roots`** (owner approval,
  2026-09-17), not the system store via `rustls-native-certs`: a static musl
  binary on an Omen device cannot assume `/etc/ssl` exists or is populated, and
  a compiled-in root set is the same on every lane. `sslrootcert` still takes
  precedence when the connection string names a file, matching libpq.
- `sslrootcert`, `sslcert`, `sslkey` map onto rustls' PEM loading; client
  certificates and `sslcrl` are covered by rustls, `sslpassword` for encrypted
  keys needs a small PKCS#8 decrypt (evaluate when reached).
- GSSAPI encryption stays feature-gated off, as #40 anticipates.
