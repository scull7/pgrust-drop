//! The TLS handshake, over rustls with the `ring` provider (ADR-0006): the
//! part of `fe-secure-openssl.c`'s `pgtls_open_client` (`:97`),
//! `initialize_SSL` (`:771`) and `open_client_SSL` (`:1370`) that needs a
//! TLS library. What to do before and after it — the SSLRequest, the root
//! certificate lookup, the SNI and ALPN decisions — is `crate::secure`.
//!
//! This slice handshakes only when [`RootCert::Absent`](crate::secure::RootCert):
//! libpq's `SSL_VERIFY_NONE` case (`fe-secure-openssl.c:1352`), which is
//! every `disable`/`allow`/`prefer`/`require` connection without a root
//! certificate file. The certificate chain is then not checked, exactly as
//! libpq does not check it; the handshake signature still is, so the peer
//! has proved it holds the key of the certificate it sent.
//!
//! That signature is checked against the certificate's SubjectPublicKeyInfo
//! read straight out of its DER, not through rustls' own helpers: those
//! parse the whole certificate with webpki, which refuses an X.509 v1
//! certificate — and OpenSSL, unverifying, takes one. Upstream's test
//! certificates are v1 (`src/test/ssl/ssl/server-cn-only.crt`).

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, SignatureVerificationAlgorithm, UnixTime};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, PeerMisbehaved,
    SignatureScheme, StreamOwned,
};

use crate::secure::{PG_ALPN_PROTOCOL, TlsError};

/// An established TLS session over its TCP socket.
pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// `SSL_VERIFY_NONE`: any certificate is accepted, but the handshake
/// signatures are verified with the provider's algorithms.
#[derive(Debug)]
struct NoRootCert(Arc<CryptoProvider>);

impl ServerCertVerifier for NoRootCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // TLS 1.2: every algorithm the scheme maps to may fit the key.
        self.verify(message, cert, dss, usize::MAX)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // TLS 1.3: the scheme names one algorithm, curve and all.
        if !allowed_in_tls13(dss.scheme) {
            return Err(PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme.into());
        }
        self.verify(message, cert, dss, 1)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl NoRootCert {
    /// The handshake signature, checked with the first `candidates`
    /// algorithms the provider maps `dss.scheme` to whose key type is the
    /// certificate's — the rule rustls' `verify_tls12_signature` and
    /// `verify_tls13_signature` apply, without their certificate parser.
    fn verify(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
        candidates: usize,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let (key_algorithm, key) = subject_public_key_info(cert).ok_or(
            rustls::Error::InvalidCertificate(CertificateError::BadEncoding),
        )?;
        let algorithms: &[&dyn SignatureVerificationAlgorithm] = self
            .0
            .signature_verification_algorithms
            .mapping
            .iter()
            .find(|(scheme, _)| *scheme == dss.scheme)
            .map(|(_, algorithms)| *algorithms)
            .ok_or(PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme)?;
        let algorithm = algorithms
            .iter()
            .take(candidates)
            .find(|algorithm| algorithm.public_key_alg_id().as_ref() == key_algorithm)
            .ok_or(rustls::Error::InvalidCertificate(
                CertificateError::BadSignature,
            ))?;
        algorithm
            .verify_signature(key, message, dss.signature())
            .map(|()| HandshakeSignatureValid::assertion())
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadSignature))
    }
}

/// Calculation: RFC 8446 §4.2.3 — TLS 1.3 signs with neither SHA-1 nor
/// RSASSA-PKCS1-v1_5. These are the schemes in the `ring` provider's table
/// that it rules out.
fn allowed_in_tls13(scheme: SignatureScheme) -> bool {
    !matches!(
        scheme,
        SignatureScheme::RSA_PKCS1_SHA1
            | SignatureScheme::ECDSA_SHA1_Legacy
            | SignatureScheme::RSA_PKCS1_SHA256
            | SignatureScheme::RSA_PKCS1_SHA384
            | SignatureScheme::RSA_PKCS1_SHA512
    )
}

