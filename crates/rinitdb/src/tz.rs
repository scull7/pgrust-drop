//! The part of PostgreSQL's timezone library that `findtimezone.c` needs: read
//! a zic-compiled TZif file, and turn an instant into a broken-down local time.
//!
//! Ported from `src/timezone/localtime.c` and the `struct state` of
//! `src/timezone/pgtz.h` (PostgreSQL 18.6). Only the read path is here —
//! `tzload`, `tzparse`, `localsub`/`timesub` and `pg_tz_acceptable` — because
//! that is the whole of what `select_default_timezone` calls. `pg_mktime`,
//! `pg_next_dst_boundary` and the abbreviation lookups are not ported.
//!
//! Everything in this module is a pure calculation over bytes: `tzload` takes
//! the file image rather than a path, so the only action involved is the read
//! that [`crate::findtimezone`] performs.

/// `TZ_STRLEN_MAX` (`include/pgtime.h:54`): the longest zone name, sans NUL.
pub const TZ_STRLEN_MAX: usize = 255;

/// `tzfile.h:100`-`:108`.
const TZ_MAX_TIMES: usize = 2000;
const TZ_MAX_TYPES: usize = 256;
const TZ_MAX_CHARS: usize = 50;
const TZ_MAX_LEAPS: usize = 50;

/// `private.h:97`-`:105`.
const SECS_PER_MIN: i64 = 60;
const MINS_PER_HOUR: i64 = 60;
const HOURS_PER_DAY: i64 = 24;
const DAYS_PER_WEEK: i64 = 7;
const DAYS_PER_NYEAR: i64 = 365;
const DAYS_PER_LYEAR: i64 = 366;
const SECS_PER_HOUR: i64 = SECS_PER_MIN * MINS_PER_HOUR;
const SECS_PER_DAY: i64 = SECS_PER_HOUR * HOURS_PER_DAY;
const MONS_PER_YEAR: usize = 12;

/// `private.h:128`, `:130`, `:131` (`EPOCH_WDAY` is `TM_THURSDAY`, `:111`).
const TM_YEAR_BASE: i32 = 1900;
const EPOCH_YEAR: i32 = 1970;
const EPOCH_WDAY: i64 = 4;

/// `private.h:95`, `:154`, `:155`.
const YEARS_PER_REPEAT: i64 = 400;
const AVG_SECS_PER_YEAR: i64 = 31_556_952;
const SECS_PER_REPEAT: i64 = YEARS_PER_REPEAT * AVG_SECS_PER_YEAR;

/// `localtime.c:61`. Used whenever a POSIX TZ string names a DST zone but no
/// rules; upstream reaches for `TZDEFRULES` first and this port, like
/// PostgreSQL's own (`localtime.c:984`), never does.
const TZ_DEFRULESTRING: &str = ",M3.2.0,M11.1.0";

/// `struct state`'s `chars` member is
/// `BIGGEST(BIGGEST(TZ_MAX_CHARS + 1, 4), 2 * (TZ_STRLEN_MAX + 1))` bytes wide
/// (`pgtz.h:52`), and `tzparse` refuses a name pair that would not fit.
const CHARS_CAP: usize = 2 * (TZ_STRLEN_MAX + 1);

/// The fixed part of a TZif file: `struct tzhead` (`tzfile.h:39`).
const TZHEADSIZE: usize = 44;

/// `localtime.c:626`.
const MON_LENGTHS: [[i64; MONS_PER_YEAR]; 2] = [
    [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
];

/// `localtime.c:631`.
const YEAR_LENGTHS: [i64; 2] = [DAYS_PER_NYEAR, DAYS_PER_LYEAR];

/// `private.h:133`.
fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// `isleap` as the index `MON_LENGTHS` / `YEAR_LENGTHS` are subscripted by.
fn leap_index(y: i32) -> usize {
    usize::from(is_leap(y))
}

// --------------------------------------------------------------------------
// Data
// --------------------------------------------------------------------------

/// `struct ttinfo` (`pgtz.h:26`): one local time type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TtInfo {
    /// `tt_utoff`: UT offset in seconds.
    utoff: i32,
    /// `tt_isdst`.
    isdst: bool,
    /// `tt_desigidx`: index into [`State::chars`].
    desigidx: usize,
    /// `tt_ttisstd`.
    ttisstd: bool,
    /// `tt_ttisut`.
    ttisut: bool,
}

/// `init_ttinfo` (`localtime.c:108`).
fn init_ttinfo(utoff: i32, isdst: bool, desigidx: usize) -> TtInfo {
    TtInfo {
        utoff,
        isdst,
        desigidx,
        ttisstd: false,
        ttisut: false,
    }
}

/// `struct lsinfo` (`pgtz.h:35`): one leap-second correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LsInfo {
    trans: i64,
    corr: i64,
}

/// `struct state` (`pgtz.h:41`): a loaded timezone.
///
/// Upstream's `timecnt`, `typecnt`, `charcnt` and `leapcnt` are the lengths of
/// the four arrays; only `charcnt` survives as a field, because `chars` is a
/// fixed-width buffer there and here (`tzparse` bounds names against its size).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    goback: bool,
    goahead: bool,
    /// `ats`: transition times, ascending.
    ats: Vec<i64>,
    /// `types`: the time type in force from `ats[i]` onwards.
    types: Vec<u8>,
    /// `ttis`.
    ttis: Vec<TtInfo>,
    /// `chars`: NUL-separated abbreviations, `CHARS_CAP` bytes wide.
    chars: Vec<u8>,
    /// `charcnt`: how much of `chars` is in use.
    charcnt: usize,
    /// `lsis`.
    lsis: Vec<LsInfo>,
    /// `defaulttype`: the type for instants before the first transition.
    defaulttype: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            goback: false,
            goahead: false,
            ats: Vec::new(),
            types: Vec::new(),
            ttis: Vec::new(),
            chars: vec![0; CHARS_CAP],
            charcnt: 0,
            lsis: Vec::new(),
            defaulttype: 0,
        }
    }
}

/// `struct pg_tm` (`include/pgtime.h:34`).
///
/// The field names keep upstream's `tm_` meanings: `mon` counts from 0 and
/// `year` is relative to 1900 (`pgtime.h:29`). `zone` borrows out of the
/// [`State`] it was computed from, as upstream's `tm_zone` points into
/// `sp->chars` (`localtime.c:1337`), and it is bytes because that is what
/// `strcmp` compares there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgTm<'a> {
    pub sec: i32,
    pub min: i32,
    pub hour: i32,
    pub mday: i32,
    pub mon: i32,
    pub year: i32,
    pub wday: i32,
    pub yday: i32,
    pub isdst: i32,
    pub gmtoff: i64,
    pub zone: &'a [u8],
}

