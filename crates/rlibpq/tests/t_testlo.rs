//! The large object interface against a real PostgreSQL 18 server:
//! `src/test/examples/testlo.c` and `testlo64.c`, ported.
//!
//! Upstream builds both programs (`src/test/examples/Makefile:17`) but checks
//! no expected output, so each port here runs the program as upstream wrote
//! it — its stdout and stderr captured, the same `printf`s in the same order
//! — and then proves what it did three ways:
//!
//! - the stdout and stderr are exactly the text the C program prints for this
//!   input;
//! - the server's own view of the large object (`lo_get`, and the
//!   server-side `lo_export`) agrees byte for byte with the file rlibpq
//!   exported and with the edit the program made;
//! - C libpq, driven through the reference `psql` (`\lo_import`,
//!   `\lo_export`, which call `lo_import` / `lo_export`,
//!   `src/bin/psql/large_obj.c:187`, `:151`), imports the same file to the
//!   same bytes and exports rlibpq's object to the same file.
//!
//! One substitution, in `testlo64`: its two `lo_export`s would each write a
//! file of over 3 GiB (the object is sparse at 4294967000), so they are
//! replaced by the server's `lo_get` of the regions the program touched and
//! `lo_lseek64(…, SEEK_END)` for the size. Everything else runs as written.
//!
//! The helpers `importFile` and `exportFile`, commented out of both `main`s
//! in favour of `lo_import` / `lo_export`, are ported too and run after
//! `main`, so `lo_creat`, `lo_write`, `lo_read` and friends are driven the
//! way the example drives them.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

// `lobj_id` and `lobj_fd` are the example's own names (`lobjId`, `lobj_fd`).
#![allow(clippy::doc_markdown, clippy::similar_names)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use rlibpq::lobj::{INV_READ, INV_WRITE, SEEK_END, SEEK_SET};
use rlibpq::{Connection, ExecStatus};

mod common;

use common::{Cluster, only};

/// `BUFSIZE`, `testlo.c:26`, `testlo64.c:27`.
const BUFSIZE: usize = 1024;

/// What the C program would have written to its two streams.
#[derive(Default)]
struct Output {
    stdout: String,
    stderr: Vec<u8>,
}

/// `importFile`, `testlo.c:34`: `lo_creat`, `lo_open`, then `lo_write` of
/// each 1 kB read.
fn import_file(conn: &mut Connection, out: &mut Output, filename: &Path) -> u32 {
    let data = std::fs::read(filename).unwrap_or_else(|_| {
        let _ = writeln!(
            out.stderr,
            "cannot open unix file\"{}\"",
            filename.display()
        );
        Vec::new()
    });
    let lobj_id = conn.lo_creat(INV_READ | INV_WRITE).unwrap_or(0);
    if lobj_id == 0 {
        out.stderr.extend_from_slice(b"cannot create large object");
    }
    let lobj_fd = conn.lo_open(lobj_id, INV_WRITE).unwrap_or(-1);
    for chunk in data.chunks(BUFSIZE) {
        let tmp = conn.lo_write(lobj_fd, chunk).unwrap_or(-1);
        if usize::try_from(tmp).map_or(true, |tmp| tmp < chunk.len()) {
            let _ = write!(out.stderr, "error while reading \"{}\"", filename.display());
        }
    }
    let _ = conn.lo_close(lobj_fd);
    lobj_id
}

