//! The half of SSL negotiation that does not need a TLS library: the
//! SSLRequest packet and its one-byte answer, where the root certificate
//! comes from, whether the server name goes out as SNI, the ALPN protocol,
//! and every message these steps can fail with.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c` — `PQconnectPoll`'s
//! `CONNECTION_MADE` (`:3662`-`:3697`) and `CONNECTION_SSL_STARTUP`
//! (`:3755`-`:3869`) — and from `fe-secure-openssl.c`'s `initialize_SSL`
//! (root certificate, `:902`-`:1002`; SNI, `:1105`-`:1127`) and
//! `open_client_SSL` (ALPN, `:1478`-`:1503`). Everything here is pure and
//! compiled whatever the features; the handshake itself is `crate::tls`,
//! behind the `tls` feature (ADR-0006).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::negotiate::SslMode;
use crate::text::RawText;

/// `NEGOTIATE_SSL_CODE`, `pqcomm.h:172` — `PG_PROTOCOL(1234,5679)`, sent
/// where a startup packet's version would be.
pub const NEGOTIATE_SSL_CODE: u32 = (1234 << 16) + 5679;

/// `PG_ALPN_PROTOCOL`, `pqcomm.h:165`: the one ALPN protocol libpq offers,
/// on every SSL connection (`fe-secure-openssl.c:1129`), and the one a direct
/// SSL connection must end up with (`:1478`).
pub const PG_ALPN_PROTOCOL: &[u8] = b"postgresql";

/// `ROOT_CERT_FILE`, `libpq-int.h:700`, under the home directory.
pub const ROOT_CERT_FILE: &str = ".postgresql/root.crt";

/// The SSLRequest packet, `fe-connect.c:3680`: `pqPacketSend(conn, 0, &pv,
/// 4)` with no type byte — the length word counting itself (8), then
/// [`NEGOTIATE_SSL_CODE`].
#[must_use]
pub fn ssl_request() -> [u8; 8] {
    let mut packet = [0u8; 8];
    packet[..4].copy_from_slice(&8u32.to_be_bytes());
    packet[4..].copy_from_slice(&NEGOTIATE_SSL_CODE.to_be_bytes());
    packet
}

/// The server's one-byte answer to an SSLRequest, `fe-connect.c:3791`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslResponse {
    /// `S`: go ahead with the handshake.
    Accepted,
    /// `N`: no SSL here; the socket is still good for a startup packet.
    Refused,
    /// `E`: the server failed (to fork a backend, say). Its message is not
    /// read, because the server is not authenticated yet (`:3816`).
    Error,
    /// Anything else.
    Invalid(u8),
}

impl SslResponse {
    #[must_use]
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            b'S' => SslResponse::Accepted,
            b'N' => SslResponse::Refused,
            b'E' => SslResponse::Error,
            other => SslResponse::Invalid(other),
        }
    }
}

/// Where `initialize_SSL` finds the root certificate, which decides whether
/// the server's certificate is verified at all (`have_rootcert`,
/// `fe-secure-openssl.c:1352`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootCert {
    /// No file: the handshake goes ahead without verifying the server, which
    /// `disable`, `allow`, `prefer` and `require` accept (`:1001`).
    Absent,
    /// This file exists and holds the roots to verify against (`:932`-`:976`).
    File(PathBuf),
    /// `sslrootcert=system`: the platform's roots (`:911`).
    System,
}

/// `initialize_SSL`'s root certificate lookup, `fe-secure-openssl.c:902`-
/// `:1002`: `sslrootcert` if it is set and not empty, else
/// `~/.postgresql/root.crt`; a file that does not exist is only an error for
/// `verify-ca` and `verify-full`.
///
/// `home` is `pqGetHomeDirectory`'s answer and `exists` is the `stat()`,
/// both injected so the lookup is a pure function.
///
/// # Errors
/// `verify-ca` or `verify-full` with no root certificate file.
pub fn root_cert(
    sslrootcert: Option<&[u8]>,
    home: Option<&Path>,
    sslmode: SslMode,
    exists: impl Fn(&Path) -> bool,
) -> Result<RootCert, TlsError> {
    let fnbuf: Option<PathBuf> = match sslrootcert {
        Some(value) if !value.is_empty() => {
            if value == b"system" {
                return Ok(RootCert::System);
            }
            Some(PathBuf::from(OsStr::from_bytes(value)))
        }
        _ => home.map(|home| home.join(ROOT_CERT_FILE)),
    };
    match fnbuf {
        Some(path) if exists(&path) => Ok(RootCert::File(path)),
        // :985 — "verify-ca" or "verify-full".
        _ if matches!(sslmode, SslMode::VerifyCa | SslMode::VerifyFull) => Err(match fnbuf {
            Some(path) => {
                TlsError::RootCertMissing(RawText::new(path.as_os_str().as_bytes().to_vec()))
            }
            None => TlsError::NoHomeForRootCert,
        }),
        _ => Ok(RootCert::Absent),
    }
}