impl PgTm<'_> {
    /// The calendar year, as `identify_system_timezone` reads it
    /// (`findtimezone.c:372`: `tm->tm_year + 1900`).
    #[must_use]
    pub fn calendar_year(&self) -> i32 {
        self.year + TM_YEAR_BASE
    }

    /// `compare_tm` (`findtimezone.c:207`), plus the zone-abbreviation check
    /// `score_timezone` makes right after it (`:292`).
    ///
    /// Upstream compares a system `struct tm` against a `struct pg_tm`; here
    /// both sides are `pg_tm`, so the two comparisons fold into one.
    /// The abbreviation as text. Only callers that must render it pay for
    /// this; the comparisons above are over the bytes.
    #[must_use]
    pub fn zone_name(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.zone)
    }

    #[must_use]
    pub fn matches(&self, other: &PgTm<'_>) -> bool {
        self.sec == other.sec
            && self.min == other.min
            && self.hour == other.hour
            && self.mday == other.mday
            && self.mon == other.mon
            && self.year == other.year
            && self.wday == other.wday
            && self.yday == other.yday
            && self.isdst == other.isdst
            && self.zone == other.zone
    }
}

// --------------------------------------------------------------------------
// Calculations: reading a TZif image
// --------------------------------------------------------------------------

/// `detzcode` (`localtime.c:118`). Every machine this runs on is two's
/// complement, so upstream's sign reconstruction is `i32::from_be_bytes`.
fn detzcode(b: &[u8]) -> i32 {
    i32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// `detzcode64` (`localtime.c:144`).
fn detzcode64(b: &[u8]) -> i64 {
    i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// The NUL-terminated abbreviation starting at `idx`, as `chars + idx` is read
/// throughout `localtime.c`.
fn c_str(chars: &[u8], idx: usize) -> &[u8] {
    let rest = &chars[idx.min(chars.len())..];
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    &rest[..end]
}

/// `differ_by_repeat` (`localtime.c:170`). `pg_time_t` is `int64`, so the
/// width test at `:172` is always false and only the subtraction survives.
fn differ_by_repeat(t1: i64, t0: i64) -> bool {
    t1.checked_sub(t0) == Some(SECS_PER_REPEAT)
}

/// `typesequiv` (`localtime.c:602`).
fn typesequiv(sp: &State, a: usize, b: usize) -> bool {
    match (sp.ttis.get(a), sp.ttis.get(b)) {
        (Some(ap), Some(bp)) => {
            ap.utoff == bp.utoff
                && ap.isdst == bp.isdst
                && ap.ttisstd == bp.ttisstd
                && ap.ttisut == bp.ttisut
                && c_str(&sp.chars, ap.desigidx) == c_str(&sp.chars, bp.desigidx)
        }
        _ => false,
    }
}

/// `leapcorr` (`localtime.c:1574`).
fn leapcorr(sp: &State, t: i64) -> i64 {
    sp.lsis
        .iter()
        .rev()
        .find(|lp| t >= lp.trans)
        .map_or(0, |lp| lp.corr)
}

/// `increment_overflow_time` (`localtime.c:1557`), over `pg_time_t`.
fn increment_overflow_time(tp: &mut i64, j: i64) -> bool {
    match tp.checked_add(j) {
        Some(v) => {
            *tp = v;
            false
        }
        None => true,
    }
}

/// `increment_overflow` (`localtime.c:1539`), over `int`.
fn increment_overflow(ip: &mut i32, j: i32) -> bool {
    match ip.checked_add(j) {
        Some(v) => {
            *ip = v;
            false
        }
        None => true,
    }
}

/// `tzload` (`localtime.c:586`) over the file image rather than a file
/// descriptor: the `open`/`read` upstream does at `:233`-`:237` is the caller's
/// action. `None` is upstream's nonzero return.
///
/// `doextend` is upstream's: read the v2+ footer's POSIX TZ string and splice
/// its transitions on (`:415`).
#[must_use]
pub fn load(image: &[u8], doextend: bool) -> Option<State> {
    let mut sp = State::default();
    load_body(image, doextend, &mut sp)?;
    Some(sp)
}

/// `tzloadbody` (`localtime.c:211`).
#[allow(clippy::too_many_lines)] // One upstream function, kept in one piece.
fn load_body(image: &[u8], doextend: bool, sp: &mut State) -> Option<()> {
    if image.len() < TZHEADSIZE {
        return None;
    }
    let mut buf: &[u8] = image;
    // `for (stored = 4; stored <= 8; stored *= 2)`: the 32-bit block, then the
    // 64-bit one that a version 2 or 3 file repeats it in.
    let mut stored = 4usize;
    loop {
        if buf.len() < TZHEADSIZE {
            return None;
        }
        let ttisutcnt = detzcode(&buf[20..]);
        let ttisstdcnt = detzcode(&buf[24..]);
        let leapcnt = detzcode(&buf[28..]);
        let timecnt = detzcode(&buf[32..]);
        let typecnt = detzcode(&buf[36..]);
        let charcnt = detzcode(&buf[40..]);

        // localtime.c:264 — the header's own bounds. The conversion standing
        // in for upstream's six `0 <=` tests fails on exactly the values they
        // reject.
        let (Ok(ttisutcnt), Ok(ttisstdcnt), Ok(leapcnt), Ok(timecnt), Ok(typecnt), Ok(charcnt)) = (
            usize::try_from(ttisutcnt),
            usize::try_from(ttisstdcnt),
            usize::try_from(leapcnt),
            usize::try_from(timecnt),
            usize::try_from(typecnt),
            usize::try_from(charcnt),
        ) else {
            return None;
        };
        if !(leapcnt < TZ_MAX_LEAPS
            && typecnt < TZ_MAX_TYPES
            && timecnt < TZ_MAX_TIMES
            && charcnt < TZ_MAX_CHARS
            && (ttisstdcnt == typecnt || ttisstdcnt == 0)
            && (ttisutcnt == typecnt || ttisutcnt == 0))
        {
            return None;
        }

        // localtime.c:271 — and the size the counts imply.
        if buf.len()
            < TZHEADSIZE
                + timecnt * stored
                + timecnt
                + typecnt * 6
                + charcnt
                + leapcnt * (stored + 4)
                + ttisstdcnt
                + ttisutcnt
        {
            return None;
        }

        let mut p = TZHEADSIZE;

        // localtime.c:287 — read transitions. A 64-bit pg_time_t holds every
        // value the file can carry, so upstream's out-of-range discard (`:297`,
        // `:302`) never fires and only its duplicate-transition rule survives.
        // `keep` is upstream's re-use of `sp->types[i]` as that flag.
        let mut keep = vec![true; timecnt];
        let mut ats: Vec<i64> = Vec::with_capacity(timecnt);
        // The loop variable indexes `keep[i - 1]`, not `keep[i]`.
        #[allow(clippy::needless_range_loop)]
        for i in 0..timecnt {
            let at = if stored == 4 {
                i64::from(detzcode(&buf[p..]))
            } else {
                detzcode64(&buf[p..])
            };
            if ats.last().is_some_and(|&last| at <= last) {
                if at < ats[ats.len() - 1] {
                    return None;
                }
                // Upstream drops the *earlier* of the pair (`localtime.c:308`).
                if i > 0 {
                    keep[i - 1] = false;
                }
                ats.pop();
            }
            ats.push(at);
            p += stored;
        }

        let mut types: Vec<u8> = Vec::with_capacity(ats.len());
        #[allow(clippy::needless_range_loop)] // `keep[i]` and `buf[p]` advance together.
        for i in 0..timecnt {
            let typ = buf[p];
            p += 1;
            if usize::from(typ) >= typecnt {
                return None;
            }
            if keep[i] {
                types.push(typ);
            }
        }

        let mut ttis: Vec<TtInfo> = Vec::with_capacity(typecnt);
        for _ in 0..typecnt {
            let utoff = detzcode(&buf[p..]);
            p += 4;
            let isdst = buf[p];
            p += 1;
            if isdst >= 2 {
                return None;
            }
            let desigidx = buf[p];
            p += 1;
            if usize::from(desigidx) >= charcnt {
                return None;
            }
            ttis.push(init_ttinfo(utoff, isdst == 1, usize::from(desigidx)));
        }

        let mut chars = vec![0u8; CHARS_CAP];
        chars[..charcnt].copy_from_slice(&buf[p..p + charcnt]);
        p += charcnt;

        // localtime.c:349 — leap seconds, with upstream's sanity rule.
        let mut lsis: Vec<LsInfo> = Vec::with_capacity(leapcnt);
        let mut prevtr = 0i64;
        let mut prevcorr = 0i32;
        for _ in 0..leapcnt {
            let tr = if stored == 4 {
                i64::from(detzcode(&buf[p..]))
            } else {
                detzcode64(&buf[p..])
            };
            let corr = detzcode(&buf[p + stored..]);
            p += stored + 4;
            if tr < 0 {
                return None;
            }
            if tr - prevtr < 28 * SECS_PER_DAY - 1 || (corr != prevcorr - 1 && corr != prevcorr + 1)
            {
                return None;
            }
            prevtr = tr;
            prevcorr = corr;
            lsis.push(LsInfo {
                trans: tr,
                corr: i64::from(corr),
            });
        }

        for tti in &mut ttis {
            if ttisstdcnt != 0 {
                if buf[p] > 1 {
                    return None;
                }
                tti.ttisstd = buf[p] == 1;
                p += 1;
            }
        }
        for tti in &mut ttis {
            if ttisutcnt != 0 {
                if buf[p] > 1 {
                    return None;
                }
                tti.ttisut = buf[p] == 1;
                p += 1;
            }
        }

        sp.ats = ats;
        sp.types = types;
        sp.ttis = ttis;
        sp.chars = chars;
        sp.charcnt = charcnt;
        sp.lsis = lsis;

        // localtime.c:410 — an old file has no second block.
        if buf[4] == 0 {
            break;
        }
        buf = &buf[p..];
        if stored == 8 {
            break;
        }
        stored = 8;
    }

    // localtime.c:415 — splice in the footer's POSIX TZ string.
    if doextend && buf.len() > 2 && buf[0] == b'\n' && buf[buf.len() - 1] == b'\n' {
        extend(sp, &buf[1..buf.len() - 1]);
    }

    if sp.ttis.is_empty() {
        return None;
    }

    // localtime.c:496 — can the zone's tail be extrapolated 400 years either
    // way?
    if sp.ats.len() > 1 {
        let last = sp.ats.len() - 1;
        for i in 1..sp.ats.len() {
            if typesequiv(sp, usize::from(sp.types[i]), usize::from(sp.types[0]))
                && differ_by_repeat(sp.ats[i], sp.ats[0])
            {
                sp.goback = true;
                break;
            }
        }
        for i in (0..last).rev() {
            if typesequiv(sp, usize::from(sp.types[last]), usize::from(sp.types[i]))
                && differ_by_repeat(sp.ats[last], sp.ats[i])
            {
                sp.goahead = true;
                break;
            }
        }
    }

    sp.defaulttype = infer_default_type(sp);
    Some(())
}

/// `tzloadbody`'s extension block (`localtime.c:415`-`:493`), split out.
fn extend(sp: &mut State, footer: &[u8]) {
    if sp.ttis.len() + 2 > TZ_MAX_TYPES {
        return;
    }
    let Ok(text) = std::str::from_utf8(footer) else {
        return;
    };
    let Some(mut ts) = parse(text, false) else {
        return;
    };

    // localtime.c:425 — reuse abbreviations already in sp->chars where we can.
    let mut gotabbr = 0usize;
    let mut charcnt = sp.charcnt;
    for i in 0..ts.ttis.len() {
        let tsabbr = c_str(&ts.chars, ts.ttis[i].desigidx).to_vec();
        let found = (0..charcnt).find(|&j| c_str(&sp.chars, j) == tsabbr.as_slice());
        if let Some(j) = found {
            ts.ttis[i].desigidx = j;
            gotabbr += 1;
        } else {
            let j = charcnt;
            if j + tsabbr.len() < TZ_MAX_CHARS {
                sp.chars[j..j + tsabbr.len()].copy_from_slice(&tsabbr);
                sp.chars[j + tsabbr.len()] = 0;
                charcnt = j + tsabbr.len() + 1;
                ts.ttis[i].desigidx = j;
                gotabbr += 1;
            }
        }
    }
    if gotabbr != ts.ttis.len() {
        return;
    }
    sp.charcnt = charcnt;

    // localtime.c:469 — drop zic's trailing no-op transitions.
    while sp.ats.len() > 1 && sp.types[sp.ats.len() - 1] == sp.types[sp.ats.len() - 2] {
        sp.ats.pop();
        sp.types.pop();
    }

    let mut i = 0usize;
    while i < ts.ats.len() {
        if sp.ats.is_empty() || sp.ats[sp.ats.len() - 1] < ts.ats[i] + leapcorr(sp, ts.ats[i]) {
            break;
        }
        i += 1;
    }
    let typecnt_before = sp.ttis.len();
    while i < ts.ats.len() && sp.ats.len() < TZ_MAX_TIMES {
        let at = ts.ats[i] + leapcorr(sp, ts.ats[i]);
        sp.ats.push(at);
        // typecnt_before + ts->types[i] is at most 255; see the TZ_MAX_TYPES
        // guard above (`localtime.c:417`).
        sp.types
            .push(u8::try_from(typecnt_before + usize::from(ts.types[i])).unwrap_or(u8::MAX));
        i += 1;
    }
    sp.ttis.extend(ts.ttis);
}

/// `tzloadbody`'s `defaulttype` heuristics (`localtime.c:517`-`:575`), which
/// work around 32-bit data from tzdb 2018e or earlier. For any recent release
/// the answer is zero.
fn infer_default_type(sp: &State) -> usize {
    // If type 0 is unused in transitions, it's the type to use for early times.
    if !sp.types.contains(&0) {
        return 0;
    }
    // Absent the above, if the first transition is to a daylight time, find the
    // standard type less than and closest to the type of that transition.
    if !sp.ats.is_empty() && sp.ttis[usize::from(sp.types[0])].isdst {
        let first = usize::from(sp.types[0]);
        if let Some(i) = (0..first).rev().find(|&i| !sp.ttis[i].isdst) {
            return i;
        }
    }
    // If no result yet, the first standard type; if there is none, type zero.
    sp.ttis.iter().position(|tti| !tti.isdst).unwrap_or(0)
}

// --------------------------------------------------------------------------
// Calculations: parsing a POSIX TZ string
// --------------------------------------------------------------------------

/// `struct rule`'s `r_type` (`localtime.c:65`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    /// `JULIAN_DAY`: `Jn`, 1 == January 1 even in leap years.
    JulianDay,
    /// `DAY_OF_YEAR`: `n`, zero-origin, counting February 29.
    DayOfYear,
    /// `MONTH_NTH_DAY_OF_WEEK`: `Mm.n.d`.
    MonthNthDayOfWeek,
}

/// `struct rule` (`localtime.c:72`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    kind: RuleKind,
    day: i64,
    week: i64,
    mon: i64,
    time: i64,
}

