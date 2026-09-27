//! Encryption negotiation: which of plaintext, SSL and GSSAPI a connection
//! attempt may use, and in what order it tries them.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c`: the `sslmode`,
//! `sslnegotiation` and `gssencmode` checks in `pqConnectOptions2`
//! (`:1747`-`:1987`), and the state machine behind `PQconnectPoll`'s
//! encryption fallback — `init_allowed_encryption_methods` (`:4696`),
//! `encryption_negotiation_failed` (`:4761`), `connection_failed` (`:4786`)
//! and `select_next_encryption_method` (`:4801`). All of it is pure: the
//! options are read out of a `ConnInfo`, and the machine is told what
//! happened on the wire rather than looking.
//!
//! C decides with `#ifdef USE_SSL` and `#ifdef ENABLE_GSS`; here those are
//! the two fields of [`Build`], so both arms of every `#ifdef` are tested in
//! one build. [`Build::THIS`] is the one `Connection` runs with.

use crate::conninfo::ConnInfo;
use crate::error::ConnError;
use crate::pg_config::{
    DEFAULT_GSS_MODE, DEFAULT_SSL_MODE, DEFAULT_SSL_NEGOTIATION, ENABLE_GSS, USE_SSL,
};
use crate::text::RawText;

/// The two `#ifdef`s the negotiation depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Build {
    /// `USE_SSL`.
    pub use_ssl: bool,
    /// `ENABLE_GSS`.
    pub enable_gss: bool,
}

impl Build {
    /// This crate's arms, from `pg_config`.
    pub const THIS: Build = Build {
        use_ssl: USE_SSL,
        enable_gss: ENABLE_GSS,
    };
}

/// `sslmode`, told apart by its first byte as `fe-connect.c` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Allow,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    fn parse(value: &[u8]) -> Option<Self> {
        Some(match value {
            b"disable" => SslMode::Disable,
            b"allow" => SslMode::Allow,
            b"prefer" => SslMode::Prefer,
            b"require" => SslMode::Require,
            b"verify-ca" => SslMode::VerifyCa,
            b"verify-full" => SslMode::VerifyFull,
            _ => return None,
        })
    }

    /// `sslmode[0] == 'r' || sslmode[0] == 'v'`: the modes that refuse to
    /// fall back to plaintext.
    fn requires_ssl(self) -> bool {
        matches!(
            self,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
        )
    }
}

/// `sslnegotiation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslNegotiation {
    /// An SSLRequest first, then the handshake.
    Postgres,
    /// The TLS handshake straight away (PostgreSQL 17 and later servers).
    Direct,
}

/// `gssencmode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GssEncMode {
    Disable,
    Prefer,
    Require,
}

/// The three options, validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptionOptions {
    pub sslmode: SslMode,
    pub sslnegotiation: SslNegotiation,
    pub gssencmode: GssEncMode,
}

impl EncryptionOptions {
    /// The encryption checks of `pqConnectOptions2`, in its order. An unset
    /// option takes its compiled-in default, which is always valid.
    ///
    /// # Errors
    /// The first check C would fail, with the message it appends.
    pub fn from_conninfo(conninfo: &ConnInfo, build: Build) -> Result<Self, ConnError> {
        let not_compiled_in = |option: &'static str, value: &[u8]| {
            Err(ConnError::SslNotCompiledIn {
                option,
                value: RawText::new(value.to_vec()),
            })
        };

        // fe-connect.c:1754 — sslrootcert=system is refused first, because
        // it may be what strengthened the default sslmode.
        let sslrootcert = conninfo.get("sslrootcert");
        if !build.use_ssl && sslrootcert == Some(b"system") {
            return not_compiled_in("sslrootcert", b"system");
        }

