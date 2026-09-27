//! Does the server's certificate name the host we meant to reach? The check
//! `sslmode=verify-full` adds on top of chain verification.
//!
//! Ported from `src/interfaces/libpq/fe-secure-common.c` — the wildcard rule
//! (`:44`-`:75`), the name and address comparisons (`:86`-`:244`) and
//! `pq_verify_peer_name_matches_certificate` (`:251`-`:307`) — and from
//! `fe-secure-openssl.c`'s `pgtls_verify_peer_name_matches_certificate_guts`
//! (`:555`-`:715`), which decides which of the certificate's names count.
//! libpq does all of this itself rather than asking OpenSSL, so it is ported
//! rather than delegated to rustls, whose name check has neither the Common
//! Name fallback nor the IP-address-in-a-dNSName match.
//!
//! Everything here is pure and compiled whatever the features. The names come
//! out of the certificate's DER with [`CertificateNames::from_der`], the one
//! place that knows X.509.

use std::fmt::Write as _;
use std::net::Ipv6Addr;

use crate::der::{self, BOOLEAN, EXTENSIONS, OCTET_STRING, OID, SEQUENCE, SET};
use crate::negotiate::SslMode;
use crate::text::RawText;

/// `id-ce-subjectAltName`, 2.5.29.17 (`NID_subject_alt_name`).
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
/// `id-at-commonName`, 2.5.4.3 (`NID_commonName`).
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
/// GeneralName's `dNSName [2] IA5String` (`GEN_DNS`).
const DNS_NAME: u8 = 0x82;
/// GeneralName's `iPAddress [7] OCTET STRING` (`GEN_IPADD`).
const IP_ADDRESS: u8 = 0x87;

/// One entry of a subjectAltName extension, as far as libpq looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneralName {
    /// `GEN_DNS`: the IA5String's bytes.
    Dns(Vec<u8>),
    /// `GEN_IPADD`: the address in network byte order — 4 or 16 bytes in a
    /// sane certificate, but any length in the DER.
    IpAddress(Vec<u8>),
    /// Any other kind (e-mail, URI, directory name …), which libpq skips.
    Other,
}

/// The names in a server certificate that
/// `pgtls_verify_peer_name_matches_certificate_guts` examines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CertificateNames {
    /// The subjectAltName entries, in certificate order. Empty when the
    /// extension is absent, or appears more than once — the two cases in
    /// which `X509_get_ext_d2i` returns `NULL` (`fe-secure-openssl.c:618`).
    pub subject_alt_names: Vec<GeneralName>,
    /// The first Common Name in the subject, its string's bytes as they are
    /// encoded (`X509_NAME_get_index_by_NID(…, -1)`, `:692`).
    pub common_name: Option<Vec<u8>>,
}

impl CertificateNames {
    /// Calculation: the names out of a DER certificate, v1 or v3.
    ///
    /// `None` when the certificate is not well-formed DER as far as these
    /// fields go. OpenSSL would have refused such a certificate during the
    /// handshake, so the caller treats it as a failed verification.
    #[must_use]
    pub fn from_der(cert: &[u8]) -> Option<Self> {
        let mut tbs = der::tbs_fields(cert)?;
        // serialNumber, signature, issuer, validity.
        for _ in 0..4 {
            tbs = der::element(tbs)?.2;
        }
        let (SEQUENCE, subject, rest) = der::element(tbs)? else {
            return None;
        };
        // subjectPublicKeyInfo, then the optional issuerUniqueID [1],
        // subjectUniqueID [2] and extensions [3].
        let mut rest = der::element(rest)?.2;
        let mut extensions = None;
        while !rest.is_empty() {
            let (tag, contents, next) = der::element(rest)?;
            if tag == EXTENSIONS {
                extensions = Some(contents);
            }
            rest = next;
        }
        Some(CertificateNames {
            subject_alt_names: match extensions {
                Some(extensions) => subject_alt_names(extensions)?,
                None => Vec::new(),
            },
            common_name: first_common_name(subject).ok()?,
        })
    }
}

