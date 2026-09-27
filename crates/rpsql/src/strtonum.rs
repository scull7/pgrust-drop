//! The C library's number conversions psql leans on, as they read in the C
//! locale: `strtod` and base-10 `strtol`, `strtoint` over it
//! (`src/common/string.c:50`), and `printf`'s `%g`.
//!
//! `\watch` reads its interval with `strtod` and its counts with `strtoint`
//! (`command.c:3425`, `:3445`, `:3465`, `:3491`), `ParseVariableDouble`
//! reads `WATCH_INTERVAL` with `strtod` (`variables.c:212`), and `do_watch`
//! titles each run with `%g` (`command.c:5995`). All of them report through
//! `errno` and `endptr`, so each conversion here hands back the value, how
//! far it read, and whether it would have set `ERANGE`.
//!
//! psql calls `setlocale(LC_ALL, "")` (`src/common/exec.c:437`), so C reads
//! the decimal point of `LC_NUMERIC`; these read `.` whatever the locale (see
//! `docs/divergences.md`).

/// What a conversion hands back: the value, how many bytes it consumed
/// (`*endptr - str`; 0 when there was no number at all), and whether it set
/// `errno` to `ERANGE`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Converted<T> {
    /// The converted value.
    pub value: T,
    /// Bytes consumed.
    pub end: usize,
    /// `errno == ERANGE`.
    pub erange: bool,
}

/// `isspace` in the C locale.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Leading white space and an optional sign: where the number starts, and
/// whether it is negative.
fn skip_space_and_sign(b: &[u8]) -> (usize, bool) {
    let mut i = b.iter().take_while(|&&c| is_c_space(c)).count();
    let negative = b.get(i) == Some(&b'-');
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    (i, negative)
}

fn starts_with_ignore_case(b: &[u8], word: &[u8]) -> bool {
    b.len() >= word.len() && b[..word.len()].eq_ignore_ascii_case(word)
}

/// `ERANGE` for a finite, nonzero input: the result overflowed to an
/// infinity, or underflowed to zero or below the normal range. (C99 lets
/// the library choose for a subnormal result; glibc, musl and macOS all set
/// `ERANGE` when it is inexact, which a decimal subnormal practically always
/// is.)
fn out_of_range(value: f64, nonzero: bool) -> bool {
    value.is_infinite() || (nonzero && value.abs() < f64::MIN_POSITIVE)
}

