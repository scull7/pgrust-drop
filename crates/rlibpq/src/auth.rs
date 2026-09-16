//! The authentication request and the client's answer to it.
//!
//! Ported from `src/interfaces/libpq/fe-auth.c`: `pg_fe_sendauth` (`:1066`)
//! decides what to do with an AuthenticationRequest, `pg_password_sendauth`
//! (`:795`) builds the PasswordMessage, and `pg_SASL_init` (`:435`) /
//! `pg_SASL_continue` (`:704`) drive SCRAM.
//!
//! [`Authenticator`] is the whole decision as a calculation: a request in, the
//! frontend message to send out, no socket and no clock. The one input it
//! cannot compute is the SCRAM nonce, which is handed to it.

use crate::md5;
use crate::message::{Frontend, ProtocolError, Reader};
use crate::scram::{
    Mechanism, SCRAM_SHA_256_NAME, SCRAM_SHA_256_PLUS_NAME, ScramClient, ScramError,
};

/// The `AUTH_REQ_*` codes of `protocol.h:74`-`:87`, under upstream's names so
/// the port stays grep-able against that header. [`AuthRequest::decode`] and
/// [`AuthRequest::code`] both read the wire encoding from here rather than
/// spelling it twice; 6 is missing because upstream retired it with the SCM
/// credentials method and left the number unused.
pub const AUTH_REQ_OK: u32 = 0;
/// `AUTH_REQ_KRB4` — Kerberos V4, not supported any more.
pub const AUTH_REQ_KRB4: u32 = 1;
/// `AUTH_REQ_KRB5` — Kerberos V5, not supported any more.
pub const AUTH_REQ_KRB5: u32 = 2;
/// `AUTH_REQ_PASSWORD` — cleartext password.
pub const AUTH_REQ_PASSWORD: u32 = 3;
/// `AUTH_REQ_CRYPT` — crypt password, not supported any more.
pub const AUTH_REQ_CRYPT: u32 = 4;
/// `AUTH_REQ_MD5` — md5 password.
pub const AUTH_REQ_MD5: u32 = 5;
/// `AUTH_REQ_GSS` — GSSAPI without `wrap()`.
pub const AUTH_REQ_GSS: u32 = 7;
/// `AUTH_REQ_GSS_CONT` — continue a GSS exchange.
pub const AUTH_REQ_GSS_CONT: u32 = 8;
/// `AUTH_REQ_SSPI` — SSPI negotiate without `wrap()`.
pub const AUTH_REQ_SSPI: u32 = 9;
/// `AUTH_REQ_SASL` — begin SASL authentication.
pub const AUTH_REQ_SASL: u32 = 10;
/// `AUTH_REQ_SASL_CONT` — continue SASL authentication.
pub const AUTH_REQ_SASL_CONT: u32 = 11;
/// `AUTH_REQ_SASL_FIN` — the final SASL message.
pub const AUTH_REQ_SASL_FIN: u32 = 12;
/// `AUTH_REQ_MAX` — the largest code upstream assigns. Anything above it is
/// `pg_fe_sendauth`'s default arm, i.e. [`AuthRequest::Unknown`].
pub const AUTH_REQ_MAX: u32 = AUTH_REQ_SASL_FIN;

/// The `AUTH_REQ_*` codes of `protocol.h:74`-`:87`, with their payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRequest {
    /// `AUTH_REQ_OK` — authenticated.
    Ok,
    /// `AUTH_REQ_KRB4`, not supported any more.
    KerberosV4,
    /// `AUTH_REQ_KRB5`, not supported any more.
    KerberosV5,
    /// `AUTH_REQ_PASSWORD` — cleartext password.
    CleartextPassword,
    /// `AUTH_REQ_CRYPT`, not supported any more.
    Crypt,
    /// `AUTH_REQ_MD5`, with the four salt bytes `pg_password_sendauth` reads
    /// at `fe-auth.c:806`.
    Md5Password([u8; 4]),
    /// `AUTH_REQ_GSS`.
    Gss,
    /// `AUTH_REQ_GSS_CONT`.
    GssContinue(Vec<u8>),
    /// `AUTH_REQ_SSPI`.
    Sspi,
    /// `AUTH_REQ_SASL` — the mechanism list, NUL-terminated strings ending
    /// with an empty one (`fe-auth.c:465`).
    Sasl(Vec<Vec<u8>>),
    /// `AUTH_REQ_SASL_CONT`.
    SaslContinue(Vec<u8>),
    /// `AUTH_REQ_SASL_FIN`.
    SaslFinal(Vec<u8>),
    /// Anything above `AUTH_REQ_MAX` — `pg_fe_sendauth`'s default arm.
    Unknown(u32),
}

