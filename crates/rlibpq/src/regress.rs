//! The printer `libpq_uri_regress` uses, as a calculation.
//!
//! `src/interfaces/libpq/test/libpq_uri_regress.c:52` walks the parsed options
//! and the defaults in lockstep and prints every option whose value differs
//! from its default, then says whether the connection would be a Unix-domain
//! socket or an inet one. Its own comment is the reason [`ConnInfo`] keeps
//! upstream's order: "XXX this coding assumes that PQconninfoOption structs
//! always have the keywords in the same order."

use crate::conninfo::ConnInfo;

/// The bytes `libpq_uri_regress` writes to stdout for a conninfo string that
/// parsed, trailing newline included.
#[must_use]
pub fn regress_report(parsed: &ConnInfo, defaults: &ConnInfo) -> Vec<u8> {
    let mut out = Vec::new();
    let mut local = true;

    for (option, default) in parsed.iter().zip(defaults.iter()) {
        let Some(value) = option.value else { continue };

        if default.value.is_none_or(|default| default != value) {
            out.extend_from_slice(option.def.keyword.as_bytes());
            out.extend_from_slice(b"='");
            out.extend_from_slice(value);
            out.extend_from_slice(b"' ");
        }

        // Try to detect if this is a Unix-domain socket or inet.  This is a
        // bit grotty but it's the same thing that libpq itself does.  Note
        // that we directly test for '/' instead of using is_absolute_path, as
        // that would be considerably more messy.
        if !value.is_empty()
            && (option.def.keyword == "hostaddr"
                || (option.def.keyword == "host" && value[0] != b'/'))
        {
            local = false;
        }
    }

    out.extend_from_slice(if local { b"(local)\n" } else { b"(inet)\n" });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conninfo::{Env, conndefaults, parse_conninfo};

    fn report(conninfo: &[u8], env: &Env) -> String {
        let parsed = parse_conninfo(conninfo).expect("parses");
        String::from_utf8(regress_report(&parsed, &conndefaults(env))).expect("ascii")
    }

    #[test]
    fn options_are_printed_in_the_option_tables_order() {
        assert_eq!(
            report(b"postgresql://uri-user:secret@host:12345/db", &Env::empty()),
            "user='uri-user' password='secret' dbname='db' host='host' port='12345' (inet)\n"
        );
    }

    /// The printer's whole job: only what differs from the default is shown,
    /// which is why `port=5432` disappears and `port=12345` does not.
    #[test]
    fn a_value_equal_to_its_default_is_not_printed() {
        assert_eq!(
            report(b"postgresql://host:5432/", &Env::empty()),
            "host='host' (inet)\n"
        );
        assert_eq!(
            report(b"postgresql://host:12345/", &Env::empty()),
            "host='host' port='12345' (inet)\n"
        );
    }

    /// A default that came from the environment counts too.
    #[test]
    fn a_value_equal_to_an_environment_default_is_not_printed() {
        let env = Env::empty().with("PGDATABASE", "db");
        assert_eq!(
            report(b"postgresql://host/db", &env),
            "host='host' (inet)\n"
        );
    }

    /// `libpq_uri_regress.c:60`: a host starting with `/` is a socket
    /// directory, `hostaddr` is always inet, and an empty value decides nothing.
    #[test]
    fn the_local_and_inet_verdict_follows_the_host_value() {
        assert_eq!(report(b"postgresql://", &Env::empty()), "(local)\n");
        assert_eq!(
            report(b"postgresql://host", &Env::empty()),
            "host='host' (inet)\n"
        );
        assert_eq!(
            report(b"postgres://?host=/path/to/socket/dir", &Env::empty()),
            "host='/path/to/socket/dir' (local)\n"
        );
        assert_eq!(
            report(b"postgresql://?hostaddr=127.0.0.1", &Env::empty()),
            "hostaddr='127.0.0.1' (inet)\n"
        );
        assert_eq!(report(b"host=''", &Env::empty()), "host='' (local)\n");
    }
}
