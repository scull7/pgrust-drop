# libpq C ABI coverage

<!-- Generated; do not edit. Regenerate with
     cargo run -p rlibpq-ffi --example libpq_abi_md > docs/libpq-abi.md
     `rlibpq-ffi`'s abi::tests fail when this file drifts from its source. -->

Every symbol PostgreSQL 18.6's libpq exports, from
`src/interfaces/libpq/exports.txt` at `REL_18_6` (vendored unmodified as
`crates/rlibpq/ffi/upstream/exports.txt`), and how far `rlibpq-ffi`
covers it:

- **implemented**: exported, and answers what C libpq answers;
- **stubbed with an error**: exported so a program links, but reports the
  call as unsupported;
- **not yet**: not exported, so a program calling it does not link.

The library is `libpq.a` (crate `rlibpq-ffi`, library name `pq`). There is no
`libpq.so` yet: rustup's musl target links the C runtime statically and drops
a `cdylib` crate type, and musl is the lane CI gates first (ADR-0007).

"Follows" is the C definition at `REL_18_6`, relative to
`src/interfaces/libpq/`. Where C picks an arm by build configuration, the arm
is the one a libpq built without SSL, OpenSSL or GSSAPI takes, because
`rlibpq` has none of the three yet.

54 of 210 symbols implemented, 0 stubbed with an error, 156 not yet.