        // fe-connect.c:1767
        let raw = conninfo
            .get("sslmode")
            .unwrap_or(DEFAULT_SSL_MODE.as_bytes());
        let sslmode = SslMode::parse(raw).ok_or_else(|| invalid("sslmode", raw))?;
        // fe-connect.c:1783
        if !build.use_ssl && sslmode.requires_ssl() {
            return not_compiled_in("sslmode", raw);
        }

        // fe-connect.c:1814
        let raw = conninfo
            .get("sslnegotiation")
            .unwrap_or(DEFAULT_SSL_NEGOTIATION.as_bytes());
        let sslnegotiation = match raw {
            b"postgres" => SslNegotiation::Postgres,
            b"direct" => SslNegotiation::Direct,
            _ => return Err(invalid("sslnegotiation", raw)),
        };
        // fe-connect.c:1826
        if !build.use_ssl && sslnegotiation != SslNegotiation::Postgres {
            return not_compiled_in("sslnegotiation", raw);
        }
        // fe-connect.c:1845
        if sslnegotiation == SslNegotiation::Direct && !sslmode.requires_ssl() {
            return Err(ConnError::WeakSslModeWithDirect(sslmode_text(conninfo)));
        }

        // fe-connect.c:1866
        if build.use_ssl && sslrootcert == Some(b"system") && sslmode != SslMode::VerifyFull {
            return Err(ConnError::WeakSslModeWithSystemRoot(sslmode_text(conninfo)));
        }

        // fe-connect.c:1962
        let raw = conninfo
            .get("gssencmode")
            .unwrap_or(DEFAULT_GSS_MODE.as_bytes());
        let gssencmode = match raw {
            b"disable" => GssEncMode::Disable,
            b"prefer" => GssEncMode::Prefer,
            b"require" => GssEncMode::Require,
            _ => return Err(invalid("gssencmode", raw)),
        };
        // fe-connect.c:1973
        if !build.enable_gss && gssencmode == GssEncMode::Require {
            return Err(ConnError::GssNotCompiledIn(RawText::new(raw.to_vec())));
        }

        Ok(EncryptionOptions {
            sslmode,
            sslnegotiation,
            gssencmode,
        })
    }
}

fn invalid(option: &'static str, value: &[u8]) -> ConnError {
    ConnError::InvalidValue {
        option,
        value: RawText::new(value.to_vec()),
    }
}

/// `conn->sslmode` as the user spelled it, for the two "weak sslmode"
/// messages; it has already been validated.
fn sslmode_text(conninfo: &ConnInfo) -> RawText {
    RawText::new(
        conninfo
            .get("sslmode")
            .unwrap_or(DEFAULT_SSL_MODE.as_bytes())
            .to_vec(),
    )
}

/// One encryption method, `ENC_PLAINTEXT`, `ENC_GSSAPI` or `ENC_SSL`
/// (`libpq-int.h:230`-`:232`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncMethod {
    Plaintext,
    Gssapi,
    Ssl,
}

impl EncMethod {
    /// The method's bit in `allowed_enc_methods` and `failed_enc_methods`.
    fn bit(self) -> u8 {
        match self {
            EncMethod::Plaintext => 0x01,
            EncMethod::Gssapi => 0x02,
            EncMethod::Ssl => 0x04,
        }
    }
}

/// What `encryption_negotiation_failed` tells `PQconnectPoll` to do when the
/// server refused SSL or GSSAPI but the socket is still good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterRefusal {
    /// `0`: nothing is left; fail with the caller's message.
    GiveUp,
    /// `1`: send the next method's first packet on the same socket.
    SameConnection,
    /// `2`: open a new socket first — a direct SSL handshake cannot follow
    /// an SSLRequest or GSSENCRequest the server already answered.
    Reconnect,
}

/// `allowed_enc_methods`, `failed_enc_methods` and `current_enc_method` for
/// one server address, with the `sslmode` and `sslnegotiation` that order
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiation {
    sslmode: SslMode,
    sslnegotiation: SslNegotiation,
    allowed: u8,
    failed: u8,
    /// `None` is `ENC_ERROR`: nothing is left to try.
    current: Option<EncMethod>,
}