/// `is_digit` as `localtime.c` uses it: ASCII only.
fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

/// `getzname` (`localtime.c:642`).
fn getzname(b: &[u8], mut i: usize) -> usize {
    while let Some(&c) = b.get(i) {
        if is_digit(c) || c == b',' || c == b'-' || c == b'+' {
            break;
        }
        i += 1;
    }
    i
}

/// `getqzname` (`localtime.c:663`).
fn getqzname(b: &[u8], mut i: usize, delim: u8) -> usize {
    while let Some(&c) = b.get(i) {
        if c == delim {
            break;
        }
        i += 1;
    }
    i
}

/// `getnum` (`localtime.c:680`).
fn getnum(b: &[u8], mut i: usize, min: i64, max: i64) -> Option<(usize, i64)> {
    if !b.get(i).copied().is_some_and(is_digit) {
        return None;
    }
    let mut num: i64 = 0;
    while let Some(&c) = b.get(i) {
        if !is_digit(c) {
            break;
        }
        num = num * 10 + i64::from(c - b'0');
        if num > max {
            return None;
        }
        i += 1;
    }
    if num < min {
        return None;
    }
    Some((i, num))
}

/// `getsecs` (`localtime.c:710`).
fn getsecs(b: &[u8], i: usize) -> Option<(usize, i64)> {
    // `HOURSPERDAY * DAYSPERWEEK - 1` allows quasi-POSIX rules like "M10.4.6/26".
    let (mut i, num) = getnum(b, i, 0, HOURS_PER_DAY * DAYS_PER_WEEK - 1)?;
    let mut secs = num * SECS_PER_HOUR;
    if b.get(i) == Some(&b':') {
        i += 1;
        let (ni, num) = getnum(b, i, 0, MINS_PER_HOUR - 1)?;
        i = ni;
        secs += num * SECS_PER_MIN;
        if b.get(i) == Some(&b':') {
            i += 1;
            // `SECSPERMIN` allows for leap seconds.
            let (ni, num) = getnum(b, i, 0, SECS_PER_MIN)?;
            i = ni;
            secs += num;
        }
    }
    Some((i, secs))
}

