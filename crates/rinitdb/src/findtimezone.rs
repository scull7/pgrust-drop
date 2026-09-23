//! `select_default_timezone` (`src/bin/initdb/findtimezone.c`, PostgreSQL
//! 18.6): the zone `setup_config` writes into `timezone` and `log_timezone`.
//!
//! Upstream scores every zone in the timezone database against the behaviour of
//! the C library's `localtime()` over a probe-date set, and takes the one that
//! matches furthest into the past; on Linux and macOS it first tries the
//! `/etc/localtime` symlink, which names the zone directly. Both paths are
//! here. The Windows arm (`findtimezone.c:727`-`:1721`, a hand-made mapping
//! table) is not.
//!
//! Layout: [`TzSource`] is the only place this module touches the machine —
//! the timezone directory, the `/etc/localtime` symlink, `getenv("TZ")` and
//! the clock. Everything else is a pure calculation over [`crate::tz`]:
//! [`Probe`] holds the test times and what the system's zone makes of them,
//! and scores a candidate against them.

use std::path::{Path, PathBuf};

use crate::tz::{self, PgTm, State, TZ_STRLEN_MAX};

/// `findtimezone.c:152`-`:154`.
const T_DAY: i64 = 60 * 60 * 24;
const T_WEEK: i64 = 60 * 60 * 24 * 7;
const T_MONTH: i64 = 60 * 60 * 24 * 31;

/// `MAX_TEST_TIMES` (`findtimezone.c:156`): 100 years of weekly probes.
const MAX_TEST_TIMES: usize = 52 * 100;

/// `TZDEFAULT` (`tzfile.h:27`).
pub const TZDEFAULT: &str = "/etc/localtime";

/// Where `PGRUST_TZDIR` is read from; see the crate's `docs/divergences.md`
/// row for why a system directory is the fallback.
pub const TZDIR_ENV: &str = "PGRUST_TZDIR";

/// Where to look for a system timezone database when `PGRUST_TZDIR` is unset.
/// The first that exists wins.
///
/// This list and its order are this port's own choice, not upstream's.
/// PostgreSQL has no list: a `--with-system-tzdata=DIRECTORY` build compiles
/// the one directory it was given in as `SYSTEMTZDIR` (`findtimezone.c:44`),
/// and neither `configure`, `configure.ac` nor `meson.build` names a default.
/// The only mention of any of these paths at `REL_18_6` is the installation
/// guide's "`/usr/share/zoneinfo` is a likely directory on some operating
/// systems" (`doc/src/sgml/installation.sgml:1379`, and `:2866` for meson).
/// See `docs/divergences.md`.
const SYSTEM_TZDIRS: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/etc/zoneinfo",
];

/// No real timezone database nests deeper than `America/Argentina/Buenos_Aires`;
/// the bound only stops a symlink cycle under a hand-set `PGRUST_TZDIR` from
/// recursing forever, where C would run out of stack.
const MAX_SCAN_DEPTH: usize = 16;

// --------------------------------------------------------------------------
// The machine
// --------------------------------------------------------------------------

/// Everything `findtimezone.c` reads from outside itself.
pub trait TzSource {
    /// `pg_open_tzfile` (`findtimezone.c:65`) and the `read` that follows it:
    /// the bytes of `TZDIR/name`, or `None` when there is no such file.
    fn read_tzfile(&self, name: &str) -> Option<Vec<u8>>;

    /// The zone names under `TZDIR`, as `scan_available_timezones` (`:657`)
    /// walks them: `/`-joined, "hidden" entries skipped, directories recursed
    /// into. Order does not matter — the tie-break at `:709` is a total order.
    fn zone_names(&self) -> Vec<String>;

    /// `readlink(linkname)` (`:556`).
    fn read_link(&self, linkname: &str) -> Option<String>;

    /// The bytes of a plain path, for the file the C library's `localtime()`
    /// reads. Not an upstream call; see the module comment on [`system_state`].
    fn read_path(&self, path: &str) -> Option<Vec<u8>>;

