//! `PQconninfoOptions[]` and the key/value connection-string parser.
//!
//! Data / Calculations / Actions:
//!
//! - [`CONNINFO_OPTIONS`] is `fe-connect.c:200`'s table, transcribed in its
//!   order, which the `libpq_uri_regress` printer depends on ("XXX this coding
//!   assumes that `PQconninfoOption` structs always have the keywords in the
//!   same order", `libpq_uri_regress.c:50`).
//! - [`ConnInfo`] is one working copy of that table (`conninfo_init`,
//!   `fe-connect.c:6197`), and every parser here is a pure function over bytes.
//! - [`Env::from_process`] is the only action: reading this process's
//!   environment so [`conndefaults`] can fill the fallbacks in.

use std::collections::BTreeMap;

use crate::cstr::{at, is_space};
use crate::error::ConnError;
use crate::pg_config::{
    DEF_PGPORT_STR, DEFAULT_CHANNEL_BINDING, DEFAULT_GSS_MODE, DEFAULT_LOAD_BALANCE_HOSTS,
    DEFAULT_OPTION, DEFAULT_SSL_MODE, DEFAULT_SSL_NEGOTIATION, DEFAULT_TARGET_SESSION_ATTRS,
    PG_KRB_SRVNAM, SCRAM_MAX_KEY_LEN,
};
use crate::text::RawText;
use crate::uri;

/// How a connect dialog should show a field: `fe-connect.c:185`'s `dispchar`.
///
/// An enum rather than the `char *` C keeps, because the three values are the
/// whole domain and server-side clients branch on them (`fe-connect.c:163`:
/// postgres_fdw disallows setting `"D"` options at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispchar {
    /// `""` — display the entered value as is.
    Plain,
    /// `"*"` — password field, hide the value.
    Password,
    /// `"D"` — debug option, do not show by default.
    Debug,
}

impl Dispchar {
    /// The string `PQconninfoOption.dispchar` holds.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Dispchar::Plain => "",
            Dispchar::Password => "*",
            Dispchar::Debug => "D",
        }
    }
}

/// One row of `PQconninfoOptions[]`, minus the `connofs` field that only
/// exists to `memcpy` into a `PGconn` (`fe-connect.c:197`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnOptionDef {
    /// The keyword of the option.
    pub keyword: &'static str,
    /// Fallback environment variable name.
    pub envvar: Option<&'static str>,
    /// Fallback compiled-in default value.
    pub compiled: Option<&'static str>,
    /// Label for the field in a connect dialog.
    pub label: &'static str,
    /// How to display this field in a connect dialog.
    pub dispchar: Dispchar,
    /// Field size in characters for a dialog.
    pub dispsize: usize,
}

const fn option(
    keyword: &'static str,
    envvar: Option<&'static str>,
    compiled: Option<&'static str>,
    label: &'static str,
    dispchar: Dispchar,
    dispsize: usize,
) -> ConnOptionDef {
    ConnOptionDef {
        keyword,
        envvar,
        compiled,
        label,
        dispchar,
        dispsize,
    }
}