impl AuthRequest {
    /// The body of an AuthenticationRequest message, after the length word.
    ///
    /// # Errors
    /// The body is shorter than the code it carries requires.
    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader::new(body, b'R');
        let areq = r.u32()?;
        Ok(match areq {
            AUTH_REQ_OK => AuthRequest::Ok,
            AUTH_REQ_KRB4 => AuthRequest::KerberosV4,
            AUTH_REQ_KRB5 => AuthRequest::KerberosV5,
            AUTH_REQ_PASSWORD => AuthRequest::CleartextPassword,
            AUTH_REQ_CRYPT => AuthRequest::Crypt,
            AUTH_REQ_MD5 => {
                let salt = r.take(4)?;
                AuthRequest::Md5Password([salt[0], salt[1], salt[2], salt[3]])
            }
            AUTH_REQ_GSS => AuthRequest::Gss,
            AUTH_REQ_GSS_CONT => AuthRequest::GssContinue(r.rest().to_vec()),
            AUTH_REQ_SSPI => AuthRequest::Sspi,
            AUTH_REQ_SASL => {
                // fe-auth.c:465 — read strings until the empty one.
                let mut mechanisms = Vec::new();
                loop {
                    let name = r.cstring()?;
                    if name.is_empty() {
                        break;
                    }
                    mechanisms.push(name.to_vec());
                }
                AuthRequest::Sasl(mechanisms)
            }
            AUTH_REQ_SASL_CONT => AuthRequest::SaslContinue(r.rest().to_vec()),
            AUTH_REQ_SASL_FIN => AuthRequest::SaslFinal(r.rest().to_vec()),
            other => AuthRequest::Unknown(other),
        })
    }

    /// The `AUTH_REQ_*` code itself.
    #[must_use]
    pub fn code(&self) -> u32 {
        match self {
            AuthRequest::Ok => AUTH_REQ_OK,
            AuthRequest::KerberosV4 => AUTH_REQ_KRB4,
            AuthRequest::KerberosV5 => AUTH_REQ_KRB5,
            AuthRequest::CleartextPassword => AUTH_REQ_PASSWORD,
            AuthRequest::Crypt => AUTH_REQ_CRYPT,
            AuthRequest::Md5Password(_) => AUTH_REQ_MD5,
            AuthRequest::Gss => AUTH_REQ_GSS,
            AuthRequest::GssContinue(_) => AUTH_REQ_GSS_CONT,
            AuthRequest::Sspi => AUTH_REQ_SSPI,
            AuthRequest::Sasl(_) => AUTH_REQ_SASL,
            AuthRequest::SaslContinue(_) => AUTH_REQ_SASL_CONT,
            AuthRequest::SaslFinal(_) => AUTH_REQ_SASL_FIN,
            AuthRequest::Unknown(code) => *code,
        }
    }

    /// `auth_method_description`, `fe-auth.c:868`.
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self {
            AuthRequest::CleartextPassword => "server requested a cleartext password",
            AuthRequest::Md5Password(_) => "server requested a hashed password",
            AuthRequest::Gss | AuthRequest::GssContinue(_) => {
                "server requested GSSAPI authentication"
            }
            AuthRequest::Sspi => "server requested SSPI authentication",
            AuthRequest::Sasl(_) | AuthRequest::SaslContinue(_) | AuthRequest::SaslFinal(_) => {
                "server requested SASL authentication"
            }
            _ => "server requested an unknown authentication type",
        }
    }
}

