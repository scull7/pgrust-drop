//! The URI arm of the connection-string parser.
//!
//! `postgresql://[user[:password]@][netloc][:port][,...][/dbname][?p=v&...]`,
//! transcribed from `conninfo_uri_parse_options` (`fe-connect.c:6813`), its
//! query-parameter loop (`:7054`) and the percent-decoder (`:7187`).
//!
//! The C routines walk a mutable copy of the URI and cut it into pieces by
//! writing NULs; these walk the original and take slices, which is why every
//! loop bound reads through [`crate::cstr::at`] — the terminator a C loop tests
//! for is "past the end" here, and nothing else about the loops changes.

use crate::conninfo::{ConnInfo, uri_prefix_length};
use crate::cstr::at;
use crate::error::ConnError;

/// `conninfo_uri_parse` (`fe-connect.c:6759`) with `use_defaults` false.
///
/// # Errors
/// The [`ConnError`] libpq would have left in its error buffer.
pub fn parse_uri(uri: &[u8]) -> Result<ConnInfo, ConnError> {
    let mut options = ConnInfo::new();
    parse_uri_options(&mut options, uri)?;
    Ok(options)
}

/// Store a value under a keyword this module took from `PQconninfoOptions[]`
/// itself, where `conninfo_find` cannot fail.
fn store_builtin(options: &mut ConnInfo, keyword: &'static str, value: &[u8]) {
    assert!(
        options.set(keyword.as_bytes(), value).is_ok(),
        "\"{keyword}\" is not in PQconninfoOptions"
    );
}

/// `conninfo_uri_parse_options` (`fe-connect.c:6813`).
fn parse_uri_options(options: &mut ConnInfo, uri: &[u8]) -> Result<(), ConnError> {
    let buf = uri;

    // Skip the URI prefix
    let prefix_len = uri_prefix_length(uri);
    if prefix_len == 0 {
        // Should never happen
        return Err(ConnError::InvalidUriPropagated(uri.into()));
    }
    let start = prefix_len;
    let mut p = start;
    let mut prevchar = 0u8;

    // Look ahead for possible user credentials designator
    while at(buf, p) != 0 && at(buf, p) != b'@' && at(buf, p) != b'/' {
        p += 1;
    }
    if at(buf, p) == b'@' {
        // "scheme://user[:password]@[netloc]"
        let user_start = start;
        p = user_start;
        while at(buf, p) != b':' && at(buf, p) != b'@' {
            p += 1;
        }
        prevchar = at(buf, p);
        let user = &buf[user_start..p];
        if !user.is_empty() {
            let user = uri_decode(user)?;
            store_builtin(options, "user", &user);
        }
        if prevchar == b':' {
            let password_start = p + 1;
            while at(buf, p) != b'@' {
                p += 1;
            }
            let password = &buf[password_start..p];
            if !password.is_empty() {
                let password = uri_decode(password)?;
                store_builtin(options, "password", &password);
            }
        }
        // Advance past end of parsed user name or password token
        p += 1;
    } else {
        // No username/password designator found.  Reset to start of URI.
        p = start;
    }

    let mut hostbuf = Vec::new();
    let mut portbuf = Vec::new();
    parse_netlocs(buf, &mut p, &mut prevchar, &mut hostbuf, &mut portbuf)?;

    // Save final values for host and port.
    if !hostbuf.is_empty() {
        let host = uri_decode(&hostbuf)?;
        store_builtin(options, "host", &host);
    }
    if !portbuf.is_empty() {
        let port = uri_decode(&portbuf)?;
        store_builtin(options, "port", &port);
    }

    if prevchar != 0 && prevchar != b'?' {
        // Advance past host terminator
        p += 1;
        let dbname_start = p;
        while at(buf, p) != 0 && at(buf, p) != b'?' {
            p += 1;
        }
        prevchar = at(buf, p);
        // Avoid setting dbname to an empty string, as it forces the default
        // value (username) and ignores $PGDATABASE, as opposed to not setting
        // it at all.
        let dbname = &buf[dbname_start..p];
        if !dbname.is_empty() {
            let dbname = uri_decode(dbname)?;
            store_builtin(options, "dbname", &dbname);
        }
    }

    if prevchar != 0 {
        // Advance past terminator
        p += 1;
        parse_uri_params(&buf[p..], options)?;
    }

    Ok(())
}

