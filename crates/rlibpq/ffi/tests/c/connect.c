/*
 * connect.c
 *		Opens connections every way rlibpq-ffi exports -- PQconnectdbParams,
 *		PQsetdbLogin, PQconnectStart with the PQconnectPoll loop libpq.sgml
 *		describes, PQreset, PQresetStart with PQresetPoll -- and prints what
 *		the PGconn accessors answer on each.
 *
 * Usage: connect HOST PORT, a server where gateuser connects by trust.
 * Whatever depends on the server rather than on libpq is printed as the
 * query it was checked against: PQbackendPID against pg_backend_pid(),
 * PQserverVersion against server_version_num, the host as <host>.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>

#include "libpq-fe.h"

static const char *host;

static const char *
or_null(const char *text)
{
	return text ? text : "(null)";
}

/* The single value "sql" returns, in a buffer the next call reuses. */
static const char *
value(PGconn *conn, const char *sql)
{
	static char buf[256];
	PGresult   *res = PQexec(conn, sql);

	if (PQresultStatus(res) != PGRES_TUPLES_OK || PQntuples(res) != 1)
		snprintf(buf, sizeof(buf), "failed: %s", PQerrorMessage(conn));
	else
		snprintf(buf, sizeof(buf), "%s", PQgetvalue(res, 0, 0));
	PQclear(res);
	return buf;
}

static void
run(PGconn *conn, const char *sql)
{
	PQclear(PQexec(conn, sql));
	printf("%s: PQtransactionStatus %d\n", sql, PQtransactionStatus(conn));
}

static void
print_conn(const char *label, PGconn *conn)
{
	int			pid = PQbackendPID(conn);
	int			version = PQserverVersion(conn);

	printf("-- %s\n", label);
	printf("PQstatus %d PQerrorMessage \"%s\"\n", PQstatus(conn), PQerrorMessage(conn));
	printf("PQdb %s PQuser %s PQpass \"%s\" PQoptions \"%s\" PQtty \"%s\"\n",
		   or_null(PQdb(conn)), or_null(PQuser(conn)), PQpass(conn),
		   or_null(PQoptions(conn)), PQtty(conn));
	printf("PQhost %s PQport %s\n",
		   strcmp(PQhost(conn), host) == 0 ? "<host>" : PQhost(conn), PQport(conn));
	printf("PQsocket %s PQtransactionStatus %d\n",
		   PQsocket(conn) >= 0 ? "open" : "-1", PQtransactionStatus(conn));
	if (PQstatus(conn) != CONNECTION_OK)
	{
		printf("PQbackendPID %d PQserverVersion %d\n", pid, version);
		return;
	}
	printf("PQbackendPID %s\n",
		   pid == atoi(value(conn, "select pg_backend_pid()")) ? "pg_backend_pid()" : "other");
	printf("PQserverVersion %s\n",
		   version == atoi(value(conn, "show server_version_num")) ? "server_version_num" : "other");
	printf("PQparameterStatus server_encoding %s DateStyle %s nosuch %s\n",
		   or_null(PQparameterStatus(conn, "server_encoding")),
		   or_null(PQparameterStatus(conn, "DateStyle")),
		   or_null(PQparameterStatus(conn, "nosuch")));
}

/* The rows of PQconninfo that hold a value, for the keywords named. */
static void
print_conninfo(PGconn *conn)
{
	static const char *const keywords[] = {
		"user", "password", "passfile", "dbname", "host", "hostaddr", "port",
		"application_name", NULL
	};
	PQconninfoOption *options = PQconninfo(conn);

	for (const char *const *keyword = keywords; *keyword; keyword++)
		for (PQconninfoOption *option = options; option->keyword; option++)
			if (strcmp(option->keyword, *keyword) == 0)
				printf("PQconninfo %s=%s\n", option->keyword,
					   option->val && strcmp(option->val, host) == 0 ? "<host>" : or_null(option->val));
	PQconninfoFree(options);
}

/* Wait until the socket can be read, or written. */
static void
wait_on(PGconn *conn, int reading)
{
	int			sock = PQsocket(conn);
	fd_set		fds;

	FD_ZERO(&fds);
	FD_SET(sock, &fds);
	select(sock + 1, reading ? &fds : NULL, reading ? NULL : &fds, NULL, NULL);
}

static const char *
polling_status(PostgresPollingStatusType status)
{
	switch (status)
	{
		case PGRES_POLLING_FAILED:
			return "PGRES_POLLING_FAILED";
		case PGRES_POLLING_OK:
			return "PGRES_POLLING_OK";
		default:
			return "other";
	}
}