/// Calculation: one DER element — its tag, its contents, and what follows.
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let width = usize::from(first & 0x7f);
        if width == 0 || width > 4 || rest.len() < width {
            return None;
        }
        let len = rest[..width]
            .iter()
            .fold(0usize, |len, byte| (len << 8) | usize::from(*byte));
        (len, &rest[width..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Calculation: a certificate's SubjectPublicKeyInfo (RFC 5280 §4.1), as
/// the contents of its AlgorithmIdentifier and the subjectPublicKey bits —
/// the two halves a `SignatureVerificationAlgorithm` takes. Works for every
/// X.509 version: the `[0]` version field is optional and skipped.
fn subject_public_key_info(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    const SEQUENCE: u8 = 0x30;
    const BIT_STRING: u8 = 0x03;
    const VERSION: u8 = 0xa0; // [0] EXPLICIT

    let (SEQUENCE, certificate, _) = der_element(cert)? else {
        return None;
    };
    let (SEQUENCE, mut tbs, _) = der_element(certificate)? else {
        return None;
    };
    if tbs.first() == Some(&VERSION) {
        tbs = der_element(tbs)?.2;
    }
    // serialNumber, signature, issuer, validity, subject.
    for _ in 0..5 {
        tbs = der_element(tbs)?.2;
    }
    let (SEQUENCE, spki, _) = der_element(tbs)? else {
        return None;
    };
    let (SEQUENCE, algorithm, rest) = der_element(spki)? else {
        return None;
    };
    // A key's bit string has no unused bits.
    let (BIT_STRING, [0, key @ ..], _) = der_element(rest)? else {
        return None;
    };
    Some((algorithm, key))
}

/// Action: run the client handshake over `tcp` with no root certificate,
/// and, for a direct SSL connection, insist on the `postgresql` ALPN
/// protocol (`fe-secure-openssl.c:1478`).
///
/// `sni` is [`crate::secure::sni_host`]'s answer for the connection's host.
///
/// # Errors
/// The handshake failed, the socket failed or closed under it, or a direct
/// connection came up without ALPN.
pub fn open_client_unverified(
    mut tcp: TcpStream,
    sni: Option<&str>,
    direct: bool,
) -> Result<TlsStream, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|err| TlsError::Ssl(err.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoRootCert(provider)))
        .with_no_client_auth();
    // fe-secure-openssl.c:1129 — ALPN goes out on every SSL connection.
    config.alpn_protocols = vec![PG_ALPN_PROTOCOL.to_vec()];
    // rustls wants a name even when none is sent; with no SNI (sslsni=0, an
    // IP literal, or a host rustls cannot parse as a name) the placeholder
    // is an address, which rustls never puts in SNI and, unverified, never
    // checks.
    let server_name = sni.and_then(|host| ServerName::try_from(host.to_owned()).ok());
    config.enable_sni = server_name.is_some();
    let server_name = server_name.unwrap_or(ServerName::IpAddress(Ipv4Addr::LOCALHOST.into()));

    let mut session = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|err| TlsError::Ssl(err.to_string()))?;
    while session.is_handshaking() {
        session
            .complete_io(&mut tcp)
            .map_err(|err| handshake_error(&err))?;
    }
    // Anything the handshake left to send (a TLS 1.2 Finished, say).
    while session.wants_write() {
        session
            .write_tls(&mut tcp)
            .map_err(|err| handshake_error(&err))?;
    }

    if direct {
        match session.alpn_protocol() {
            None => return Err(TlsError::DirectSslWithoutAlpn),
            Some(protocol) if protocol != PG_ALPN_PROTOCOL => {
                return Err(TlsError::UnexpectedAlpn);
            }
            Some(_) => {}
        }
    }
    Ok(StreamOwned::new(session, tcp))
}