/// `fe-connect.c:200` — the connection parameters and their fallbacks, in
/// upstream's order, without the terminating NULL entry (a slice knows its own
/// length). Comments on `dispsize` are upstream's.
pub const CONNINFO_OPTIONS: [ConnOptionDef; 50] = [
    option(
        "service",
        Some("PGSERVICE"),
        None,
        "Database-Service",
        Dispchar::Plain,
        20,
    ),
    option(
        "user",
        Some("PGUSER"),
        None,
        "Database-User",
        Dispchar::Plain,
        20,
    ),
    option(
        "password",
        Some("PGPASSWORD"),
        None,
        "Database-Password",
        Dispchar::Password,
        20,
    ),
    option(
        "passfile",
        Some("PGPASSFILE"),
        None,
        "Database-Password-File",
        Dispchar::Plain,
        64,
    ),
    // sizeof("require") == 8
    option(
        "channel_binding",
        Some("PGCHANNELBINDING"),
        Some(DEFAULT_CHANNEL_BINDING),
        "Channel-Binding",
        Dispchar::Plain,
        8,
    ),
    // strlen(INT32_MAX) == 10
    option(
        "connect_timeout",
        Some("PGCONNECT_TIMEOUT"),
        None,
        "Connect-timeout",
        Dispchar::Plain,
        10,
    ),
    option(
        "dbname",
        Some("PGDATABASE"),
        None,
        "Database-Name",
        Dispchar::Plain,
        20,
    ),
    option(
        "host",
        Some("PGHOST"),
        None,
        "Database-Host",
        Dispchar::Plain,
        40,
    ),
    option(
        "hostaddr",
        Some("PGHOSTADDR"),
        None,
        "Database-Host-IP-Address",
        Dispchar::Plain,
        45,
    ),
    option(
        "port",
        Some("PGPORT"),
        Some(DEF_PGPORT_STR),
        "Database-Port",
        Dispchar::Plain,
        6,
    ),
    option(
        "client_encoding",
        Some("PGCLIENTENCODING"),
        None,
        "Client-Encoding",
        Dispchar::Plain,
        10,
    ),
    option(
        "options",
        Some("PGOPTIONS"),
        Some(DEFAULT_OPTION),
        "Backend-Options",
        Dispchar::Plain,
        40,
    ),
    option(
        "application_name",
        Some("PGAPPNAME"),
        None,
        "Application-Name",
        Dispchar::Plain,
        64,
    ),
    option(
        "fallback_application_name",
        None,
        None,
        "Fallback-Application-Name",
        Dispchar::Plain,
        64,
    ),
    // should be just '0' or '1'
    option(
        "keepalives",
        None,
        None,
        "TCP-Keepalives",
        Dispchar::Plain,
        1,
    ),
    option(
        "keepalives_idle",
        None,
        None,
        "TCP-Keepalives-Idle",
        Dispchar::Plain,
        10,
    ),
    option(
        "keepalives_interval",
        None,
        None,
        "TCP-Keepalives-Interval",
        Dispchar::Plain,
        10,
    ),
    option(
        "keepalives_count",
        None,
        None,
        "TCP-Keepalives-Count",
        Dispchar::Plain,
        10,
    ),
    option(
        "tcp_user_timeout",
        None,
        None,
        "TCP-User-Timeout",
        Dispchar::Plain,
        10,
    ),
    // sizeof("verify-full") == 12
    option(
        "sslmode",
        Some("PGSSLMODE"),
        Some(DEFAULT_SSL_MODE),
        "SSL-Mode",
        Dispchar::Plain,
        12,
    ),
    // sizeof("postgres") == 9
    option(
        "sslnegotiation",
        Some("PGSSLNEGOTIATION"),
        Some(DEFAULT_SSL_NEGOTIATION),
        "SSL-Negotiation",
        Dispchar::Plain,
        9,
    ),
    option(
        "sslcompression",
        Some("PGSSLCOMPRESSION"),
        Some("0"),
        "SSL-Compression",
        Dispchar::Plain,
        1,
    ),
    option(
        "sslcert",
        Some("PGSSLCERT"),
        None,
        "SSL-Client-Cert",
        Dispchar::Plain,
        64,
    ),
    option(
        "sslkey",
        Some("PGSSLKEY"),
        None,
        "SSL-Client-Key",
        Dispchar::Plain,
        64,
    ),
    // sizeof("disable") == 8
    option(
        "sslcertmode",
        Some("PGSSLCERTMODE"),
        None,
        "SSL-Client-Cert-Mode",
        Dispchar::Plain,
        8,
    ),
    option(
        "sslpassword",
        None,
        None,
        "SSL-Client-Key-Password",
        Dispchar::Password,
        20,
    ),
    option(
        "sslrootcert",
        Some("PGSSLROOTCERT"),
        None,
        "SSL-Root-Certificate",
        Dispchar::Plain,
        64,
    ),
    option(
        "sslcrl",
        Some("PGSSLCRL"),
        None,
        "SSL-Revocation-List",
        Dispchar::Plain,
        64,
    ),
    option(
        "sslcrldir",
        Some("PGSSLCRLDIR"),
        None,
        "SSL-Revocation-List-Dir",
        Dispchar::Plain,
        64,
    ),
    option(
        "sslsni",
        Some("PGSSLSNI"),
        Some("1"),
        "SSL-SNI",
        Dispchar::Plain,
        1,
    ),
    option(
        "requirepeer",
        Some("PGREQUIREPEER"),
        None,
        "Require-Peer",
        Dispchar::Plain,
        10,
    ),
    // sizeof("scram-sha-256") == 14
    option(
        "require_auth",
        Some("PGREQUIREAUTH"),
        None,
        "Require-Auth",
        Dispchar::Plain,
        14,
    ),
    // sizeof("latest") = 6
    option(
        "min_protocol_version",
        Some("PGMINPROTOCOLVERSION"),
        None,
        "Min-Protocol-Version",
        Dispchar::Plain,
        6,
    ),
    option(
        "max_protocol_version",
        Some("PGMAXPROTOCOLVERSION"),
        None,
        "Max-Protocol-Version",
        Dispchar::Plain,
        6,
    ),
    // sizeof("TLSv1.x") == 8
    option(
        "ssl_min_protocol_version",
        Some("PGSSLMINPROTOCOLVERSION"),
        Some("TLSv1.2"),
        "SSL-Minimum-Protocol-Version",
        Dispchar::Plain,
        8,
    ),
    option(
        "ssl_max_protocol_version",
        Some("PGSSLMAXPROTOCOLVERSION"),
        None,
        "SSL-Maximum-Protocol-Version",
        Dispchar::Plain,
        8,
    ),
    // sizeof("disable") == 8
    option(
        "gssencmode",
        Some("PGGSSENCMODE"),
        Some(DEFAULT_GSS_MODE),
        "GSSENC-Mode",
        Dispchar::Plain,
        8,
    ),
    option(
        "krbsrvname",
        Some("PGKRBSRVNAME"),
        Some(PG_KRB_SRVNAM),
        "Kerberos-service-name",
        Dispchar::Plain,
        20,
    ),
    // sizeof("gssapi") == 7
    option(
        "gsslib",
        Some("PGGSSLIB"),
        None,
        "GSS-library",
        Dispchar::Plain,
        7,
    ),
    option(
        "gssdelegation",
        Some("PGGSSDELEGATION"),
        Some("0"),
        "GSS-delegation",
        Dispchar::Plain,
        1,
    ),
    option("replication", None, None, "Replication", Dispchar::Debug, 5),
    // sizeof("prefer-standby") = 15
    option(
        "target_session_attrs",
        Some("PGTARGETSESSIONATTRS"),
        Some(DEFAULT_TARGET_SESSION_ATTRS),
        "Target-Session-Attrs",
        Dispchar::Plain,
        15,
    ),
    // sizeof("disable") = 8
    option(
        "load_balance_hosts",
        Some("PGLOADBALANCEHOSTS"),
        Some(DEFAULT_LOAD_BALANCE_HOSTS),
        "Load-Balance-Hosts",
        Dispchar::Plain,
        8,
    ),
    option(
        "scram_client_key",
        None,
        None,
        "SCRAM-Client-Key",
        Dispchar::Debug,
        SCRAM_MAX_KEY_LEN * 2,
    ),
    option(
        "scram_server_key",
        None,
        None,
        "SCRAM-Server-Key",
        Dispchar::Debug,
        SCRAM_MAX_KEY_LEN * 2,
    ),
    option(
        "oauth_issuer",
        None,
        None,
        "OAuth-Issuer",
        Dispchar::Plain,
        40,
    ),
    option(
        "oauth_client_id",
        None,
        None,
        "OAuth-Client-ID",
        Dispchar::Plain,
        40,
    ),
    option(
        "oauth_client_secret",
        None,
        None,
        "OAuth-Client-Secret",
        Dispchar::Password,
        40,
    ),
    option(
        "oauth_scope",
        None,
        None,
        "OAuth-Scope",
        Dispchar::Plain,
        15,
    ),
    option(
        "sslkeylogfile",
        None,
        None,
        "SSL-Key-Log-File",
        Dispchar::Debug,
        64,
    ),
];