/// `getoffset` (`localtime.c:751`).
fn getoffset(b: &[u8], mut i: usize) -> Option<(usize, i64)> {
    let mut neg = false;
    match b.get(i) {
        Some(&b'-') => {
            neg = true;
            i += 1;
        }
        Some(&b'+') => i += 1,
        _ => {}
    }
    let (i, secs) = getsecs(b, i)?;
    Some((i, if neg { -secs } else { secs }))
}

/// `getrule` (`localtime.c:778`).
fn getrule(b: &[u8], mut i: usize) -> Option<(usize, Rule)> {
    let mut rule = Rule {
        kind: RuleKind::DayOfYear,
        day: 0,
        week: 0,
        mon: 0,
        time: 0,
    };
    match b.get(i) {
        Some(&b'J') => {
            rule.kind = RuleKind::JulianDay;
            i += 1;
            let (ni, day) = getnum(b, i, 1, DAYS_PER_NYEAR)?;
            i = ni;
            rule.day = day;
        }
        Some(&b'M') => {
            rule.kind = RuleKind::MonthNthDayOfWeek;
            i += 1;
            let (ni, mon) = getnum(b, i, 1, 12)?; // MONSPERYEAR
            i = ni;
            rule.mon = mon;
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
            let (ni, week) = getnum(b, i, 1, 5)?;
            i = ni;
            rule.week = week;
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
            let (ni, day) = getnum(b, i, 0, DAYS_PER_WEEK - 1)?;
            i = ni;
            rule.day = day;
        }
        Some(&c) if is_digit(c) => {
            rule.kind = RuleKind::DayOfYear;
            let (ni, day) = getnum(b, i, 0, DAYS_PER_LYEAR - 1)?;
            i = ni;
            rule.day = day;
        }
        _ => return None,
    }
    if b.get(i) == Some(&b'/') {
        i += 1;
        let (ni, time) = getoffset(b, i)?;
        i = ni;
        rule.time = time;
    } else {
        rule.time = 2 * SECS_PER_HOUR; // default = 2:00:00
    }
    Some((i, rule))
}

/// `transtime` (`localtime.c:839`).
fn transtime(year: i32, rule: &Rule, offset: i64) -> i64 {
    let leap = leap_index(year);
    let value = match rule.kind {
        RuleKind::JulianDay => {
            let mut v = (rule.day - 1) * SECS_PER_DAY;
            if leap == 1 && rule.day >= 60 {
                v += SECS_PER_DAY;
            }
            v
        }
        RuleKind::DayOfYear => rule.day * SECS_PER_DAY,
        RuleKind::MonthNthDayOfWeek => {
            // Zeller's Congruence for the day-of-week of the 1st of the month.
            let m1 = (rule.mon + 9) % 12 + 1;
            let yy0 = if rule.mon <= 2 {
                i64::from(year) - 1
            } else {
                i64::from(year)
            };
            let yy1 = yy0 / 100;
            let yy2 = yy0 % 100;
            let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
            if dow < 0 {
                dow += DAYS_PER_WEEK;
            }
            let mut d = rule.day - dow;
            if d < 0 {
                d += DAYS_PER_WEEK;
            }
            let mon = usize::try_from(rule.mon).unwrap_or(1);
            for _ in 1..rule.week {
                if d + DAYS_PER_WEEK >= MON_LENGTHS[leap][mon - 1] {
                    break;
                }
                d += DAYS_PER_WEEK;
            }
            let mut v = d * SECS_PER_DAY;
            for length in &MON_LENGTHS[leap][..mon - 1] {
                v += length * SECS_PER_DAY;
            }
            v
        }
    };
    value + rule.time + offset
}