/// Calculation: the subjectAltName entries out of `extensions [3]`'s
/// contents, or none if the extension is absent or repeated.
fn subject_alt_names(extensions: &[u8]) -> Option<Vec<GeneralName>> {
    let (SEQUENCE, mut list, _) = der::element(extensions)? else {
        return None;
    };
    let mut found = Vec::new();
    while !list.is_empty() {
        let (SEQUENCE, extension, next) = der::element(list)? else {
            return None;
        };
        let (OID, oid, mut rest) = der::element(extension)? else {
            return None;
        };
        // critical BOOLEAN DEFAULT FALSE.
        if rest.first() == Some(&BOOLEAN) {
            rest = der::element(rest)?.2;
        }
        let (OCTET_STRING, value, _) = der::element(rest)? else {
            return None;
        };
        if oid == SUBJECT_ALT_NAME {
            found.push(value);
        }
        list = next;
    }
    let [value] = found[..] else {
        return Some(Vec::new());
    };
    let (SEQUENCE, mut names, _) = der::element(value)? else {
        return None;
    };
    let mut out = Vec::new();
    while !names.is_empty() {
        let (tag, contents, next) = der::element(names)?;
        out.push(match tag {
            DNS_NAME => GeneralName::Dns(contents.to_vec()),
            IP_ADDRESS => GeneralName::IpAddress(contents.to_vec()),
            _ => GeneralName::Other,
        });
        names = next;
    }
    Some(out)
}

/// A Name that is not well-formed DER.
#[derive(Debug, PartialEq, Eq)]
struct Malformed;

/// Calculation: the first commonName in a Name (RDNSequence), in order.
fn first_common_name(mut name: &[u8]) -> Result<Option<Vec<u8>>, Malformed> {
    while !name.is_empty() {
        let Some((SET, mut rdn, next)) = der::element(name) else {
            return Err(Malformed);
        };
        while !rdn.is_empty() {
            let Some((SEQUENCE, attribute, after)) = der::element(rdn) else {
                return Err(Malformed);
            };
            let Some((OID, oid, value)) = der::element(attribute) else {
                return Err(Malformed);
            };
            if oid == COMMON_NAME {
                let (_, string, _) = der::element(value).ok_or(Malformed)?;
                return Ok(Some(string.to_vec()));
            }
            rdn = after;
        }
        name = next;
    }
    Ok(None)
}

/// Why a `verify-full` connection's certificate was not accepted for its
/// host, each message as libpq appends it (without the newline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerNameError {
    /// `fe-secure-common.c:269`.
    HostNameRequired,
    /// `fe-secure-common.c:123` — a name with a NUL in it, which could make
    /// `evil.com\0.example.com` pass for `evil.com` (CVE-2009-4034).
    EmbeddedNull,
    /// `fe-secure-common.c:228`.
    InvalidIpLength(usize),
    /// `fe-secure-common.c:286`, `:294`: the first name examined, how many
    /// others there were, and the host.
    Mismatch {
        first_name: RawText,
        others: usize,
        host: RawText,
    },
    /// `fe-secure-common.c:299` — neither a DNS or IP SAN nor a CN.
    NoName,
}

impl PeerNameError {
    /// The bytes libpq's error buffer would hold.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            PeerNameError::HostNameRequired => {
                b"host name must be specified for a verified SSL connection".to_vec()
            }
            PeerNameError::EmbeddedNull => {
                b"SSL certificate's name contains embedded null".to_vec()
            }
            PeerNameError::InvalidIpLength(len) => {
                format!("certificate contains IP address with invalid length {len}").into_bytes()
            }
            PeerNameError::Mismatch {
                first_name,
                others,
                host,
            } => {
                let mut out = b"server certificate for \"".to_vec();
                out.extend_from_slice(first_name.as_bytes());
                match others {
                    0 => out.push(b'"'),
                    // libpq_ngettext's singular for 1, plural otherwise.
                    1 => out.extend_from_slice(b"\" (and 1 other name)"),
                    n => out.extend_from_slice(format!("\" (and {n} other names)").as_bytes()),
                }
                out.extend_from_slice(b" does not match host name \"");
                out.extend_from_slice(host.as_bytes());
                out.push(b'"');
                out
            }
            PeerNameError::NoName => {
                b"could not get server's host name from server certificate".to_vec()
            }
        }
    }
}

