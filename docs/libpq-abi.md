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
`rlibpq` has none of the three yet. "blocking" marks a call C makes step
by step without waiting and `rlibpq-ffi` makes in one blocking step
(`docs/divergences.md`).

90 of 210 symbols implemented, 0 stubbed with an error, 120 not yet.

| ordinal | symbol | coverage | follows |
|--:|---|---|---|
| 1 | `PQconnectdb` | implemented | fe-connect.c:820 |
| 2 | `PQsetdbLogin` | implemented | fe-connect.c:2231 |
| 3 | `PQconndefaults` | implemented | fe-connect.c:2193 |
| 4 | `PQfinish` | implemented | fe-connect.c:5301 |
| 5 | `PQreset` | implemented | fe-connect.c:5315 |
| 6 | `PQrequestCancel` | not yet |  |
| 7 | `PQdb` | implemented | fe-connect.c:7472 |
| 8 | `PQuser` | implemented | fe-connect.c:7480 |
| 9 | `PQpass` | implemented | fe-connect.c:7488 |
| 10 | `PQhost` | implemented | fe-connect.c:7505 |
| 11 | `PQport` | implemented | fe-connect.c:7541 |
| 12 | `PQtty` | implemented | fe-connect.c:7559 |
| 13 | `PQoptions` | implemented | fe-connect.c:7567 |
| 14 | `PQstatus` | implemented | fe-connect.c:7575 |
| 15 | `PQerrorMessage` | implemented | fe-connect.c:7638 |
| 16 | `PQsocket` | implemented | fe-connect.c:7664 |
| 17 | `PQbackendPID` | implemented | fe-connect.c:7674 |
| 18 | `PQtrace` | not yet |  |
| 19 | `PQuntrace` | not yet |  |
| 20 | `PQsetNoticeProcessor` | implemented | fe-connect.c:7819 |
| 21 | `PQexec` | implemented | fe-exec.c:2279 |
| 22 | `PQnotifies` | implemented | fe-exec.c:2684 |
| 23 | `PQsendQuery` | implemented | fe-exec.c:1433 |
| 24 | `PQgetResult` | implemented | fe-exec.c:2079 |
| 25 | `PQisBusy` | implemented | fe-exec.c:2048 |
| 26 | `PQconsumeInput` | implemented | fe-exec.c:2001 |
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
| 79 | `PQconnectPoll` | implemented | fe-connect.c:2908, blocking |
| 80 | `PQconnectStart` | implemented | fe-connect.c:948, blocking |
| 81 | `PQflush` | implemented | fe-exec.c:4031 |
| 82 | `PQisnonblocking` | implemented | fe-exec.c:4014 |
| 83 | `PQresetPoll` | implemented | fe-connect.c:5367, blocking |
| 84 | `PQresetStart` | implemented | fe-connect.c:5348, blocking |
| 85 | `PQsetClientEncoding` | not yet |  |
| 86 | `PQsetnonblocking` | implemented | fe-exec.c:3975 |
| 87 | `PQfreeNotify` | implemented | fe-exec.c:4080 |
| 88 | `PQescapeString` | not yet |  |
| 89 | `PQescapeBytea` | not yet |  |
| 90 | `printfPQExpBuffer` | not yet |  |
| 91 | `appendPQExpBuffer` | not yet |  |
| 92 | `pg_encoding_to_char` | not yet |  |
| 93 | `pg_utf_mblen` | not yet |  |
| 94 | `PQunescapeBytea` | not yet |  |
| 95 | `PQfreemem` | implemented | fe-exec.c:4063 |
| 96 | `PQtransactionStatus` | implemented | fe-connect.c:7583 |
| 97 | `PQparameterStatus` | implemented | fe-connect.c:7593 |
| 98 | `PQprotocolVersion` | not yet |  |
| 99 | `PQsetErrorVerbosity` | not yet |  |
| 100 | `PQsetNoticeReceiver` | implemented | fe-connect.c:7802 |
| 101 | `PQexecParams` | implemented | fe-exec.c:2293 |
| 102 | `PQsendQueryParams` | implemented | fe-exec.c:1509 |
| 103 | `PQputCopyData` | not yet |  |
| 104 | `PQputCopyEnd` | not yet |  |
| 105 | `PQgetCopyData` | not yet |  |
| 106 | `PQresultErrorField` | implemented | fe-exec.c:3497 |
| 107 | `PQftable` | implemented | fe-exec.c:3717 |
| 108 | `PQftablecol` | implemented | fe-exec.c:3728 |
| 109 | `PQfformat` | implemented | fe-exec.c:3739 |
| 110 | `PQexecPrepared` | implemented | fe-exec.c:2340 |
| 111 | `PQsendQueryPrepared` | implemented | fe-exec.c:1650 |
| 112 | `PQdsplen` | not yet |  |
| 113 | `PQserverVersion` | implemented | fe-connect.c:7628 |
| 114 | `PQgetssl` | implemented | fe-secure.c:452, without SSL |
| 115 | `pg_char_to_encoding` | not yet |  |
| 116 | `pg_valid_server_encoding` | not yet |  |
| 117 | `pqsignal` | not yet |  |
| 118 | `PQprepare` | implemented | fe-exec.c:2323 |
| 119 | `PQsendPrepare` | implemented | fe-exec.c:1553 |
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
| 135 | `PQsendDescribePrepared` | implemented | fe-exec.c:2508 |
| 136 | `PQsendDescribePortal` | implemented | fe-exec.c:2521 |
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
| 156 | `PQconnectdbParams` | implemented | fe-connect.c:765 |
| 157 | `PQconnectStartParams` | implemented | fe-connect.c:867, blocking |
| 158 | `PQping` | not yet |  |
| 159 | `PQpingParams` | not yet |  |
| 160 | `PQlibVersion` | implemented | fe-misc.c:65 |
| 161 | `PQsetSingleRowMode` | not yet |  |
| 162 | `lo_lseek64` | not yet |  |
| 163 | `lo_tell64` | not yet |  |
| 164 | `lo_truncate64` | not yet |  |
| 165 | `PQconninfo` | implemented | fe-connect.c:7415 |
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