/// The name to send as SNI, `fe-secure-openssl.c:1110`: the host, unless
/// `sslsni` is not `1` or the host is an IP literal — all digits and dots,
/// or anything with a colon (RFC 6066 forbids an address there).
#[must_use]
pub fn sni_host<'a>(sslsni: Option<&[u8]>, host: &'a str) -> Option<&'a str> {
    if sslsni.and_then(|value| value.first()) != Some(&b'1') || host.is_empty() {
        return None;
    }
    let ip_literal = host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || host.contains(':');
    (!ip_literal).then_some(host)
}

/// Everything SSL negotiation can fail with, each message as libpq appends
/// it, except where a variant says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsError {
    /// `fe-connect.c:3812` — the server answered `N` and the mode allows
    /// nothing else.
    SslRequired,
    /// `fe-connect.c:3822` — the server answered `E`.
    ErrorResponseDuringSslExchange,
    /// `fe-connect.c:3827`.
    InvalidSslResponse(u8),
    /// `fe-connect.c:3847` — bytes that arrived before the handshake, and so
    /// unencrypted, possibly injected by a man in the middle.
    UnencryptedDataAfterSslResponse,
    /// `fe-secure-openssl.c:993`.
    NoHomeForRootCert,
    /// `fe-secure-openssl.c:996`.
    RootCertMissing(RawText),
    /// This port's own message: a root certificate was found, so libpq
    /// would verify the server against it, and this build cannot yet.
    /// Refused rather than connected unverified (`docs/divergences.md`).
    VerificationNotSupported(RawText),
    /// `fe-secure-openssl.c:1425`, `SSL error: %s` — the text after the
    /// colon is rustls', not OpenSSL's.
    Ssl(String),
    /// `fe-secure-openssl.c:1414`, the socket failed mid-handshake.
    SslSyscall(String),
    /// `fe-secure-openssl.c:1417`, the server hung up mid-handshake.
    SslSyscallEof,
    /// `fe-secure-openssl.c:1488`.
    DirectSslWithoutAlpn,
    /// `fe-secure-openssl.c:1500`.
    UnexpectedAlpn,
}

