//! Port of the host-name rows of `src/test/ssl/t/001_ssltests.pl`
//! (PostgreSQL REL_18_6), `:293`-`:511` and `:549`-`:554`: which host a
//! `verify-full` connection accepts a server certificate for, and the message
//! it fails with otherwise.
//!
//! Upstream switches the server to each certificate (`switch_server_cert`)
//! and connects with `psql`; the certificates are upstream's own, vendored
//! byte for byte (`tests/ssl/README.md`). This port runs the check those
//! connections end in — `pq_verify_peer_name_matches_certificate`,
//! `fe-secure-common.c:251` — on the same certificate, host and `sslmode`,
//! and compares the whole message where upstream matches it as a substring
//! of stderr. It is a pure gate: no server, no handshake. The live
//! connections, and with them every row that is about the chain rather than
//! the name, come with chain verification (NAT-392).
//!
//! Rows are in upstream order and named after upstream's test names. Each
//! cites the line of its `connect_ok` / `connect_fails`.

#![allow(clippy::doc_markdown)]

use rlibpq::{CertificateNames, SslMode, verify_peer_name_matches_certificate};

/// Calculation: the DER of the first certificate in a PEM file.
fn der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .skip_while(|line| *line != "-----BEGIN CERTIFICATE-----")
        .skip(1)
        .take_while(|line| *line != "-----END CERTIFICATE-----")
        .collect();
    rlibpq::base64::decode(body.as_bytes()).expect("PEM body is base64")
}

/// The names in one of `tests/ssl/`'s certificates — the one
/// `switch_server_cert($node, certfile => …)` installs.
macro_rules! certfile {
    ($name:literal) => {
        CertificateNames::from_der(&der(include_str!(concat!("ssl/", $name, ".crt"))))
            .expect(concat!($name, ".crt parses"))
    };
}

/// Calculation: `connect_ok` (`None`) or `connect_fails` with this message.
fn check(names: &CertificateNames, sslmode: SslMode, host: &str) -> Option<String> {
    verify_peer_name_matches_certificate(sslmode, host.as_bytes(), names)
        .err()
        .map(|err| err.to_string())
}

fn connect_ok(names: &CertificateNames, sslmode: SslMode, host: &str) {
    assert_eq!(check(names, sslmode, host), None, "host={host}");
}

fn connect_fails(names: &CertificateNames, sslmode: SslMode, host: &str, expected: &str) {
    assert_eq!(
        check(names, sslmode, host).as_deref(),
        Some(expected),
        "host={host}"
    );
}

use SslMode::{Require, VerifyCa, VerifyFull};

// :140 — switch_server_cert($node, certfile => 'server-cn-only');

/// `:298`.
#[test]
fn mismatch_between_host_name_and_server_certificate_sslmode_require() {
    connect_ok(&certfile!("server-cn-only"), Require, "wronghost.test");
}

/// `:300`.
#[test]
fn mismatch_between_host_name_and_server_certificate_sslmode_verify_ca() {
    connect_ok(&certfile!("server-cn-only"), VerifyCa, "wronghost.test");
}

/// `:303`.
#[test]
fn mismatch_between_host_name_and_server_certificate_sslmode_verify_full() {
    connect_fails(
        &certfile!("server-cn-only"),
        VerifyFull,
        "wronghost.test",
        r#"server certificate for "common-name.pg-ssltest.test" does not match host name "wronghost.test""#,
    );
}

// :312 — switch_server_cert($node, certfile => 'server-ip-cn-only');

/// `:317`.
#[test]
fn ip_address_in_the_common_name() {
    connect_ok(&certfile!("server-ip-cn-only"), VerifyFull, "192.0.2.1");
}

/// `:320`: a CN is compared as a string, so the alternate form misses.
#[test]
fn mismatch_between_host_name_and_server_certificate_ip_address() {
    connect_fails(
        &certfile!("server-ip-cn-only"),
        VerifyFull,
        "192.000.002.001",
        r#"server certificate for "192.0.2.1" does not match host name "192.000.002.001""#,
    );
}

// :329 — switch_server_cert($node, certfile => 'server-ip-in-dnsname');

/// `:331`.
#[test]
fn ip_address_in_a_dnsname() {
    connect_ok(&certfile!("server-ip-in-dnsname"), VerifyFull, "192.0.2.1");
}

// :335 — switch_server_cert($node, certfile => 'server-multiple-alt-names');

/// `:340`.
#[test]
fn host_name_matching_with_x509_subject_alternative_names_1() {
    connect_ok(
        &certfile!("server-multiple-alt-names"),
        VerifyFull,
        "dns1.alt-name.pg-ssltest.test",
    );
}

/// `:343`.
#[test]
fn host_name_matching_with_x509_subject_alternative_names_2() {
    connect_ok(
        &certfile!("server-multiple-alt-names"),
        VerifyFull,
        "dns2.alt-name.pg-ssltest.test",
    );
}