impl std::fmt::Display for PeerNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for PeerNameError {}

/// `pq_verify_peer_name_matches_certificate`, `fe-secure-common.c:251`, with
/// `pgtls_verify_peer_name_matches_certificate_guts` (`fe-secure-openssl.c:576`)
/// inlined: only `verify-full` checks the name; the SANs of the host's own
/// kind (DNS or IP) rule out the Common Name, and an IP host may still match
/// a CN when the SANs hold only DNS names (`:589`-`:608`).
///
/// # Errors
/// No name matched, there was none, or a name was malformed.
pub fn verify_peer_name_matches_certificate(
    sslmode: SslMode,
    host: &[u8],
    names: &CertificateNames,
) -> Result<(), PeerNameError> {
    if sslmode != SslMode::VerifyFull {
        return Ok(());
    }
    if host.is_empty() {
        return Err(PeerNameError::HostNameRequired);
    }
    let host_is_ip = is_ip_address(host);

    let mut names_examined = 0usize;
    let mut first_name: Option<Vec<u8>> = None;
    let mut matched = false;
    let mut check_cn = true;
    for name in &names.subject_alt_names {
        let (is_match, shown) = match name {
            GeneralName::Dns(data) => {
                if !host_is_ip {
                    check_cn = false;
                }
                matches_certificate_name(host, data)?
            }
            GeneralName::IpAddress(data) => {
                if host_is_ip {
                    check_cn = false;
                }
                matches_certificate_ip(host, data)?
            }
            GeneralName::Other => continue,
        };
        names_examined += 1;
        first_name.get_or_insert(shown);
        if is_match {
            matched = true;
            check_cn = false;
            break;
        }
    }
    if check_cn && let Some(common_name) = &names.common_name {
        names_examined += 1;
        let (is_match, shown) = matches_certificate_name(host, common_name)?;
        first_name.get_or_insert(shown);
        matched = is_match;
    }

    match (matched, first_name) {
        (true, _) => Ok(()),
        (false, Some(first_name)) => Err(PeerNameError::Mismatch {
            first_name: RawText::new(first_name),
            others: names_examined - 1,
            host: RawText::from(host),
        }),
        (false, None) => Err(PeerNameError::NoName),
    }
}

/// `pq_verify_peer_name_matches_certificate_name`, `fe-secure-common.c:86`:
/// an exact or wildcard match, case-insensitive, and the name to show.
fn matches_certificate_name(host: &[u8], name: &[u8]) -> Result<(bool, Vec<u8>), PeerNameError> {
    if name.contains(&0) {
        return Err(PeerNameError::EmbeddedNull);
    }
    let is_match = name.eq_ignore_ascii_case(host) || wildcard_certificate_match(name, host);
    Ok((is_match, name.to_vec()))
}

/// `wildcard_certificate_match`, `fe-secure-common.c:44`: `*.` at the start
/// of the pattern only, standing for exactly one leading component of the
/// host — never a dot.
fn wildcard_certificate_match(pattern: &[u8], string: &[u8]) -> bool {
    let (lenpat, lenstr) = (pattern.len(), string.len());
    if lenpat < 3 || pattern[0] != b'*' || pattern[1] != b'.' {
        return false;
    }
    if lenpat > lenstr {
        return false;
    }
    if !pattern[1..].eq_ignore_ascii_case(&string[lenstr - lenpat + 1..]) {
        return false;
    }
    // :70 — `strchr(string, '.') < string + lenstr - lenpat`; the suffix
    // just compared starts with a dot, so there is always one.
    string
        .iter()
        .position(|&byte| byte == b'.')
        .is_some_and(|dot| dot >= lenstr - lenpat)
}