/// The `for (;;)` over comma-separated `netloc[:port]` pairs
/// (`fe-connect.c:6893`), filling the two buffers whose contents become the
/// `host` and `port` values.
fn parse_netlocs(
    buf: &[u8],
    p: &mut usize,
    prevchar: &mut u8,
    hostbuf: &mut Vec<u8>,
    portbuf: &mut Vec<u8>,
) -> Result<(), ConnError> {
    loop {
        let host_start;
        let host_end;
        if at(buf, *p) == b'[' {
            // Look for IPv6 address.
            *p += 1;
            host_start = *p;
            while at(buf, *p) != 0 && at(buf, *p) != b']' {
                *p += 1;
            }
            if at(buf, *p) == 0 {
                return Err(ConnError::Ipv6Unterminated(buf.into()));
            }
            if *p == host_start {
                return Err(ConnError::Ipv6Empty(buf.into()));
            }
            host_end = *p;
            // Cut off the bracket and advance
            *p += 1;
            // The address may be followed by a port specifier or a slash or a
            // query or a separator comma.
            let character = at(buf, *p);
            if character != 0 && !matches!(character, b':' | b'/' | b'?' | b',') {
                return Err(ConnError::UnexpectedCharacter {
                    character,
                    position: *p + 1,
                    uri: buf.into(),
                });
            }
        } else {
            // not an IPv6 address: DNS-named or IPv4 netloc
            host_start = *p;
            while at(buf, *p) != 0 && !matches!(at(buf, *p), b':' | b'/' | b'?' | b',') {
                *p += 1;
            }
            host_end = *p;
        }

        // Save the hostname terminator before we null it
        *prevchar = at(buf, *p);
        hostbuf.extend_from_slice(&buf[host_start..host_end]);

        if *prevchar == b':' {
            // advance past host terminator
            *p += 1;
            let port_start = *p;
            while at(buf, *p) != 0 && !matches!(at(buf, *p), b'/' | b'?' | b',') {
                *p += 1;
            }
            *prevchar = at(buf, *p);
            portbuf.extend_from_slice(&buf[port_start..*p]);
        }

        if *prevchar != b',' {
            return Ok(());
        }
        // advance past comma separator
        *p += 1;
        hostbuf.push(b',');
        portbuf.push(b',');
    }
}

/// `conninfo_uri_parse_params` (`fe-connect.c:7054`): split on `&` and `=`
/// *before* decoding, then decode each half.
fn parse_uri_params(params: &[u8], options: &mut ConnInfo) -> Result<(), ConnError> {
    let mut pos = 0usize;
    while at(params, pos) != 0 {
        let start = pos;
        let mut separator: Option<usize> = None;
        let mut p = pos;
        // Scan the params string for '=' and '&', marking the end of keyword
        // and value respectively.
        let (keyword, value) = loop {
            let character = at(params, p);
            if character == b'=' {
                // Was there '=' already?
                if let Some(first) = separator {
                    return Err(ConnError::ExtraSeparator(params[start..first].into()));
                }
                // Cut off keyword, advance to value
                separator = Some(p);
                p += 1;
            } else if character == b'&' || character == 0 {
                let end = p;
                // If not at the end, cut off value and advance; leave pos
                // pointing to the start of the next parameter, if any.
                pos = if character == 0 { end } else { end + 1 };
                // Was there '=' at all?
                let Some(first) = separator else {
                    return Err(ConnError::MissingSeparator(params[start..end].into()));
                };
                break (&params[start..first], &params[first + 1..end]);
            } else {
                // Advance over all other bytes.
                p += 1;
            }
        };

        let keyword = uri_decode(keyword)?;
        let value = uri_decode(value)?;

        // Special keyword handling for improved JDBC compatibility
        // (fe-connect.c:7127).
        let (keyword, value) = if keyword == b"ssl" && value == b"true" {
            (b"sslmode".to_vec(), b"require".to_vec())
        } else {
            (keyword, value)
        };

        // Store the value if the corresponding option exists; C passes
        // ignoreMissing and then supplies this message itself when no other
        // one was appended (fe-connect.c:7147).
        options
            .set(&keyword, &value)
            .map_err(|unknown| ConnError::InvalidUriQueryParameter(unknown.into_keyword()))?;
    }
    Ok(())
}