impl Negotiation {
    /// `init_allowed_encryption_methods`, `fe-connect.c:4696`: the methods
    /// the options allow for an address, and the first to try.
    ///
    /// GSSAPI's credential-cache check (`pg_GSS_have_cred_cache`, `:4831`)
    /// is not here: it belongs to a GSSAPI build, which this crate is not.
    ///
    /// # Errors
    /// `gssencmode=require` over a Unix socket (`fe-connect.c:4707`).
    pub fn start(
        options: &EncryptionOptions,
        build: Build,
        unix_socket: bool,
    ) -> Result<Self, ConnError> {
        let mut negotiation = Negotiation {
            sslmode: options.sslmode,
            sslnegotiation: options.sslnegotiation,
            allowed: 0,
            failed: 0,
            current: None,
        };
        let gss_required = options.gssencmode == GssEncMode::Require;

        // fe-connect.c:4698 — neither SSL nor GSSAPI over a Unix socket.
        if unix_socket {
            if gss_required {
                return Err(ConnError::GssapiOverLocalSocket);
            }
            negotiation.allowed = EncMethod::Plaintext.bit();
            negotiation.current = Some(EncMethod::Plaintext);
            return Ok(negotiation);
        }

        // fe-connect.c:4726
        if build.use_ssl && options.sslmode != SslMode::Disable && !gss_required {
            negotiation.allowed |= EncMethod::Ssl.bit();
        }
        // fe-connect.c:4733
        if build.enable_gss && options.gssencmode != GssEncMode::Disable {
            negotiation.allowed |= EncMethod::Gssapi.bit();
        }
        // fe-connect.c:4737
        if !options.sslmode.requires_ssl() && !gss_required {
            negotiation.allowed |= EncMethod::Plaintext.bit();
        }

        negotiation.select_next();
        Ok(negotiation)
    }

    /// `current_enc_method`; `None` is `ENC_ERROR`.
    #[must_use]
    pub fn current(&self) -> Option<EncMethod> {
        self.current
    }

    /// `encryption_negotiation_failed`, `fe-connect.c:4761`: the server
    /// answered the current method's request with a refusal.
    pub fn encryption_negotiation_failed(&mut self) -> AfterRefusal {
        self.mark_current_failed();
        if !self.select_next() {
            return AfterRefusal::GiveUp;
        }
        // fe-connect.c:4769
        if self.current == Some(EncMethod::Ssl) && self.sslnegotiation == SslNegotiation::Direct {
            AfterRefusal::Reconnect
        } else {
            AfterRefusal::SameConnection
        }
    }

    /// `connection_failed`, `fe-connect.c:4786`: the attempt with the
    /// current method failed outright (a handshake error, or an
    /// ErrorResponse to the startup packet). `true` means reconnect and try
    /// [`Negotiation::current`].
    pub fn connection_failed(&mut self) -> bool {
        self.mark_current_failed();
        self.select_next()
    }

    fn mark_current_failed(&mut self) {
        if let Some(method) = self.current {
            self.failed |= method.bit();
        }
    }