/// `pq_verify_peer_name_matches_certificate_ip`, `fe-secure-common.c:156`:
/// the host, read as an address of the certificate entry's length, against
/// the entry; and the entry as `pg_inet_net_ntop` prints it.
fn matches_certificate_ip(host: &[u8], ip: &[u8]) -> Result<(bool, Vec<u8>), PeerNameError> {
    let (is_match, shown) = match ip.len() {
        4 => (
            inet_aton(host).is_some_and(|addr| addr[..] == *ip),
            ip.iter().map(u8::to_string).collect::<Vec<_>>().join("."),
        ),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(ip);
            (
                inet_pton6(host).is_some_and(|addr| addr == octets),
                inet_net_ntop_ipv6(&octets),
            )
        }
        len => return Err(PeerNameError::InvalidIpLength(len)),
    };
    Ok((is_match, shown.into_bytes()))
}

/// `is_ip_address`, `fe-secure-openssl.c:556`.
fn is_ip_address(host: &[u8]) -> bool {
    inet_aton(host).is_some() || inet_pton6(host).is_some()
}

/// Calculation: `inet_aton`, which libpq uses on purpose for its lenient
/// forms (`fe-secure-common.c:192`): one to four parts, each decimal, octal
/// with a leading `0` or hex with `0x`; the last part fills the bytes left.
/// So `192.000.002.001` is `192.0.2.1`, and so is `3221225985`.
///
/// Trailing text is refused as musl refuses it; glibc and the BSDs accept an
/// address followed by whitespace and anything (`docs/divergences.md`).
fn inet_aton(host: &[u8]) -> Option<[u8; 4]> {
    let mut parts: Vec<u32> = Vec::with_capacity(4);
    let mut rest = host;
    loop {
        let (value, after) = strtoul(rest)?;
        parts.push(value);
        match after.split_first() {
            None => break,
            Some((b'.', next)) if parts.len() < 4 => rest = next,
            Some(_) => return None,
        }
    }
    let (last, leading) = parts.split_last()?;
    let mut addr = [0u8; 4];
    for (byte, part) in addr.iter_mut().zip(leading) {
        *byte = u8::try_from(*part).ok()?;
    }
    let tail = &mut addr[leading.len()..];
    let last = last.to_be_bytes();
    // The last part must fit the bytes it fills (glibc's `max[]`).
    if last[..4 - tail.len()].iter().any(|&byte| byte != 0) {
        return None;
    }
    tail.copy_from_slice(&last[4 - tail.len()..]);
    Some(addr)
}

/// Calculation: `strtoul(s, &end, 0)` for an input that must start with a
/// digit, as `inet_aton` requires, and must fit 32 bits.
fn strtoul(input: &[u8]) -> Option<(u32, &[u8])> {
    let (radix, digits) = match input {
        [b'0', b'x' | b'X', next, ..] if next.is_ascii_hexdigit() => (16, &input[2..]),
        [b'0', ..] => (8, input),
        [first, ..] if first.is_ascii_digit() => (10, input),
        _ => return None,
    };
    let len = digits
        .iter()
        .take_while(|byte| char::from(**byte).is_digit(radix))
        .count();
    let mut value: u32 = 0;
    for byte in &digits[..len] {
        let digit = char::from(*byte).to_digit(radix)?;
        value = value.checked_mul(radix)?.checked_add(digit)?;
    }
    Some((value, &digits[len..]))
}

/// Calculation: `inet_pton(AF_INET6, …)`. The standard library's parser takes
/// the same text: up to eight groups of one to four hex digits, one `::`,
/// and an optional dotted-quad tail without leading zeros.
fn inet_pton6(host: &[u8]) -> Option<[u8; 16]> {
    std::str::from_utf8(host)
        .ok()?
        .parse::<Ipv6Addr>()
        .ok()
        .map(|addr| addr.octets())
}