/// `strtod(s, &end)` in the C locale: leading white space, a sign, then
/// `inf`/`infinity`, `nan` or `nan(…)`, a hexadecimal float (`0x1.8p3`),
/// or a decimal one, read as far as it forms a number.
#[must_use]
pub fn strtod(s: &str) -> Converted<f64> {
    let b = s.as_bytes();
    let (start, negative) = skip_space_and_sign(b);
    let signed = |v: f64| if negative { -v } else { v };
    let rest = &b[start..];
    let none = Converted {
        value: 0.0,
        end: 0,
        erange: false,
    };

    if starts_with_ignore_case(rest, b"inf") {
        let len = if starts_with_ignore_case(rest, b"infinity") {
            8
        } else {
            3
        };
        return Converted {
            value: signed(f64::INFINITY),
            end: start + len,
            erange: false,
        };
    }
    if starts_with_ignore_case(rest, b"nan") {
        let mut len = 3;
        if rest.get(3) == Some(&b'(') {
            let inner = rest[4..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                .count();
            if rest.get(4 + inner) == Some(&b')') {
                len = 5 + inner;
            }
        }
        return Converted {
            value: signed(f64::NAN),
            end: start + len,
            erange: false,
        };
    }
    if (rest.starts_with(b"0x") || rest.starts_with(b"0X"))
        && let Some((magnitude, len, nonzero)) = hex_float(&rest[2..])
    {
        let value = signed(magnitude);
        return Converted {
            value,
            end: start + 2 + len,
            erange: out_of_range(value, nonzero),
        };
    }

    // A decimal float: digits, an optional point and digits (at least one
    // digit between them), then an exponent only if it has digits.
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int_digits = digits(start);
    let mut end = start + int_digits;
    let mut frac_digits = 0;
    if b.get(end) == Some(&b'.') {
        frac_digits = digits(end + 1);
        if int_digits + frac_digits > 0 {
            end += 1 + frac_digits;
        }
    }
    if int_digits + frac_digits == 0 {
        return none;
    }
    let nonzero = b[start..end].iter().any(|c| (b'1'..=b'9').contains(c));
    if matches!(b.get(end), Some(b'e' | b'E')) {
        let mut exp = end + 1;
        if matches!(b.get(exp), Some(b'+' | b'-')) {
            exp += 1;
        }
        let exp_digits = digits(exp);
        if exp_digits > 0 {
            end = exp + exp_digits;
        }
    }
    // Rust's parser rounds correctly, as C's does, and needs no sign.
    let magnitude: f64 = s[start..end].parse().unwrap_or(0.0);
    let value = signed(magnitude);
    Converted {
        value,
        end,
        erange: out_of_range(value, nonzero),
    }
}

/// The part of a hexadecimal float after its `0x`: the magnitude, the bytes
/// consumed, and whether any mantissa digit was nonzero; `None` when there
/// is no hex digit, so that `strtod` reads the `0` alone.
fn hex_float(b: &[u8]) -> Option<(f64, usize, bool)> {
    let mut mantissa: u64 = 0;
    let mut sticky = false;
    let mut exp: i64 = 0;
    let mut any = false;
    let mut i = 0;
    let mut seen_point = false;
    loop {
        match b.get(i) {
            Some(&c) if c.is_ascii_hexdigit() => {
                any = true;
                let d = u64::from(char::from(c).to_digit(16).unwrap_or(0));
                if mantissa >> 60 == 0 {
                    mantissa = mantissa << 4 | d;
                    if seen_point {
                        exp -= 4;
                    }
                } else {
                    sticky |= d != 0;
                    if !seen_point {
                        exp += 4;
                    }
                }
            }
            Some(b'.') if !seen_point => seen_point = true,
            _ => break,
        }
        i += 1;
    }
    if !any {
        return None;
    }
    if matches!(b.get(i), Some(b'p' | b'P')) {
        let mut j = i + 1;
        let negative = b.get(j) == Some(&b'-');
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let digits = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            let e = b[j..j + digits].iter().fold(0_i64, |acc, &c| {
                acc.saturating_mul(10).saturating_add(i64::from(c - b'0'))
            });
            exp = exp.saturating_add(if negative { -e } else { e });
            i = j + digits;
        }
    }
    let nonzero = mantissa != 0 || sticky;
    // The sticky bit sits below the 53 bits a double keeps, so the one
    // rounding the conversion does is still to nearest.
    #[allow(clippy::cast_precision_loss)]
    let mut value = (mantissa | u64::from(sticky)) as f64;
    let mut exp = exp.clamp(-5000, 5000);
    while exp > 1000 {
        value *= 2f64.powi(1000);
        exp -= 1000;
    }
    while exp < -1000 {
        value *= 2f64.powi(-1000);
        exp += 1000;
    }
    let exp = i32::try_from(exp).unwrap_or(0);
    Some((value * 2f64.powi(exp), i, nonzero))
}

/// `strtol(s, &end, 10)` for a 64-bit `long`: white space, a sign, digits.
/// Out of range, it is `LONG_MAX` or `LONG_MIN` with `ERANGE`.
#[must_use]
pub fn strtol(s: &str) -> Converted<i64> {
    let b = s.as_bytes();
    let (start, negative) = skip_space_and_sign(b);
    let digits = b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return Converted {
            value: 0,
            end: 0,
            erange: false,
        };
    }
    let mut value: i64 = 0;
    let mut erange = false;
    for &c in &b[start..start + digits] {
        let d = i64::from(c - b'0');
        let next = value.checked_mul(10).and_then(|v| {
            if negative {
                v.checked_sub(d)
            } else {
                v.checked_add(d)
            }
        });
        if let Some(v) = next {
            value = v;
        } else {
            erange = true;
            value = if negative { i64::MIN } else { i64::MAX };
            break;
        }
    }
    Converted {
        value,
        end: start + digits,
        erange,
    }
}

/// `strtoint(s, &end, 10)` (`src/common/string.c:50`): `strtol`, then
/// `ERANGE` as well when the `long` does not fit an `int`.
#[must_use]
pub fn strtoint(s: &str) -> Converted<i32> {
    let long = strtol(s);
    let fits = i32::try_from(long.value);
    Converted {
        // `(int) val`: the low 32 bits.
        #[allow(clippy::cast_possible_truncation)]
        value: long.value as i32,
        end: long.end,
        erange: long.erange || fits.is_err(),
    }
}

