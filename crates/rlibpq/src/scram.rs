//! SCRAM-SHA-256, ported from `src/common/scram-common.c` and the client half
//! in `src/interfaces/libpq/fe-auth-scram.c`.
//!
//! The exchange is a pure calculation: every message in and out is bytes, the
//! only inputs are the password, the mechanism and the client nonce, and the
//! nonce is supplied by the caller rather than drawn here — so the whole
//! handshake can be replayed against a recorded trace (see
//! `the_rfc_7677_exchange`). Drawing the nonce is the one action, and it lives
//! in [`crate::connection`].

use crate::base64;
use crate::hmac::{HmacSha256, hmac_sha256};
use crate::sha256;

/// `scram-common.h:20` — the IANA mechanism name.
pub const SCRAM_SHA_256_NAME: &[u8] = b"SCRAM-SHA-256";
/// `scram-common.h:21` — the channel-binding variant.
pub const SCRAM_SHA_256_PLUS_NAME: &[u8] = b"SCRAM-SHA-256-PLUS";
/// `scram-common.h:24` — `SCRAM_SHA_256_KEY_LEN`.
pub const KEY_LEN: usize = sha256::DIGEST_LENGTH;
/// `scram-common.h:37` — `SCRAM_RAW_NONCE_LEN`, in bytes before base64.
pub const RAW_NONCE_LEN: usize = 18;

/// `scram_SaltedPassword`, `scram-common.c:38` — PBKDF2-HMAC-SHA-256 with one
/// output block, which is all a 32-byte key needs.
#[must_use]
pub fn salted_password(password: &[u8], salt: &[u8], iterations: u32) -> [u8; KEY_LEN] {
    // First iteration: HMAC(password, salt || INT(1)).
    let mut ctx = HmacSha256::new(password);
    ctx.update(salt);
    ctx.update(&1u32.to_be_bytes()); // pg_hton32(1), scram-common.c:44
    let mut ui_prev = ctx.finish();
    let mut result = ui_prev;

    for _ in 1..iterations {
        let ui = hmac_sha256(password, &ui_prev);
        for (r, u) in result.iter_mut().zip(ui.iter()) {
            *r ^= *u;
        }
        ui_prev = ui;
    }
    result
}

/// `scram_H`, `scram-common.c:112`.
#[must_use]
pub fn scram_h(input: &[u8]) -> [u8; KEY_LEN] {
    sha256::sha256(input)
}

/// `scram_ClientKey`, `scram-common.c:142`.
#[must_use]
pub fn client_key(salted_password: &[u8]) -> [u8; KEY_LEN] {
    hmac_sha256(salted_password, b"Client Key")
}

/// `scram_ServerKey`, `scram-common.c:172`.
#[must_use]
pub fn server_key(salted_password: &[u8]) -> [u8; KEY_LEN] {
    hmac_sha256(salted_password, b"Server Key")
}

/// `timingsafe_bcmp`, `src/port/timingsafe_bcmp.c:30` — the `#else` arm, since
/// this build has no OpenSSL `CRYPTO_memcmp` to defer to.
#[must_use]
pub fn timingsafe_bcmp(b1: &[u8], b2: &[u8]) -> bool {
    if b1.len() != b2.len() {
        return true;
    }
    let mut ret = 0u8;
    for (a, b) in b1.iter().zip(b2.iter()) {
        ret |= a ^ b;
    }
    ret != 0
}

/// The mechanism chosen in `pg_SASL_init` (`fe-auth.c:487`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// `SCRAM-SHA-256`.
    ScramSha256,
    /// `SCRAM-SHA-256-PLUS`. Selected only over TLS, which this build does not
    /// have yet (ADR-0006, NAT-392), so [`ScramClient`] refuses it the way the
    /// `#else` arm of `build_client_final_message` does
    /// (`fe-auth-scram.c:538`).
    ScramSha256Plus,
}

impl Mechanism {
    #[must_use]
    pub fn name(self) -> &'static [u8] {
        match self {
            Mechanism::ScramSha256 => SCRAM_SHA_256_NAME,
            Mechanism::ScramSha256Plus => SCRAM_SHA_256_PLUS_NAME,
        }
    }
}