/// `conninfo_find` (`fe-connect.c:7397`) knew no such keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKeyword(RawText);

impl UnknownKeyword {
    #[must_use]
    pub fn keyword(&self) -> &RawText {
        &self.0
    }

    #[must_use]
    pub fn into_keyword(self) -> RawText {
        self.0
    }
}

/// One row of a working copy: its definition and its current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnOption<'a> {
    pub def: &'static ConnOptionDef,
    /// `None` is C's NULL `val`: the option was not given and has no default.
    pub value: Option<&'a [u8]>,
}

/// A working copy of `PQconninfoOptions[]` (`conninfo_init`,
/// `fe-connect.c:6197`): the same rows in the same order, each with a value or
/// not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnInfo {
    values: [Option<RawText>; CONNINFO_OPTIONS.len()],
}

impl Default for ConnInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnInfo {
    /// `conninfo_init`: every row present, every value NULL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: std::array::from_fn(|_| None),
        }
    }

    /// `conninfo_find` (`fe-connect.c:7397`): a byte-for-byte keyword match,
    /// which is what `strcmp` does.
    #[must_use]
    pub fn index_of(keyword: &[u8]) -> Option<usize> {
        CONNINFO_OPTIONS
            .iter()
            .position(|option| option.keyword.as_bytes() == keyword)
    }

    /// `conninfo_getval` (`fe-connect.c:7307`).
    #[must_use]
    pub fn get(&self, keyword: &str) -> Option<&[u8]> {
        let index = Self::index_of(keyword.as_bytes())?;
        self.values[index].as_ref().map(RawText::as_bytes)
    }

    /// `conninfo_storeval` (`fe-connect.c:7333`) with `uri_decode` false: the
    /// value replaces any previous one.
    ///
    /// The `requiressl` compatibility rewrite lives here because it lives in
    /// `conninfo_storeval` upstream, so it applies to a URI query parameter
    /// exactly as it applies to a `key=value` pair.
    ///
    /// # Errors
    /// [`UnknownKeyword`] when the table has no such option. C's `ignoreMissing`
    /// is the caller's decision here: `conninfo_parse` turns it into
    /// [`ConnError::InvalidConnectionOption`] and the URI query parser into
    /// [`ConnError::InvalidUriQueryParameter`].
    pub fn set(&mut self, keyword: &[u8], value: &[u8]) -> Result<(), UnknownKeyword> {
        // fe-connect.c:7343: requiressl=1 means sslmode=require, anything else
        // means sslmode=prefer (which is the default for sslmode).
        let (keyword, value): (&[u8], &[u8]) = if keyword == b"requiressl" {
            if at(value, 0) == b'1' {
                (b"sslmode", b"require")
            } else {
                (b"sslmode", b"prefer")
            }
        } else {
            (keyword, value)
        };
        let index = Self::index_of(keyword).ok_or_else(|| UnknownKeyword(keyword.into()))?;
        self.values[index] = Some(value.into());
        Ok(())
    }

    /// The rows in `PQconninfoOptions[]` order.
    pub fn iter(&self) -> impl Iterator<Item = ConnOption<'_>> {
        CONNINFO_OPTIONS
            .iter()
            .zip(self.values.iter())
            .map(|(def, value)| ConnOption {
                def,
                value: value.as_ref().map(RawText::as_bytes),
            })
    }

    /// `conninfo_add_defaults` (`fe-connect.c:6624`): fill every unset option
    /// from its environment variable, then its compiled-in default.
    ///
    /// Failure to find a default is not an error upstream either — the value
    /// just stays NULL — so this returns nothing. The one thing it cannot do
    /// is `parseServiceInfo`; see `docs/divergences.md` and NAT-393.
    pub fn add_defaults(&mut self, env: &Env) {
        let mut sslmode_default = None;
        for (index, def) in CONNINFO_OPTIONS.iter().enumerate() {
            if self.values[index].is_some() {
                continue; // Value was in conninfo or service
            }
            if let Some(value) = def.envvar.and_then(|var| env.get(var)) {
                self.values[index] = Some(value.into());
                continue;
            }
            if def.keyword == "sslmode" {
                // fe-connect.c:6673: the deprecated PGREQUIRESSL, where a value
                // starting with "1" means sslmode=require and anything else is
                // ignored. PGSSLMODE has already won above if it was set.
                if env.get("PGREQUIRESSL").is_some_and(|v| at(v, 0) == b'1') {
                    self.values[index] = Some("require".into());
                    continue;
                }
                // Let the compiled default stand for now; sslrootcert=system
                // overrides it below.
                sslmode_default = Some(index);
            }
            if let Some(compiled) = def.compiled {
                self.values[index] = Some(compiled.into());
                continue;
            }
            if def.keyword == "user" {
                self.values[index] = env.effective_user().map(RawText::new);
            }
        }
        // fe-connect.c:6730: sslrootcert=system with no explicit sslmode
        // strengthens the default to verify-full.
        if let Some(index) = sslmode_default
            && self.get("sslrootcert") == Some(b"system")
        {
            self.values[index] = Some("verify-full".into());
        }
    }
}