/// `:346`.
#[test]
fn host_name_matching_with_x509_subject_alternative_names_wildcard() {
    connect_ok(
        &certfile!("server-multiple-alt-names"),
        VerifyFull,
        "foo.wildcard.pg-ssltest.test",
    );
}

/// `:349`.
#[test]
fn host_name_not_matching_with_x509_subject_alternative_names() {
    connect_fails(
        &certfile!("server-multiple-alt-names"),
        VerifyFull,
        "wronghost.alt-name.pg-ssltest.test",
        r#"server certificate for "dns1.alt-name.pg-ssltest.test" (and 2 other names) does not match host name "wronghost.alt-name.pg-ssltest.test""#,
    );
}

/// `:355`.
#[test]
fn host_name_not_matching_with_x509_subject_alternative_names_wildcard() {
    connect_fails(
        &certfile!("server-multiple-alt-names"),
        VerifyFull,
        "deep.subdomain.wildcard.pg-ssltest.test",
        r#"server certificate for "dns1.alt-name.pg-ssltest.test" (and 2 other names) does not match host name "deep.subdomain.wildcard.pg-ssltest.test""#,
    );
}

// :364 — switch_server_cert($node, certfile => 'server-single-alt-name');

/// `:369`.
#[test]
fn host_name_matching_with_a_single_x509_subject_alternative_name() {
    connect_ok(
        &certfile!("server-single-alt-name"),
        VerifyFull,
        "single.alt-name.pg-ssltest.test",
    );
}

/// `:373`.
#[test]
fn host_name_not_matching_with_a_single_x509_subject_alternative_name() {
    connect_fails(
        &certfile!("server-single-alt-name"),
        VerifyFull,
        "wronghost.alt-name.pg-ssltest.test",
        r#"server certificate for "single.alt-name.pg-ssltest.test" does not match host name "wronghost.alt-name.pg-ssltest.test""#,
    );
}

/// `:379`.
#[test]
fn host_name_not_matching_with_a_single_x509_subject_alternative_name_wildcard() {
    connect_fails(
        &certfile!("server-single-alt-name"),
        VerifyFull,
        "deep.subdomain.wildcard.pg-ssltest.test",
        r#"server certificate for "single.alt-name.pg-ssltest.test" does not match host name "deep.subdomain.wildcard.pg-ssltest.test""#,
    );
}

// :392 — switch_server_cert($node, certfile => 'server-ip-alt-names'). The
// SKIP around it (:386) is for a build without inet_pton, never this one.

/// `:394`.
#[test]
fn host_matching_an_ipv4_address_subject_alternative_name_1() {
    connect_ok(&certfile!("server-ip-alt-names"), VerifyFull, "192.0.2.1");
}

/// `:397`: an iPAddress is compared as an address, so the alternate form
/// matches.
#[test]
fn host_matching_an_ipv4_address_in_alternate_form_subject_alternative_name_1() {
    connect_ok(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "192.000.002.001",
    );
}

/// `:402`.
#[test]
fn host_not_matching_an_ipv4_address_subject_alternative_name_1() {
    connect_fails(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "192.0.2.2",
        r#"server certificate for "192.0.2.1" (and 1 other name) does not match host name "192.0.2.2""#,
    );
}

/// `:409`.
#[test]
fn host_matching_an_ipv6_address_subject_alternative_name_2() {
    connect_ok(&certfile!("server-ip-alt-names"), VerifyFull, "2001:DB8::1");
}

/// `:412`.
#[test]
fn host_matching_an_ipv6_address_in_alternate_form_subject_alternative_name_2() {
    connect_ok(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "2001:db8:0:0:0:0:0:1",
    );
}

/// `:417`.
#[test]
fn host_matching_an_ipv6_address_in_mixed_form_subject_alternative_name_2() {
    connect_ok(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "2001:db8::0.0.0.1",
    );
}

/// `:422`.
#[test]
fn host_not_matching_an_ipv6_address_subject_alternative_name_2() {
    connect_fails(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "::1",
        r#"server certificate for "192.0.2.1" (and 1 other name) does not match host name "::1""#,
    );
}

/// `:429`.
#[test]
fn ipv6_host_with_cidr_mask_does_not_match() {
    connect_fails(
        &certfile!("server-ip-alt-names"),
        VerifyFull,
        "2001:DB8::1/128",
        r#"server certificate for "192.0.2.1" (and 1 other name) does not match host name "2001:DB8::1/128""#,
    );
}

// :439 — switch_server_cert($node, certfile => 'server-cn-and-alt-names').

/// `:444`.
#[test]
fn certificate_with_both_a_cn_and_sans_1() {
    connect_ok(
        &certfile!("server-cn-and-alt-names"),
        VerifyFull,
        "dns1.alt-name.pg-ssltest.test",
    );
}