    /// `getenv("TZ")` (`:1767`).
    fn tz_env(&self) -> Option<String>;

    /// `time(NULL)` (`:368`).
    fn now(&self) -> i64;
}

/// [`TzSource`] over the real machine.
#[derive(Debug, Clone)]
pub struct RealTzSource {
    tzdir: PathBuf,
}

impl RealTzSource {
    /// `pg_TZDIR()` (`:37`). `PGRUST_TZDIR` stands in for `share_path/timezone`
    /// and a system directory for `SYSTEMTZDIR`; `None` when neither is there,
    /// which is upstream's "no timezone database" and lands on GMT.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if let Some(dir) = std::env::var_os(TZDIR_ENV) {
            let dir = PathBuf::from(dir);
            return dir.is_dir().then_some(Self { tzdir: dir });
        }
        SYSTEM_TZDIRS
            .iter()
            .map(PathBuf::from)
            .find(|dir| dir.is_dir())
            .map(|tzdir| Self { tzdir })
    }

    /// The directory this source reads zones from.
    #[must_use]
    pub fn tzdir(&self) -> &Path {
        &self.tzdir
    }
}

/// `scan_available_timezones`' recursive walk (`findtimezone.c:657`), as far as
/// the names go: the scoring itself is [`scan_available_timezones`].
fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // pgfnames returns NULL and the caller just returns (`:665`).
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Ignore . and .., plus any other "hidden" files (`:673`).
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // `stat`, so a symlink to a directory is recursed into (`:680`).
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let sub = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if meta.is_dir() {
            walk(&path, &sub, depth + 1, out);
        } else {
            out.push(sub);
        }
    }
}

impl TzSource for RealTzSource {
    fn read_tzfile(&self, name: &str) -> Option<Vec<u8>> {
        // `strlen(fullname) + 1 + strlen(name) >= MAXPGPATH` (`:73`) is the only
        // check upstream makes; a name that escapes the directory would have
        // been rejected by `open` there and by `read` here just the same.
        std::fs::read(self.tzdir.join(name)).ok()
    }

    fn zone_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        walk(&self.tzdir, "", 0, &mut names);
        names.sort_unstable();
        names
    }

    fn read_link(&self, linkname: &str) -> Option<String> {
        std::fs::read_link(linkname)
            .ok()
            .and_then(|target| target.to_str().map(str::to_owned))
    }

    fn read_path(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn tz_env(&self) -> Option<String> {
        std::env::var("TZ").ok()
    }

    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }
}

// --------------------------------------------------------------------------
// Calculations
// --------------------------------------------------------------------------

/// `struct tztry` (`findtimezone.c:158`), with the system's own answer for each
/// test time alongside.
///
/// Upstream calls `localtime()` inside the scoring loop; caching it here is the
/// same values in the same order, computed once instead of once per candidate.
#[derive(Debug, Clone)]
pub struct Probe<'a> {
    test_times: Vec<i64>,
    system: Vec<Option<PgTm<'a>>>,
}

impl<'a> Probe<'a> {
    /// `identify_system_timezone`'s probe-date set (`findtimezone.c:349`):
    /// January and July 15 of the current year, then every week for 100 years
    /// back from that July, each rounded back to GMT midnight Thursday.
    #[must_use]
    pub fn build(system: &'a State, now: i64) -> Option<Self> {
        let thisyear = tz::localtime(system, now)?.calendar_year();

        let mut test_times = Vec::with_capacity(MAX_TEST_TIMES);
        // The rounding depends on the time_t origin being Thu Jan 01 1970.
        let january = tz::build_time_t(system, thisyear, 1, 15)?;
        test_times.push(january - january % T_WEEK);

        let july = tz::build_time_t(system, thisyear, 7, 15)?;
        let mut t = july - july % T_WEEK;
        test_times.push(t);
        while test_times.len() < MAX_TEST_TIMES {
            t -= T_WEEK;
            test_times.push(t);
        }

        let system = test_times
            .iter()
            .map(|&t| tz::localtime(system, t))
            .collect();
        Some(Self { test_times, system })
    }