| ordinal | symbol | coverage | follows |
|--:|---|---|---|
| 1 | `PQconnectdb` | implemented | fe-connect.c:820 |
| 2 | `PQsetdbLogin` | not yet |  |
| 3 | `PQconndefaults` | implemented | fe-connect.c:2193 |
| 4 | `PQfinish` | implemented | fe-connect.c:5301 |
| 5 | `PQreset` | not yet |  |
| 6 | `PQrequestCancel` | not yet |  |
| 7 | `PQdb` | not yet |  |
| 8 | `PQuser` | not yet |  |
| 9 | `PQpass` | not yet |  |
| 10 | `PQhost` | not yet |  |
| 11 | `PQport` | not yet |  |
| 12 | `PQtty` | not yet |  |
| 13 | `PQoptions` | not yet |  |
| 14 | `PQstatus` | implemented | fe-connect.c:7575 |
| 15 | `PQerrorMessage` | implemented | fe-connect.c:7638 |
| 16 | `PQsocket` | not yet |  |
| 17 | `PQbackendPID` | not yet |  |
| 18 | `PQtrace` | not yet |  |
| 19 | `PQuntrace` | not yet |  |
| 20 | `PQsetNoticeProcessor` | not yet |  |
| 21 | `PQexec` | implemented | fe-exec.c:2279 |
| 22 | `PQnotifies` | not yet |  |
| 23 | `PQsendQuery` | not yet |  |
| 24 | `PQgetResult` | not yet |  |
| 25 | `PQisBusy` | not yet |  |
| 26 | `PQconsumeInput` | not yet |  |
| 27 | `PQgetline` | not yet |  |
| 28 | `PQputline` | not yet |  |
| 29 | `PQgetlineAsync` | not yet |  |
| 30 | `PQputnbytes` | not yet |  |
| 31 | `PQendcopy` | not yet |  |
| 32 | `PQfn` | not yet |  |
| 33 | `PQresultStatus` | implemented | fe-exec.c:3442 |
| 34 | `PQntuples` | implemented | fe-exec.c:3512 |
| 35 | `PQnfields` | implemented | fe-exec.c:3520 |
| 36 | `PQbinaryTuples` | implemented | fe-exec.c:3528 |
| 37 | `PQfname` | implemented | fe-exec.c:3598 |
| 38 | `PQfnumber` | implemented | fe-exec.c:3620 |
| 39 | `PQftype` | implemented | fe-exec.c:3750 |
| 40 | `PQfsize` | implemented | fe-exec.c:3761 |
| 41 | `PQfmod` | implemented | fe-exec.c:3772 |
| 42 | `PQcmdStatus` | implemented | fe-exec.c:3783 |
| 43 | `PQoidStatus` | implemented | fe-exec.c:3796 |
| 44 | `PQcmdTuples` | implemented | fe-exec.c:3853 |
| 45 | `PQgetvalue` | implemented | fe-exec.c:3907 |
| 46 | `PQgetlength` | implemented | fe-exec.c:3918 |
| 47 | `PQgetisnull` | implemented | fe-exec.c:3932 |
| 48 | `PQclear` | implemented | fe-exec.c:727 |
| 49 | `PQmakeEmptyPGresult` | not yet |  |
| 50 | `PQprint` | not yet |  |
| 51 | `PQdisplayTuples` | not yet |  |
| 52 | `PQprintTuples` | not yet |  |
| 53 | `lo_open` | not yet |  |
| 54 | `lo_close` | not yet |  |
| 55 | `lo_read` | not yet |  |
| 56 | `lo_write` | not yet |  |
| 57 | `lo_lseek` | not yet |  |
| 58 | `lo_creat` | not yet |  |
| 59 | `lo_tell` | not yet |  |
| 60 | `lo_unlink` | not yet |  |
| 61 | `lo_import` | not yet |  |
| 62 | `lo_export` | not yet |  |
| 63 | `pgresStatus` | not yet |  |
| 64 | `PQmblen` | not yet |  |
| 65 | `PQresultErrorMessage` | implemented | fe-exec.c:3458 |
| 66 | `PQresStatus` | implemented | fe-exec.c:3450 |
| 67 | `termPQExpBuffer` | not yet |  |
| 68 | `appendPQExpBufferChar` | not yet |  |
| 69 | `initPQExpBuffer` | not yet |  |
| 70 | `resetPQExpBuffer` | not yet |  |
| 71 | `PQoidValue` | implemented | fe-exec.c:3824 |
| 72 | `PQclientEncoding` | not yet |  |
| 73 | `PQenv2encoding` | not yet |  |
| 74 | `appendBinaryPQExpBuffer` | not yet |  |
| 75 | `appendPQExpBufferStr` | not yet |  |
| 76 | `destroyPQExpBuffer` | not yet |  |
| 77 | `createPQExpBuffer` | not yet |  |
| 78 | `PQconninfoFree` | implemented | fe-connect.c:7459 |
| 79 | `PQconnectPoll` | not yet |  |
| 80 | `PQconnectStart` | not yet |  |
| 81 | `PQflush` | not yet |  |
| 82 | `PQisnonblocking` | not yet |  |
| 83 | `PQresetPoll` | not yet |  |
| 84 | `PQresetStart` | not yet |  |
| 85 | `PQsetClientEncoding` | not yet |  |
| 86 | `PQsetnonblocking` | not yet |  |
| 87 | `PQfreeNotify` | implemented | fe-exec.c:4080 |
| 88 | `PQescapeString` | not yet |  |
| 89 | `PQescapeBytea` | not yet |  |
| 90 | `printfPQExpBuffer` | not yet |  |
| 91 | `appendPQExpBuffer` | not yet |  |
| 92 | `pg_encoding_to_char` | not yet |  |
| 93 | `pg_utf_mblen` | not yet |  |
| 94 | `PQunescapeBytea` | not yet |  |
| 95 | `PQfreemem` | implemented | fe-exec.c:4063 |
| 96 | `PQtransactionStatus` | not yet |  |
| 97 | `PQparameterStatus` | not yet |  |
| 98 | `PQprotocolVersion` | not yet |  |
| 99 | `PQsetErrorVerbosity` | not yet |  |
| 100 | `PQsetNoticeReceiver` | not yet |  |
| 101 | `PQexecParams` | implemented | fe-exec.c:2293 |
| 102 | `PQsendQueryParams` | not yet |  |
| 103 | `PQputCopyData` | not yet |  |
| 104 | `PQputCopyEnd` | not yet |  |
| 105 | `PQgetCopyData` | not yet |  |
| 106 | `PQresultErrorField` | implemented | fe-exec.c:3497 |
| 107 | `PQftable` | implemented | fe-exec.c:3717 |
| 108 | `PQftablecol` | implemented | fe-exec.c:3728 |
| 109 | `PQfformat` | implemented | fe-exec.c:3739 |
| 110 | `PQexecPrepared` | implemented | fe-exec.c:2340 |
| 111 | `PQsendQueryPrepared` | not yet |  |
| 112 | `PQdsplen` | not yet |  |
| 113 | `PQserverVersion` | not yet |  |
| 114 | `PQgetssl` | implemented | fe-secure.c:452, without SSL |
| 115 | `pg_char_to_encoding` | not yet |  |
| 116 | `pg_valid_server_encoding` | not yet |  |
| 117 | `pqsignal` | not yet |  |
| 118 | `PQprepare` | implemented | fe-exec.c:2323 |
| 119 | `PQsendPrepare` | not yet |  |
| 120 | `PQgetCancel` | not yet |  |
| 121 | `PQfreeCancel` | not yet |  |
| 122 | `PQcancel` | not yet |  |
| 123 | `lo_create` | not yet |  |
| 124 | `PQinitSSL` | implemented | fe-secure.c:117 |
| 125 | `PQregisterThreadLock` | not yet |  |
| 126 | `PQescapeStringConn` | not yet |  |
| 127 | `PQescapeByteaConn` | not yet |  |
| 128 | `PQencryptPassword` | not yet |  |
| 129 | `PQisthreadsafe` | implemented | fe-exec.c:4023 |
| 130 | `enlargePQExpBuffer` | not yet |  |
| 131 | `PQnparams` | implemented | fe-exec.c:3946 |
| 132 | `PQparamtype` | implemented | fe-exec.c:3957 |
| 133 | `PQdescribePrepared` | implemented | fe-exec.c:2472 |
| 134 | `PQdescribePortal` | implemented | fe-exec.c:2491 |
| 135 | `PQsendDescribePrepared` | not yet |  |
| 136 | `PQsendDescribePortal` | not yet |  |
| 137 | `lo_truncate` | not yet |  |
| 138 | `PQconnectionUsedPassword` | not yet |  |
| 139 | `pg_valid_server_encoding_id` | not yet |  |
| 140 | `PQconnectionNeedsPassword` | not yet |  |
| 141 | `lo_import_with_oid` | not yet |  |
| 142 | `PQcopyResult` | not yet |  |
| 143 | `PQsetResultAttrs` | not yet |  |
| 144 | `PQsetvalue` | not yet |  |
| 145 | `PQresultAlloc` | not yet |  |
| 146 | `PQregisterEventProc` | not yet |  |
| 147 | `PQinstanceData` | not yet |  |
| 148 | `PQsetInstanceData` | not yet |  |
| 149 | `PQresultInstanceData` | not yet |  |
| 150 | `PQresultSetInstanceData` | not yet |  |
| 151 | `PQfireResultCreateEvents` | not yet |  |
| 152 | `PQconninfoParse` | implemented | fe-connect.c:6175 |
| 153 | `PQinitOpenSSL` | implemented | fe-secure.c:129 |
| 154 | `PQescapeLiteral` | not yet |  |
| 155 | `PQescapeIdentifier` | not yet |  |
| 156 | `PQconnectdbParams` | not yet |  |
| 157 | `PQconnectStartParams` | not yet |  |
| 158 | `PQping` | not yet |  |
| 159 | `PQpingParams` | not yet |  |
| 160 | `PQlibVersion` | implemented | fe-misc.c:65 |
| 161 | `PQsetSingleRowMode` | not yet |  |
| 162 | `lo_lseek64` | not yet |  |
| 163 | `lo_tell64` | not yet |  |
| 164 | `lo_truncate64` | not yet |  |
| 165 | `PQconninfo` | not yet |  |
| 166 | `PQsslInUse` | implemented | fe-secure.c:103, without SSL |
| 167 | `PQsslStruct` | implemented | fe-secure.c:458, without SSL |
| 168 | `PQsslAttributeNames` | implemented | fe-secure.c:470, without SSL |
| 169 | `PQsslAttribute` | implemented | fe-secure.c:464, without SSL |
| 170 | `PQsetErrorContextVisibility` | not yet |  |
| 171 | `PQresultVerboseErrorMessage` | not yet |  |
| 172 | `PQencryptPasswordConn` | not yet |  |
| 173 | `PQresultMemorySize` | not yet |  |
| 174 | `PQhostaddr` | not yet |  |
| 175 | `PQgssEncInUse` | implemented | fe-secure.c:513, without GSSAPI |
| 176 | `PQgetgssctx` | implemented | fe-secure.c:507, without GSSAPI |
| 177 | `PQsetSSLKeyPassHook_OpenSSL` | implemented | fe-secure.c:491, without OpenSSL |
| 178 | `PQgetSSLKeyPassHook_OpenSSL` | implemented | fe-secure.c:485, without OpenSSL |
| 179 | `PQdefaultSSLKeyPassHook_OpenSSL` | implemented | fe-secure.c:497, without OpenSSL |
| 180 | `PQenterPipelineMode` | not yet |  |
| 181 | `PQexitPipelineMode` | not yet |  |
| 182 | `PQpipelineSync` | not yet |  |
| 183 | `PQpipelineStatus` | not yet |  |
| 184 | `PQsetTraceFlags` | not yet |  |
| 185 | `PQmblenBounded` | not yet |  |
| 186 | `PQsendFlushRequest` | not yet |  |
| 187 | `PQconnectionUsedGSSAPI` | not yet |  |
| 188 | `PQclosePrepared` | not yet |  |
| 189 | `PQclosePortal` | not yet |  |
| 190 | `PQsendClosePrepared` | not yet |  |
| 191 | `PQsendClosePortal` | not yet |  |
| 192 | `PQchangePassword` | not yet |  |
| 193 | `PQsendPipelineSync` | not yet |  |
| 194 | `PQcancelBlocking` | not yet |  |
| 195 | `PQcancelStart` | not yet |  |
| 196 | `PQcancelCreate` | not yet |  |
| 197 | `PQcancelPoll` | not yet |  |
| 198 | `PQcancelStatus` | not yet |  |
| 199 | `PQcancelSocket` | not yet |  |
| 200 | `PQcancelErrorMessage` | not yet |  |
| 201 | `PQcancelReset` | not yet |  |
| 202 | `PQcancelFinish` | not yet |  |
| 203 | `PQsocketPoll` | not yet |  |
| 204 | `PQsetChunkedRowsMode` | not yet |  |
| 205 | `PQgetCurrentTimeUSec` | not yet |  |
| 206 | `PQsetAuthDataHook` | not yet |  |
| 207 | `PQgetAuthDataHook` | not yet |  |
| 208 | `PQdefaultAuthDataHook` | not yet |  |
| 209 | `PQfullProtocolVersion` | not yet |  |
| 210 | `appendPQExpBufferVA` | not yet |  |