/// The environment `conninfo_add_defaults` reads its fallbacks from.
///
/// A value, not a `getenv` call, so filling in defaults stays a calculation:
/// every unit test below states the environment it is talking about instead of
/// depending on the one the test runner happens to have.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, Vec<u8>>,
}

impl Env {
    /// An environment with nothing in it.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Action: read this process's environment.
    ///
    /// Values are bytes: `PGOPTIONS` can hold anything the shell can export,
    /// and libpq passes it through without decoding it. Keys that are not
    /// UTF-8 cannot name a conninfo option, so they are dropped.
    #[must_use]
    pub fn from_process() -> Self {
        let vars = std::env::vars_os()
            .filter_map(|(key, value)| {
                let key = key.into_string().ok()?;
                Some((key, value.as_encoded_bytes().to_vec()))
            })
            .collect();
        Self { vars }
    }

    /// Set one variable, for a test that states its own environment.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<Vec<u8>>) -> Self {
        self.vars.insert(key.to_owned(), value.into());
        self
    }

    /// `getenv` (`fe-connect.c:6656`).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.vars.get(key).map(Vec::as_slice)
    }

    /// `pg_fe_getauthname(NULL)` (`fe-connect.c:6724`), which is
    /// `pg_fe_getusername(geteuid())` (`fe-auth.c:1349`) and so a `getpwuid`
    /// lookup.
    ///
    /// Neither call is reachable from the standard library, this crate is
    /// `#![deny(unsafe_code)]` and no libc dependency is approved (AGENTS.md),
    /// so the name comes from `USER`/`LOGNAME` — the same substitution
    /// `rinitdb::validate` makes for `get_id()`. See `docs/divergences.md`.
    /// An exported but empty variable is not a user name, so `USER=""` falls
    /// through to `LOGNAME` rather than answering "nobody".
    #[must_use]
    pub fn effective_user(&self) -> Option<&[u8]> {
        self.named_user("USER")
            .or_else(|| self.named_user("LOGNAME"))
    }

    fn named_user(&self, key: &str) -> Option<&[u8]> {
        self.get(key).filter(|name| !name.is_empty())
    }
}