/// `printf("%g", x)`: six significant digits, in `%e` style when the
/// exponent is below -4 or at least 6, else `%f` style, trailing zeros and
/// a trailing point dropped either way.
#[must_use]
pub fn format_g(x: f64) -> String {
    if x.is_nan() {
        return if x.is_sign_negative() { "-nan" } else { "nan" }.to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let trim = |s: String| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    // The exponent `%e` would print at precision 5 decides the style.
    let e_style = format!("{x:.5e}");
    let (mantissa, exp) = e_style.split_once('e').unwrap_or((&e_style, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let precision = usize::try_from(5 - exp).unwrap_or(0);
        trim(format!("{x:.precision$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!(
            "{}e{sign}{:02}",
            trim(mantissa.to_string()),
            exp.unsigned_abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> (f64, usize, bool) {
        let c = strtod(s);
        (c.value, c.end, c.erange)
    }

    #[test]
    fn strtod_reads_as_far_as_a_number_goes() {
        assert_eq!(d("0.01"), (0.01, 4, false));
        assert_eq!(d("  -2.5e1x"), (-25.0, 8, false));
        assert_eq!(d("10ab"), (10.0, 2, false));
        assert_eq!(d("1e"), (1.0, 1, false));
        assert_eq!(d("1e+"), (1.0, 1, false));
        assert_eq!(d(".5"), (0.5, 2, false));
        assert_eq!(d("5."), (5.0, 2, false));
        assert_eq!(d("1.e2"), (100.0, 4, false));
        assert_eq!(d("."), (0.0, 0, false));
        assert_eq!(d(""), (0.0, 0, false));
        assert_eq!(d("x"), (0.0, 0, false));
        assert_eq!(d("+"), (0.0, 0, false));
        assert_eq!(d("-0"), (0.0, 2, false));
        assert!(strtod("-0").value.is_sign_negative());
    }

    #[test]
    fn strtod_sets_erange_on_overflow_and_underflow() {
        assert_eq!(d("10e400"), (f64::INFINITY, 6, true));
        assert_eq!(d("-1e500"), (f64::NEG_INFINITY, 6, true));
        assert_eq!(d("1e-400"), (0.0, 6, true));
        assert!(strtod("1e-310").erange);
        assert!(!strtod("0e-400").erange);
        assert!(!strtod("0.0").erange);
        assert!(!strtod("1e308").erange);
    }

    #[test]
    fn strtod_reads_infinities_nans_and_hex() {
        assert_eq!(d("inf"), (f64::INFINITY, 3, false));
        assert_eq!(d("-Infinity"), (f64::NEG_INFINITY, 9, false));
        assert_eq!(d("infinit"), (f64::INFINITY, 3, false));
        let nan = strtod("nan(x_1)y");
        assert!(nan.value.is_nan());
        assert_eq!(nan.end, 8);
        assert_eq!(strtod("NaN(").end, 3);
        assert_eq!(d("0x10"), (16.0, 4, false));
        assert_eq!(d("0x1p-3"), (0.125, 6, false));
        assert_eq!(d("0X1.8P1"), (3.0, 7, false));
        assert_eq!(d("0x.8"), (0.5, 4, false));
        assert_eq!(d("0x1p"), (1.0, 3, false));
        assert_eq!(d("0xg"), (0.0, 1, false));
        assert_eq!(d("0x1p2000"), (f64::INFINITY, 8, true));
        assert_eq!(d("0x1p-2000"), (0.0, 9, true));
        assert_eq!(
            d("0x123456789abcdef01p0"),
            (20_988_295_479_420_645_121.0, 21, false)
        );
    }

    #[test]
    fn strtoint_is_strtol_narrowed_with_erange() {
        let i = |s: &str| {
            let c = strtoint(s);
            (c.value, c.end, c.erange)
        };
        assert_eq!(i("3"), (3, 1, false));
        assert_eq!(i(" -7x"), (-7, 3, false));
        assert_eq!(i("x"), (0, 0, false));
        assert_eq!(i(""), (0, 0, false));
        assert_eq!(i("2147483647"), (i32::MAX, 10, false));
        assert!(strtoint("2147483648").erange);
        assert!(strtoint("99999999999999999999").erange);
        assert_eq!(strtol("99999999999999999999").value, i64::MAX);
        assert_eq!(strtol("-99999999999999999999").value, i64::MIN);
        assert_eq!(strtoint("0x10").end, 1);
    }

    #[test]
    fn format_g_is_printfs() {
        for (x, g) in [
            (2.0, "2"),
            (0.01, "0.01"),
            (0.001, "0.001"),
            (0.5, "0.5"),
            (0.125, "0.125"),
            (16.0, "16"),
            (123_456.0, "123456"),
            (1_000_000.0, "1e+06"),
            (1_234_567.0, "1.23457e+06"),
            (999_999.5, "1e+06"),
            (0.0001, "0.0001"),
            (0.000_01, "1e-05"),
            (1e100, "1e+100"),
            (0.0, "0"),
            (-1.5, "-1.5"),
            (f64::INFINITY, "inf"),
        ] {
            assert_eq!(format_g(x), g, "{x}");
        }
    }
}