impl TlsError {
    /// The bytes libpq's error buffer would hold.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        const EITHER: &[u8] = b"Either provide the file, use the system's trusted roots with sslrootcert=system, or change sslmode to disable server certificate verification.";
        match self {
            TlsError::SslRequired => b"server does not support SSL, but SSL was required".to_vec(),
            TlsError::ErrorResponseDuringSslExchange => {
                b"server sent an error response during SSL exchange".to_vec()
            }
            TlsError::InvalidSslResponse(byte) => {
                let mut out = b"received invalid response to SSL negotiation: ".to_vec();
                out.push(*byte);
                out
            }
            TlsError::UnencryptedDataAfterSslResponse => {
                b"received unencrypted data after SSL response".to_vec()
            }
            TlsError::NoHomeForRootCert => {
                let mut out =
                    b"could not get home directory to locate root certificate file\n".to_vec();
                out.extend_from_slice(EITHER);
                out
            }
            TlsError::RootCertMissing(path) => {
                let mut out = b"root certificate file \"".to_vec();
                out.extend_from_slice(path.as_bytes());
                out.extend_from_slice(b"\" does not exist\n");
                out.extend_from_slice(EITHER);
                out
            }
            TlsError::VerificationNotSupported(root) => {
                let mut out =
                    b"server certificate verification is not supported by this build yet (root certificate \""
                        .to_vec();
                out.extend_from_slice(root.as_bytes());
                out.extend_from_slice(b"\")");
                out
            }
            TlsError::Ssl(detail) => format!("SSL error: {detail}").into_bytes(),
            TlsError::SslSyscall(detail) => format!("SSL SYSCALL error: {detail}").into_bytes(),
            TlsError::SslSyscallEof => b"SSL SYSCALL error: EOF detected".to_vec(),
            TlsError::DirectSslWithoutAlpn => {
                b"direct SSL connection was established without ALPN protocol negotiation extension"
                    .to_vec()
            }
            TlsError::UnexpectedAlpn => {
                b"SSL connection was established with unexpected ALPN protocol".to_vec()
            }
        }
    }
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for TlsError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pqcomm.h:172`; the bytes are the ones a server's
    /// `ProcessStartupPacket` reads as `NEGOTIATE_SSL_CODE`.
    #[test]
    fn the_ssl_request_is_eight_bytes_ending_in_1234_5679() {
        assert_eq!(NEGOTIATE_SSL_CODE, 80_877_103);
        assert_eq!(
            ssl_request(),
            [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f] // 8, 1234, 5679
        );
    }

    #[test]
    fn the_answer_is_one_of_three_bytes() {
        assert_eq!(SslResponse::from_byte(b'S'), SslResponse::Accepted);
        assert_eq!(SslResponse::from_byte(b'N'), SslResponse::Refused);
        assert_eq!(SslResponse::from_byte(b'E'), SslResponse::Error);
        assert_eq!(SslResponse::from_byte(b'R'), SslResponse::Invalid(b'R'));
        assert_eq!(
            TlsError::InvalidSslResponse(b'R').to_string(),
            "received invalid response to SSL negotiation: R"
        );
    }

    /// `fe-secure-openssl.c:904`-`:907`: `sslrootcert` beats
    /// `~/.postgresql/root.crt`, and an empty one is unset.
    #[test]
    fn sslrootcert_beats_the_home_directory_file() {
        let home = Path::new("/home/u");
        let everything = |_: &Path| true;
        assert_eq!(
            root_cert(
                Some(b"/etc/ca.pem"),
                Some(home),
                SslMode::Require,
                everything
            ),
            Ok(RootCert::File(PathBuf::from("/etc/ca.pem")))
        );
        assert_eq!(
            root_cert(Some(b""), Some(home), SslMode::Require, everything),
            Ok(RootCert::File(PathBuf::from(
                "/home/u/.postgresql/root.crt"
            )))
        );
        assert_eq!(
            root_cert(Some(b"system"), Some(home), SslMode::VerifyFull, everything),
            Ok(RootCert::System)
        );
    }

    /// `fe-secure-openssl.c:977`-`:1001`: no file means no verification,
    /// unless the mode demands it — which is why 005's `require` rows
    /// connect to a server whose certificate the client has never seen.
    #[test]
    fn a_missing_root_file_is_an_error_only_for_the_verify_modes() {
        let home = Path::new("/home/u");
        let nothing = |_: &Path| false;
        for mode in [
            SslMode::Disable,
            SslMode::Allow,
            SslMode::Prefer,
            SslMode::Require,
        ] {
            assert_eq!(
                root_cert(None, Some(home), mode, nothing),
                Ok(RootCert::Absent)
            );
            assert_eq!(root_cert(None, None, mode, nothing), Ok(RootCert::Absent));
        }
        assert_eq!(
            root_cert(None, Some(home), SslMode::VerifyCa, nothing)
                .unwrap_err()
                .to_string(),
            "root certificate file \"/home/u/.postgresql/root.crt\" does not exist\n\
             Either provide the file, use the system's trusted roots with sslrootcert=system, or change sslmode to disable server certificate verification."
        );
        assert_eq!(
            root_cert(None, None, SslMode::VerifyFull, nothing)
                .unwrap_err()
                .to_string(),
            "could not get home directory to locate root certificate file\n\
             Either provide the file, use the system's trusted roots with sslrootcert=system, or change sslmode to disable server certificate verification."
        );
    }

    /// `fe-secure-openssl.c:1110`-`:1116`.
    #[test]
    fn sni_goes_out_for_a_host_name_only() {
        assert_eq!(
            sni_host(Some(b"1"), "db.example.com"),
            Some("db.example.com")
        );
        assert_eq!(sni_host(Some(b"1"), "127.0.0.1"), None);
        assert_eq!(sni_host(Some(b"1"), "::1"), None);
        assert_eq!(sni_host(Some(b"1"), "fe80::1%eth0"), None);
        assert_eq!(sni_host(Some(b"0"), "db.example.com"), None);
        assert_eq!(sni_host(None, "db.example.com"), None);
        assert_eq!(sni_host(Some(b"1"), ""), None);
    }
}