/// Calculation: `inet_net_ntop_ipv6` (`src/port/inet_net_ntop.c:178`) for a
/// whole address (`bits` 128): lower-case hex, the longest run of two or more
/// zero groups — the first, on a tie — as `::`, and the last four bytes as a
/// dotted quad for an IPv4-compatible or -mapped address.
fn inet_net_ntop_ipv6(src: &[u8; 16]) -> String {
    let words: Vec<u16> = src
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    // :214 — the longest run of zero words.
    let mut best: Option<(usize, usize)> = None;
    let mut cur: Option<(usize, usize)> = None;
    for (i, &word) in words.iter().enumerate() {
        if word == 0 {
            cur = Some(cur.map_or((i, 1), |(base, len)| (base, len + 1)));
        } else if let Some(run) = cur.take()
            && best.is_none_or(|(_, len)| run.1 > len)
        {
            best = Some(run);
        }
    }
    if let Some(run) = cur
        && best.is_none_or(|(_, len)| run.1 > len)
    {
        best = Some(run);
    }
    let best = best.filter(|&(_, len)| len >= 2);

    // :244 — format.
    let mut out = String::new();
    for (i, &word) in words.iter().enumerate() {
        if let Some((base, len)) = best
            && (base..base + len).contains(&i)
        {
            if i == base {
                out.push(':');
            }
            continue;
        }
        if i != 0 {
            out.push(':');
        }
        // :259 — an encapsulated IPv4 address.
        if i == 6
            && let Some((0, len)) = best
            && (len == 6 || (len == 7 && words[7] != 0x0001) || (len == 5 && words[5] == 0xffff))
        {
            let quad: Vec<String> = src[12..].iter().map(u8::to_string).collect();
            out.push_str(&quad.join("."));
            break;
        }
        let _ = write!(out, "{word:x}");
    }
    // :278 — a trailing run.
    if best.is_some_and(|(base, len)| base + len == words.len()) {
        out.push(':');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sans: Vec<GeneralName>, cn: Option<&[u8]>) -> CertificateNames {
        CertificateNames {
            subject_alt_names: sans,
            common_name: cn.map(<[u8]>::to_vec),
        }
    }

    fn verify_full(host: &str, names: &CertificateNames) -> Result<(), String> {
        verify_peer_name_matches_certificate(SslMode::VerifyFull, host.as_bytes(), names)
            .map_err(|err| err.to_string())
    }

    /// `fe-secure-common.c:32`-`:42`: one leading `*`, standing for one
    /// component, case-insensitive.
    #[test]
    fn the_wildcard_stands_for_one_leading_component() {
        assert!(wildcard_certificate_match(
            b"*.example.com",
            b"db.example.com"
        ));
        assert!(wildcard_certificate_match(
            b"*.EXAMPLE.com",
            b"DB.example.COM"
        ));
        assert!(!wildcard_certificate_match(
            b"*.example.com",
            b"a.db.example.com"
        ));
        assert!(!wildcard_certificate_match(
            b"*.example.com",
            b"example.com"
        ));
        assert!(!wildcard_certificate_match(
            b"*.example.com",
            b".example.com"
        ));
        assert!(!wildcard_certificate_match(b"db.*.com", b"db.example.com"));
        assert!(!wildcard_certificate_match(
            b"*example.com",
            b"dbexample.com"
        ));
        assert!(!wildcard_certificate_match(b"*.", b"a."));
        // :70 lets the component the `*` stands for end in a dot of its own:
        // `strchr` finds it at `lenstr - lenpat`, which is not left of it.
        assert!(wildcard_certificate_match(b"*.b", b"a..b"));
    }

    /// `fe-secure-common.c:120`.
    #[test]
    fn a_name_with_a_nul_is_an_error_not_a_mismatch() {
        let cert = names(
            vec![GeneralName::Dns(b"evil.test\0.good.test".to_vec())],
            None,
        );
        assert_eq!(
            verify_full("evil.test", &cert),
            Err("SSL certificate's name contains embedded null".into())
        );
    }

    /// `fe-secure-common.c:222`: an address entry of any length but 4 or 16
    /// is an error, even for a DNS host.
    #[test]
    fn an_address_of_the_wrong_length_is_an_error() {
        let cert = names(vec![GeneralName::IpAddress(vec![192, 0, 2])], None);
        assert_eq!(
            verify_full("db.test", &cert),
            Err("certificate contains IP address with invalid length 3".into())
        );
    }

    /// `fe-secure-common.c:263`, `:267`.
    #[test]
    fn only_verify_full_needs_a_host() {
        let cert = CertificateNames::default();
        for mode in [SslMode::Require, SslMode::VerifyCa] {
            assert_eq!(
                verify_peer_name_matches_certificate(mode, b"", &cert),
                Ok(())
            );
        }
        assert_eq!(
            verify_full("", &cert),
            Err("host name must be specified for a verified SSL connection".into())
        );
    }

    /// `fe-secure-openssl.c:630`-`:669`: an entry of the other kind is
    /// still examined and counted, and a match anywhere ends the search.
    #[test]
    fn every_dns_and_ip_entry_is_examined_in_order() {
        let cert = names(
            vec![
                GeneralName::Other,
                GeneralName::IpAddress(vec![192, 0, 2, 9]),
                GeneralName::Dns(b"a.test".to_vec()),
                GeneralName::Dns(b"b.test".to_vec()),
            ],
            Some(b"cn.test"),
        );
        assert_eq!(verify_full("b.test", &cert), Ok(()));
        assert_eq!(
            verify_full("c.test", &cert),
            Err(
                "server certificate for \"192.0.2.9\" (and 2 other names) does not match host name \"c.test\""
                    .into()
            )
        );
        // An IP SAN rules the CN out for an IP host …
        assert_eq!(
            verify_full("192.0.2.1", &cert),
            Err(
                "server certificate for \"192.0.2.9\" (and 2 other names) does not match host name \"192.0.2.1\""
                    .into()
            )
        );
        // … but DNS SANs alone do not (:598): the CN is tried, and counted.
        let dns_only = names(
            vec![GeneralName::Dns(b"a.test".to_vec())],
            Some(b"192.0.2.1"),
        );
        assert_eq!(verify_full("192.0.2.1", &dns_only), Ok(()));
        assert_eq!(
            verify_full("192.0.2.2", &dns_only),
            Err(
                "server certificate for \"a.test\" (and 1 other name) does not match host name \"192.0.2.2\""
                    .into()
            )
        );
        assert_eq!(
            verify_full("x.test", &names(vec![GeneralName::Other], None)),
            Err("could not get server's host name from server certificate".into())
        );
    }

    /// `inet_aton`'s forms, as `fe-secure-common.c:192` wants them.
    #[test]
    fn inet_aton_takes_the_lenient_forms() {
        let addr = Some([192, 0, 2, 1]);
        assert_eq!(inet_aton(b"192.0.2.1"), addr);
        assert_eq!(inet_aton(b"192.000.002.001"), addr);
        assert_eq!(inet_aton(b"0300.0.2.1"), addr);
        assert_eq!(inet_aton(b"0xc0.0.0x2.1"), addr);
        assert_eq!(inet_aton(b"192.0.513"), addr);
        assert_eq!(inet_aton(b"192.513"), addr);
        assert_eq!(inet_aton(b"3221225985"), addr);
        assert_eq!(inet_aton(b"0xffffffff"), Some([255; 4]));
        for bad in [
            &b""[..],
            b"192.0.2.256",
            b"192.0.2.1.",
            b"192.0.2.1.5",
            b"192..2.1",
            b".192.0.2.1",
            b"08.0.0.1",
            b"0x.0.0.1",
            b"192.0.65536",
            b"0x100000000",
            b"4294967296",
            b"+1.2.3.4",
            b" 1.2.3.4",
            b"1.2.3.4 ",
            b"db.test",
            b"2001:db8::1",
        ] {
            assert_eq!(inet_aton(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    /// `inet_pton(AF_INET6, …)`'s forms.
    #[test]
    fn inet_pton6_takes_the_standard_forms() {
        let one = Some([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(inet_pton6(b"2001:DB8::1"), one);
        assert_eq!(inet_pton6(b"2001:db8:0:0:0:0:0:1"), one);
        assert_eq!(inet_pton6(b"2001:db8::0.0.0.1"), one);
        assert_eq!(inet_pton6(b"2001:0db8:0000::0001"), one);
        assert!(inet_pton6(b"1:2:3:4:5:6:7::").is_some());
        assert!(inet_pton6(b"::1:2:3:4:5:6:7").is_some());
        assert!(inet_pton6(b"::").is_some());
        assert!(inet_pton6(b"::ffff:192.0.2.1").is_some());
        for bad in [
            &b"2001:DB8::1/128"[..],
            b"2001:db8::00001",
            b"2001::db8::1",
            b"1:2:3:4:5:6:7:8:9",
            b"fe80::1%eth0",
            b"::192.0.2.01",
            b"[::1]",
            b"192.0.2.1",
        ] {
            assert_eq!(inet_pton6(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    /// `inet_net_ntop_ipv6`, `src/port/inet_net_ntop.c:178`, with 128 bits.
    #[test]
    fn an_ipv6_address_prints_as_pg_inet_net_ntop_prints_it() {
        let cases: [(&str, &str); 10] = [
            ("2001:DB8:0:0:0:0:0:1", "2001:db8::1"),
            ("::", "::"),
            ("::1", "::1"),
            ("1::", "1::"),
            ("1:0:0:2:0:0:0:3", "1:0:0:2::3"),
            ("1:0:0:2:0:0:3:4", "1::2:0:0:3:4"),
            ("1:0:2:3:4:5:6:7", "1:0:2:3:4:5:6:7"),
            ("::ffff:192.0.2.1", "::ffff:192.0.2.1"),
            ("::192.0.2.1", "::192.0.2.1"),
            ("::1:192.0.2.1", "::1:c000:201"),
        ];
        for (input, printed) in cases {
            let octets = input.parse::<Ipv6Addr>().unwrap().octets();
            assert_eq!(inet_net_ntop_ipv6(&octets), printed, "{input}");
        }
    }

    /// A multi-valued RDN, the CN not first: the first CN in order wins.
    #[test]
    fn the_first_common_name_is_the_one() {
        // SET { SEQUENCE { OID 2.5.4.11, "u" }, SEQUENCE { OID 2.5.4.3, "a" } },
        // SET { SEQUENCE { OID 2.5.4.3, "b" } }.
        let name = [
            0x31, 0x14, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x0b, 0x0c, 0x01, b'u', 0x30, 0x08,
            0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x01, b'a', 0x31, 0x0a, 0x30, 0x08, 0x06, 0x03,
            0x55, 0x04, 0x03, 0x13, 0x01, b'b',
        ];
        assert_eq!(first_common_name(&name), Ok(Some(b"a".to_vec())));
        assert_eq!(first_common_name(&name[..0x16]), Ok(Some(b"a".to_vec())));
        assert_eq!(first_common_name(&[]), Ok(None));
        assert_eq!(first_common_name(&[0x31, 0x05]), Err(Malformed));
    }

    /// `X509_get_ext_d2i` returns `NULL` for a repeated extension, so the
    /// names in it do not count; a critical flag is skipped.
    #[test]
    fn a_repeated_subject_alt_name_extension_counts_as_none() {
        // SEQUENCE { OID 2.5.29.17, BOOLEAN TRUE, OCTET STRING { SEQUENCE { [2] "a" } } }
        let one = [
            0x30, 0x0f, 0x06, 0x03, 0x55, 0x1d, 0x11, 0x01, 0x01, 0xff, 0x04, 0x05, 0x30, 0x03,
            0x82, 0x01, b'a',
        ];
        let mut list = vec![0x30, 0x11];
        list.extend_from_slice(&one);
        assert_eq!(
            subject_alt_names(&list),
            Some(vec![GeneralName::Dns(b"a".to_vec())])
        );
        let mut twice = vec![0x30, 0x22];
        twice.extend_from_slice(&one);
        twice.extend_from_slice(&one);
        assert_eq!(subject_alt_names(&twice), Some(Vec::new()));
    }
}