/// `fe_scram_state_enum`, `fe-auth-scram.c:44`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Init,
    NonceSent,
    ProofSent,
    Finished,
}

/// Every `libpq_append_conn_error` call site the SCRAM client can reach.
/// [`ScramError::message`] is that call's format string with its arguments
/// substituted, as bytes, for the same reason [`crate::ConnError`] is bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramError {
    /// `fe-auth-scram.c:228`.
    EmptyMessage,
    /// `fe-auth-scram.c:233`.
    LengthMismatch,
    /// `fe-auth-scram.c:315`.
    AttributeExpected(u8),
    /// `fe-auth-scram.c:324`.
    EqualsExpected(u8),
    /// `fe-auth-scram.c:636`.
    NonceMismatch,
    /// `fe-auth-scram.c:666`.
    InvalidSalt,
    /// `fe-auth-scram.c:679`.
    InvalidIterationCount,
    /// `fe-auth-scram.c:684`.
    GarbageAtEndOfServerFirstMessage,
    /// `fe-auth-scram.c:733`.
    GarbageAtEndOfServerFinalMessage,
    /// `fe-auth-scram.c:718`.
    ErrorFromServer(Vec<u8>),
    /// `fe-auth-scram.c:750`.
    InvalidServerSignature,
    /// `fe-auth-scram.c:283`.
    IncorrectServerSignature,
    /// `fe-auth-scram.c:292`.
    InvalidExchangeState,
    /// `fe-auth-scram.c:539` — the `#else` arm taken by a build without TLS.
    ChannelBindingNotSupported,
}

impl ScramError {
    /// The bytes libpq's error buffer would hold, without the newline
    /// `libpq_append_error` adds (`fe-misc.c:1539`).
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            ScramError::EmptyMessage => b"malformed SCRAM message (empty message)".to_vec(),
            ScramError::LengthMismatch => b"malformed SCRAM message (length mismatch)".to_vec(),
            ScramError::AttributeExpected(attr) => {
                let mut out = b"malformed SCRAM message (attribute \"".to_vec();
                out.push(*attr);
                out.extend_from_slice(b"\" expected)");
                out
            }
            ScramError::EqualsExpected(attr) => {
                let mut out =
                    b"malformed SCRAM message (expected character \"=\" for attribute \"".to_vec();
                out.push(*attr);
                out.extend_from_slice(b"\")");
                out
            }
            ScramError::NonceMismatch => b"invalid SCRAM response (nonce mismatch)".to_vec(),
            ScramError::InvalidSalt => b"malformed SCRAM message (invalid salt)".to_vec(),
            ScramError::InvalidIterationCount => {
                b"malformed SCRAM message (invalid iteration count)".to_vec()
            }
            ScramError::GarbageAtEndOfServerFirstMessage => {
                b"malformed SCRAM message (garbage at end of server-first-message)".to_vec()
            }
            ScramError::GarbageAtEndOfServerFinalMessage => {
                b"malformed SCRAM message (garbage at end of server-final-message)".to_vec()
            }
            ScramError::ErrorFromServer(msg) => {
                let mut out = b"error received from server in SCRAM exchange: ".to_vec();
                out.extend_from_slice(msg);
                out
            }
            ScramError::InvalidServerSignature => {
                b"malformed SCRAM message (invalid server signature)".to_vec()
            }
            ScramError::IncorrectServerSignature => b"incorrect server signature".to_vec(),
            ScramError::InvalidExchangeState => b"invalid SCRAM exchange state".to_vec(),
            ScramError::ChannelBindingNotSupported => {
                b"channel binding not supported by this build".to_vec()
            }
        }
    }
}

impl std::fmt::Display for ScramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for ScramError {}

/// `read_attr_value`, `fe-auth-scram.c:308`: `attr=value` up to the next comma.
/// Returns the value and the rest of the input after the comma.
fn read_attr_value(input: &[u8], attr: u8) -> Result<(&[u8], &[u8]), ScramError> {
    let mut begin = input;
    if begin.first() != Some(&attr) {
        return Err(ScramError::AttributeExpected(attr));
    }
    begin = &begin[1..];
    if begin.first() != Some(&b'=') {
        return Err(ScramError::EqualsExpected(attr));
    }
    begin = &begin[1..];

    match begin.iter().position(|&c| c == b',') {
        Some(end) => Ok((&begin[..end], &begin[end + 1..])),
        None => Ok((begin, &begin[begin.len()..])),
    }
}