/// `conninfo_uri_decode` (`fe-connect.c:7187`): replace every `%xy` triplet,
/// tolerate leading and trailing spaces, reject any other space.
///
/// # Errors
/// [`ConnError::InvalidPercentEncoding`],
/// [`ConnError::ForbiddenNulInPercentEncoding`] or
/// [`ConnError::UnexpectedSpaces`], each quoting `str` as it was handed in —
/// spaces included, which is what the stolen table expects.
pub fn uri_decode(str: &[u8]) -> Result<Vec<u8>, ConnError> {
    let mut buf = Vec::with_capacity(str.len());
    let mut q = 0usize;

    // skip leading whitespaces
    while at(str, q) == b' ' {
        q += 1;
    }

    loop {
        if at(str, q) == b'%' {
            // skip the percent sign itself
            q += 1;
            // Possible EOL will be caught by the first call to get_hexdigit(),
            // so we never dereference an invalid q pointer.
            let hi = hexdigit(at(str, q));
            q += 1;
            let Some(hi) = hi else {
                return Err(ConnError::InvalidPercentEncoding(str.into()));
            };
            let lo = hexdigit(at(str, q));
            q += 1;
            let Some(lo) = lo else {
                return Err(ConnError::InvalidPercentEncoding(str.into()));
            };
            let character = (hi << 4) | lo;
            if character == 0 {
                return Err(ConnError::ForbiddenNulInPercentEncoding(str.into()));
            }
            buf.push(character);
        } else {
            // if found a whitespace or NUL, the string ends
            if at(str, q) == b' ' || at(str, q) == 0 {
                break;
            }
            // copy character
            buf.push(at(str, q));
            q += 1;
        }
    }

    // skip trailing whitespaces
    while at(str, q) == b' ' {
        q += 1;
    }

    // Not at the end of the string yet?  Fail.
    if at(str, q) != 0 {
        return Err(ConnError::UnexpectedSpaces(str.into()));
    }

    Ok(buf)
}