/// `tzparse` (`localtime.c:936`): a POSIX section 8-style TZ string.
///
/// `lastditch` is upstream's: take the whole string as the standard-zone name
/// at offset zero, which is how `"GMT"` is loaded (`pg_tzset`, `pgtz.c:275`).
#[must_use]
#[allow(clippy::too_many_lines)] // One upstream function, kept in one piece.
pub fn parse(name: &str, lastditch: bool) -> Option<State> {
    let b = name.as_bytes();
    let mut sp = State::default();
    let mut i = 0usize;

    let (stdname, stdoffset): (&[u8], i64) = if lastditch {
        // Unlike IANA, do not assume the name is exactly "GMT".
        i = b.len();
        (b, 0)
    } else {
        let std = if b.first() == Some(&b'<') {
            i = 1;
            let start = i;
            i = getqzname(b, i, b'>');
            if b.get(i) != Some(&b'>') {
                return None;
            }
            let std = &b[start..i];
            i += 1;
            std
        } else {
            let start = i;
            i = getzname(b, i);
            &b[start..i]
        };
        // We allow an empty STD abbreviation, unlike IANA.
        if i >= b.len() {
            return None;
        }
        let (ni, off) = getoffset(b, i)?;
        i = ni;
        (std, off)
    };

    let stdlen = stdname.len();
    let mut charcnt = stdlen + 1;
    if CHARS_CAP < charcnt {
        return None;
    }

    // localtime.c:984 — upstream deliberately never loads TZDEFRULES, so
    // `load_ok` is false from here on and the `else` arm at `:1127` is dead:
    // it is reached only when the string has unparsed trailing text, which its
    // own first statement (`:1136`) rejects.
    sp.goback = false;
    sp.goahead = false;
    sp.lsis.clear();

    let mut dstname: &[u8] = b"";
    let mut dstlen = 0usize;

    if i < b.len() {
        let dst = if b[i] == b'<' {
            i += 1;
            let start = i;
            i = getqzname(b, i, b'>');
            if b.get(i) != Some(&b'>') {
                return None;
            }
            let dst = &b[start..i];
            i += 1;
            dst
        } else {
            let start = i;
            i = getzname(b, i);
            &b[start..i]
        };
        dstname = dst;
        dstlen = dstname.len();
        if dstlen == 0 {
            return None;
        }
        charcnt += dstlen + 1;
        if CHARS_CAP < charcnt {
            return None;
        }

        let dstoffset = if i < b.len() && b[i] != b',' && b[i] != b';' {
            let (ni, off) = getoffset(b, i)?;
            i = ni;
            off
        } else {
            stdoffset - SECS_PER_HOUR
        };

        // localtime.c:1027 — with no rules and no TZDEFRULES, use the US ones.
        let rest: Vec<u8> = if i >= b.len() {
            TZ_DEFRULESTRING.as_bytes().to_vec()
        } else {
            b[i..].to_vec()
        };
        if !(rest.first() == Some(&b',') || rest.first() == Some(&b';')) {
            return None;
        }

        let mut k = 1usize;
        let (nk, start_rule) = getrule(&rest, k)?;
        k = nk;
        if rest.get(k) != Some(&b',') {
            return None;
        }
        k += 1;
        let (nk, end_rule) = getrule(&rest, k)?;
        k = nk;
        if k != rest.len() {
            return None;
        }

        // Two transitions per year, from EPOCH_YEAR forward (`:1051`).
        sp.ttis = vec![
            init_ttinfo(clamp_offset(-stdoffset), false, 0),
            init_ttinfo(clamp_offset(-dstoffset), true, stdlen + 1),
        ];
        sp.defaulttype = 0;

        let mut ats: Vec<i64> = Vec::new();
        let mut types: Vec<u8> = Vec::new();
        let mut janfirst: i64 = 0;
        let mut janoffset: i64 = 0;
        let mut yearbeg = EPOCH_YEAR;
        loop {
            let yearsecs = YEAR_LENGTHS[leap_index(yearbeg - 1)] * SECS_PER_DAY;
            yearbeg -= 1;
            if increment_overflow_time(&mut janfirst, -yearsecs) {
                janoffset = -yearsecs;
                break;
            }
            if i64::from(EPOCH_YEAR) - YEARS_PER_REPEAT / 2 >= i64::from(yearbeg) {
                break;
            }
        }

        let mut yearlim = i64::from(yearbeg) + YEARS_PER_REPEAT + 1;
        let mut year = i64::from(yearbeg);
        while year < yearlim {
            let y = i32::try_from(year).ok()?;
            let mut starttime = transtime(y, &start_rule, stdoffset);
            let mut endtime = transtime(y, &end_rule, dstoffset);
            let yearsecs = YEAR_LENGTHS[leap_index(y)] * SECS_PER_DAY;
            let reversed = endtime < starttime;
            if reversed {
                std::mem::swap(&mut starttime, &mut endtime);
            }
            if reversed
                || (starttime < endtime
                    && (endtime - starttime < yearsecs + (stdoffset - dstoffset)))
            {
                if TZ_MAX_TIMES - 2 < ats.len() {
                    break;
                }
                let mut at = janfirst;
                if !increment_overflow_time(&mut at, janoffset + starttime) {
                    ats.push(at);
                    types.push(u8::from(!reversed));
                }
                let mut at = janfirst;
                if !increment_overflow_time(&mut at, janoffset + endtime) {
                    ats.push(at);
                    types.push(u8::from(reversed));
                    yearlim = year + YEARS_PER_REPEAT + 1;
                }
            }
            if increment_overflow_time(&mut janfirst, janoffset + yearsecs) {
                break;
            }
            janoffset = 0;
            year += 1;
        }
        sp.ats = ats;
        sp.types = types;
        if sp.ats.is_empty() {
            sp.ttis[0] = sp.ttis[1];
            sp.ttis.truncate(1); // Perpetual DST.
        } else if YEARS_PER_REPEAT < year - i64::from(yearbeg) {
            sp.goback = true;
            sp.goahead = true;
        }
    } else {
        // `dstlen = 0` (`:1225`) — already so here; in C the variable is
        // uninitialized until this point.
        sp.ttis = vec![init_ttinfo(clamp_offset(-stdoffset), false, 0)];
        sp.ats.clear();
        sp.types.clear();
        sp.defaulttype = 0;
    }

    sp.charcnt = charcnt;
    sp.chars = vec![0; CHARS_CAP];
    sp.chars[..stdlen].copy_from_slice(stdname);
    if dstlen != 0 {
        sp.chars[stdlen + 1..stdlen + 1 + dstlen].copy_from_slice(dstname);
    }
    Some(sp)
}

/// `init_ttinfo` takes an `int32`; `getoffset` has already bounded the value to
/// `±(24 * 7 - 1)` hours, so this cast is upstream's implicit one.
fn clamp_offset(secs: i64) -> i32 {
    i32::try_from(secs).unwrap_or(0)
}

// --------------------------------------------------------------------------
// Calculations: an instant as a local time
// --------------------------------------------------------------------------

/// `leaps_thru_end_of` (`localtime.c:1406`).
fn leaps_thru_end_of(y: i64) -> i64 {
    if y < 0 {
        -1 - leaps_thru_end_of_nonneg(-1 - y)
    } else {
        leaps_thru_end_of_nonneg(y)
    }
}