/// SASLprep as this build can do it: `pg_saslprep`'s pure-ASCII fast path
/// (`saslprep.c:1067`) returns the password unchanged, and anything else takes
/// `scram_init`'s "not SASLPREP_SUCCESS" arm (`fe-auth-scram.c:133`), which
/// uses the raw password. See `docs/divergences.md`.
#[must_use]
pub fn saslprep(password: &[u8]) -> Vec<u8> {
    password.to_vec()
}

/// `fe_scram_state`, `fe-auth-scram.c:52`, with the four functions that drive
/// it. The nonce is an input, not a draw, so the exchange is reproducible.
#[derive(Debug, Clone)]
pub struct ScramClient {
    state: State,
    password: Vec<u8>,
    mechanism: Mechanism,
    client_nonce: Vec<u8>,
    client_first_message_bare: Vec<u8>,
    client_final_message_without_proof: Vec<u8>,
    server_first_message: Vec<u8>,
    salt: Vec<u8>,
    iterations: u32,
    nonce: Vec<u8>,
    salted_password: [u8; KEY_LEN],
    server_signature: [u8; KEY_LEN],
    /// The `libpq_append_conn_error` calls upstream makes *without* failing
    /// the exchange — the two "garbage at end of …" cases
    /// (`fe-auth-scram.c:683`, `:732`), which append to `conn->errorMessage`
    /// and then return true.
    appended: Vec<ScramError>,
}

impl ScramClient {
    /// `scram_init`, `fe-auth-scram.c:97`, plus the nonce draw that
    /// `build_client_first_message` does at `:363`: `raw_nonce` is the
    /// [`RAW_NONCE_LEN`] bytes `pg_strong_random` would have produced.
    #[must_use]
    pub fn new(password: &[u8], mechanism: Mechanism, raw_nonce: &[u8]) -> Self {
        Self {
            state: State::Init,
            password: saslprep(password),
            mechanism,
            client_nonce: base64::encode(raw_nonce),
            client_first_message_bare: Vec::new(),
            client_final_message_without_proof: Vec::new(),
            server_first_message: Vec::new(),
            salt: Vec::new(),
            iterations: 0,
            nonce: Vec::new(),
            salted_password: [0; KEY_LEN],
            server_signature: [0; KEY_LEN],
            appended: Vec::new(),
        }
    }

    /// The errors upstream appended to `conn->errorMessage` without failing.
    #[must_use]
    pub fn appended_errors(&self) -> &[ScramError] {
        &self.appended
    }

    /// `scram_channel_bound`, `fe-auth-scram.c:158`.
    #[must_use]
    pub fn channel_bound(&self) -> bool {
        self.state == State::Finished && self.mechanism == Mechanism::ScramSha256Plus
    }