/// Every way `pg_fe_sendauth` and its helpers can refuse a request. The
/// rendering is the `libpq_append_conn_error` format string at that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// `fe-auth.c:1081`.
    Kerberos4NotSupported,
    /// `fe-auth.c:1085`.
    Kerberos5NotSupported,
    /// `fe-auth.c:1193`.
    CryptNotSupported,
    /// `fe-auth.c:1155`.
    GssapiNotSupported,
    /// `fe-auth.c:1186`.
    SspiNotSupported,
    /// `fe-auth.c:1267`.
    MethodNotSupported(u32),
    /// `libpq-fe.h:633` — `PQnoPasswordSupplied`, appended at `fe-auth.c:1207`
    /// and `:600`. It is the one message that carries its own newline.
    NoPasswordSupplied,
    /// `fe-auth.c:551`.
    NoSaslMechanismSupported,
    /// `fe-auth.c:529`.
    ScramPlusOverNonSsl,
    /// `fe-auth.c:455`.
    DuplicateSaslRequest,
    /// `fe-auth.c:1244`.
    SaslContinueWithoutSasl,
    /// `fe-auth.c:758`.
    SaslFinalBeforeCompletion,
    /// `fe-auth.c:449`.
    ChannelBindingRequiredWithoutSsl,
    /// A failure inside the SCRAM exchange itself.
    Scram(ScramError),
}

impl AuthError {
    /// The bytes libpq's error buffer would hold. Only
    /// [`AuthError::NoPasswordSupplied`] ends in a newline, because upstream
    /// appends that one with `appendPQExpBufferStr` rather than
    /// `libpq_append_conn_error`.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            AuthError::Kerberos4NotSupported => {
                b"Kerberos 4 authentication not supported".to_vec()
            }
            AuthError::Kerberos5NotSupported => {
                b"Kerberos 5 authentication not supported".to_vec()
            }
            AuthError::CryptNotSupported => b"Crypt authentication not supported".to_vec(),
            AuthError::GssapiNotSupported => b"GSSAPI authentication not supported".to_vec(),
            AuthError::SspiNotSupported => b"SSPI authentication not supported".to_vec(),
            AuthError::MethodNotSupported(code) => {
                format!("authentication method {code} not supported").into_bytes()
            }
            AuthError::NoPasswordSupplied => b"fe_sendauth: no password supplied\n".to_vec(),
            AuthError::NoSaslMechanismSupported => {
                b"none of the server's SASL authentication mechanisms are supported".to_vec()
            }
            AuthError::ScramPlusOverNonSsl => {
                b"server offered SCRAM-SHA-256-PLUS authentication over a non-SSL connection"
                    .to_vec()
            }
            AuthError::DuplicateSaslRequest => b"duplicate SASL authentication request".to_vec(),
            AuthError::SaslContinueWithoutSasl => {
                b"fe_sendauth: invalid authentication request from server: AUTH_REQ_SASL_CONT without AUTH_REQ_SASL"
                    .to_vec()
            }
            AuthError::SaslFinalBeforeCompletion => {
                b"AuthenticationSASLFinal received from server, but SASL authentication was not completed"
                    .to_vec()
            }
            AuthError::ChannelBindingRequiredWithoutSsl => {
                b"channel binding required, but SSL not in use".to_vec()
            }
            AuthError::Scram(err) => err.message(),
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for AuthError {}

impl From<ScramError> for AuthError {
    fn from(err: ScramError) -> Self {
        AuthError::Scram(err)
    }
}

/// What the client does with one authentication request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStep {
    /// Send this message and keep waiting for more requests.
    Send(Frontend),
    /// The request needed no answer: `pg_SASL_continue` produces no output
    /// once the exchange is complete (`fe-auth.c:776` only sends when the
    /// mechanism returned one).
    Nothing,
    /// `AUTH_REQ_OK`: authentication is over.
    Complete,
}

/// `channel_binding`, `fe-connect.c`'s option of the same name. Without TLS
/// only `disable` and `prefer` can be honoured; `require` fails at
/// `fe-auth.c:446`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelBinding {
    Disable,
    #[default]
    Prefer,
    Require,
}

/// `pg_fe_sendauth`, `fe-auth.c:1066`, as a state machine over requests.
#[derive(Debug, Clone)]
pub struct Authenticator {
    user: Vec<u8>,
    password: Option<Vec<u8>>,
    channel_binding: ChannelBinding,
    /// The bytes `pg_strong_random` would have drawn for the SCRAM nonce
    /// (`fe-auth-scram.c:363`), supplied so the exchange stays reproducible.
    raw_nonce: Vec<u8>,
    scram: Option<ScramClient>,
    /// `conn->client_finished_auth`, `fe-auth.c:1219`.
    client_finished_auth: bool,
}

impl Authenticator {
    #[must_use]
    pub fn new(user: &[u8], password: Option<&[u8]>, raw_nonce: &[u8]) -> Self {
        Self {
            user: user.to_vec(),
            password: password.map(<[u8]>::to_vec),
            channel_binding: ChannelBinding::default(),
            raw_nonce: raw_nonce.to_vec(),
            scram: None,
            client_finished_auth: false,
        }
    }