    /// How many test times this probe holds (`tt->n_test_times`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.test_times.len()
    }

    /// Whether this probe holds no test times.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.test_times.is_empty()
    }

    /// `score_timezone` (`findtimezone.c:234`): how many test times, counted
    /// from the most recent, the candidate reproduces. `-1` for a zone that is
    /// unusable at all — upstream's "worse than zero".
    #[must_use]
    pub fn score(&self, candidate: &State) -> i32 {
        // Reject if leap seconds involved (`:248`).
        if !tz::acceptable(candidate) {
            return -1;
        }
        for (i, &t) in self.test_times.iter().enumerate() {
            let Some(theirs) = tz::localtime(candidate, t) else {
                return -1; // probably shouldn't happen
            };
            let Some(ours) = self.system[i].as_ref() else {
                return i32::try_from(i).unwrap_or(i32::MAX); // system had no data
            };
            if !ours.matches(&theirs) {
                return i32::try_from(i).unwrap_or(i32::MAX);
            }
        }
        i32::try_from(self.test_times.len()).unwrap_or(i32::MAX)
    }

    /// `perfect_timezone_match` (`findtimezone.c:320`).
    #[must_use]
    pub fn is_perfect_match(&self, candidate: &State) -> bool {
        self.score(candidate) == i32::try_from(self.test_times.len()).unwrap_or(i32::MAX)
    }
}

/// `zone_name_pref` (`findtimezone.c:615`): which of two equally good names to
/// prefer. Larger is more preferred, 0 is neutral.
#[must_use]
pub fn zone_name_pref(zonename: &str) -> i32 {
    // Prefer UTC over alternatives such as UCT, and Etc/UTC over Etc/UCT; but
    // UTC is preferred to Etc/UTC.
    match zonename {
        "UTC" => 50,
        "Etc/UTC" => 40,
        // Neither of these is a real zone name.
        "localtime" | "posixrules" => -50,
        _ => 0,
    }
}

/// `scan_available_timezones`' tie-break (`findtimezone.c:709`): does
/// `candidate` beat `best` at an equal score?
#[must_use]
pub fn breaks_tie(candidate: &str, best: &str) -> bool {
    let namepref = zone_name_pref(candidate) - zone_name_pref(best);
    namepref > 0
        || (namepref == 0
            && (candidate.len() < best.len()
                || (candidate.len() == best.len() && candidate < best)))
}

/// `pg_load_tz` (`findtimezone.c:91`).
fn load_tz(src: &impl TzSource, name: &str) -> Option<State> {
    if name.len() > TZ_STRLEN_MAX {
        return None; // not going to fit
    }
    // "GMT" is always sent to tzparse; see the comments for pg_tzset().
    if name == "GMT" {
        return tz::parse(name, true);
    }
    // tzload strips a leading ':' itself (`localtime.c:230`).
    let file = name.strip_prefix(':').unwrap_or(name);
    if let Some(state) = src
        .read_tzfile(file)
        .and_then(|image| tz::load(&image, true))
    {
        return Some(state);
    }
    if name.starts_with(':') {
        return None;
    }
    tz::parse(name, false)
}

/// `validate_zone` (`findtimezone.c:1728`).
fn validate_zone(src: &impl TzSource, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    load_tz(src, name).is_some_and(|state| tz::acceptable(&state))
}

