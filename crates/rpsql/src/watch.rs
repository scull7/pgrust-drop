//! `\watch`: run the query buffer again and again (`command.c:3370`,
//! `exec_command_watch`, and `:5873`, `do_watch`).
//!
//! The arguments, the title each run carries and the timer's schedule are
//! pure calculations; [`do_watch`] is the loop that runs the query through
//! [`crate::common::psql_exec_watch`] and sleeps between runs.
//!
//! Three things of upstream's are not here, each recorded in
//! `docs/divergences.md`:
//! - `PSQL_WATCH_PAGER` (`command.c:5940`): no pager is ever started, as for
//!   every other output of this port;
//! - SIGINT: nothing catches it yet (NAT-405), so `^C` ends the process, not
//!   just the `\watch`; a `\watch` without a count runs until then;
//! - the title's time is UTC in the C locale's `%c`, where C formats
//!   `localtime` in the session's `LC_TIME`.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::common::{CommandSource, Executor, psql_exec_watch};
use crate::logging;
use crate::mainloop::Session;
use crate::output::Output;
use crate::settings::Pager;
use crate::strtonum::{format_g, strtod, strtoint};

/// What `exec_command_watch` hands `do_watch` (`command.c:3509`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchArgs {
    /// `sleep_ms`: the interval, `(long) (sleep * 1000)` (`command.c:5875`).
    pub sleep_ms: i64,
    /// `iter`: how many runs, or 0 for no limit.
    pub iter: i32,
    /// `min_rows`: stop once a run returns fewer rows, or 0 for never.
    pub min_rows: i32,
}

/// `strtod` as `\watch` judges it (`command.c:3425`-`:3430`): the whole
/// value read, not negative, not out of range.
// `!(x < 0.0)`, not `x >= 0.0`: C's `sleep < 0` lets a NaN through.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn interval(value: &str) -> Option<f64> {
    let c = strtod(value);
    (!(c.value < 0.0) && c.end == value.len() && !c.erange).then_some(c.value)
}

/// `strtoint` as `\watch` judges a count (`command.c:3445`-`:3450`): the
/// whole value read, positive, in range.
fn count(value: &str) -> Option<i32> {
    let c = strtoint(value);
    (c.value > 0 && c.end == value.len() && !c.erange).then_some(c.value)
}

/// `exec_command_watch()`'s argument loop (`command.c:3392`-`:3501`): an
/// unlabeled interval, or `i=`/`interval=`, `c=`/`count=` and
/// `m=`/`min_rows=`, each at most once. The first bad argument ends the
/// parse with its message; `watch_interval` is `WATCH_INTERVAL`'s value.
///
/// # Errors
/// The message upstream logs for the first argument it refuses.
pub fn parse_watch_args(options: &[&str], watch_interval: f64) -> Result<WatchArgs, String> {
    let mut sleep = None;
    let mut iter = None;
    let mut min_rows = None;
    let twice = |what: &str| format!("\\watch: {what} specified more than once");
    for &opt in options {
        let Some((_, valptr)) = opt.split_once('=') else {
            // Unlabeled argument: take it as interval.
            if sleep.is_some() {
                return Err(twice("interval value is"));
            }
            sleep = Some(
                interval(opt)
                    .ok_or_else(|| format!("\\watch: incorrect interval value \"{opt}\""))?,
            );
            continue;
        };
        if opt.starts_with("i=") || opt.starts_with("interval=") {
            if sleep.is_some() {
                return Err(twice("interval value is"));
            }
            sleep = Some(
                interval(valptr)
                    .ok_or_else(|| format!("\\watch: incorrect interval value \"{valptr}\""))?,
            );
        } else if opt.starts_with("c=") || opt.starts_with("count=") {
            if iter.is_some() {
                return Err(twice("iteration count is"));
            }
            iter = Some(
                count(valptr)
                    .ok_or_else(|| format!("\\watch: incorrect iteration count \"{valptr}\""))?,
            );
        } else if opt.starts_with("m=") || opt.starts_with("min_rows=") {
            if min_rows.is_some() {
                return Err(twice("minimum row count"));
            }
            min_rows = Some(
                count(valptr)
                    .ok_or_else(|| format!("\\watch: incorrect minimum row count \"{valptr}\""))?,
            );
        } else {
            return Err(format!("\\watch: unrecognized parameter \"{opt}\""));
        }
    }
    let sleep = sleep.unwrap_or(watch_interval);
    Ok(WatchArgs {
        // `(long) (sleep * 1000)`: truncated toward zero.
        #[allow(clippy::cast_possible_truncation)]
        sleep_ms: (sleep * 1000.0) as i64,
        iter: iter.unwrap_or(0),
        min_rows: min_rows.unwrap_or(0),
    })
}