/// `PQconndefaults` (`fe-connect.c:2193`): a working copy with nothing set,
/// then all the defaults filled in.
#[must_use]
pub fn conndefaults(env: &Env) -> ConnInfo {
    let mut options = ConnInfo::new();
    options.add_defaults(env);
    options
}

/// `uri_prefix_length` (`fe-connect.c:6256`): how long the URI designator is,
/// or 0 when the string does not start with one.
#[must_use]
pub fn uri_prefix_length(connstr: &[u8]) -> usize {
    const URI_DESIGNATOR: &[u8] = b"postgresql://";
    const SHORT_URI_DESIGNATOR: &[u8] = b"postgres://";
    if connstr.starts_with(URI_DESIGNATOR) {
        URI_DESIGNATOR.len()
    } else if connstr.starts_with(SHORT_URI_DESIGNATOR) {
        SHORT_URI_DESIGNATOR.len()
    } else {
        0
    }
}

/// `recognized_connection_string` (`fe-connect.c:6279`).
#[must_use]
pub fn recognized_connection_string(connstr: &[u8]) -> bool {
    uri_prefix_length(connstr) != 0 || connstr.contains(&b'=')
}

/// `PQconninfoParse` (`fe-connect.c:6175`), which is `parse_connection_string`
/// (`fe-connect.c:6236`) with `use_defaults` false: a URI if it starts with a
/// designator, `key=value` pairs otherwise.
///
/// # Errors
/// The [`ConnError`] libpq would have left in its error buffer.
pub fn parse_conninfo(conninfo: &[u8]) -> Result<ConnInfo, ConnError> {
    if uri_prefix_length(conninfo) == 0 {
        parse_keyword_value(conninfo)
    } else {
        uri::parse_uri(conninfo)
    }
}

/// `conninfo_parse` (`fe-connect.c:6290`) with `use_defaults` false: a string
/// of `key = value` pairs, where a value may be `'`-quoted and `\` escapes the
/// next byte in either form.
///
/// # Errors
/// [`ConnError::MissingEquals`], [`ConnError::UnterminatedQuotedString`] or
/// [`ConnError::InvalidConnectionOption`].
pub fn parse_keyword_value(conninfo: &[u8]) -> Result<ConnInfo, ConnError> {
    let mut options = ConnInfo::new();
    let buf = conninfo;
    let mut cp = 0usize;

    while at(buf, cp) != 0 {
        // Skip blanks before the parameter name
        if is_space(at(buf, cp)) {
            cp += 1;
            continue;
        }

        // Get the parameter name
        let name_start = cp;
        let mut name_end = None;
        while at(buf, cp) != 0 {
            if at(buf, cp) == b'=' {
                break;
            }
            if is_space(at(buf, cp)) {
                name_end = Some(cp);
                cp += 1;
                while at(buf, cp) != 0 && is_space(at(buf, cp)) {
                    cp += 1;
                }
                break;
            }
            cp += 1;
        }
        let name_end = name_end.unwrap_or(cp);

        // Check that there is a following '='
        if at(buf, cp) != b'=' {
            return Err(ConnError::MissingEquals(buf[name_start..name_end].into()));
        }
        cp += 1;

        // Skip blanks after the '='
        while at(buf, cp) != 0 && is_space(at(buf, cp)) {
            cp += 1;
        }

        let value = if at(buf, cp) == b'\'' {
            cp += 1;
            read_quoted_value(buf, &mut cp)?
        } else {
            read_bare_value(buf, &mut cp)
        };

        options
            .set(&buf[name_start..name_end], &value)
            .map_err(|unknown| ConnError::InvalidConnectionOption(unknown.into_keyword()))?;
    }

    Ok(options)
}