/// `check_system_link_file` (`findtimezone.c:544`): does the tail of the
/// symlink's target name a zone whose behaviour matches the system's exactly?
#[must_use]
pub fn check_system_link_file(
    link_target: &str,
    probe: &Probe,
    load: impl Fn(&str) -> Option<State>,
) -> Option<String> {
    let bytes = link_target.as_bytes();
    // Nobody puts their zone DB in the root directory, so the first component
    // is definitely skippable; after that it is trial and error.
    let mut cur = 0usize;
    while cur < bytes.len() {
        // Advance to the next segment of the path.
        let Some(offset) = bytes[cur + 1..].iter().position(|&c| c == b'/') else {
            break;
        };
        cur += 1 + offset;
        // If there are consecutive slashes, skip all, as the kernel would.
        loop {
            cur += 1;
            if bytes.get(cur) != Some(&b'/') {
                break;
            }
        }
        if cur >= bytes.len() {
            break;
        }
        let rest = &link_target[cur..];
        // Relative paths might contain ".."; also defend against overlength
        // names.
        if !rest.starts_with('.')
            && rest.len() <= TZ_STRLEN_MAX
            && load(rest).is_some_and(|state| probe.is_perfect_match(&state))
        {
            return Some(rest.to_owned());
        }
    }
    None
}

/// `scan_available_timezones` (`findtimezone.c:657`), over the names the walk
/// produced. Returns the best score and the name that earned it.
#[must_use]
pub fn scan_available_timezones(
    zones: &[String],
    probe: &Probe,
    load: impl Fn(&str) -> Option<State>,
) -> (i32, String) {
    let mut bestscore = -1i32;
    let mut bestzonename = String::new();
    for name in zones {
        let score = load(name).map_or(-1, |state| probe.score(&state));
        if score > bestscore {
            bestscore = score;
            bestzonename.clone_from(name);
        } else if score == bestscore && breaks_tie(name, &bestzonename) {
            bestzonename.clone_from(name);
        }
    }
    (bestscore, bestzonename)
}

/// `identify_system_timezone`'s constructed-name stage (`findtimezone.c:424`):
/// the local standard and daylight abbreviations and the standard offset,
/// found by scanning forward up to 14 months.
#[must_use]
pub fn local_zone_abbrevs(system: &State, now: i64) -> (String, String, i64) {
    let mut std_zone_name = String::new();
    let mut dst_zone_name = String::new();
    let mut std_ofs = 0i64;

    // Round back to a GMT midnight so results do not depend on time of day.
    let tnow = now - now % T_DAY;
    let mut t = tnow;
    while t <= tnow + T_MONTH * 14 {
        if let Some(tm) = tz::localtime(system, t) {
            if tm.isdst == 0 && std_zone_name.is_empty() {
                std_zone_name = tm.zone_name().into_owned();
                std_ofs = tm.gmtoff;
            }
            if tm.isdst > 0 && dst_zone_name.is_empty() {
                dst_zone_name = tm.zone_name().into_owned();
            }
            if !std_zone_name.is_empty() && !dst_zone_name.is_empty() {
                break;
            }
        }
        t += T_MONTH;
    }
    (std_zone_name, dst_zone_name, std_ofs)
}

/// `identify_system_timezone` (`findtimezone.c:331`), minus the `tzset()` at
/// `:347`: the system's behaviour arrives as `system`.
fn identify_system_timezone(src: &impl TzSource, system: &State) -> Option<String> {
    let now = src.now();
    let probe = Probe::build(system, now)?;

    // Try to avoid the brute-force search by recognizing the setting directly.
    if let Some(name) = src
        .read_link(TZDEFAULT)
        .and_then(|target| check_system_link_file(&target, &probe, |n| load_tz(src, n)))
    {
        return Some(name);
    }

    // No luck, so search for the best-matching timezone file.
    let (bestscore, bestzonename) =
        scan_available_timezones(&src.zone_names(), &probe, |n| load_tz(src, n));
    if bestscore > 0 {
        // Ignore IANA's rather silly "Factory" zone; use GMT instead.
        if bestzonename == "Factory" {
            return None;
        }
        return Some(bestzonename);
    }

    // Couldn't find a match in the database, so try constructed zone names.
    let (std_zone_name, dst_zone_name, std_ofs) = local_zone_abbrevs(system, now);
    if std_zone_name.is_empty() {
        return None; // go to GMT
    }
    let usable = |name: &str| -> bool { load_tz(src, name).is_some_and(|s| probe.score(&s) > 0) };

    // If we found DST then try STD<ofs>DST.
    if !dst_zone_name.is_empty() {
        let candidate = format!("{std_zone_name}{}{dst_zone_name}", -std_ofs / 3600);
        if usable(&candidate) {
            return Some(candidate);
        }
    }
    // Try just the STD timezone (works for GMT at least).
    if usable(&std_zone_name) {
        return Some(std_zone_name);
    }
    // Try STD<ofs>.
    let candidate = format!("{std_zone_name}{}", -std_ofs / 3600);
    if usable(&candidate) {
        return Some(candidate);
    }

    // Fall back to a GMT-offset zone. The IANA database names these in POSIX
    // style: plus is west of Greenwich.
    let hours = -std_ofs / 3600;
    Some(format!(
        "Etc/GMT{}{hours}",
        if -std_ofs > 0 { "+" } else { "" }
    ))
}