    #[must_use]
    pub fn with_channel_binding(mut self, channel_binding: ChannelBinding) -> Self {
        self.channel_binding = channel_binding;
        self
    }

    /// True once the client has sent everything the method needs
    /// (`conn->client_finished_auth`).
    #[must_use]
    pub fn client_finished_auth(&self) -> bool {
        self.client_finished_auth
    }

    /// The errors upstream appends without failing, from the SCRAM exchange.
    #[must_use]
    pub fn appended_errors(&self) -> &[ScramError] {
        match &self.scram {
            Some(scram) => scram.appended_errors(),
            None => &[],
        }
    }

    /// `pg_fe_sendauth`, `fe-auth.c:1075`'s switch.
    ///
    /// # Errors
    /// The method is one this build cannot do, the password is missing, or
    /// the SCRAM exchange failed.
    pub fn respond(&mut self, request: &AuthRequest) -> Result<AuthStep, AuthError> {
        match request {
            AuthRequest::Ok => Ok(AuthStep::Complete),
            AuthRequest::KerberosV4 => Err(AuthError::Kerberos4NotSupported),
            AuthRequest::KerberosV5 => Err(AuthError::Kerberos5NotSupported),
            AuthRequest::Crypt => Err(AuthError::CryptNotSupported),
            // fe-auth.c:1153 — the build has neither GSSAPI nor SSPI.
            AuthRequest::Gss | AuthRequest::GssContinue(_) => Err(AuthError::GssapiNotSupported),
            AuthRequest::Sspi => Err(AuthError::SspiNotSupported),
            AuthRequest::CleartextPassword => {
                let password = self.password()?.to_vec();
                self.client_finished_auth = true;
                Ok(AuthStep::Send(Frontend::PasswordMessage(password)))
            }
            AuthRequest::Md5Password(salt) => {
                // fe-auth.c:818 — md5(md5(password || user) || salt).
                let password = self.password()?.to_vec();
                let first = md5::md5_encrypt(&password, &self.user);
                let crypt_pwd = md5::md5_encrypt(&first[b"md5".len()..], salt);
                self.client_finished_auth = true;
                Ok(AuthStep::Send(Frontend::PasswordMessage(crypt_pwd)))
            }
            AuthRequest::Sasl(mechanisms) => self.sasl_init(mechanisms),
            AuthRequest::SaslContinue(challenge) => self.sasl_continue(challenge, false),
            AuthRequest::SaslFinal(challenge) => self.sasl_continue(challenge, true),
            AuthRequest::Unknown(code) => Err(AuthError::MethodNotSupported(*code)),
        }
    }

    /// `fe-auth.c:1205` / `:598` — the password must be there and non-empty.
    fn password(&self) -> Result<&[u8], AuthError> {
        match &self.password {
            Some(password) if !password.is_empty() => Ok(password),
            _ => Err(AuthError::NoPasswordSupplied),
        }
    }

    /// `pg_SASL_init`, `fe-auth.c:435`, minus the arms a build without TLS,
    /// GSSAPI or OAuth cannot reach.
    fn sasl_init(&mut self, mechanisms: &[Vec<u8>]) -> Result<AuthStep, AuthError> {
        // fe-auth.c:446 — require without SSL fails before anything else.
        if self.channel_binding == ChannelBinding::Require {
            return Err(AuthError::ChannelBindingRequiredWithoutSsl);
        }
        if self.scram.is_some() {
            return Err(AuthError::DuplicateSaslRequest);
        }

        let mut selected: Option<Mechanism> = None;
        for mechanism in mechanisms {
            if mechanism == SCRAM_SHA_256_PLUS_NAME {
                // fe-auth.c:518 — offered without SSL, which is not sane.
                return Err(AuthError::ScramPlusOverNonSsl);
            } else if mechanism == SCRAM_SHA_256_NAME && selected.is_none() {
                selected = Some(Mechanism::ScramSha256);
            }
        }
        let Some(mechanism) = selected else {
            return Err(AuthError::NoSaslMechanismSupported);
        };

        let password = self.password()?.to_vec();
        let mut scram = ScramClient::new(&password, mechanism, &self.raw_nonce);
        let initial_response = scram.client_first_message()?;
        self.scram = Some(scram);

        Ok(AuthStep::Send(Frontend::SaslInitialResponse {
            mechanism: mechanism.name().to_vec(),
            initial_response: Some(initial_response),
        }))
    }