/// `pickout`, `testlo.c:78` and `testlo64.c:79`: seek to `start`, then
/// `lo_read` until `len` bytes, each printed as a C string.
fn pickout(conn: &mut Connection, out: &mut Output, lobj_id: u32, start: i64, len: usize) {
    let lobj_fd = conn.lo_open(lobj_id, INV_READ).unwrap_or(-1);
    if lobj_fd < 0 {
        let _ = write!(out.stderr, "cannot open large object {lobj_id}");
    }
    // testlo64.c:90 — testlo.c:89 is the same call through lo_lseek, whose
    // error it ignores.
    if let Err(err) = conn.lo_lseek64(lobj_fd, start, SEEK_SET) {
        out.stderr.extend_from_slice(b"error in lo_lseek64: ");
        out.stderr.extend_from_slice(&err.message());
    }
    if conn.lo_tell64(lobj_fd).ok() != Some(start) {
        out.stderr.extend_from_slice(b"error in lo_tell64: ");
    }
    let mut buf = vec![0u8; len];
    let mut nread = 0;
    while len - nread > 0 {
        let nbytes = conn.lo_read(lobj_fd, &mut buf[..len - nread]).unwrap_or(0);
        // fprintf(stderr, ">>> %s", buf): the bytes up to the first NUL.
        out.stderr.extend_from_slice(b">>> ");
        let shown = buf[..nbytes].split(|&b| b == 0).next().unwrap_or_default();
        out.stderr.extend_from_slice(shown);
        nread += nbytes;
        if nbytes == 0 {
            break;
        }
    }
    out.stderr.push(b'\n');
    let _ = conn.lo_close(lobj_fd);
}

/// `overwrite`, `testlo.c:108` and `testlo64.c:114`: `len` X's at `start`.
fn overwrite(conn: &mut Connection, out: &mut Output, lobj_id: u32, start: i64, len: usize) {
    let lobj_fd = conn.lo_open(lobj_id, INV_WRITE).unwrap_or(-1);
    if lobj_fd < 0 {
        let _ = write!(out.stderr, "cannot open large object {lobj_id}");
    }
    if let Err(err) = conn.lo_lseek64(lobj_fd, start, SEEK_SET) {
        out.stderr.extend_from_slice(b"error in lo_lseek64: ");
        out.stderr.extend_from_slice(&err.message());
    }
    let buf = vec![b'X'; len];
    let mut nwritten = 0;
    while len - nwritten > 0 {
        let nbytes = conn.lo_write(lobj_fd, &buf[nwritten..]).unwrap_or(-1);
        if nbytes <= 0 {
            out.stderr.extend_from_slice(b"\nWRITE FAILED!\n");
            break;
        }
        nwritten += usize::try_from(nbytes).expect("positive here");
    }
    out.stderr.push(b'\n');
    let _ = conn.lo_close(lobj_fd);
}

/// `my_truncate`, `testlo64.c:152`.
fn my_truncate(conn: &mut Connection, out: &mut Output, lobj_id: u32, len: i64) {
    let lobj_fd = conn.lo_open(lobj_id, INV_READ | INV_WRITE).unwrap_or(-1);
    if lobj_fd < 0 {
        let _ = write!(out.stderr, "cannot open large object {lobj_id}");
    }
    if let Err(err) = conn.lo_truncate64(lobj_fd, len) {
        out.stderr.extend_from_slice(b"error in lo_truncate64: ");
        out.stderr.extend_from_slice(&err.message());
    }
    let _ = conn.lo_close(lobj_fd);
}

/// `exportFile`, `testlo.c:150`: `lo_read` 1 kB at a time into the file.
fn export_file(conn: &mut Connection, out: &mut Output, lobj_id: u32, filename: &Path) {
    let lobj_fd = conn.lo_open(lobj_id, INV_READ).unwrap_or(-1);
    if lobj_fd < 0 {
        let _ = write!(out.stderr, "cannot open large object {lobj_id}");
    }
    let mut data = Vec::new();
    let mut buf = [0u8; BUFSIZE];
    loop {
        match conn.lo_read(lobj_fd, &mut buf) {
            Ok(nbytes) if nbytes > 0 => data.extend_from_slice(&buf[..nbytes]),
            _ => break,
        }
    }
    let _ = conn.lo_close(lobj_fd);
    std::fs::write(filename, data).expect("the export file is written");
}

/// `main`'s prologue, `testlo.c:233`-`:243`: an always-secure search path,
/// then `begin`.
fn begin(conn: &mut Connection) {
    let res = only(
        conn.exec(b"SELECT pg_catalog.set_config('search_path', '', false)")
            .expect("PQexec"),
    );
    assert_eq!(res.status(), ExecStatus::TuplesOk, "SET failed: {res:?}");
    conn.exec(b"begin").expect("PQexec");
}