/// `:446`.
#[test]
fn certificate_with_both_a_cn_and_sans_2() {
    connect_ok(
        &certfile!("server-cn-and-alt-names"),
        VerifyFull,
        "dns2.alt-name.pg-ssltest.test",
    );
}

/// `:448`.
#[test]
fn certificate_with_both_a_cn_and_sans_ignores_cn() {
    connect_fails(
        &certfile!("server-cn-and-alt-names"),
        VerifyFull,
        "common-name.pg-ssltest.test",
        r#"server certificate for "dns1.alt-name.pg-ssltest.test" (and 1 other name) does not match host name "common-name.pg-ssltest.test""#,
    );
}

// :461 — switch_server_cert($node, certfile => 'server-cn-and-ip-alt-names'),
// inside the same inet_pton SKIP (:455).

/// `:463`.
#[test]
fn certificate_with_both_a_cn_and_ip_sans_matches_cn() {
    connect_ok(
        &certfile!("server-cn-and-ip-alt-names"),
        VerifyFull,
        "common-name.pg-ssltest.test",
    );
}

/// `:466`.
#[test]
fn certificate_with_both_a_cn_and_ip_sans_matches_san_1() {
    connect_ok(
        &certfile!("server-cn-and-ip-alt-names"),
        VerifyFull,
        "192.0.2.1",
    );
}

/// `:468`.
#[test]
fn certificate_with_both_a_cn_and_ip_sans_matches_san_2() {
    connect_ok(
        &certfile!("server-cn-and-ip-alt-names"),
        VerifyFull,
        "2001:db8::1",
    );
}

// :472 — switch_server_cert($node, certfile => 'server-ip-cn-and-alt-names').

/// `:474`.
#[test]
fn certificate_with_both_an_ip_cn_and_ip_sans_1() {
    connect_ok(
        &certfile!("server-ip-cn-and-alt-names"),
        VerifyFull,
        "192.0.2.2",
    );
}

/// `:476`.
#[test]
fn certificate_with_both_an_ip_cn_and_ip_sans_2() {
    connect_ok(
        &certfile!("server-ip-cn-and-alt-names"),
        VerifyFull,
        "2001:db8::1",
    );
}

/// `:478`.
#[test]
fn certificate_with_both_an_ip_cn_and_ip_sans_ignores_cn() {
    connect_fails(
        &certfile!("server-ip-cn-and-alt-names"),
        VerifyFull,
        "192.0.2.1",
        r#"server certificate for "192.0.2.2" (and 1 other name) does not match host name "192.0.2.1""#,
    );
}

// :486 — switch_server_cert($node, certfile => 'server-ip-cn-and-dns-alt-names').

/// `:488`: DNS SANs do not rule out the CN for an IP host.
#[test]
fn certificate_with_both_an_ip_cn_and_dns_sans_matches_cn() {
    connect_ok(
        &certfile!("server-ip-cn-and-dns-alt-names"),
        VerifyFull,
        "192.0.2.1",
    );
}

/// `:490`.
#[test]
fn certificate_with_both_an_ip_cn_and_dns_sans_matches_san_1() {
    connect_ok(
        &certfile!("server-ip-cn-and-dns-alt-names"),
        VerifyFull,
        "dns1.alt-name.pg-ssltest.test",
    );
}

/// `:493`.
#[test]
fn certificate_with_both_an_ip_cn_and_dns_sans_matches_san_2() {
    connect_ok(
        &certfile!("server-ip-cn-and-dns-alt-names"),
        VerifyFull,
        "dns2.alt-name.pg-ssltest.test",
    );
}

// :499 — switch_server_cert($node, certfile => 'server-no-names').

/// `:503`.
#[test]
fn server_certificate_without_cn_or_sans_sslmode_verify_ca() {
    connect_ok(
        &certfile!("server-no-names"),
        VerifyCa,
        "common-name.pg-ssltest.test",
    );
}

/// `:506`.
#[test]
fn server_certificate_without_cn_or_sans_sslmode_verify_full() {
    connect_fails(
        &certfile!("server-no-names"),
        VerifyFull,
        "common-name.pg-ssltest.test",
        "could not get server's host name from server certificate",
    );
}

// :514 — switch_server_cert($node, certfile => 'server-cn-only+server_ca', …):
// the chain's first certificate, the one the name is read from, is
// server-cn-only.crt. `sslrootcert=system` makes the mode verify-full
// (fe-connect.c:6736; `conninfo`'s
// sslrootcert_system_strengthens_the_default_sslmode_to_verify_full).

/// `:549`.
#[test]
fn sslrootcert_system_defaults_to_sslmode_verify_full() {
    connect_fails(
        &certfile!("server-cn-only"),
        VerifyFull,
        "common-name.pg-ssltest.test.bad",
        r#"server certificate for "common-name.pg-ssltest.test" does not match host name "common-name.pg-ssltest.test.bad""#,
    );
}
