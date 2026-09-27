//! A DER reader just large enough for the fields libpq takes out of a server
//! certificate: the SubjectPublicKeyInfo (`crate::tls`), and the subject and
//! subjectAltName that the host-name check reads (`crate::peer_name`).
//! Every function is pure, and fails with `None` on anything malformed.

/// `SEQUENCE`, `SEQUENCE OF`.
pub(crate) const SEQUENCE: u8 = 0x30;
/// `SET`, `SET OF`.
pub(crate) const SET: u8 = 0x31;
/// `OBJECT IDENTIFIER`.
pub(crate) const OID: u8 = 0x06;
/// `BOOLEAN`.
pub(crate) const BOOLEAN: u8 = 0x01;
/// `BIT STRING`: a SubjectPublicKeyInfo's key, which only `crate::tls` reads.
#[cfg(feature = "tls")]
pub(crate) const BIT_STRING: u8 = 0x03;
/// `OCTET STRING`.
pub(crate) const OCTET_STRING: u8 = 0x04;
/// TBSCertificate's `version [0] EXPLICIT`, absent from an X.509 v1
/// certificate.
pub(crate) const VERSION: u8 = 0xa0;
/// TBSCertificate's `extensions [3] EXPLICIT`, v3 only.
pub(crate) const EXTENSIONS: u8 = 0xa3;

/// Calculation: one DER element — its tag, its contents, and what follows.
pub(crate) fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
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

/// Calculation: the contents of a certificate's TBSCertificate (RFC 5280
/// §4.1) from its `version` on, with the `version` itself skipped when present
/// — so a v1 and a v3 certificate both start at `serialNumber`.
pub(crate) fn tbs_fields(cert: &[u8]) -> Option<&[u8]> {
    let (SEQUENCE, certificate, _) = element(cert)? else {
        return None;
    };
    let (SEQUENCE, tbs, _) = element(certificate)? else {
        return None;
    };
    if tbs.first() == Some(&VERSION) {
        return Some(element(tbs)?.2);
    }
    Some(tbs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_element_is_split_off_its_tail() {
        assert_eq!(element(&[]), None);
        assert_eq!(element(&[0x30, 0x81]), None);
        assert_eq!(element(&[0x30, 0x02, 0x01]), None);
        assert_eq!(
            element(&[0x02, 0x81, 0x01, 0x07, 0xff]),
            Some((0x02, &[0x07][..], &[0xff][..]))
        );
    }

    #[test]
    fn the_version_is_skipped_only_when_present() {
        // Certificate { TBS { [0] { INTEGER 2 }, INTEGER 1 } }.
        let v3 = [
            0x30, 0x0a, 0x30, 0x08, 0xa0, 0x03, 0x02, 0x01, 0x02, 0x02, 0x01, 0x01,
        ];
        assert_eq!(tbs_fields(&v3), Some(&[0x02, 0x01, 0x01][..]));
        // Certificate { TBS { INTEGER 1 } }.
        let v1 = [0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x01];
        assert_eq!(tbs_fields(&v1), Some(&[0x02, 0x01, 0x01][..]));
        assert_eq!(tbs_fields(&[0x30, 0x05, 0x30]), None);
    }
}