/*
 * The loop libpq.sgml gives for PQconnectPoll and PQresetPoll: start as if
 * the last poll said PGRES_POLLING_WRITING, and wait on the socket as each
 * poll says until one says OK or FAILED.
 */
static PostgresPollingStatusType
poll_until_done(PGconn *conn, PostgresPollingStatusType (*poll) (PGconn *))
{
	PostgresPollingStatusType status = PGRES_POLLING_WRITING;

	while (status != PGRES_POLLING_OK && status != PGRES_POLLING_FAILED)
	{
		if (PQsocket(conn) >= 0)
			wait_on(conn, status == PGRES_POLLING_READING);
		status = poll(conn);
	}
	return status;
}

int
main(int argc, char **argv)
{
	const char *port;
	char		conninfo[1024];
	char		expanded[1024];
	PGconn	   *conn;
	int			pid;

	if (argc != 3)
	{
		fprintf(stderr, "usage: %s HOST PORT\n", argv[0]);
		return 2;
	}
	host = argv[1];
	port = argv[2];
	snprintf(conninfo, sizeof(conninfo), "host='%s' port=%s user=gateuser dbname=postgres",
			 host, port);

	{
		const char *const keywords[] = {
			"host", "port", "user", "dbname", "application_name", NULL
		};
		const char *const values[] = {host, port, "gateuser", "postgres", "connect_c", NULL};

		conn = PQconnectdbParams(keywords, values, 0);
	}
	print_conn("PQconnectdbParams", conn);
	print_conninfo(conn);
	printf("PQparameterStatus application_name %s\n",
		   or_null(PQparameterStatus(conn, "application_name")));
	run(conn, "set application_name = renamed");
	printf("PQparameterStatus application_name %s\n",
		   or_null(PQparameterStatus(conn, "application_name")));
	run(conn, "begin");
	run(conn, "select 1/0");
	run(conn, "rollback");
	run(conn, "copy (select 1) to stdout");
	PQfinish(conn);

	/* dbname expanded as a connection string; a later keyword still wins */
	snprintf(expanded, sizeof(expanded), "host='%s' port=%s dbname=template1 user=nobody",
			 host, port);
	{
		const char *const keywords[] = {"user", "dbname", "user", NULL};
		const char *const values[] = {"ignored", expanded, "gateuser", NULL};

		conn = PQconnectdbParams(keywords, values, 1);
	}
	print_conn("PQconnectdbParams expand_dbname", conn);
	PQfinish(conn);

	conn = PQsetdbLogin(host, port, NULL, NULL, "template1", "gateuser", NULL);
	print_conn("PQsetdbLogin", conn);

	/* the connection is lost: what the server said stays until a reset */
	PQclear(PQexec(conn, "select pg_terminate_backend(pg_backend_pid())"));
	printf("-- terminated\n");
	printf("PQstatus %d PQsocket %d PQtransactionStatus %d PQbackendPID %d PQserverVersion %d\n",
		   PQstatus(conn), PQsocket(conn), PQtransactionStatus(conn), PQbackendPID(conn),
		   PQserverVersion(conn));
	printf("PQparameterStatus server_encoding %s PQdb %s\n",
		   or_null(PQparameterStatus(conn, "server_encoding")), PQdb(conn));
	PQreset(conn);
	print_conn("PQreset", conn);

	pid = PQbackendPID(conn);
	printf("PQresetStart %d\n", PQresetStart(conn));
	printf("PQresetPoll %s\n", polling_status(poll_until_done(conn, PQresetPoll)));
	printf("new backend %s\n", PQbackendPID(conn) != pid ? "yes" : "no");
	print_conn("PQresetStart", conn);
	PQfinish(conn);

	conn = PQconnectStart(conninfo);
	if (PQstatus(conn) == CONNECTION_BAD)
		printf("PQconnectStart failed: %s", PQerrorMessage(conn));
	printf("PQconnectPoll %s\n", polling_status(poll_until_done(conn, PQconnectPoll)));
	print_conn("PQconnectStart", conn);
	PQfinish(conn);

	/* a server that is not there */
	{
		const char *const keywords[] = {"host", "port", "user", NULL};
		const char *const values[] = {host, "1", "gateuser", NULL};

		conn = PQconnectStartParams(keywords, values, 0);
	}
	printf("-- PQconnectStartParams, no server\n");
	printf("PQstatus %d PQconnectPoll %s PQhost %s PQport %s PQsocket %d\n",
		   PQstatus(conn), polling_status(poll_until_done(conn, PQconnectPoll)),
		   strcmp(PQhost(conn), host) == 0 ? "<host>" : PQhost(conn), PQport(conn),
		   PQsocket(conn));
	PQfinish(conn);
	return 0;
}
