# ADR-0006: rlibpq's TLS backend is rustls

Status: accepted (owner decision 2026-09-16).

## Context

pgrust #40 wants a pure-Rust libpq that removes the libc/OpenSSL
cross-compilation pain. libpq's negotiation (`sslmode`, `sslnegotiation`,
`t/005_negotiate_encryption.pl`) is independent of the TLS implementation;
only the handshake and certificate verification are.

## Decision

`rustls` behind a `tls` Cargo feature (on by default), with a `NoTls` backend
always compiled so the negotiation state machine and its stolen tests run
without it. The crypto provider (rustls default `aws-lc-rs` vs `ring`) is
chosen at implementation time in NAT-392 and recorded here; both build without
a system OpenSSL, neither is pure Rust.

## Consequences

- `sslmode=verify-ca|verify-full` need root stores: `webpki-roots` or the
  system store via `rustls-native-certs`; decided with the provider.
- `sslrootcert`, `sslcert`, `sslkey` map onto rustls' PEM loading; client
  certificates and `sslcrl` are covered by rustls, `sslpassword` for encrypted
  keys needs a small PKCS#8 decrypt (evaluate when reached).
- GSSAPI encryption stays feature-gated off, as #40 anticipates.
