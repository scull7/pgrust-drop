//! The messages `PQconninfoParse` leaves in its error buffer, and the ones
//! `PQconnectPoll` appends before it opens anything.
//!
//! Each variant is one `libpq_append_error` / `libpq_append_conn_error` call
//! site in `fe-connect.c`, and the rendering is that call's format string with
//! its arguments substituted.
//! The message is built as *bytes* in one place ([`ConnError::message`]) and
//! [`fmt::Display`] is derived from it, so there is a single spelling of each
//! string to drift and a token that is not UTF-8 still reaches stderr as the
//! bytes C would have written.

use std::fmt;

use crate::text::RawText;

/// Why a connection string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// `fe-connect.c:6347` — a keyword with no `=` after it.
    MissingEquals(RawText),
    /// `fe-connect.c:6395` — a `'`-quoted value that never closes.
    UnterminatedQuotedString,
    /// `fe-connect.c:7360` — `conninfo_find` knows no such keyword, and the
    /// caller was not willing to ignore that.
    InvalidConnectionOption(RawText),
    /// `fe-connect.c:6849` — `parse_connection_string` routed a non-URI here.
    InvalidUriPropagated(RawText),
    /// `fe-connect.c:6926` — `[` with no `]`.
    Ipv6Unterminated(RawText),
    /// `fe-connect.c:6933` — `[]`.
    Ipv6Empty(RawText),
    /// `fe-connect.c:6948` — something other than `:`, `/`, `?` or `,` after
    /// the closing bracket. `position` is 1-based and counts bytes from the
    /// start of the whole URI.
    UnexpectedCharacter {
        character: u8,
        position: usize,
        uri: RawText,
    },
    /// `fe-connect.c:7077` — `?key=a=b`.
    ExtraSeparator(RawText),
    /// `fe-connect.c:7097` — `?key` with no `=`.
    MissingSeparator(RawText),
    /// `fe-connect.c:7149` — a query parameter that is not a conninfo keyword.
    InvalidUriQueryParameter(RawText),
    /// `fe-connect.c:7233` — `%` not followed by two hex digits.
    InvalidPercentEncoding(RawText),
    /// `fe-connect.c:7243` — `%00`.
    ForbiddenNulInPercentEncoding(RawText),
    /// `fe-connect.c:7265` — a space that is not leading or trailing.
    UnexpectedSpaces(RawText),
    /// `fe-connect.c:3046` — `PQconnectPoll` read the `port` as an `int`, but
    /// it is outside 1..=65535.
    InvalidPortNumber(RawText),
    /// `fe-connect.c:8231` — `pqParseIntParam` could not read the whole value
    /// of an integer-valued option. `option` is its `context` argument, a
    /// literal at the call site rather than anything that came off the wire.
    InvalidIntegerValue {
        value: RawText,
        option: &'static str,
    },
}

impl ConnError {
    /// The bytes libpq's error buffer would hold, without the trailing newline
    /// `libpq_append_error` adds (`fe-misc.c:1539`).
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            ConnError::MissingEquals(name) => wrap(
                "missing \"=\" after \"",
                name,
                "\" in connection info string",
            ),
            ConnError::UnterminatedQuotedString => {
                b"unterminated quoted string in connection info string".to_vec()
            }
            ConnError::InvalidConnectionOption(keyword) => {
                wrap("invalid connection option \"", keyword, "\"")
            }
            ConnError::InvalidUriPropagated(uri) => wrap(
                "invalid URI propagated to internal parser routine: \"",
                uri,
                "\"",
            ),
            ConnError::Ipv6Unterminated(uri) => wrap(
                "end of string reached when looking for matching \"]\" in IPv6 host address in URI: \"",
                uri,
                "\"",
            ),
            ConnError::Ipv6Empty(uri) => {
                wrap("IPv6 host address may not be empty in URI: \"", uri, "\"")
            }
            ConnError::UnexpectedCharacter {
                character,
                position,
                uri,
            } => {
                let mut out = b"unexpected character \"".to_vec();
                out.push(*character);
                out.extend_from_slice(
                    format!("\" at position {position} in URI (expected \":\" or \"/\"): \"")
                        .as_bytes(),
                );
                out.extend_from_slice(uri.as_bytes());
                out.push(b'"');
                out
            }
            ConnError::ExtraSeparator(keyword) => wrap(
                "extra key/value separator \"=\" in URI query parameter: \"",
                keyword,
                "\"",
            ),
            ConnError::MissingSeparator(keyword) => wrap(
                "missing key/value separator \"=\" in URI query parameter: \"",
                keyword,
                "\"",
            ),
            ConnError::InvalidUriQueryParameter(keyword) => {
                wrap("invalid URI query parameter: \"", keyword, "\"")
            }
            ConnError::InvalidPercentEncoding(token) => {
                wrap("invalid percent-encoded token: \"", token, "\"")
            }
            ConnError::ForbiddenNulInPercentEncoding(token) => wrap(
                "forbidden value %00 in percent-encoded value: \"",
                token,
                "\"",
            ),
            ConnError::UnexpectedSpaces(token) => wrap(
                "unexpected spaces found in \"",
                token,
                "\", use percent-encoded spaces (%20) instead",
            ),
            ConnError::InvalidPortNumber(port) => wrap("invalid port number: \"", port, "\""),
            ConnError::InvalidIntegerValue { value, option } => wrap(
                "invalid integer value \"",
                value,
                &format!("\" for connection option \"{option}\""),
            ),
        }
    }
}