/// `leaps_thru_end_of_nonneg` (`localtime.c:1400`).
fn leaps_thru_end_of_nonneg(y: i64) -> i64 {
    y / 4 - y / 100 + y / 400
}

/// `timesub` (`localtime.c:1414`).
#[allow(clippy::cast_possible_truncation)] // Every value is bounded below.
fn timesub(t: i64, offset: i64, sp: &State) -> Option<PgTm<'_>> {
    let mut corr = 0i64;
    let mut hit = false;
    for (idx, lp) in sp.lsis.iter().enumerate().rev() {
        if t >= lp.trans {
            corr = lp.corr;
            hit = t == lp.trans && (if idx == 0 { 0 } else { sp.lsis[idx - 1].corr }) < corr;
            break;
        }
    }

    let mut y: i32 = EPOCH_YEAR;
    let mut tdays = t.div_euclid(SECS_PER_DAY);
    let mut rem = t - tdays * SECS_PER_DAY;
    // C's `/` and `%` truncate toward zero; div_euclid/the remainder above give
    // the same pair after the normalizing loops below, which is all this uses.
    while tdays < 0 || tdays >= YEAR_LENGTHS[leap_index(y)] {
        let tdelta = tdays / DAYS_PER_LYEAR;
        let mut idelta = i32::try_from(tdelta).ok()?;
        if idelta == 0 {
            idelta = if tdays < 0 { -1 } else { 1 };
        }
        let mut newy = y;
        if increment_overflow(&mut newy, idelta) {
            return None;
        }
        let leapdays = leaps_thru_end_of(i64::from(newy) - 1) - leaps_thru_end_of(i64::from(y) - 1);
        tdays -= (i64::from(newy) - i64::from(y)) * DAYS_PER_NYEAR;
        tdays -= leapdays;
        y = newy;
    }

    let mut idays = tdays;
    rem += offset - corr;
    while rem < 0 {
        rem += SECS_PER_DAY;
        idays -= 1;
    }
    while rem >= SECS_PER_DAY {
        rem -= SECS_PER_DAY;
        idays += 1;
    }
    while idays < 0 {
        if increment_overflow(&mut y, -1) {
            return None;
        }
        idays += YEAR_LENGTHS[leap_index(y)];
    }
    while idays >= YEAR_LENGTHS[leap_index(y)] {
        idays -= YEAR_LENGTHS[leap_index(y)];
        if increment_overflow(&mut y, 1) {
            return None;
        }
    }

    let mut tm_year = y;
    if increment_overflow(&mut tm_year, -TM_YEAR_BASE) {
        return None;
    }
    let yday = idays;

    // The "extra" mods avoid overflow, as upstream's comment says (`:1501`).
    let mut wday = EPOCH_WDAY
        + i64::from((y - EPOCH_YEAR) % (DAYS_PER_WEEK as i32)) * (DAYS_PER_NYEAR % DAYS_PER_WEEK)
        + leaps_thru_end_of(i64::from(y) - 1)
        - leaps_thru_end_of(i64::from(EPOCH_YEAR) - 1)
        + idays;
    wday %= DAYS_PER_WEEK;
    if wday < 0 {
        wday += DAYS_PER_WEEK;
    }

    let hour = rem / SECS_PER_HOUR;
    rem %= SECS_PER_HOUR;
    let min = rem / SECS_PER_MIN;
    // A positive leap second is rendered "... ??:59:60".
    let sec = rem % SECS_PER_MIN + i64::from(hit);

    let mut mon = 0usize;
    let mut left = idays;
    while left >= MON_LENGTHS[leap_index(y)][mon] {
        left -= MON_LENGTHS[leap_index(y)][mon];
        mon += 1;
    }

    Some(PgTm {
        sec: sec as i32,
        min: min as i32,
        hour: hour as i32,
        mday: (left + 1) as i32,
        mon: i32::try_from(mon).unwrap_or(0),
        year: tm_year,
        wday: wday as i32,
        yday: yday as i32,
        isdst: 0,
        gmtoff: offset,
        zone: b"",
    })
}