/// `get_hexdigit` (`fe-connect.c:7276`): lower- and upper-case A-F are treated
/// identically.
fn hexdigit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(uri: &[u8], keyword: &str) -> Option<String> {
        let options = parse_uri(uri).expect("parses");
        options
            .get(keyword)
            .map(|value| String::from_utf8_lossy(value).into_owned())
    }

    fn decoded(str: &[u8]) -> String {
        String::from_utf8_lossy(&uri_decode(str).expect("decodes")).into_owned()
    }

    #[test]
    fn a_percent_triplet_is_case_insensitive() {
        assert_eq!(decoded(b"%68ost"), "host");
        assert_eq!(decoded(b"%68%4Fst"), "hOst");
        assert_eq!(decoded(b"%2fvar%2Flib"), "/var/lib");
    }

    #[test]
    fn a_triplet_may_decode_to_a_byte_that_is_not_utf8() {
        assert_eq!(uri_decode(b"a%C3b"), Ok(vec![b'a', 0xC3, b'b']));
    }

    #[test]
    fn leading_and_trailing_spaces_are_stripped_but_inner_ones_are_an_error() {
        assert_eq!(decoded(b"  user  "), "user");
        assert_eq!(
            uri_decode(b"  user user  "),
            Err(ConnError::UnexpectedSpaces("  user user  ".into()))
        );
        // The message quotes the token as it was handed in, spaces included.
        assert_eq!(
            uri_decode(b" 12345 12 "),
            Err(ConnError::UnexpectedSpaces(" 12345 12 ".into()))
        );
    }

    #[test]
    fn a_percent_sign_needs_two_hex_digits_after_it() {
        for token in [&b"%"[..], b"%1", b"%zz", b"%XXfoo"] {
            assert_eq!(
                uri_decode(token),
                Err(ConnError::InvalidPercentEncoding(token.into())),
                "{}",
                String::from_utf8_lossy(token)
            );
        }
    }

    #[test]
    fn percent_zero_zero_is_forbidden_rather_than_a_nul_byte() {
        assert_eq!(
            uri_decode(b"a%00b"),
            Err(ConnError::ForbiddenNulInPercentEncoding("a%00b".into()))
        );
    }

    #[test]
    fn an_empty_string_decodes_to_an_empty_string() {
        assert_eq!(uri_decode(b""), Ok(Vec::new()));
        assert_eq!(uri_decode(b"   "), Ok(Vec::new()));
    }

    /// The comma extension `conninfo_uri_parse_options` documents
    /// (`fe-connect.c:6828`): one `host` and one `port` value, each a list.
    /// `001_uri.pl` has no case for it; NAT-394 is where it earns its tests.
    #[test]
    fn several_netlocs_become_one_comma_separated_host_and_port() {
        assert_eq!(
            value_of(b"postgresql://a:1,b:2,c:3/db", "host").as_deref(),
            Some("a,b,c")
        );
        assert_eq!(
            value_of(b"postgresql://a:1,b:2,c:3/db", "port").as_deref(),
            Some("1,2,3")
        );
        // A netloc without a port still contributes its empty slot, so the two
        // lists stay aligned.
        assert_eq!(
            value_of(b"postgresql://a,b:2/db", "port").as_deref(),
            Some(",2")
        );
    }

    #[test]
    fn a_bracketed_ipv6_address_loses_its_brackets() {
        assert_eq!(
            value_of(b"postgresql://[2001:db8::1234]:5/db", "host").as_deref(),
            Some("2001:db8::1234")
        );
        assert_eq!(
            value_of(b"postgresql://[2001:db8::1234]:5/db", "port").as_deref(),
            Some("5")
        );
    }

    /// `fe-connect.c:6948` counts bytes from the start of the whole URI, 1-based.
    #[test]
    fn the_unexpected_character_position_counts_from_the_start_of_the_uri() {
        assert_eq!(
            parse_uri(b"postgres://[::1]z"),
            Err(ConnError::UnexpectedCharacter {
                character: b'z',
                position: 17,
                uri: "postgres://[::1]z".into(),
            })
        );
    }

    /// Special keyword handling for improved JDBC compatibility
    /// (`fe-connect.c:7127`), which no row of `001_uri.pl` exercises.
    #[test]
    fn ssl_true_is_rewritten_to_sslmode_require() {
        assert_eq!(
            value_of(b"postgresql://host?ssl=true", "sslmode").as_deref(),
            Some("require")
        );
        // Only that exact pair; anything else is the ordinary unknown keyword.
        assert_eq!(
            parse_uri(b"postgresql://host?ssl=false"),
            Err(ConnError::InvalidUriQueryParameter("ssl".into()))
        );
    }

    /// A query parameter is split on `&` and `=` before it is decoded, so a
    /// percent-encoded separator is data and not a separator
    /// (`fe-connect.c:7068`).
    #[test]
    fn a_percent_encoded_separator_does_not_split_a_parameter() {
        assert_eq!(
            value_of(b"postgresql://host/db?options=a%26b", "options").as_deref(),
            Some("a&b")
        );
        assert_eq!(
            value_of(b"postgresql://host/db?options=a%3Db", "options").as_deref(),
            Some("a=b")
        );
    }

    #[test]
    fn a_non_uri_never_reaches_this_parser() {
        assert_eq!(
            parse_uri(b"host=example.com"),
            Err(ConnError::InvalidUriPropagated("host=example.com".into()))
        );
    }

    #[test]
    fn a_hex_digit_is_only_a_hex_digit() {
        assert_eq!(hexdigit(b'0'), Some(0));
        assert_eq!(hexdigit(b'9'), Some(9));
        assert_eq!(hexdigit(b'a'), Some(10));
        assert_eq!(hexdigit(b'F'), Some(15));
        assert_eq!(hexdigit(b'g'), None);
        assert_eq!(hexdigit(b'G'), None);
        assert_eq!(hexdigit(b' '), None);
    }
}