/// `prefix`, the token's raw bytes, then `suffix` — the shape of all but one
/// of the format strings above.
fn wrap(prefix: &str, token: &RawText, suffix: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + token.len() + suffix.len());
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(token.as_bytes());
    out.extend_from_slice(suffix.as_bytes());
    out
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for ConnError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seven URI messages `001_uri.pl` pins, spelled as the table spells
    /// them (`t/001_uri.pl:147`, `:152`, `:157`, `:162`, `:172`, `:176`, `:181`).
    #[test]
    fn the_uri_messages_are_the_ones_the_stolen_table_expects() {
        let cases: [(ConnError, &str); 7] = [
            (
                ConnError::Ipv6Unterminated("postgres://[::1".into()),
                "end of string reached when looking for matching \"]\" in IPv6 host address in URI: \"postgres://[::1\"",
            ),
            (
                ConnError::Ipv6Empty("postgres://[]".into()),
                "IPv6 host address may not be empty in URI: \"postgres://[]\"",
            ),
            (
                ConnError::UnexpectedCharacter {
                    character: b'z',
                    position: 17,
                    uri: "postgres://[::1]z".into(),
                },
                "unexpected character \"z\" at position 17 in URI (expected \":\" or \"/\"): \"postgres://[::1]z\"",
            ),
            (
                ConnError::MissingSeparator("zzz".into()),
                "missing key/value separator \"=\" in URI query parameter: \"zzz\"",
            ),
            (
                ConnError::ExtraSeparator("key".into()),
                "extra key/value separator \"=\" in URI query parameter: \"key\"",
            ),
            (
                ConnError::InvalidPercentEncoding("%XXfoo".into()),
                "invalid percent-encoded token: \"%XXfoo\"",
            ),
            (
                ConnError::ForbiddenNulInPercentEncoding("a%00b".into()),
                "forbidden value %00 in percent-encoded value: \"a%00b\"",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.message(), expected.as_bytes());
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn the_key_value_messages_are_the_ones_conninfo_parse_appends() {
        assert_eq!(
            ConnError::MissingEquals("postgre://".into()).to_string(),
            "missing \"=\" after \"postgre://\" in connection info string"
        );
        assert_eq!(
            ConnError::UnterminatedQuotedString.to_string(),
            "unterminated quoted string in connection info string"
        );
        assert_eq!(
            ConnError::InvalidConnectionOption("uzer".into()).to_string(),
            "invalid connection option \"uzer\""
        );
    }

    /// The quoted token is copied through as bytes: `Display` may have to be
    /// lossy, but `message` is what reaches stderr and it is not.
    #[test]
    fn a_token_that_is_not_utf8_reaches_the_message_unchanged() {
        let error = ConnError::InvalidUriQueryParameter(RawText::new(vec![0xFF, 0xFE]));
        assert_eq!(
            error.message(),
            b"invalid URI query parameter: \"\xff\xfe\"".to_vec()
        );
    }

    /// The two messages a bad `port` produces: the range one at
    /// `fe-connect.c:3046`, and `pqParseIntParam`'s at `:8231` for a value
    /// `strtol` cannot read in full.
    #[test]
    fn the_port_messages_are_the_ones_pq_connect_poll_appends() {
        assert_eq!(
            ConnError::InvalidPortNumber("99999".into()).to_string(),
            "invalid port number: \"99999\""
        );
        assert_eq!(
            ConnError::InvalidIntegerValue {
                value: "abc".into(),
                option: "port",
            }
            .to_string(),
            "invalid integer value \"abc\" for connection option \"port\""
        );
    }

    /// `libpq_append_error` appends the newline, not the format string
    /// (`fe-misc.c:1539`), so `message` must not carry one.
    #[test]
    fn a_message_does_not_carry_the_newline_libpq_append_error_adds() {
        assert!(
            !ConnError::UnterminatedQuotedString
                .message()
                .ends_with(b"\n")
        );
    }
}