    /// True once the server signature has been verified — `client_finished_auth`
    /// at `fe-auth-scram.c:286`.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state == State::Finished
    }

    /// `build_client_first_message`, `fe-auth-scram.c:350`.
    ///
    /// Without TLS the gs2-header is always `n`, the `#ifdef USE_SSL` arm at
    /// `:404` being compiled out; `SCRAM-SHA-256-PLUS` cannot be reached
    /// because `pg_SASL_init` only selects it when `ssl_in_use`
    /// (`fe-auth.c:489`).
    ///
    /// # Errors
    /// The exchange is not at its start, or the mechanism needs channel
    /// binding this build does not have.
    pub fn client_first_message(&mut self) -> Result<Vec<u8>, ScramError> {
        if self.state != State::Init {
            return Err(ScramError::InvalidExchangeState);
        }
        if self.mechanism == Mechanism::ScramSha256Plus {
            return Err(ScramError::ChannelBindingNotSupported);
        }

        self.client_first_message_bare = b"n=,r=".to_vec();
        self.client_first_message_bare
            .extend_from_slice(&self.client_nonce);

        let mut out = b"n,,".to_vec();
        out.extend_from_slice(&self.client_first_message_bare);
        self.state = State::NonceSent;
        Ok(out)
    }

    /// `read_server_first_message` (`fe-auth-scram.c:607`) followed by
    /// `build_client_final_message` (`:455`).
    ///
    /// # Errors
    /// The server-first-message is malformed, or its nonce does not extend
    /// ours.
    pub fn client_final_message(&mut self, server_first: &[u8]) -> Result<Vec<u8>, ScramError> {
        if self.state != State::NonceSent {
            return Err(ScramError::InvalidExchangeState);
        }
        Self::check_message(server_first)?;
        self.read_server_first_message(server_first)?;

        // fe-auth-scram.c:550 — base64 of "n,,", the only arm without TLS.
        let mut without_proof = b"c=biws,r=".to_vec();
        without_proof.extend_from_slice(&self.nonce);
        self.client_final_message_without_proof = without_proof;

        let proof = self.calculate_client_proof();
        let mut out = self.client_final_message_without_proof.clone();
        out.extend_from_slice(b",p=");
        out.extend_from_slice(&base64::encode(&proof));

        self.state = State::ProofSent;
        Ok(out)
    }

    /// `read_server_final_message` (`fe-auth-scram.c:693`) and
    /// `verify_server_signature` (`:846`).
    ///
    /// # Errors
    /// The server reported an error, the message is malformed, or the
    /// signature does not match the one this client computed.
    pub fn verify_server_final_message(&mut self, server_final: &[u8]) -> Result<(), ScramError> {
        if self.state != State::ProofSent {
            return Err(ScramError::InvalidExchangeState);
        }
        Self::check_message(server_final)?;
        self.read_server_final_message(server_final)?;

        // fe-auth-scram.c:866 — ServerKey from the SaltedPassword kept above.
        let mut ctx = HmacSha256::new(&server_key(&self.salted_password));
        ctx.update(&self.client_first_message_bare);
        ctx.update(b",");
        ctx.update(&self.server_first_message);
        ctx.update(b",");
        ctx.update(&self.client_final_message_without_proof);
        let expected = ctx.finish();

        self.state = State::Finished;
        if timingsafe_bcmp(&expected, &self.server_signature) {
            return Err(ScramError::IncorrectServerSignature);
        }
        Ok(())
    }

    /// `scram_exchange`'s length check, `fe-auth-scram.c:224`. The wire message
    /// is bytes with no NUL terminator; an embedded NUL is what C sees as a
    /// length mismatch, since `inputlen != strlen(input)` there.
    fn check_message(input: &[u8]) -> Result<(), ScramError> {
        if input.is_empty() {
            return Err(ScramError::EmptyMessage);
        }
        if input.contains(&0) {
            return Err(ScramError::LengthMismatch);
        }
        Ok(())
    }

    /// `read_server_first_message`, `fe-auth-scram.c:607`.
    fn read_server_first_message(&mut self, input: &[u8]) -> Result<(), ScramError> {
        self.server_first_message = input.to_vec();

        let (nonce, rest) = read_attr_value(input, b'r')?;
        // fe-auth-scram.c:633 — the server must have kept our nonce as a prefix.
        if nonce.len() < self.client_nonce.len()
            || timingsafe_bcmp(&nonce[..self.client_nonce.len()], &self.client_nonce)
        {
            return Err(ScramError::NonceMismatch);
        }
        self.nonce = nonce.to_vec();

        let (encoded_salt, rest) = read_attr_value(rest, b's')?;
        self.salt = base64::decode(encoded_salt).ok_or(ScramError::InvalidSalt)?;

        let (iterations, rest) = read_attr_value(rest, b'i')?;
        // fe-auth-scram.c:676 — strtol, then "*endptr != '\0' || < 1".
        let iterations = std::str::from_utf8(iterations)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&i| i >= 1)
            .ok_or(ScramError::InvalidIterationCount)?;
        self.iterations = iterations;

        // fe-auth-scram.c:683 — appended, but the exchange continues.
        if !rest.is_empty() {
            self.appended
                .push(ScramError::GarbageAtEndOfServerFirstMessage);
        }
        Ok(())
    }

    /// `read_server_final_message`, `fe-auth-scram.c:693`.
    fn read_server_final_message(&mut self, input: &[u8]) -> Result<(), ScramError> {
        // fe-auth-scram.c:708 — an `e=` message is the server's error.
        if input.first() == Some(&b'e') {
            let (errmsg, _) = read_attr_value(input, b'e')?;
            return Err(ScramError::ErrorFromServer(errmsg.to_vec()));
        }

        let (encoded_signature, rest) = read_attr_value(input, b'v')?;
        // fe-auth-scram.c:732 — appended, but parsing continues.
        if !rest.is_empty() {
            self.appended
                .push(ScramError::GarbageAtEndOfServerFinalMessage);
        }

        let decoded = base64::decode(encoded_signature).unwrap_or_default();
        if decoded.len() != KEY_LEN {
            return Err(ScramError::InvalidServerSignature);
        }
        self.server_signature.copy_from_slice(&decoded);
        Ok(())
    }

    /// `calculate_client_proof`, `fe-auth-scram.c:766`.
    fn calculate_client_proof(&mut self) -> [u8; KEY_LEN] {
        self.salted_password = salted_password(&self.password, &self.salt, self.iterations);
        let client_key = client_key(&self.salted_password);
        let stored_key = scram_h(&client_key);

        let mut ctx = HmacSha256::new(&stored_key);
        ctx.update(&self.client_first_message_bare);
        ctx.update(b",");
        ctx.update(&self.server_first_message);
        ctx.update(b",");
        ctx.update(&self.client_final_message_without_proof);
        let client_signature = ctx.finish();

        let mut proof = [0u8; KEY_LEN];
        for i in 0..KEY_LEN {
            proof[i] = client_key[i] ^ client_signature[i];
        }
        proof
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7677 section 3's transcript, computed here from its own inputs:
    /// password `pencil`, salt `W22ZaJ0SNY7soEsUEjb6gQ==` (the sixteen bytes
    /// `scram-common.h:43` points at), 4096 iterations and the client nonce
    /// `rOprNGfwEbeRWgbNEkqO`. This is the third-party vector — the proof and
    /// the signature below are the RFC's own `p=` and `v=` — and it pins
    /// PBKDF2, ClientKey, StoredKey, ServerKey and both signatures without
    /// going through [`ScramClient`] at all.
    ///
    /// Note the `n=user` in the client-first-message-bare: the RFC's client
    /// sends the user name, libpq leaves it empty ("the backend uses the value
    /// provided by the startup packet", `fe-auth-scram.c:387`). That one
    /// difference is why `the_libpq_exchange` below has a different proof over
    /// the same key material, and why this test exists separately.
    #[test]
    fn the_rfc_7677_vector() {
        let salt = base64::decode(b"W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        let salted = salted_password(b"pencil", &salt, 4096);
        let client_key = client_key(&salted);
        let stored_key = scram_h(&client_key);

        let auth_message: Vec<u8> = [
            &b"n=user,r=rOprNGfwEbeRWgbNEkqO"[..],
            b",",
            b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            b",",
            b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0",
        ]
        .concat();

        let client_signature = hmac_sha256(&stored_key, &auth_message);
        let mut proof = [0u8; KEY_LEN];
        for i in 0..KEY_LEN {
            proof[i] = client_key[i] ^ client_signature[i];
        }
        assert_eq!(
            base64::encode(&proof),
            b"dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=".to_vec(),
            "RFC 7677 p="
        );
        assert_eq!(
            base64::encode(&hmac_sha256(&server_key(&salted), &auth_message)),
            b"6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=".to_vec(),
            "RFC 7677 v="
        );
    }

    /// The same exchange as libpq drives it — `n=` instead of `n=user` — run
    /// through [`ScramClient`] end to end, so the message shapes are pinned on
    /// top of the key material `the_rfc_7677_vector` already pinned.
    #[test]
    fn the_libpq_exchange() {
        // The raw nonce whose base64 is "rOprNGfwEbeRWgbNEkqO".
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);

        assert_eq!(
            client.client_first_message().unwrap(),
            b"n,,n=,r=rOprNGfwEbeRWgbNEkqO".to_vec()
        );

        let server_first =
            b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        assert_eq!(
            client.client_final_message(server_first).unwrap(),
            b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=qvT2SWdEH5Q06albL+hjSYuUhCG7VndFyzIb7CK4n9k=".to_vec()
        );

        client
            .verify_server_final_message(b"v=3HO6Qt1M4MKJrmlKaoOqLAI0/0TV0HZe7J9H3MBtSOg=")
            .unwrap();
        assert!(client.is_finished());
        assert!(!client.channel_bound());
        assert!(client.appended_errors().is_empty());
    }

    /// A server that returns a different signature is rejected, so the vector
    /// above is not passing by construction.
    #[test]
    fn a_wrong_server_signature_is_rejected() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        client
            .client_final_message(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            )
            .unwrap();
        assert_eq!(
            client.verify_server_final_message(b"v=4HO6Qt1M4MKJrmlKaoOqLAI0/0TV0HZe7J9H3MBtSOg="),
            Err(ScramError::IncorrectServerSignature)
        );
    }

    /// A wrong password changes the proof, which is what the server checks.
    #[test]
    fn a_wrong_password_changes_the_client_proof() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil2", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        let final_message = client
            .client_final_message(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            )
            .unwrap();
        assert!(!final_message.ends_with(b",p=qvT2SWdEH5Q06albL+hjSYuUhCG7VndFyzIb7CK4n9k="));
    }

    /// `read_attr_value`, `fe-auth-scram.c:308`, including its two errors.
    #[test]
    fn read_attr_value_stops_at_the_comma() {
        assert_eq!(
            read_attr_value(b"r=abc,s=def", b'r').unwrap(),
            (&b"abc"[..], &b"s=def"[..])
        );
        assert_eq!(
            read_attr_value(b"i=4096", b'i').unwrap(),
            (&b"4096"[..], &b""[..])
        );
        assert_eq!(
            read_attr_value(b"r=,s=x", b'r').unwrap(),
            (&b""[..], &b"s=x"[..])
        );
        assert_eq!(
            read_attr_value(b"s=abc", b'r'),
            Err(ScramError::AttributeExpected(b'r'))
        );
        assert_eq!(
            read_attr_value(b"r:abc", b'r'),
            Err(ScramError::EqualsExpected(b'r'))
        );
        assert_eq!(
            read_attr_value(b"", b'r'),
            Err(ScramError::AttributeExpected(b'r'))
        );
    }

    /// The nonce check at `fe-auth-scram.c:633`: the server must echo our
    /// nonce as a prefix of its own.
    #[test]
    fn a_server_nonce_that_drops_ours_is_a_mismatch() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        assert_eq!(
            client.client_final_message(b"r=somethingelse,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
            Err(ScramError::NonceMismatch)
        );
        // Truncated to a prefix of ours is a mismatch too (the length test).
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        assert_eq!(
            client.client_final_message(b"r=rOprNGfwEbeRWgbNEkq,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
            Err(ScramError::NonceMismatch)
        );
    }

    /// The malformed server-first messages upstream names, each by its own
    /// message.
    #[test]
    fn a_malformed_server_first_message_is_rejected() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let cases: [(&[u8], ScramError); 5] = [
            (b"", ScramError::EmptyMessage),
            (
                b"s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
                ScramError::AttributeExpected(b'r'),
            ),
            (
                b"r=rOprNGfwEbeRWgbNEkqO,s=not base64!,i=4096",
                ScramError::InvalidSalt,
            ),
            (
                b"r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=0",
                ScramError::InvalidIterationCount,
            ),
            (
                b"r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=40x6",
                ScramError::InvalidIterationCount,
            ),
        ];
        for (input, expected) in cases {
            let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
            client.client_first_message().unwrap();
            assert_eq!(
                client.client_final_message(input),
                Err(expected),
                "input {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    /// `fe-auth-scram.c:683` and `:732` append an error and *keep going*; the
    /// exchange still completes. Losing that would turn a talkative server into
    /// a failed connection where libpq connects.
    #[test]
    fn garbage_at_the_end_is_recorded_but_not_fatal() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        client
            .client_final_message(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096,extra=x",
            )
            .unwrap();
        assert_eq!(
            client.appended_errors(),
            [ScramError::GarbageAtEndOfServerFirstMessage]
        );
        // The signature is over the server-first-message *including* its
        // garbage, since that is what the AuthMessage carries
        // (`fe-auth-scram.c:882` hashes `state->server_first_message`).
        assert_eq!(
            client
                .verify_server_final_message(b"v=JdJxE+E3l5wwCFKkJ8NP3jR1XX9Y/MAfphSU0EAmaI0=,x=1"),
            Ok(())
        );
        assert_eq!(
            client.appended_errors(),
            [
                ScramError::GarbageAtEndOfServerFirstMessage,
                ScramError::GarbageAtEndOfServerFinalMessage
            ]
        );
    }

    /// `fe-auth-scram.c:708` — `e=` in the server-final message.
    #[test]
    fn an_error_in_the_server_final_message_is_reported() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        client
            .client_final_message(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            )
            .unwrap();
        let err = client
            .verify_server_final_message(b"e=invalid-proof")
            .unwrap_err();
        assert_eq!(err, ScramError::ErrorFromServer(b"invalid-proof".to_vec()));
        assert_eq!(
            err.message(),
            b"error received from server in SCRAM exchange: invalid-proof".to_vec()
        );
    }

    /// A signature that decodes to the wrong length is malformed, not a
    /// mismatch (`fe-auth-scram.c:747`).
    #[test]
    fn a_short_server_signature_is_malformed() {
        let raw_nonce = base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256, &raw_nonce);
        client.client_first_message().unwrap();
        client
            .client_final_message(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            )
            .unwrap();
        assert_eq!(
            client.verify_server_final_message(b"v=c2hvcnQ="),
            Err(ScramError::InvalidServerSignature)
        );
    }

    /// Without TLS the only mechanism this build can complete is plain
    /// SCRAM-SHA-256 (`fe-auth-scram.c:538`, the `#else` arm).
    #[test]
    fn channel_binding_is_refused_by_this_build() {
        let mut client = ScramClient::new(b"pencil", Mechanism::ScramSha256Plus, &[0u8; 18]);
        assert_eq!(
            client.client_first_message(),
            Err(ScramError::ChannelBindingNotSupported)
        );
        assert_eq!(Mechanism::ScramSha256Plus.name(), b"SCRAM-SHA-256-PLUS");
        assert_eq!(Mechanism::ScramSha256.name(), b"SCRAM-SHA-256");
    }

    /// The SASLprep this build does: an ASCII password is what
    /// `pg_saslprep`'s fast path (`saslprep.c:1067`) returns, and a non-ASCII
    /// one is used raw, which is `scram_init`'s fallback arm
    /// (`fe-auth-scram.c:133`). See `docs/divergences.md`.
    #[test]
    fn an_ascii_password_is_unchanged_by_saslprep() {
        for password in [&b"pencil"[..], b"", b"a b c", b"~!@#$%^&*()_+"] {
            assert_eq!(saslprep(password), password.to_vec());
        }
        // The non-ASCII case is the divergence: no mapping, no folding.
        assert_eq!(
            saslprep("pa\u{00AD}ss".as_bytes()),
            "pa\u{00AD}ss".as_bytes()
        );
    }

    /// `timingsafe_bcmp` returns *nonzero for different*, like `memcmp`.
    #[test]
    fn timingsafe_bcmp_is_zero_only_for_equal_buffers() {
        assert!(!timingsafe_bcmp(b"abc", b"abc"));
        assert!(timingsafe_bcmp(b"abc", b"abd"));
        assert!(timingsafe_bcmp(b"abc", b"ab"));
        assert!(!timingsafe_bcmp(b"", b""));
    }

    /// The messages are upstream's format strings, byte for byte.
    #[test]
    fn every_message_is_upstreams_format_string() {
        assert_eq!(
            ScramError::AttributeExpected(b'r').message(),
            b"malformed SCRAM message (attribute \"r\" expected)".to_vec()
        );
        assert_eq!(
            ScramError::EqualsExpected(b's').message(),
            b"malformed SCRAM message (expected character \"=\" for attribute \"s\")".to_vec()
        );
        assert_eq!(
            ScramError::NonceMismatch.message(),
            b"invalid SCRAM response (nonce mismatch)".to_vec()
        );
        assert_eq!(
            ScramError::EmptyMessage.message(),
            b"malformed SCRAM message (empty message)".to_vec()
        );
    }
}