    /// `select_next_encryption_method`, `fe-connect.c:4801`: GSSAPI first;
    /// then plaintext before SSL under `allow` and after it otherwise.
    fn select_next(&mut self) -> bool {
        let remaining = self.allowed & !self.failed;
        let plaintext_first = self.sslmode == SslMode::Allow;
        let order: [EncMethod; 3] = if plaintext_first {
            [EncMethod::Gssapi, EncMethod::Plaintext, EncMethod::Ssl]
        } else {
            [EncMethod::Gssapi, EncMethod::Ssl, EncMethod::Plaintext]
        };
        self.current = order
            .into_iter()
            .find(|method| remaining & method.bit() != 0);
        self.current.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conninfo::parse_conninfo;

    const NO_SSL: Build = Build {
        use_ssl: false,
        enable_gss: false,
    };
    const SSL: Build = Build {
        use_ssl: true,
        enable_gss: false,
    };
    const SSL_AND_GSS: Build = Build {
        use_ssl: true,
        enable_gss: true,
    };

    fn options(conninfo: &str, build: Build) -> Result<EncryptionOptions, String> {
        let info = parse_conninfo(conninfo.as_bytes()).expect("conninfo parses");
        EncryptionOptions::from_conninfo(&info, build).map_err(|err| err.to_string())
    }

    fn tcp(conninfo: &str, build: Build) -> Negotiation {
        Negotiation::start(&options(conninfo, build).unwrap(), build, false).unwrap()
    }

    #[test]
    fn unset_options_take_the_compiled_in_defaults() {
        assert_eq!(
            options("", Build::THIS),
            Ok(EncryptionOptions {
                sslmode: if Build::THIS.use_ssl {
                    SslMode::Prefer
                } else {
                    SslMode::Disable
                },
                sslnegotiation: SslNegotiation::Postgres,
                gssencmode: GssEncMode::Disable,
            })
        );
    }

    /// `fe-connect.c:1777`, `:1820`, `:1969`.
    #[test]
    fn an_unknown_value_is_an_invalid_value() {
        for (conninfo, expected) in [
            ("sslmode=on", "invalid sslmode value: \"on\""),
            (
                "sslnegotiation=tls",
                "invalid sslnegotiation value: \"tls\"",
            ),
            ("gssencmode=allow", "invalid gssencmode value: \"allow\""),
        ] {
            assert_eq!(options(conninfo, SSL_AND_GSS), Err(expected.to_string()));
            assert_eq!(options(conninfo, NO_SSL), Err(expected.to_string()));
        }
    }

    /// The `#ifndef USE_SSL` arms, `fe-connect.c:1758`, `:1797`, `:1829`,
    /// and `#ifndef ENABLE_GSS`'s, `:1976` — why every "compiled without SSL
    /// support" row of `005_negotiate_encryption.pl` with `require` or
    /// `direct` fails with no events.
    #[test]
    fn a_build_without_ssl_refuses_what_needs_it() {
        for (conninfo, expected) in [
            (
                "sslrootcert=system",
                "sslrootcert value \"system\" invalid when SSL support is not compiled in",
            ),
            (
                "sslmode=require",
                "sslmode value \"require\" invalid when SSL support is not compiled in",
            ),
            (
                "sslmode=verify-ca",
                "sslmode value \"verify-ca\" invalid when SSL support is not compiled in",
            ),
            (
                "sslmode=verify-full",
                "sslmode value \"verify-full\" invalid when SSL support is not compiled in",
            ),
            (
                "sslnegotiation=direct",
                "sslnegotiation value \"direct\" invalid when SSL support is not compiled in",
            ),
            (
                "gssencmode=require",
                "gssencmode value \"require\" invalid when GSSAPI support is not compiled in",
            ),
        ] {
            assert_eq!(options(conninfo, NO_SSL), Err(expected.to_string()));
        }
        for conninfo in ["sslmode=allow", "sslmode=prefer", "gssencmode=prefer"] {
            assert!(options(conninfo, NO_SSL).is_ok(), "{conninfo}");
        }
    }

    /// The checks run in `pqConnectOptions2`'s order, so the first failure
    /// is C's: sslrootcert before sslmode (`:1750`'s comment), sslmode before
    /// sslnegotiation, both before gssencmode.
    #[test]
    fn the_first_failing_check_is_the_one_c_reports() {
        assert_eq!(
            options("sslmode=bogus sslrootcert=system", NO_SSL),
            Err(
                "sslrootcert value \"system\" invalid when SSL support is not compiled in"
                    .to_string()
            )
        );
        assert_eq!(
            options("sslmode=require sslnegotiation=direct", NO_SSL),
            Err(
                "sslmode value \"require\" invalid when SSL support is not compiled in".to_string()
            )
        );
        assert_eq!(
            options("sslnegotiation=bogus gssencmode=bogus", NO_SSL),
            Err("invalid sslnegotiation value: \"bogus\"".to_string())
        );
    }

    /// `fe-connect.c:1845` — the `*  *  disable|allow|prefer  direct  - ->
    /// fail` rows of every SSL-supported table in `005_negotiate_encryption.pl`.
    #[test]
    fn direct_negotiation_needs_a_mode_that_requires_ssl() {
        for mode in ["disable", "allow", "prefer"] {
            assert_eq!(
                options(&format!("sslmode={mode} sslnegotiation=direct"), SSL),
                Err(format!(
                    "weak sslmode \"{mode}\" may not be used with sslnegotiation=direct (use \"require\", \"verify-ca\", or \"verify-full\")"
                ))
            );
        }
        for mode in ["require", "verify-ca", "verify-full"] {
            assert!(options(&format!("sslmode={mode} sslnegotiation=direct"), SSL).is_ok());
        }
    }

    /// `fe-connect.c:1866`, the `#ifdef USE_SSL` arm.
    #[test]
    fn a_system_root_store_needs_verify_full() {
        assert_eq!(
            options("sslrootcert=system sslmode=require", SSL),
            Err(
                "weak sslmode \"require\" may not be used with sslrootcert=system (use \"verify-full\")"
                    .to_string()
            )
        );
        assert!(options("sslrootcert=system sslmode=verify-full", SSL).is_ok());
    }

    /// The guarantee `Connection::connect` leans on: in this build GSSAPI is
    /// never current, and without the `tls` feature neither is SSL — so every
    /// combination the options accept starts, and ends, with plaintext.
    #[test]
    fn this_build_never_negotiates_what_it_cannot_do() {
        for sslmode in ["disable", "allow", "prefer", "require", "verify-full"] {
            for gssencmode in ["disable", "prefer", "require"] {
                for sslnegotiation in ["postgres", "direct"] {
                    let conninfo = format!(
                        "sslmode={sslmode} gssencmode={gssencmode} sslnegotiation={sslnegotiation}"
                    );
                    let Ok(options) = options(&conninfo, Build::THIS) else {
                        continue;
                    };
                    for unix_socket in [false, true] {
                        let mut negotiation =
                            Negotiation::start(&options, Build::THIS, unix_socket).unwrap();
                        assert!(negotiation.current().is_some(), "{conninfo}");
                        while let Some(method) = negotiation.current() {
                            assert_ne!(method, EncMethod::Gssapi, "{conninfo}");
                            if !Build::THIS.use_ssl || unix_socket {
                                assert_eq!(method, EncMethod::Plaintext, "{conninfo}");
                            }
                            negotiation.connection_failed();
                        }
                    }
                }
            }
        }
    }

    /// `fe-connect.c:4855`-`:4861`: `allow` tries plaintext and then SSL,
    /// `prefer` the reverse — `connect, authfail, reconnect, sslaccept` and
    /// `connect, sslaccept, authfail, reconnect, authok` in 005's
    /// SSL-enabled table.
    #[test]
    fn allow_and_prefer_try_plaintext_and_ssl_in_opposite_orders() {
        let mut allow = tcp("sslmode=allow", SSL);
        assert_eq!(allow.current(), Some(EncMethod::Plaintext));
        assert!(allow.connection_failed());
        assert_eq!(allow.current(), Some(EncMethod::Ssl));
        assert!(!allow.connection_failed());
        assert_eq!(allow.current(), None);

        let mut prefer = tcp("sslmode=prefer", SSL);
        assert_eq!(prefer.current(), Some(EncMethod::Ssl));
        assert!(prefer.connection_failed());
        assert_eq!(prefer.current(), Some(EncMethod::Plaintext));
        assert!(!prefer.connection_failed());
    }

    /// `sslreject, authok` under `prefer`, and `sslreject -> fail` or
    /// `directsslreject -> fail` under `require` (005's first
    /// SSL-supported table).
    #[test]
    fn a_refused_sslrequest_falls_back_only_where_the_mode_allows() {
        let mut prefer = tcp("sslmode=prefer", SSL);
        assert_eq!(
            prefer.encryption_negotiation_failed(),
            AfterRefusal::SameConnection
        );
        assert_eq!(prefer.current(), Some(EncMethod::Plaintext));

        for conninfo in [
            "sslmode=require",
            "sslmode=require sslnegotiation=direct",
            "sslmode=verify-full",
        ] {
            let mut require = tcp(conninfo, SSL);
            assert_eq!(require.current(), Some(EncMethod::Ssl), "{conninfo}");
            assert_eq!(
                require.encryption_negotiation_failed(),
                AfterRefusal::GiveUp
            );
            assert_eq!(require.current(), None);
        }
    }

    /// `fe-connect.c:4769`, as in 005's `nogssuser prefer require direct`
    /// row: `gssaccept, authfail, reconnect, directsslreject` — after a
    /// refused GSSENCRequest the socket cannot carry a direct handshake.
    #[test]
    fn a_direct_handshake_after_a_refusal_needs_a_new_socket() {
        let mut negotiation = tcp(
            "gssencmode=prefer sslmode=require sslnegotiation=direct",
            SSL_AND_GSS,
        );
        assert_eq!(negotiation.current(), Some(EncMethod::Gssapi));
        assert_eq!(
            negotiation.encryption_negotiation_failed(),
            AfterRefusal::Reconnect
        );
        assert_eq!(negotiation.current(), Some(EncMethod::Ssl));

        let mut negotiation = tcp("gssencmode=prefer sslmode=require", SSL_AND_GSS);
        assert_eq!(
            negotiation.encryption_negotiation_failed(),
            AfterRefusal::SameConnection
        );
    }

    /// `fe-connect.c:4726`, `:4737`: `gssencmode=require` rules out both SSL
    /// and plaintext, and GSSAPI is tried before SSL whatever `sslmode` says
    /// ("GSS is chosen over SSL, even if sslmode=require", 005).
    #[test]
    fn gssapi_comes_first_and_require_leaves_nothing_else() {
        let mut negotiation = tcp("gssencmode=require sslmode=prefer", SSL_AND_GSS);
        assert_eq!(negotiation.current(), Some(EncMethod::Gssapi));
        assert!(!negotiation.connection_failed());

        let mut negotiation = tcp("gssencmode=prefer sslmode=prefer", SSL_AND_GSS);
        assert_eq!(negotiation.current(), Some(EncMethod::Gssapi));
        assert!(negotiation.connection_failed());
        assert_eq!(negotiation.current(), Some(EncMethod::Ssl));
        assert!(negotiation.connection_failed());
        assert_eq!(negotiation.current(), Some(EncMethod::Plaintext));
    }

    /// `fe-connect.c:4698`-`:4718`: a Unix socket is plaintext only, and
    /// refuses `gssencmode=require` outright.
    #[test]
    fn a_unix_socket_is_plaintext_only() {
        let preferring = options("sslmode=require gssencmode=prefer", SSL_AND_GSS).unwrap();
        let mut negotiation = Negotiation::start(&preferring, SSL_AND_GSS, true).unwrap();
        assert_eq!(negotiation.current(), Some(EncMethod::Plaintext));
        assert!(!negotiation.connection_failed());

        let requiring = options("gssencmode=require", SSL_AND_GSS).unwrap();
        assert_eq!(
            Negotiation::start(&requiring, SSL_AND_GSS, true)
                .unwrap_err()
                .to_string(),
            "GSSAPI encryption required but it is not supported over a local socket"
        );
    }
}