    /// `pg_SASL_continue`, `fe-auth.c:704`.
    fn sasl_continue(&mut self, challenge: &[u8], final_: bool) -> Result<AuthStep, AuthError> {
        let Some(scram) = self.scram.as_mut() else {
            return Err(AuthError::SaslContinueWithoutSasl);
        };

        if final_ {
            scram.verify_server_final_message(challenge)?;
            self.client_finished_auth = true;
            // fe-auth.c:776 — a completed exchange produces no output, so
            // nothing is sent; the next message is AuthenticationOk.
            Ok(AuthStep::Nothing)
        } else {
            let output = scram.client_final_message(challenge)?;
            Ok(AuthStep::Send(Frontend::SaslResponse(output)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code of `protocol.h:74`-`:87` with the payload a server sends
    /// after it, so the table below is the whole wire vocabulary.
    fn every_code_with_its_payload() -> Vec<(u32, &'static [u8], AuthRequest)> {
        vec![
            (AUTH_REQ_OK, b"", AuthRequest::Ok),
            (AUTH_REQ_KRB4, b"", AuthRequest::KerberosV4),
            (AUTH_REQ_KRB5, b"", AuthRequest::KerberosV5),
            (AUTH_REQ_PASSWORD, b"", AuthRequest::CleartextPassword),
            (AUTH_REQ_CRYPT, b"", AuthRequest::Crypt),
            (
                AUTH_REQ_MD5,
                b"\x01\x02\x03\x04",
                AuthRequest::Md5Password([1, 2, 3, 4]),
            ),
            (AUTH_REQ_GSS, b"", AuthRequest::Gss),
            (
                AUTH_REQ_GSS_CONT,
                b"gss-token",
                AuthRequest::GssContinue(b"gss-token".to_vec()),
            ),
            (AUTH_REQ_SSPI, b"", AuthRequest::Sspi),
            (
                AUTH_REQ_SASL,
                b"SCRAM-SHA-256\0\0",
                AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()]),
            ),
            (
                AUTH_REQ_SASL_CONT,
                b"r=abc,s=c2FsdA==,i=4096",
                AuthRequest::SaslContinue(b"r=abc,s=c2FsdA==,i=4096".to_vec()),
            ),
            (
                AUTH_REQ_SASL_FIN,
                b"v=c2ln",
                AuthRequest::SaslFinal(b"v=c2ln".to_vec()),
            ),
            // protocol.h:80 — 6 was SCM credentials and is now a hole, so it
            // decodes like anything above AUTH_REQ_MAX.
            (6, b"", AuthRequest::Unknown(6)),
            (
                AUTH_REQ_MAX + 1,
                b"",
                AuthRequest::Unknown(AUTH_REQ_MAX + 1),
            ),
        ]
    }

    /// `decode` and `code` are two directions over one table; this walks every
    /// code through both so neither can drift from the other.
    #[test]
    fn every_auth_request_code_round_trips_through_decode_and_code() {
        for (code, payload, expected) in every_code_with_its_payload() {
            let mut body = code.to_be_bytes().to_vec();
            body.extend_from_slice(payload);

            let decoded = AuthRequest::decode(&body).expect("well-formed body");

            assert_eq!(decoded, expected, "decoding code {code}");
            assert_eq!(decoded.code(), code, "re-encoding code {code}");
        }
    }

    /// The constants are the port's copy of `protocol.h:74`-`:87`; pin the
    /// integers so a rename can never quietly renumber the wire.
    #[test]
    fn the_auth_request_constants_are_the_numbers_protocol_h_assigns() {
        assert_eq!(
            [
                AUTH_REQ_OK,
                AUTH_REQ_KRB4,
                AUTH_REQ_KRB5,
                AUTH_REQ_PASSWORD,
                AUTH_REQ_CRYPT,
                AUTH_REQ_MD5,
                AUTH_REQ_GSS,
                AUTH_REQ_GSS_CONT,
                AUTH_REQ_SSPI,
                AUTH_REQ_SASL,
                AUTH_REQ_SASL_CONT,
                AUTH_REQ_SASL_FIN,
            ],
            [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12],
        );
        assert_eq!(AUTH_REQ_MAX, AUTH_REQ_SASL_FIN);
    }

    /// An AuthenticationMD5Password with no salt is a truncated message, not
    /// four zero bytes.
    #[test]
    fn a_truncated_auth_request_is_an_error() {
        assert!(AuthRequest::decode(&[0, 0, 0, 5]).is_err());
        assert!(AuthRequest::decode(&[0, 0]).is_err());
        let mut unterminated = vec![0, 0, 0, 10];
        unterminated.extend_from_slice(b"SCRAM-SHA-256");
        assert!(AuthRequest::decode(&unterminated).is_err());
    }

    /// `AUTH_REQ_OK` completes without sending anything (trust).
    #[test]
    fn trust_authentication_completes_immediately() {
        let mut auth = Authenticator::new(b"alice", None, &[0; 18]);
        assert_eq!(auth.respond(&AuthRequest::Ok), Ok(AuthStep::Complete));
    }

    /// `AUTH_REQ_PASSWORD` sends the password as a NUL-terminated
    /// PasswordMessage (`fe-auth.c:858` passes `strlen(pwd) + 1`).
    #[test]
    fn a_cleartext_password_is_sent_as_is() {
        let mut auth = Authenticator::new(b"alice", Some(b"secret"), &[0; 18]);
        let step = auth.respond(&AuthRequest::CleartextPassword).unwrap();
        assert_eq!(
            step,
            AuthStep::Send(Frontend::PasswordMessage(b"secret".to_vec()))
        );
        assert!(auth.client_finished_auth());
        let AuthStep::Send(message) = step else {
            unreachable!()
        };
        assert_eq!(message.encode(), b"p\0\0\0\x0bsecret\0".to_vec());
    }

    /// `pg_password_sendauth`'s MD5 arm, `fe-auth.c:818`: the inner hash is
    /// over password and user name, the outer over that hex string and the
    /// four salt bytes.
    #[test]
    fn an_md5_password_is_hashed_twice() {
        let mut auth = Authenticator::new(b"alice", Some(b"secret"), &[0; 18]);
        let step = auth.respond(&AuthRequest::Md5Password([0xde, 0xad, 0xbe, 0xef]));
        // The fixed vector from `md5.rs`: md5("secretalice"), then md5 of
        // that hex string followed by the four salt bytes — from the system
        // `md5sum`, not from the code under test.
        let expected = b"md53e1d73ba00a55e8805aa0277d29996c5".to_vec();
        assert_eq!(
            step,
            Ok(AuthStep::Send(Frontend::PasswordMessage(expected.clone())))
        );
        assert_eq!(expected.len(), 35, "MD5_PASSWD_LEN, md5.h:26");
    }

    /// Without a password, both password methods fail with
    /// `PQnoPasswordSupplied` — including when the password is the empty
    /// string (`fe-auth.c:1205`).
    #[test]
    fn a_password_method_without_a_password_is_refused() {
        for password in [None, Some(&b""[..])] {
            let mut auth = Authenticator::new(b"alice", password, &[0; 18]);
            assert_eq!(
                auth.respond(&AuthRequest::CleartextPassword),
                Err(AuthError::NoPasswordSupplied)
            );
            assert_eq!(
                auth.respond(&AuthRequest::Md5Password([1, 2, 3, 4])),
                Err(AuthError::NoPasswordSupplied)
            );
            let mut auth = Authenticator::new(b"alice", password, &[0; 18]);
            assert_eq!(
                auth.respond(&AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()])),
                Err(AuthError::NoPasswordSupplied)
            );
        }
        assert_eq!(
            AuthError::NoPasswordSupplied.message(),
            b"fe_sendauth: no password supplied\n".to_vec()
        );
    }