/// `open_client_SSL`'s error arms, `fe-secure-openssl.c:1376`-`:1467`: a
/// TLS alert or protocol failure is `SSL error`, a closed socket `EOF
/// detected`, anything else from the socket `SSL SYSCALL error`.
fn handshake_error(err: &io::Error) -> TlsError {
    if let Some(tls) = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        return TlsError::Ssl(tls.to_string());
    }
    match err.kind() {
        io::ErrorKind::UnexpectedEof => TlsError::SslSyscallEof,
        _ => TlsError::SslSyscall(err.to_string()),
    }
}

/// Read through the session, with a peer that closes without a
/// `close_notify` read as end of stream, so the caller reports it as it
/// reports a closed plaintext socket ("server closed the connection
/// unexpectedly"). `pgtls_read` says "SSL SYSCALL error: EOF detected" there
/// instead (`fe-secure-openssl.c:192`); the outcome, a dead connection, is
/// the same (`docs/divergences.md`).
///
/// # Errors
/// The socket or the TLS layer failed.
pub fn read(stream: &mut TlsStream, buf: &mut [u8]) -> io::Result<usize> {
    match stream.read(buf) {
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        other => other,
    }
}

/// Write through the session.
///
/// # Errors
/// The socket or the TLS layer failed.
pub fn write(stream: &mut TlsStream, buf: &[u8]) -> io::Result<usize> {
    stream.write(buf)
}

/// Push whatever the session has buffered to the socket.
///
/// # Errors
/// The socket failed.
pub fn flush(stream: &mut TlsStream) -> io::Result<()> {
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream's own test certificate, which is X.509 v1: no `[0]` version.
    #[test]
    fn the_key_of_a_v1_certificate_is_found() {
        let pem = include_str!("../tests/ssl/server-cn-only.crt");
        let body: String = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let der = crate::base64::decode(body.as_bytes()).expect("PEM body is base64");
        let (algorithm, key) = subject_public_key_info(&der).expect("an SPKI");
        // rsaEncryption, NULL parameters.
        assert_eq!(
            algorithm,
            [
                0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00
            ]
        );
        // RSAPublicKey ::= SEQUENCE { modulus, publicExponent }, 2048 bits.
        assert_eq!(key[..4], [0x30, 0x82, 0x01, 0x0a]);
    }

    /// A v3 certificate carries `[0] EXPLICIT Version`, which is skipped.
    #[test]
    fn the_version_field_is_skipped_when_present() {
        let tbs = [
            0xa0, 0x03, 0x02, 0x01, 0x02, // [0] { INTEGER 2 }: v3
            0x02, 0x01, 0x01, // serialNumber
            0x30, 0x00, 0x30, 0x00, 0x30, 0x00, 0x30, 0x00, // signature … subject
            0x30, 0x09, // subjectPublicKeyInfo
            0x30, 0x03, 0x06, 0x01, 0x2a, // AlgorithmIdentifier { OID 1.2 }
            0x03, 0x02, 0x00, 0x07, // BIT STRING, no unused bits, one key byte
        ];
        let mut cert = vec![0x30, 0x1d, 0x30, 0x1b];
        cert.extend_from_slice(&tbs);
        assert_eq!(tbs.len(), 0x1b);
        assert_eq!(
            subject_public_key_info(&cert),
            Some((&[0x06, 0x01, 0x2a][..], &[0x07][..]))
        );
    }

    #[test]
    fn a_truncated_certificate_has_no_key() {
        assert_eq!(subject_public_key_info(&[]), None);
        assert_eq!(subject_public_key_info(&[0x30, 0x05, 0x30]), None);
        assert_eq!(der_element(&[0x30, 0x81]), None);
        assert_eq!(
            der_element(&[0x02, 0x81, 0x01, 0x07, 0xff]),
            Some((0x02, &[0x07][..], &[0xff][..]))
        );
    }
}