/// `main`'s `lo_import` step, `testlo.c:245`-`:252`.
fn import(conn: &mut Connection, out: &mut Output, in_filename: &Path) -> u32 {
    let _ = writeln!(
        out.stdout,
        "importing file \"{}\" ...",
        in_filename.display()
    );
    match conn.lo_import(in_filename) {
        Ok(oid) => {
            let _ = writeln!(out.stdout, "\tas large object {oid}.");
            oid
        }
        Err(err) => {
            out.stderr.extend_from_slice(&err.message());
            out.stderr.push(b'\n');
            0
        }
    }
}

/// `main`'s `lo_export` step, `testlo.c:260`-`:263`.
fn export(conn: &mut Connection, out: &mut Output, lobj_oid: u32, out_filename: &Path) {
    let _ = writeln!(
        out.stdout,
        "exporting large object to file \"{}\" ...",
        out_filename.display()
    );
    if let Err(err) = conn.lo_export(lobj_oid, out_filename) {
        out.stderr.extend_from_slice(&err.message());
        out.stderr.push(b'\n');
    }
}

/// A text file several `LO_BUFSIZE`s long, without a NUL, so `pickout`'s
/// `%s` prints every byte it read.
fn input_text() -> Vec<u8> {
    let mut text = String::new();
    for line in 0..600 {
        let _ = writeln!(
            text,
            "line {line:05}: the quick brown fox jumps over the lazy dog"
        );
    }
    text.into_bytes()
}

/// The server's `lo_get(oid, offset, len)` (`be-fsstubs.c`), as bytes.
fn server_lo_get(conn: &mut Connection, oid: u32, offset: i64, len: usize) -> Vec<u8> {
    let query = format!("select lo_get({oid}, {offset}, {len})");
    let res = only(conn.exec(query.as_bytes()).expect("PQexec"));
    assert_eq!(res.status(), ExecStatus::TuplesOk, "{res:?}");
    let hex = res.value(0, 0).expect("a value");
    rlibpq::unescape_bytea(hex)
}

/// The object's size, by `lo_lseek64(…, SEEK_END)`.
fn size64(conn: &mut Connection, oid: u32) -> i64 {
    let fd = conn.lo_open(oid, INV_READ).expect("lo_open");
    let size = conn.lo_lseek64(fd, 0, SEEK_END).expect("lo_lseek64");
    conn.lo_close(fd).expect("lo_close");
    size
}