/// `strftime("%c")` in the C locale — `%a %b %e %H:%M:%S %Y` — for a time
/// `unix_secs` after the epoch, in UTC.
#[must_use]
pub fn c_locale_ctime(unix_secs: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    // 1970-01-01 was a Thursday.
    let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{} {} {day:>2} {:02}:{:02}:{:02} {year}",
        DAYS[weekday],
        MONTHS[usize::try_from(month - 1).unwrap_or(0)],
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`, public domain).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The title of one run (`command.c:5987`-`:5999`), newline included: "this
/// is somewhat historical but it makes for reasonably nicely formatted
/// output in simple cases".
#[must_use]
pub fn watch_title(user_title: Option<&str>, timebuf: &str, sleep_ms: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let every = format_g(sleep_ms as f64 / 1000.0);
    match user_title {
        Some(user_title) => format!("{user_title}\t{timebuf} (every {every}s)\n"),
        None => format!("{timebuf} (every {every}s)\n"),
    }
}

/// The `ITIMER_REAL` timer `do_watch` sets (`command.c:5924`): it fires
/// every `period` from the start, whether or not a query is running. A tick
/// that fired while the query ran stays pending — one, however many passed —
/// and the next `sigwait` takes it at once.
///
/// Given how long since the start and how many ticks have been taken,
/// returns the ticks taken once this wait is over and how long to wait.
#[must_use]
pub fn next_tick(elapsed: Duration, period: Duration, taken: u128) -> (u128, Duration) {
    let fired = elapsed.as_nanos() / period.as_nanos().max(1);
    if fired > taken {
        return (fired, Duration::ZERO);
    }
    let due = period.as_nanos() * (taken + 1);
    let wait = u64::try_from(due - elapsed.as_nanos()).unwrap_or(u64::MAX);
    (taken + 1, Duration::from_nanos(wait))
}

/// `do_watch()` (`command.c:5873`): run `query` until the count is used
/// up, a run returns fewer than `min_rows` rows, or a run fails, sleeping
/// `sleep_ms` between runs. Returns `res >= 0`: an error ends the loop and
/// fails the command, a short result ends it and does not.
pub fn do_watch(
    query: &[u8],
    args: WatchArgs,
    executor: &mut dyn Executor,
    session: &mut Session<'_>,
    source: &mut CommandSource<'_>,
    out: &mut Output<'_>,
    stderr: &mut dyn Write,
) -> bool {
    if query.is_empty() {
        logging::error(
            session.pset,
            "\\watch cannot be used with an empty query",
            stderr,
        );
        return false;
    }

    // No pager: `myopt.topt.pager = 0` (`command.c:5969`).
    let mut myopt = session.pset.popt.clone();
    myopt.topt.pager = Pager::Off;
    let user_title = myopt.title.take();
    let period = Duration::from_millis(u64::try_from(args.sleep_ms).unwrap_or(0));
    let start = Instant::now();
    let mut taken = 0;
    let mut iter = args.iter;

    let res = loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        myopt.title = Some(watch_title(
            user_title.as_deref(),
            &c_locale_ctime(now),
            args.sleep_ms,
        ));

        let res = psql_exec_watch(
            executor,
            query,
            &myopt,
            args.min_rows,
            session,
            source,
            out,
            stderr,
        );
        // `psql_exec_watch` handles the case where the query cannot be
        // repeated, and returns 0 or -1.
        if res <= 0 {
            break res;
        }
        // If we have an iteration count, check that it's not exceeded yet.
        if iter != 0 {
            iter -= 1;
            if iter <= 0 {
                break res;
            }
        }
        // Tight loop, no wait needed.
        if args.sleep_ms == 0 {
            continue;
        }
        out.flush_all();
        let (now_taken, wait) = next_tick(start.elapsed(), period, taken);
        taken = now_taken;
        std::thread::sleep(wait);
    };

    // `command.c:6093`: a newline, so the next prompt starts on a line of
    // its own after a `^C` the terminal echoed.
    let _ = out.stdout.write_all(b"\n");
    let _ = out.stdout.flush();
    res >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<WatchArgs, String> {
        parse_watch_args(args, 2.0)
    }

    fn args(sleep_ms: i64, iter: i32, min_rows: i32) -> WatchArgs {
        WatchArgs {
            sleep_ms,
            iter,
            min_rows,
        }
    }

    #[test]
    fn arguments_default_to_watch_interval_and_no_limits() {
        assert_eq!(parse(&[]), Ok(args(2000, 0, 0)));
        assert_eq!(parse_watch_args(&[], 0.0001), Ok(args(0, 0, 0)));
        assert_eq!(parse(&["c=3", "i=0.01"]), Ok(args(10, 3, 0)));
        assert_eq!(
            parse(&["0.5", "count=2", "min_rows=4"]),
            Ok(args(500, 2, 4))
        );
        assert_eq!(parse(&["interval=0x10", "m=1"]), Ok(args(16_000, 0, 1)));
        assert_eq!(parse(&["0.0001"]), Ok(args(0, 0, 0)));
        assert_eq!(parse(&["1.9999"]), Ok(args(1999, 0, 0)));
    }

    #[test]
    fn a_bad_argument_draws_upstreams_message() {
        // `001_basic.pl:387`-`:423`, then the ones it does not reach.
        for (args, message) in [
            (&["m=x"][..], "\\watch: incorrect minimum row count \"x\""),
            (
                &["m=1", "min_rows=2"],
                "\\watch: minimum row count specified more than once",
            ),
            (&["-10"], "\\watch: incorrect interval value \"-10\""),
            (&["10ab"], "\\watch: incorrect interval value \"10ab\""),
            (&["10e400"], "\\watch: incorrect interval value \"10e400\""),
            (
                &["1", "1"],
                "\\watch: interval value is specified more than once",
            ),
            (
                &["c=1", "c=1"],
                "\\watch: iteration count is specified more than once",
            ),
            (
                &["i=1", "2"],
                "\\watch: interval value is specified more than once",
            ),
            (&["c=0"], "\\watch: incorrect iteration count \"0\""),
            (&["count="], "\\watch: incorrect iteration count \"\""),
            (
                &["c=2147483648"],
                "\\watch: incorrect iteration count \"2147483648\"",
            ),
            (&["m=-1"], "\\watch: incorrect minimum row count \"-1\""),
            (&["i=1=2"], "\\watch: incorrect interval value \"1=2\""),
            (&["x=1"], "\\watch: unrecognized parameter \"x=1\""),
            (&["ci=1"], "\\watch: unrecognized parameter \"ci=1\""),
        ] {
            assert_eq!(parse(args), Err(message.to_string()), "{args:?}");
        }
        // `strtod` reads nothing from an empty value and stops at its end,
        // which is all C asks: the interval is 0.
        assert_eq!(parse(&[""]), Ok(args(0, 0, 0)));
        assert_eq!(parse(&["i="]), Ok(args(0, 0, 0)));
        assert_eq!(
            parse(&[" "]),
            Err("\\watch: incorrect interval value \" \"".to_string())
        );
        // The first bad argument is the one reported.
        assert_eq!(
            parse(&["c=x", "m=y"]),
            Err("\\watch: incorrect iteration count \"x\"".to_string())
        );
    }

    #[test]
    fn the_time_is_c_locale_percent_c() {
        assert_eq!(c_locale_ctime(0), "Thu Jan  1 00:00:00 1970");
        assert_eq!(c_locale_ctime(1_790_000_000), "Mon Sep 21 14:13:20 2026");
        assert_eq!(c_locale_ctime(951_782_400), "Tue Feb 29 00:00:00 2000");
        assert_eq!(c_locale_ctime(-1), "Wed Dec 31 23:59:59 1969");
    }

    #[test]
    fn the_title_carries_the_time_and_the_interval_in_percent_g() {
        let t = "Thu Jan  1 00:00:00 1970";
        assert_eq!(
            watch_title(None, t, 2000),
            "Thu Jan  1 00:00:00 1970 (every 2s)\n"
        );
        assert_eq!(
            watch_title(Some("mine"), t, 10),
            "mine\tThu Jan  1 00:00:00 1970 (every 0.01s)\n"
        );
        assert_eq!(
            watch_title(None, t, 1_000_000_000),
            "Thu Jan  1 00:00:00 1970 (every 1e+06s)\n"
        );
    }

    #[test]
    fn the_timer_keeps_its_period_and_a_missed_tick_runs_at_once() {
        let ms = Duration::from_millis;
        // A fast query waits out the rest of the period.
        assert_eq!(next_tick(ms(10), ms(100), 0), (1, ms(90)));
        // The next wait is measured from the start, not from the query.
        assert_eq!(next_tick(ms(130), ms(100), 1), (2, ms(70)));
        // A query slower than the period: the tick is pending, run at once.
        assert_eq!(next_tick(ms(150), ms(100), 0), (1, ms(0)));
        // Several missed ticks are one pending signal.
        assert_eq!(next_tick(ms(450), ms(100), 1), (4, ms(0)));
        assert_eq!(next_tick(ms(460), ms(100), 4), (5, ms(40)));
    }
}