    /// The full SCRAM exchange through the authenticator, replaying the
    /// RFC 7677 vector as three AuthenticationRequest messages.
    #[test]
    fn a_scram_exchange_runs_to_completion() {
        let raw_nonce = crate::base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &raw_nonce);

        let step = auth
            .respond(&AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()]))
            .unwrap();
        assert_eq!(
            step,
            AuthStep::Send(Frontend::SaslInitialResponse {
                mechanism: b"SCRAM-SHA-256".to_vec(),
                initial_response: Some(b"n,,n=,r=rOprNGfwEbeRWgbNEkqO".to_vec()),
            })
        );

        let step = auth
            .respond(&AuthRequest::SaslContinue(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096".to_vec(),
            ))
            .unwrap();
        assert_eq!(
            step,
            AuthStep::Send(Frontend::SaslResponse(
                b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=qvT2SWdEH5Q06albL+hjSYuUhCG7VndFyzIb7CK4n9k=".to_vec()
            ))
        );

        let step = auth
            .respond(&AuthRequest::SaslFinal(
                b"v=3HO6Qt1M4MKJrmlKaoOqLAI0/0TV0HZe7J9H3MBtSOg=".to_vec(),
            ))
            .unwrap();
        assert_eq!(step, AuthStep::Nothing);
        assert!(auth.client_finished_auth());
        assert_eq!(auth.respond(&AuthRequest::Ok), Ok(AuthStep::Complete));
    }

    /// The SASL arms `pg_SASL_init` refuses.
    #[test]
    fn the_sasl_mechanism_list_is_checked() {
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &[0; 18]);
        assert_eq!(
            auth.respond(&AuthRequest::Sasl(vec![b"OAUTHBEARER".to_vec()])),
            Err(AuthError::NoSaslMechanismSupported)
        );

        // fe-auth.c:529 — SCRAM-SHA-256-PLUS without SSL.
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &[0; 18]);
        assert_eq!(
            auth.respond(&AuthRequest::Sasl(vec![
                SCRAM_SHA_256_PLUS_NAME.to_vec(),
                SCRAM_SHA_256_NAME.to_vec(),
            ])),
            Err(AuthError::ScramPlusOverNonSsl)
        );

        // fe-auth.c:446 — channel_binding=require without SSL.
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &[0; 18])
            .with_channel_binding(ChannelBinding::Require);
        assert_eq!(
            auth.respond(&AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()])),
            Err(AuthError::ChannelBindingRequiredWithoutSsl)
        );

        // fe-auth.c:1244 — a continuation with no exchange in progress.
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &[0; 18]);
        assert_eq!(
            auth.respond(&AuthRequest::SaslContinue(b"r=x".to_vec())),
            Err(AuthError::SaslContinueWithoutSasl)
        );

        // fe-auth.c:455 — a second AUTH_REQ_SASL.
        let mut auth = Authenticator::new(b"user", Some(b"pencil"), &[0; 18]);
        auth.respond(&AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()]))
            .unwrap();
        assert_eq!(
            auth.respond(&AuthRequest::Sasl(vec![SCRAM_SHA_256_NAME.to_vec()])),
            Err(AuthError::DuplicateSaslRequest)
        );
    }

    /// The methods this build cannot do at all, each with upstream's message.
    #[test]
    fn the_unsupported_methods_report_upstreams_message() {
        let cases = [
            (
                AuthRequest::KerberosV4,
                "Kerberos 4 authentication not supported",
            ),
            (
                AuthRequest::KerberosV5,
                "Kerberos 5 authentication not supported",
            ),
            (AuthRequest::Crypt, "Crypt authentication not supported"),
            (AuthRequest::Gss, "GSSAPI authentication not supported"),
            (
                AuthRequest::GssContinue(Vec::new()),
                "GSSAPI authentication not supported",
            ),
            (AuthRequest::Sspi, "SSPI authentication not supported"),
            (
                AuthRequest::Unknown(42),
                "authentication method 42 not supported",
            ),
        ];
        for (request, expected) in cases {
            let mut auth = Authenticator::new(b"alice", Some(b"secret"), &[0; 18]);
            let err = auth.respond(&request).unwrap_err();
            assert_eq!(String::from_utf8(err.message()).unwrap(), expected);
        }
    }

    /// `auth_method_description`, `fe-auth.c:868`, for the `require_auth`
    /// messages that quote it.
    #[test]
    fn every_method_has_upstreams_description() {
        assert_eq!(
            AuthRequest::CleartextPassword.description(),
            "server requested a cleartext password"
        );
        assert_eq!(
            AuthRequest::Md5Password([0; 4]).description(),
            "server requested a hashed password"
        );
        assert_eq!(
            AuthRequest::Gss.description(),
            "server requested GSSAPI authentication"
        );
        assert_eq!(
            AuthRequest::Sspi.description(),
            "server requested SSPI authentication"
        );
        assert_eq!(
            AuthRequest::Sasl(Vec::new()).description(),
            "server requested SASL authentication"
        );
        assert_eq!(
            AuthRequest::Ok.description(),
            "server requested an unknown authentication type"
        );
    }
}