/// The zone definition the C library's `localtime()` is working from: `TZ` if
/// it names one, else `/etc/localtime`, else UTC.
///
/// Upstream calls `localtime()` itself. This port cannot — see the
/// `docs/divergences.md` row — so it reads the same file the C library reads
/// and interprets it with the same code it uses for a candidate zone. When
/// that file is absent or unreadable (a fresh container, a host whose zone was
/// never set) glibc and musl both run on UTC, and so does this.
#[must_use]
pub fn system_state(src: &impl TzSource) -> Option<State> {
    if let Some(name) = src.tz_env() {
        // An empty TZ selects UTC, as it does for the C library.
        if name.is_empty() || name == ":" {
            return tz::parse("UTC0", false);
        }
        if let Some(state) = load_tz(src, &name) {
            return Some(state);
        }
        // The C library falls back to UTC for a TZ it cannot make sense of.
        return tz::parse("UTC0", false);
    }
    src.read_path(TZDEFAULT)
        .and_then(|image| tz::load(&image, true))
        // No /etc/localtime: the C library's localtime() answers in UTC.
        .or_else(|| tz::parse("UTC0", false))
}

/// `select_default_timezone` (`findtimezone.c:1757`): `TZ` if it names a zone
/// this database knows, else whatever best matches the system's behaviour.
/// `None` means GMT, which is what `setup_config` leaves the two lines
/// commented out for (`initdb.c:1348`).
#[must_use]
pub fn select_default_timezone(src: &impl TzSource) -> Option<String> {
    // Check TZ environment variable.
    if let Some(name) = src.tz_env().filter(|name| validate_zone(src, name)) {
        return Some(name);
    }
    // Nope, so try to identify the system timezone.
    let system = system_state(src)?;
    let name = identify_system_timezone(src, &system)?;
    validate_zone(src, &name).then_some(name)
}