/// `localsub` (`localtime.c:1259`) / `pg_localtime` (`:1344`).
#[must_use]
pub fn localtime(sp: &State, t: i64) -> Option<PgTm<'_>> {
    if sp.ats.len() > 1 {
        let last = sp.ats.len() - 1;
        if (sp.goback && t < sp.ats[0]) || (sp.goahead && t > sp.ats[last]) {
            let before = t < sp.ats[0];
            let seconds = if before {
                sp.ats[0] - t
            } else {
                t - sp.ats[last]
            } - 1;
            let years = (seconds / SECS_PER_REPEAT + 1) * YEARS_PER_REPEAT;
            let shift = years * AVG_SECS_PER_YEAR;
            let newt = if before { t + shift } else { t - shift };
            if newt < sp.ats[0] || newt > sp.ats[last] {
                return None; // "cannot happen"
            }
            let mut result = localtime(sp, newt)?;
            let shifted = i64::from(result.year) + if before { -years } else { years };
            result.year = i32::try_from(shifted).ok()?;
            return Some(result);
        }
    }

    let i = if sp.ats.is_empty() || t < sp.ats[0] {
        sp.defaulttype
    } else {
        // Upstream's binary search (`:1312`).
        let mut lo = 1usize;
        let mut hi = sp.ats.len();
        while lo < hi {
            let mid = (lo + hi) >> 1;
            if t < sp.ats[mid] {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        usize::from(sp.types[lo - 1])
    };
    let ttisp = *sp.ttis.get(i)?;
    let mut result = timesub(t, i64::from(ttisp.utoff), sp)?;
    result.isdst = i32::from(ttisp.isdst);
    result.zone = c_str(&sp.chars, ttisp.desigidx);
    Some(result)
}

/// `pg_tz_acceptable` (`localtime.c:2004`): reject leap-second timekeeping by
/// insisting that GMT midnight, 2000-01-01 has `tm_sec == 0`.
#[must_use]
pub fn acceptable(sp: &State) -> bool {
    /// `(POSTGRES_EPOCH_JDATE - UNIX_EPOCH_JDATE) * SECS_PER_DAY`.
    const TIME_2000: i64 = 946_684_800;
    matches!(localtime(sp, TIME_2000), Some(tm) if tm.sec == 0)
}

/// `mktime` as `build_time_t` (`findtimezone.c:190`) uses it: the instant at
/// which this zone's wall clock reads `year-month-day 00:00:00`.
///
/// Upstream calls the C library's `mktime`, which resolves `tm_isdst == -1` by
/// searching; this settles the offset by one fixpoint step, which agrees with
/// `mktime` for every local time that is neither skipped nor repeated by a DST
/// transition — and midnight on January 15 and July 15 is neither.
#[must_use]
pub fn build_time_t(sp: &State, year: i32, month: i32, day: i32) -> Option<i64> {
    let utc = days_from_civil(year, month, day) * SECS_PER_DAY;
    let mut t = utc;
    for _ in 0..2 {
        let offset = localtime(sp, t)?.gmtoff;
        t = utc - offset;
    }
    Some(t)
}

/// Days since 1970-01-01 for a proleptic Gregorian date, by Howard Hinnant's
/// `days_from_civil`. `timesub` is the inverse and pins it.
fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One local time type for [`tzif`]: UT offset, `isdst`, index into `chars`.
    pub(crate) type Type = (i32, bool, u8);

    /// Build a TZif image (RFC 8536, the layout `tzfile.h:39` describes).
    ///
    /// `version` 0 writes the 32-bit block alone, as a version 1 file; 2 writes
    /// it, then the 64-bit block, then the `\n`-wrapped POSIX TZ footer. Test
    /// input only — this is the encoder for the decoder under test, not a port
    /// of `zic`.
    pub(crate) fn tzif(
        version: u8,
        transitions: &[(i64, u8)],
        types: &[Type],
        chars: &[u8],
        footer: &str,
    ) -> Vec<u8> {
        fn block(transitions: &[(i64, u8)], types: &[Type], chars: &[u8], width: usize) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"TZif");
            out.push(if width == 4 { 0 } else { b'2' });
            out.extend_from_slice(&[0u8; 15]);
            for count in [0u32, 0, 0] {
                // ttisutcnt, ttisstdcnt, leapcnt
                out.extend_from_slice(&count.to_be_bytes());
            }
            out.extend_from_slice(&u32::try_from(transitions.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&u32::try_from(types.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&u32::try_from(chars.len()).unwrap().to_be_bytes());
            for &(at, _) in transitions {
                if width == 4 {
                    out.extend_from_slice(&i32::try_from(at).unwrap().to_be_bytes());
                } else {
                    out.extend_from_slice(&at.to_be_bytes());
                }
            }
            for &(_, typ) in transitions {
                out.push(typ);
            }
            for &(utoff, isdst, desigidx) in types {
                out.extend_from_slice(&utoff.to_be_bytes());
                out.push(u8::from(isdst));
                out.push(desigidx);
            }
            out.extend_from_slice(chars);
            out
        }

        let mut image = block(transitions, types, chars, 4);
        // A version 1 file carries no version byte and no second block.
        if version == 0 {
            image[4] = 0;
            return image;
        }
        image[4] = b'2';
        image.extend(block(transitions, types, chars, 8));
        image.push(b'\n');
        image.extend_from_slice(footer.as_bytes());
        image.push(b'\n');
        image
    }

    /// A fixed-offset zone with no transitions, the shape of `Etc/*`.
    pub(crate) fn fixed(abbrev: &str, utoff: i32) -> Vec<u8> {
        let mut chars = abbrev.as_bytes().to_vec();
        chars.push(0);
        tzif(0, &[], &[(utoff, false, 0)], &chars, "")
    }

    fn at(state: &State, t: i64) -> PgTm<'_> {
        localtime(state, t).expect("an instant inside the zone's range")
    }

    /// `2000-01-01T00:00:00Z`, the instant `pg_tz_acceptable` probes.
    const Y2K: i64 = 946_684_800;

    #[test]
    fn a_version_one_file_with_no_transitions_is_its_one_type() {
        let state = load(&fixed("UTC", 0), true).expect("load a fixed-offset zone");
        let tm = at(&state, Y2K);
        assert_eq!(
            (tm.calendar_year(), tm.mon, tm.mday, tm.hour, tm.min, tm.sec),
            (2000, 0, 1, 0, 0, 0)
        );
        assert_eq!(tm.wday, 6, "2000-01-01 was a Saturday");
        assert_eq!(tm.yday, 0);
        assert_eq!(tm.zone_name(), "UTC");
        assert_eq!(tm.gmtoff, 0);
        assert_eq!(tm.isdst, 0);
    }

    #[test]
    fn a_fixed_offset_shifts_the_wall_clock_and_the_day() {
        // Etc/GMT+5 is five hours *west* of Greenwich (`findtimezone.c:507`).
        let state = load(&fixed("-05", -5 * 3600), true).expect("load");
        let tm = at(&state, Y2K);
        assert_eq!(
            (tm.calendar_year(), tm.mon, tm.mday, tm.hour),
            (1999, 11, 31, 19)
        );
        assert_eq!(tm.wday, 5, "1999-12-31 was a Friday");
        assert_eq!(tm.yday, 364);
        assert_eq!(tm.gmtoff, -5 * 3600);
    }

    #[test]
    fn a_transition_switches_the_type_at_its_instant_and_not_before() {
        // Two types, one transition at Y2K: "AAA" +0, then "BBB" +1h daylight.
        let image = tzif(
            0,
            &[(Y2K, 1)],
            &[(0, false, 0), (3600, true, 4)],
            b"AAA\0BBB\0",
            "",
        );
        let state = load(&image, true).expect("load");
        let before = at(&state, Y2K - 1);
        assert_eq!((before.zone_name().as_ref(), before.isdst), ("AAA", 0));
        let after = at(&state, Y2K);
        assert_eq!((after.zone_name().as_ref(), after.isdst), ("BBB", 1));
        assert_eq!(after.hour, 1, "the wall clock jumped forward an hour");
    }

    #[test]
    fn the_footer_rules_govern_instants_after_the_last_transition() {
        // zic's "slim" output: one stored transition, and a POSIX TZ string for
        // everything after it. Without the `doextend` splice (`localtime.c:415`)
        // the zone would freeze at the last stored type.
        let image = tzif(
            2,
            &[(Y2K, 0)],
            &[(-5 * 3600, false, 0), (-4 * 3600, true, 4)],
            b"EST\0EDT\0",
            "EST5EDT,M3.2.0,M11.1.0",
        );
        let state = load(&image, true).expect("load a slim file");
        // 2024-07-04T12:00:00Z is inside the US daylight window.
        let july = at(&state, 1_720_094_400);
        assert_eq!((july.zone_name().as_ref(), july.isdst), ("EDT", 1));
        assert_eq!(july.hour, 8);
        // 2024-01-04T12:00:00Z is not.
        let january = at(&state, 1_704_369_600);
        assert_eq!((january.zone_name().as_ref(), january.isdst), ("EST", 0));
        assert_eq!(january.hour, 7);
    }

    #[test]
    fn without_doextend_the_footer_is_ignored() {
        let image = tzif(
            2,
            &[(Y2K, 0)],
            &[(-5 * 3600, false, 0), (-4 * 3600, true, 4)],
            b"EST\0EDT\0",
            "EST5EDT,M3.2.0,M11.1.0",
        );
        let state = load(&image, false).expect("load");
        let july = at(&state, 1_720_094_400);
        assert_eq!(
            (july.zone_name().as_ref(), july.isdst),
            ("EST", 0),
            "the stored type runs to the end of time"
        );
    }

    #[test]
    fn a_posix_tz_string_parses_into_the_same_rules() {
        let state = parse("EST5EDT,M3.2.0,M11.1.0", false).expect("parse the US eastern rules");
        assert_eq!(at(&state, 1_720_094_400).zone_name(), "EDT");
        assert_eq!(at(&state, 1_704_369_600).zone_name(), "EST");
        // The 2024 US transitions: 2024-03-10 07:00Z and 2024-11-03 06:00Z.
        assert_eq!(at(&state, 1_710_054_000 - 1).zone_name(), "EST");
        assert_eq!(at(&state, 1_710_054_000).zone_name(), "EDT");
        assert_eq!(at(&state, 1_730_613_600 - 1).zone_name(), "EDT");
        assert_eq!(at(&state, 1_730_613_600).zone_name(), "EST");
    }

    #[test]
    fn a_dst_name_without_rules_gets_the_us_ones() {
        // localtime.c:1026 — TZDEFRULESTRING stands in, because upstream never
        // loads TZDEFRULES.
        let implied = parse("EST5EDT", false).expect("parse");
        let spelled = parse("EST5EDT,M3.2.0,M11.1.0", false).expect("parse");
        for t in [
            1_704_369_600i64,
            1_710_054_000,
            1_720_094_400,
            1_730_613_600,
        ] {
            assert_eq!(at(&implied, t), at(&spelled, t), "at {t}");
        }
    }

    #[test]
    fn a_bare_standard_name_has_no_transitions() {
        let state = parse("MST7", false).expect("parse");
        assert_eq!(state.ats.len(), 0);
        assert_eq!(state.ttis.len(), 1);
        let tm = at(&state, Y2K);
        assert_eq!(
            (tm.zone_name().as_ref(), tm.gmtoff, tm.isdst),
            ("MST", -7 * 3600, 0)
        );
    }

    #[test]
    fn the_lastditch_parse_takes_the_whole_name_at_offset_zero() {
        // pg_load_tz sends "GMT" here rather than to tzload (`findtimezone.c:99`).
        let state = parse("GMT", true).expect("parse GMT");
        let tm = at(&state, Y2K);
        assert_eq!((tm.zone_name().as_ref(), tm.gmtoff), ("GMT", 0));
        // Without lastditch, "GMT" has no offset to read and is refused.
        assert_eq!(parse("GMT", false), None);
    }

    #[test]
    fn a_quoted_abbreviation_is_taken_between_the_angle_brackets() {
        let state = parse("<+0530>-5:30", false).expect("parse");
        let tm = at(&state, Y2K);
        assert_eq!(
            (tm.zone_name().as_ref(), tm.gmtoff),
            ("+0530", 5 * 3600 + 1800)
        );
    }

    #[test]
    fn a_malformed_posix_string_is_refused_rather_than_guessed() {
        for bad in [
            "",                            // no name at all
            "EST",                         // no offset
            "<EST5",                       // unterminated quote
            "EST5EDT,",                    // a rule marker with no rule
            "EST5EDT,M3.2.0",              // one rule, not two
            "EST5EDT,M3.2.0,M11.1.0,junk", // trailing text
            "EST5EDT,M13.2.0,M11.1.0",     // month 13
        ] {
            assert_eq!(parse(bad, false), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_truncated_or_alien_image_is_refused() {
        let good = fixed("UTC", 0);
        for len in 0..good.len() {
            assert_eq!(load(&good[..len], true), None, "a {len}-byte image");
        }
        assert_eq!(load(b"# not a TZif file at all\n", true), None);
    }

    #[test]
    fn leap_seconds_make_a_zone_unacceptable() {
        // pg_tz_acceptable insists tm_sec is 0 at Y2K (`localtime.c:2004`). A
        // leap-second-aware zone counts the corrections as elapsed seconds, so
        // its clock lags: with one correction on the books Y2K reads 23:59:59
        // of the day before, and a real right/ zone (32 corrections by 2000)
        // reads 23:59:28.
        let mut image = fixed("UTC", 0);
        // Rewrite the header with one leap second, and append it.
        let leapcnt = 1u32;
        image[28..32].copy_from_slice(&leapcnt.to_be_bytes());
        image.extend_from_slice(&78_796_800i32.to_be_bytes()); // 1972-07-01
        image.extend_from_slice(&1i32.to_be_bytes());
        let state = load(&image, true).expect("load a leap-second zone");
        let tm = at(&state, Y2K);
        assert_eq!(
            (tm.calendar_year(), tm.mon, tm.mday, tm.hour, tm.min, tm.sec),
            (1999, 11, 31, 23, 59, 59),
            "one correction puts Y2K one second short"
        );
        assert!(!acceptable(&state));
        assert!(acceptable(&load(&fixed("UTC", 0), true).expect("load")));
    }

    #[test]
    fn build_time_t_lands_on_local_midnight() {
        let utc = load(&fixed("UTC", 0), true).expect("load");
        let t = build_time_t(&utc, 2026, 1, 15).expect("mktime");
        assert_eq!(t, 1_768_435_200);
        let tm = at(&utc, t);
        assert_eq!(
            (tm.calendar_year(), tm.mon, tm.mday, tm.hour, tm.min, tm.sec),
            (2026, 0, 15, 0, 0, 0)
        );

        // And in a zone with an offset, midnight is still midnight locally.
        let east = parse("EST5EDT,M3.2.0,M11.1.0", false).expect("parse");
        for (month, day) in [(1, 15), (7, 15)] {
            let t = build_time_t(&east, 2026, month, day).expect("mktime");
            let tm = at(&east, t);
            assert_eq!(
                (tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec),
                (month, day, 0, 0, 0),
                "{month}/{day}"
            );
        }
    }

    #[test]
    fn timesub_inverts_the_civil_date_for_a_century_of_days() {
        // days_from_civil feeds build_time_t; timesub is the only other place
        // this port turns days into a date, so agreeing pins both.
        let utc = load(&fixed("UTC", 0), true).expect("load");
        let mut expected_wday = 4; // 1970-01-01 was a Thursday.
        let mut yday = 0;
        let (mut y, mut m, mut d) = (1970, 1, 1);
        for day in 0..(365 * 100) {
            let tm = at(&utc, i64::from(day) * SECS_PER_DAY);
            assert_eq!(
                (tm.calendar_year(), tm.mon + 1, tm.mday, tm.wday, tm.yday),
                (y, m, d, expected_wday, yday),
                "day {day}"
            );
            expected_wday = (expected_wday + 1) % 7;
            d += 1;
            yday += 1;
            if d > i32::try_from(MON_LENGTHS[leap_index(y)][usize::try_from(m).unwrap() - 1])
                .unwrap()
            {
                d = 1;
                m += 1;
            }
            if m > 12 {
                m = 1;
                y += 1;
                yday = 0;
            }
        }
    }

    #[test]
    fn an_instant_before_the_first_transition_takes_the_default_type() {
        // localtime.c:537 — type 0 is used in a transition here, and the first
        // transition is to daylight, so early times take the standard type
        // below it rather than type 0.
        let image = tzif(
            0,
            &[(Y2K, 0)],
            &[(3600, true, 0), (0, false, 4)],
            b"BST\0GMT\0",
            "",
        );
        let state = load(&image, true).expect("load");
        assert_eq!(at(&state, 0).zone_name(), "GMT");
        assert_eq!(at(&state, Y2K).zone_name(), "BST");
    }
}