/// The unquoted arm of `conninfo_parse`'s value scanner (`fe-connect.c:6366`):
/// runs to the first unescaped space or to the end.
fn read_bare_value(buf: &[u8], cp: &mut usize) -> Vec<u8> {
    let mut value = Vec::new();
    while at(buf, *cp) != 0 {
        if is_space(at(buf, *cp)) {
            *cp += 1;
            break;
        }
        if at(buf, *cp) == b'\\' {
            *cp += 1;
            if at(buf, *cp) != 0 {
                value.push(at(buf, *cp));
                *cp += 1;
            }
        } else {
            value.push(at(buf, *cp));
            *cp += 1;
        }
    }
    value
}

/// The quoted arm (`fe-connect.c:6387`): `cp` is already past the opening
/// quote; runs to the closing one.
fn read_quoted_value(buf: &[u8], cp: &mut usize) -> Result<Vec<u8>, ConnError> {
    let mut value = Vec::new();
    loop {
        if at(buf, *cp) == 0 {
            return Err(ConnError::UnterminatedQuotedString);
        }
        if at(buf, *cp) == b'\\' {
            *cp += 1;
            if at(buf, *cp) != 0 {
                value.push(at(buf, *cp));
                *cp += 1;
            }
            continue;
        }
        if at(buf, *cp) == b'\'' {
            *cp += 1;
            return Ok(value);
        }
        value.push(at(buf, *cp));
        *cp += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keywords of `PQconninfoOptions[]` (`fe-connect.c:200`), in its
    /// order. `libpq_uri_regress` walks the parsed options and the defaults in
    /// lockstep and trusts that order (`libpq_uri_regress.c:50`), so a row
    /// moved here is a wrong answer there, not a cosmetic change.
    const UPSTREAM_KEYWORDS: [&str; 50] = [
        "service",
        "user",
        "password",
        "passfile",
        "channel_binding",
        "connect_timeout",
        "dbname",
        "host",
        "hostaddr",
        "port",
        "client_encoding",
        "options",
        "application_name",
        "fallback_application_name",
        "keepalives",
        "keepalives_idle",
        "keepalives_interval",
        "keepalives_count",
        "tcp_user_timeout",
        "sslmode",
        "sslnegotiation",
        "sslcompression",
        "sslcert",
        "sslkey",
        "sslcertmode",
        "sslpassword",
        "sslrootcert",
        "sslcrl",
        "sslcrldir",
        "sslsni",
        "requirepeer",
        "require_auth",
        "min_protocol_version",
        "max_protocol_version",
        "ssl_min_protocol_version",
        "ssl_max_protocol_version",
        "gssencmode",
        "krbsrvname",
        "gsslib",
        "gssdelegation",
        "replication",
        "target_session_attrs",
        "load_balance_hosts",
        "scram_client_key",
        "scram_server_key",
        "oauth_issuer",
        "oauth_client_id",
        "oauth_client_secret",
        "oauth_scope",
        "sslkeylogfile",
    ];

    #[test]
    fn the_option_table_is_upstreams_in_upstream_order() {
        let keywords: Vec<&str> = CONNINFO_OPTIONS
            .iter()
            .map(|option| option.keyword)
            .collect();
        assert_eq!(keywords, UPSTREAM_KEYWORDS);
    }

    #[test]
    fn every_keyword_appears_once() {
        for (index, option) in CONNINFO_OPTIONS.iter().enumerate() {
            assert_eq!(
                ConnInfo::index_of(option.keyword.as_bytes()),
                Some(index),
                "{} resolves to another row",
                option.keyword
            );
        }
    }

    /// The three options whose `dispchar` is not `""`, plus the four `"D"`
    /// ones: a server-side client decides what to expose from this
    /// (`fe-connect.c:163`).
    #[test]
    fn the_hidden_and_debug_options_are_upstreams() {
        let hidden: Vec<&str> = CONNINFO_OPTIONS
            .iter()
            .filter(|option| option.dispchar == Dispchar::Password)
            .map(|option| option.keyword)
            .collect();
        assert_eq!(
            hidden,
            ["password", "sslpassword", "oauth_client_secret"],
            "dispchar \"*\""
        );
        let debug: Vec<&str> = CONNINFO_OPTIONS
            .iter()
            .filter(|option| option.dispchar == Dispchar::Debug)
            .map(|option| option.keyword)
            .collect();
        assert_eq!(
            debug,
            [
                "replication",
                "scram_client_key",
                "scram_server_key",
                "sslkeylogfile"
            ],
            "dispchar \"D\""
        );
    }

    #[test]
    fn dispchar_renders_as_the_c_strings() {
        assert_eq!(Dispchar::Plain.as_str(), "");
        assert_eq!(Dispchar::Password.as_str(), "*");
        assert_eq!(Dispchar::Debug.as_str(), "D");
    }

    #[test]
    fn a_designator_is_recognized_and_measured() {
        assert_eq!(uri_prefix_length(b"postgresql://host"), 13);
        assert_eq!(uri_prefix_length(b"postgres://host"), 11);
        assert_eq!(uri_prefix_length(b"postgre://"), 0);
        assert_eq!(uri_prefix_length(b"host=x"), 0);
    }

    #[test]
    fn a_connection_string_is_recognized_by_a_designator_or_an_equals_sign() {
        assert!(recognized_connection_string(b"postgres://"));
        assert!(recognized_connection_string(b"host=x"));
        assert!(!recognized_connection_string(b"mydb"));
    }

    fn value_of(conninfo: &[u8], keyword: &str) -> Option<String> {
        let options = parse_conninfo(conninfo).expect("parses");
        options
            .get(keyword)
            .map(|value| String::from_utf8_lossy(value).into_owned())
    }

    #[test]
    fn a_key_value_pair_tolerates_blanks_around_the_equals_sign() {
        assert_eq!(
            value_of(b"  host  =  example.com  ", "host").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces_and_a_backslash_escapes_the_next_byte() {
        assert_eq!(
            value_of(br"options='-c work_mem=1MB' host=a\ b", "options").as_deref(),
            Some("-c work_mem=1MB")
        );
        assert_eq!(value_of(br"host=a\ b", "host").as_deref(), Some("a b"));
        assert_eq!(
            value_of(br"password='it\'s' host=h", "password").as_deref(),
            Some("it's")
        );
    }

    #[test]
    fn a_quoted_value_that_never_closes_is_an_error() {
        assert_eq!(
            parse_conninfo(b"host='example.com"),
            Err(ConnError::UnterminatedQuotedString)
        );
    }

    #[test]
    fn a_keyword_with_no_equals_sign_names_itself() {
        assert_eq!(
            parse_conninfo(b"postgre://"),
            Err(ConnError::MissingEquals("postgre://".into()))
        );
        // The name is cut at the first blank, as conninfo_parse cuts it.
        assert_eq!(
            parse_conninfo(b"host example.com"),
            Err(ConnError::MissingEquals("host".into()))
        );
    }

    #[test]
    fn an_unknown_keyword_is_an_error_naming_it() {
        assert_eq!(
            parse_conninfo(b"uzer=me"),
            Err(ConnError::InvalidConnectionOption("uzer".into()))
        );
    }

    /// `conninfo_storeval` rewrites `requiressl` before it looks the keyword up
    /// (`fe-connect.c:7343`), so the rewrite reaches a `key=value` string and a
    /// URI query parameter alike.
    #[test]
    fn requiressl_is_rewritten_to_sslmode() {
        assert_eq!(
            value_of(b"requiressl=1", "sslmode").as_deref(),
            Some("require")
        );
        assert_eq!(
            value_of(b"requiressl=0", "sslmode").as_deref(),
            Some("prefer")
        );
        assert_eq!(
            value_of(b"requiressl=", "sslmode").as_deref(),
            Some("prefer")
        );
        assert_eq!(
            value_of(b"postgresql://host?requiressl=1", "sslmode").as_deref(),
            Some("require")
        );
    }

    /// A value is bytes: `PQconninfoParse` never decodes one, so one that is
    /// not UTF-8 survives instead of being rejected or replaced.
    #[test]
    fn a_value_that_is_not_utf8_survives_the_parser() {
        let options = parse_conninfo(b"postgresql://host/db?application_name=%C3").expect("parses");
        assert_eq!(options.get("application_name"), Some(&[0xC3u8][..]));
    }

    #[test]
    fn nothing_is_set_before_the_defaults_are_added() {
        let options = ConnInfo::new();
        assert!(options.iter().all(|option| option.value.is_none()));
    }

    #[test]
    fn an_environment_variable_beats_the_compiled_default() {
        let defaults = conndefaults(&Env::empty().with("PGPORT", "6789"));
        assert_eq!(defaults.get("port"), Some(b"6789".as_slice()));
        assert_eq!(
            conndefaults(&Env::empty()).get("port"),
            Some(DEF_PGPORT_STR.as_bytes())
        );
    }

    #[test]
    fn an_option_with_neither_a_variable_nor_a_default_stays_unset() {
        let defaults = conndefaults(&Env::empty());
        assert_eq!(defaults.get("host"), None);
        assert_eq!(defaults.get("hostaddr"), None);
        assert_eq!(defaults.get("dbname"), None);
    }

    /// `fe-connect.c:6673`: a `PGREQUIRESSL` starting with "1" means
    /// `sslmode=require`; anything else is ignored; `PGSSLMODE` wins over both.
    #[test]
    fn the_deprecated_pgrequiressl_is_read_only_when_pgsslmode_is_absent() {
        let required = conndefaults(&Env::empty().with("PGREQUIRESSL", "1"));
        assert_eq!(required.get("sslmode"), Some(b"require".as_slice()));

        let ignored = conndefaults(&Env::empty().with("PGREQUIRESSL", "0"));
        assert_eq!(ignored.get("sslmode"), Some(DEFAULT_SSL_MODE.as_bytes()));

        let explicit = conndefaults(
            &Env::empty()
                .with("PGREQUIRESSL", "1")
                .with("PGSSLMODE", "allow"),
        );
        assert_eq!(explicit.get("sslmode"), Some(b"allow".as_slice()));
    }

    /// `fe-connect.c:6730` — the case the last three rows of `001_uri.pl` are
    /// there to pin.
    #[test]
    fn sslrootcert_system_strengthens_the_default_sslmode_to_verify_full() {
        let system = conndefaults(&Env::empty().with("PGSSLROOTCERT", "system"));
        assert_eq!(system.get("sslmode"), Some(b"verify-full".as_slice()));

        let other = conndefaults(&Env::empty().with("PGSSLROOTCERT", "/etc/ca.crt"));
        assert_eq!(other.get("sslmode"), Some(DEFAULT_SSL_MODE.as_bytes()));
    }

    /// The strengthening applies to the *default* sslmode only: an sslmode that
    /// came from the environment has a value already and is left alone.
    #[test]
    fn an_sslmode_that_was_given_is_not_strengthened() {
        let given = conndefaults(
            &Env::empty()
                .with("PGSSLROOTCERT", "system")
                .with("PGSSLMODE", "disable"),
        );
        assert_eq!(given.get("sslmode"), Some(b"disable".as_slice()));

        let mut parsed = parse_conninfo(b"postgresql://host?sslmode=prefer").expect("parses");
        parsed.add_defaults(&Env::empty().with("PGSSLROOTCERT", "system"));
        assert_eq!(parsed.get("sslmode"), Some(b"prefer".as_slice()));
    }

    /// See `docs/divergences.md`: `pg_fe_getauthname` is a `getpwuid` lookup
    /// upstream and `USER`/`LOGNAME` here.
    #[test]
    fn the_default_user_comes_from_user_then_logname() {
        assert_eq!(
            conndefaults(&Env::empty().with("USER", "alice")).get("user"),
            Some(b"alice".as_slice())
        );
        assert_eq!(
            conndefaults(&Env::empty().with("LOGNAME", "bob")).get("user"),
            Some(b"bob".as_slice())
        );
        assert_eq!(
            conndefaults(&Env::empty().with("USER", "alice").with("LOGNAME", "bob")).get("user"),
            Some(b"alice".as_slice())
        );
        assert_eq!(conndefaults(&Env::empty()).get("user"), None);
        // An exported but empty USER is not a user name.
        assert_eq!(
            conndefaults(&Env::empty().with("USER", "").with("LOGNAME", "bob")).get("user"),
            Some(b"bob".as_slice())
        );
    }

    /// `PGUSER` is `user`'s envvar, so it is consulted before the fallback.
    #[test]
    fn pguser_beats_the_effective_user() {
        let defaults = conndefaults(&Env::empty().with("USER", "alice").with("PGUSER", "carol"));
        assert_eq!(defaults.get("user"), Some(b"carol".as_slice()));
    }

    /// See `docs/divergences.md`: `parseServiceInfo` (`fe-connect.c:5929`) is
    /// NAT-393's, so a service name contributes no defaults here yet.
    #[test]
    fn a_service_name_contributes_no_defaults_yet() {
        let defaults = conndefaults(&Env::empty().with("PGSERVICE", "somewhere"));
        assert_eq!(defaults.get("service"), Some(b"somewhere".as_slice()));
        assert_eq!(defaults.get("host"), None);
        assert_eq!(defaults.get("dbname"), None);
    }
}