/// The action `setup_config`'s probe stage calls: `select_default_timezone`
/// over the real machine. `None` when there is no timezone database to search,
/// which is the same answer upstream gives when it can identify no zone.
#[must_use]
pub fn default_timezone() -> Option<String> {
    let src = RealTzSource::from_env()?;
    select_default_timezone(&src)
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::tz::tests::{fixed, tzif};

    /// A timezone database and a machine, as data.
    #[derive(Default)]
    struct FakeTz {
        zones: BTreeMap<String, Vec<u8>>,
        link: Option<String>,
        localtime: Option<Vec<u8>>,
        tz: Option<String>,
        now: i64,
        scans: Cell<usize>,
    }

    impl FakeTz {
        /// A database of `name -> image`, with `/etc/localtime` a copy of
        /// `system`'s bytes (as it is on a machine that copies rather than
        /// symlinks) and no symlink.
        fn new(zones: &[(&str, Vec<u8>)], system: &[u8]) -> Self {
            Self {
                zones: zones
                    .iter()
                    .map(|(n, i)| ((*n).to_owned(), i.clone()))
                    .collect(),
                localtime: Some(system.to_vec()),
                // 2026-09-16T12:00:00Z, a date inside every zone's rules.
                now: 1_789_128_000,
                ..Self::default()
            }
        }

        fn linked_to(mut self, target: &str) -> Self {
            self.link = Some(target.to_owned());
            self
        }

        /// A machine whose zone was never set: no `/etc/localtime` at all.
        fn without_localtime(mut self) -> Self {
            self.localtime = None;
            self
        }

        fn with_tz(mut self, tz: &str) -> Self {
            self.tz = Some(tz.to_owned());
            self
        }
    }

    impl TzSource for FakeTz {
        fn read_tzfile(&self, name: &str) -> Option<Vec<u8>> {
            self.zones.get(name).cloned()
        }

        fn zone_names(&self) -> Vec<String> {
            self.scans.set(self.scans.get() + 1);
            self.zones.keys().cloned().collect()
        }

        fn read_link(&self, linkname: &str) -> Option<String> {
            (linkname == TZDEFAULT).then(|| self.link.clone()).flatten()
        }

        fn read_path(&self, path: &str) -> Option<Vec<u8>> {
            (path == TZDEFAULT)
                .then(|| self.localtime.clone())
                .flatten()
        }

        fn tz_env(&self) -> Option<String> {
            self.tz.clone()
        }

        fn now(&self) -> i64 {
            self.now
        }
    }

    /// The US eastern zone as zic writes it: one stored transition and a footer.
    fn eastern() -> Vec<u8> {
        tzif(
            2,
            &[(946_684_800, 0)],
            &[(-5 * 3600, false, 0), (-4 * 3600, true, 4)],
            b"EST\0EDT\0",
            "EST5EDT,M3.2.0,M11.1.0",
        )
    }

    #[test]
    fn the_localtime_symlink_names_the_zone_without_a_search() {
        let src = FakeTz::new(
            &[
                ("America/New_York", eastern()),
                ("Etc/UTC", fixed("UTC", 0)),
                ("UTC", fixed("UTC", 0)),
            ],
            &eastern(),
        )
        .linked_to("/usr/share/zoneinfo/America/New_York");
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("America/New_York"))
        );
        assert_eq!(
            src.scans.get(),
            0,
            "the shortcut exists to skip the brute-force scan (findtimezone.c:406)"
        );
    }

    #[test]
    fn a_relative_symlink_and_doubled_slashes_are_walked_the_same_way() {
        let system = tz::load(&eastern(), true).expect("load");
        let probe = Probe::build(&system, 1_789_128_000).expect("build the probe");
        let load = |name: &str| tz::load(&eastern(), true).filter(|_| name == "America/New_York");
        for target in [
            "/usr/share/zoneinfo/America/New_York",
            "../usr/share//zoneinfo///America/New_York",
            "/etc/../usr/share/zoneinfo/America/New_York",
        ] {
            assert_eq!(
                check_system_link_file(target, &probe, load),
                Some(String::from("America/New_York")),
                "{target}"
            );
        }
        // The first component is always skipped (`findtimezone.c:569`), so a
        // link whose target *is* a zone name, with or without a leading slash,
        // finds nothing — which is why upstream only ever reaches the tail.
        for target in ["America/New_York", "/America/New_York"] {
            assert_eq!(
                check_system_link_file(target, &probe, load),
                None,
                "{target}"
            );
        }
    }

    #[test]
    fn the_scan_finds_the_zone_when_there_is_no_symlink() {
        let src = FakeTz::new(
            &[
                ("America/New_York", eastern()),
                ("Etc/UTC", fixed("UTC", 0)),
                ("Europe/London", fixed("GMT", 0)),
            ],
            &eastern(),
        );
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("America/New_York"))
        );
        assert_eq!(src.scans.get(), 1);
    }

    /// A container or a host whose zone was never set has a timezone database
    /// but no `/etc/localtime`. The C library then runs on UTC, and C initdb's
    /// scan finds the zone that behaves that way; without this the port
    /// answered `None` and wrote no `timezone` line where C writes one.
    #[test]
    fn a_machine_without_etc_localtime_runs_on_utc_as_the_c_library_does() {
        let src = FakeTz::new(
            &[
                ("America/New_York", eastern()),
                ("Etc/UTC", fixed("UTC", 0)),
                ("UTC", fixed("UTC", 0)),
            ],
            &eastern(),
        )
        .without_localtime();
        assert_eq!(select_default_timezone(&src), Some(String::from("UTC")));
        assert_eq!(
            src.scans.get(),
            1,
            "there is no symlink to short-cut the scan"
        );
    }

    #[test]
    fn utc_is_preferred_to_uct_and_a_bare_name_to_an_etc_one() {
        let utc = fixed("UTC", 0);
        let src = FakeTz::new(
            &[
                ("Etc/UCT", utc.clone()),
                ("Etc/UTC", utc.clone()),
                ("UCT", utc.clone()),
                ("UTC", utc.clone()),
            ],
            &utc,
        );
        assert_eq!(select_default_timezone(&src), Some(String::from("UTC")));
        // The preference is what keeps alphabetical order from picking UCT.
        assert!(zone_name_pref("UTC") > zone_name_pref("Etc/UTC"));
        assert!(zone_name_pref("Etc/UTC") > zone_name_pref("Etc/UCT"));
        assert!(breaks_tie("UTC", "UCT") && !breaks_tie("UCT", "UTC"));
    }

    #[test]
    fn an_equal_score_is_broken_by_length_and_then_the_alphabet() {
        let utc = fixed("UTC", 0);
        let src = FakeTz::new(
            &[
                ("Universal", utc.clone()),
                ("Zulu", utc.clone()),
                ("GMT0", utc.clone()),
            ],
            &utc,
        );
        assert_eq!(select_default_timezone(&src), Some(String::from("GMT0")));
        assert!(breaks_tie("Zulu", "Universal"), "shorter wins");
        assert!(breaks_tie("GMT0", "Zulu"), "then alphabetically earlier");
        assert!(!breaks_tie("Zulu", "GMT0"));
    }

    #[test]
    fn localtime_and_posixrules_lose_to_any_real_name() {
        let utc = fixed("UTC", 0);
        // Both are shorter than "Etc/GMT", so only the preference saves it.
        let src = FakeTz::new(
            &[("localtime", utc.clone()), ("Etc/GMT", utc.clone())],
            &utc,
        );
        assert_eq!(select_default_timezone(&src), Some(String::from("Etc/GMT")));

        // With no real name to reach for, they are still better than nothing.
        let src = FakeTz::new(&[("localtime", utc.clone())], &utc);
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("localtime"))
        );
    }

    #[test]
    fn the_factory_zone_is_ignored_in_favour_of_gmt() {
        // findtimezone.c:418 — "Ignore IANA's rather silly Factory zone".
        let utc = fixed("UTC", 0);
        let src = FakeTz::new(&[("Factory", utc.clone())], &utc);
        assert_eq!(select_default_timezone(&src), None);
    }

    #[test]
    fn tz_is_taken_as_given_when_it_names_a_zone() {
        let src = FakeTz::new(
            &[
                ("America/New_York", eastern()),
                ("Etc/UTC", fixed("UTC", 0)),
            ],
            &fixed("UTC", 0),
        )
        .linked_to("/usr/share/zoneinfo/Etc/UTC")
        .with_tz("America/New_York");
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("America/New_York")),
            "TZ wins over what the machine says (findtimezone.c:1769)"
        );
    }

    #[test]
    fn a_posix_tz_that_names_no_file_is_still_a_zone() {
        let src = FakeTz::new(&[("Etc/UTC", fixed("UTC", 0))], &fixed("UTC", 0))
            .with_tz("EST5EDT,M3.2.0,M11.1.0");
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("EST5EDT,M3.2.0,M11.1.0"))
        );
    }

    #[test]
    fn an_unusable_tz_falls_through_to_the_machine() {
        let src = FakeTz::new(&[("Etc/UTC", fixed("UTC", 0))], &fixed("UTC", 0))
            .linked_to("/usr/share/zoneinfo/Etc/UTC")
            .with_tz("Mars/Olympus_Mons");
        assert_eq!(select_default_timezone(&src), Some(String::from("Etc/UTC")));
    }

    #[test]
    fn a_constructed_name_is_tried_when_the_database_has_no_match() {
        // findtimezone.c:424 — no zone file matches, so STD<ofs> is built from
        // the abbreviation and offset the machine reports.
        let system = fixed("XYZ", -3 * 3600);
        let src = FakeTz::new(&[("Etc/UTC", fixed("UTC", 0))], &system);
        assert_eq!(select_default_timezone(&src), Some(String::from("XYZ3")));
    }

    #[test]
    fn the_last_resort_is_an_etc_gmt_name_at_the_machines_offset() {
        // An abbreviation that is itself a signed number: no constructed name
        // parses back to it, so upstream's final `Etc/GMT%s%d` (`:511`) lands.
        // The IANA names are POSIX style, so plus is west of Greenwich.
        let system = fixed("+04", 4 * 3600);
        let src = FakeTz::new(&[("Etc/UTC", fixed("UTC", 0))], &system);
        assert_eq!(
            select_default_timezone(&src),
            Some(String::from("Etc/GMT-4"))
        );
    }

    /// No timezone database and no `/etc/localtime`: the C library is on UTC,
    /// the scan has nothing to score, and the constructed-name stage
    /// (`findtimezone.c:494`-`:503`) tries `UTC` (no offset, so not a POSIX
    /// zone) and then `UTC0`, which parses. That is the line C initdb writes on such a
    /// machine; `None`, `setup_config`'s commented-out pair (`initdb.c:1348`),
    /// is reserved for the `Factory` zone.
    #[test]
    fn no_timezone_database_and_no_localtime_still_constructs_utc0() {
        let src = FakeTz {
            now: 1_789_128_000,
            ..FakeTz::default()
        };
        assert_eq!(select_default_timezone(&src), Some(String::from("UTC0")));
    }

    #[test]
    fn a_zone_that_only_matches_recently_loses_to_one_that_matches_further_back() {
        // The whole point of the scoring (`findtimezone.c:225`): both zones
        // agree about today, and only one agrees about 1990.
        let system = eastern();
        let state = tz::load(&system, true).expect("load");
        let probe = Probe::build(&state, 1_789_128_000).expect("probe");
        let full = probe.score(&state);
        assert_eq!(
            full,
            i32::try_from(probe.len()).unwrap(),
            "a zone scores full marks against itself"
        );

        // Same rules, but only from 2020: before that it is plain EST.
        let recent = tzif(
            2,
            &[(1_577_836_800, 0)],
            &[(-5 * 3600, false, 0), (-4 * 3600, true, 4)],
            b"EST\0EDT\0",
            "EST5EDT,M3.2.0,M11.1.0",
        );
        let recent = tz::load(&recent, true).expect("load");
        let partial = probe.score(&recent);
        assert!(
            partial > 0 && partial < full,
            "the truncated zone should match the recent probes only, got {partial} of {full}"
        );
    }

    #[test]
    fn a_zone_the_reader_refuses_scores_worse_than_one_that_matches_nothing() {
        let system = tz::load(&fixed("UTC", 0), true).expect("load");
        let probe = Probe::build(&system, 1_789_128_000).expect("probe");
        // findtimezone.c:246 — an unloadable name is -1, which never wins.
        let (score, name) =
            scan_available_timezones(&[String::from("zone.tab")], &probe, |_| None::<State>);
        assert_eq!((score, name.as_str()), (-1, ""));
    }

    /// Pins the list `docs/divergences.md` records as this port's own, so a
    /// change to it is a change to that row too.
    #[test]
    fn the_system_tzdir_fallbacks_are_this_ports_own_list_in_this_order() {
        assert_eq!(
            SYSTEM_TZDIRS,
            [
                "/usr/share/zoneinfo",
                "/usr/lib/zoneinfo",
                "/usr/share/lib/zoneinfo",
                "/etc/zoneinfo",
            ]
        );
    }
}