/// `testlo.c`'s `main`, then the gates described in the module comment.
#[test]
fn testlo() {
    let Some(cluster) = Cluster::start("trust", 55_530) else {
        return;
    };
    let in_filename = cluster.dir.join("testlo.in");
    let out_filename = cluster.dir.join("testlo.out");
    let input = input_text();
    std::fs::write(&in_filename, &input).expect("the input file is written");

    let mut conn = cluster.connect();
    let mut out = Output::default();
    begin(&mut conn);
    let lobj_oid = import(&mut conn, &mut out, &in_filename);
    assert_ne!(lobj_oid, 0, "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
        .push_str("picking out bytes 1000-2000 of the large object\n");
    pickout(&mut conn, &mut out, lobj_oid, 1000, 1000);
    out.stdout
        .push_str("overwriting bytes 1000-2000 of the large object with X's\n");
    overwrite(&mut conn, &mut out, lobj_oid, 1000, 1000);
    export(&mut conn, &mut out, lobj_oid, &out_filename);

    // The two streams, exactly as the C program prints them.
    let expected_stdout = format!(
        "importing file \"{in}\" ...\n\tas large object {lobj_oid}.\n\
         picking out bytes 1000-2000 of the large object\n\
         overwriting bytes 1000-2000 of the large object with X's\n\
         exporting large object to file \"{out}\" ...\n",
        in = in_filename.display(),
        out = out_filename.display(),
    );
    assert_eq!(out.stdout, expected_stdout);
    let mut expected_stderr = b">>> ".to_vec();
    expected_stderr.extend_from_slice(&input[1000..2000]);
    expected_stderr.extend_from_slice(b"\n\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&expected_stderr)
    );

    // The edit, from the server's side and in the exported file.
    let mut edited = input.clone();
    edited[1000..2000].fill(b'X');
    assert_eq!(
        server_lo_get(&mut conn, lobj_oid, 0, input.len() + 10),
        edited
    );
    let exported = std::fs::read(&out_filename).expect("rlibpq's export");
    assert_eq!(exported, edited);
    conn.exec(b"end").expect("PQexec");

    // The server-side lo_export writes the same bytes.
    let server_file = cluster.dir.join("testlo.server");
    let query = format!("select lo_export({lobj_oid}, '{}')", server_file.display());
    assert_eq!(
        only(conn.exec(query.as_bytes()).unwrap()).status(),
        ExecStatus::TuplesOk
    );
    assert_eq!(
        std::fs::read(&server_file).expect("the server's export"),
        exported
    );

    // importFile and exportFile, commented out of main: the same bytes by
    // lo_creat / lo_write and lo_read.
    let mut helper = Output::default();
    begin(&mut conn);
    let by_helper = import_file(&mut conn, &mut helper, &in_filename);
    let helper_file = cluster.dir.join("testlo.helper");
    export_file(&mut conn, &mut helper, by_helper, &helper_file);
    conn.exec(b"end").expect("PQexec");
    assert!(helper.stdout.is_empty() && helper.stderr.is_empty());
    assert_eq!(std::fs::read(&helper_file).unwrap(), input);
    assert_eq!(server_lo_get(&mut conn, by_helper, 0, input.len()), input);

    // C libpq, through the reference psql: its lo_import of the same file
    // holds the same bytes, and its lo_export of rlibpq's object writes the
    // same file.
    let (stdout, stderr, code) = cluster.psql_script(&format!(
        "\\lo_import '{}'\n\\echo :LASTOID\n",
        in_filename.display()
    ));
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let c_oid: u32 = String::from_utf8(stdout).unwrap().trim().parse().unwrap();
    assert_eq!(server_lo_get(&mut conn, c_oid, 0, input.len() + 10), input);
    let c_file = cluster.dir.join("testlo.c-libpq");
    let (_, stderr, code) =
        cluster.psql_script(&format!("\\lo_export {lobj_oid} '{}'\n", c_file.display()));
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(std::fs::read(&c_file).expect("C libpq's export"), exported);
}

/// `testlo64.c`'s `main`, with its two 3 GiB exports replaced as the module
/// comment says.
#[test]
fn testlo64() {
    const START: i64 = 4_294_967_000;
    const TRUNCATED: i64 = 3_294_968_000;
    let Some(cluster) = Cluster::start("trust", 55_531) else {
        return;
    };
    let in_filename = cluster.dir.join("testlo64.in");
    let input = input_text();
    std::fs::write(&in_filename, &input).expect("the input file is written");

    let mut conn = cluster.connect();
    let mut out = Output::default();
    begin(&mut conn);
    let lobj_oid = import(&mut conn, &mut out, &in_filename);
    assert_ne!(lobj_oid, 0, "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
        .push_str("picking out bytes 4294967000-4294968000 of the large object\n");
    pickout(&mut conn, &mut out, lobj_oid, START, 1000);
    out.stdout
        .push_str("overwriting bytes 4294967000-4294968000 of the large object with X's\n");
    overwrite(&mut conn, &mut out, lobj_oid, START, 1000);

    // In place of the first lo_export: the object now ends after the X's,
    // which are where the program wrote them, after a hole of zeros.
    assert_eq!(size64(&mut conn, lobj_oid), START + 1000);
    assert_eq!(server_lo_get(&mut conn, lobj_oid, 0, input.len()), input);
    assert_eq!(
        server_lo_get(&mut conn, lobj_oid, START - 8, 1008),
        [vec![0u8; 8], vec![b'X'; 1000]].concat()
    );

    out.stdout.push_str("truncating to 3294968000 bytes\n");
    my_truncate(&mut conn, &mut out, lobj_oid, TRUNCATED);

    // In place of the second.
    assert_eq!(size64(&mut conn, lobj_oid), TRUNCATED);
    assert_eq!(server_lo_get(&mut conn, lobj_oid, 0, input.len()), input);
    conn.exec(b"end").expect("PQexec");

    assert_eq!(
        out.stdout,
        format!(
            "importing file \"{}\" ...\n\tas large object {lobj_oid}.\n\
             picking out bytes 4294967000-4294968000 of the large object\n\
             overwriting bytes 4294967000-4294968000 of the large object with X's\n\
             truncating to 3294968000 bytes\n",
            in_filename.display()
        )
    );
    // pickout reads nothing past the end, and overwrite prints its newline.
    assert_eq!(String::from_utf8_lossy(&out.stderr), ">>> \n\n");
}

/// The failures a caller sees in `PQerrorMessage`: the server's own
/// refusals, and the client-side messages of `fe-lobj.c`.
#[test]
fn lo_failures_leave_upstreams_messages() {
    let Some(cluster) = Cluster::start("trust", 55_532) else {
        return;
    };
    let mut conn = cluster.connect();

    // fe-lobj.c:666 — the file is opened before anything is sent.
    let missing = cluster.dir.join("no-such-file");
    let err = conn.lo_import(&missing).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        format!(
            "could not open file \"{}\": No such file or directory\n",
            missing.display()
        )
    );

    // The server's refusal is the FATAL_ERROR result PQfn returned.
    conn.exec(b"begin").unwrap();
    let err = conn.lo_open(42, INV_READ).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        "ERROR:  large object 42 does not exist\n"
    );
    // With the transaction aborted, the next call is refused by the server
    // too — the function OIDs are already known, so no query is sent.
    let err = conn.lo_creat(INV_READ | INV_WRITE).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        "ERROR:  current transaction is aborted, commands ignored until end of transaction block\n"
    );
    conn.exec(b"rollback").unwrap();

    // A fresh connection whose first lo_* call meets an aborted transaction:
    // lo_initialize's query fails, and its error comes first (fe-lobj.c:904).
    let mut fresh = cluster.connect();
    fresh.exec(b"begin").unwrap();
    fresh.exec(b"select 1/0").unwrap();
    let err = fresh.lo_unlink(42).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        "ERROR:  current transaction is aborted, commands ignored until end of transaction block\n\
         query to initialize large object functions did not return data\n"
    );
    fresh.exec(b"rollback").unwrap();

    // lo_export to a directory that does not exist: the object is closed,
    // and the file's error is what is left (fe-lobj.c:780).
    let oid = conn.lo_create(0).unwrap();
    assert_ne!(oid, 0);
    let unwritable = cluster.dir.join("no-such-dir").join("x");
    conn.exec(b"begin").unwrap();
    let err = conn.lo_export(oid, &unwritable).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        format!(
            "could not open file \"{}\": No such file or directory\n",
            unwritable.display()
        )
    );
    conn.exec(b"end").unwrap();

    // lo_import_with_oid into an OID that is taken.
    let in_filename = cluster.dir.join("with-oid.in");
    std::fs::write(&in_filename, b"abc").unwrap();
    conn.exec(b"begin").unwrap();
    let err = conn.lo_import_with_oid(&in_filename, oid).unwrap_err();
    conn.exec(b"rollback").unwrap();
    assert_eq!(
        String::from_utf8_lossy(&err.message()),
        format!(
            "ERROR:  duplicate key value violates unique constraint \"pg_largeobject_metadata_oid_index\"\nDETAIL:  Key (oid)=({oid}) already exists.\n"
        )
    );
    assert_eq!(conn.lo_unlink(oid).unwrap(), 1);
    conn.exec(b"begin").unwrap();
    assert_eq!(conn.lo_import_with_oid(&in_filename, oid).unwrap(), oid);
    conn.exec(b"end").unwrap();
    assert_eq!(server_lo_get(&mut conn, oid, 0, 10), b"abc");
}
