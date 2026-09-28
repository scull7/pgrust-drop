/*
 * async.c
 *		Drives the calls rlibpq-ffi exports that do not wait -- PQsendQuery
 *		and its extended-query siblings, PQgetResult, PQconsumeInput, PQisBusy,
 *		PQnotifies, PQsetnonblocking, PQisnonblocking, PQflush -- and the
 *		notice hooks, PQsetNoticeReceiver and PQsetNoticeProcessor, and prints
 *		what each answers.
 *
 * Usage: async CONNINFO, a server where the connection succeeds.
 * Everything is printed to stdout, the notice hooks included, so the order
 * of notices and results is the order they reached the program. A server
 * process ID is printed as the call it was checked against.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>

#include "libpq-fe.h"

static PQnoticeReceiver default_receiver;

static const char *
or_null(const char *text)
{
	return text ? text : "(null)";
}

static void
processor(void *arg, const char *message)
{
	printf("processor %s: %s", (const char *) arg, message);
}

/* Print the notice, then hand it on to the receiver that was installed. */
static void
receiver(void *arg, const PGresult *res)
{
	printf("receiver %s: %s severity %s primary %s\n", (const char *) arg,
		   PQresStatus(PQresultStatus(res)),
		   or_null(PQresultErrorField(res, PG_DIAG_SEVERITY)),
		   or_null(PQresultErrorField(res, PG_DIAG_MESSAGE_PRIMARY)));
	default_receiver(arg, res);
}

/* select(2) on the socket for "events", as testlibpq2.c waits. */
static void
wait_socket(PGconn *conn, int for_write)
{
	int			sock = PQsocket(conn);
	fd_set		read_mask;
	fd_set		write_mask;

	FD_ZERO(&read_mask);
	FD_ZERO(&write_mask);
	FD_SET(sock, &read_mask);
	if (for_write)
		FD_SET(sock, &write_mask);
	if (select(sock + 1, &read_mask, &write_mask, NULL, NULL) < 0)
	{
		perror("select");
		exit(1);
	}
}

/* Read until PQgetResult would not wait; 0 when PQconsumeInput failed. */
static int
await_result(PGconn *conn)
{
	while (PQisBusy(conn))
	{
		wait_socket(conn, 0);
		if (!PQconsumeInput(conn))
		{
			printf("PQconsumeInput 0 PQstatus %d PQsocket %d\n",
				   PQstatus(conn), PQsocket(conn));
			return 0;
		}
	}
	return 1;
}

/* Every result of the command running, then the NULL that ends it. */
static void
drain(const char *label, PGconn *conn)
{
	PGresult   *res;

	await_result(conn);
	while ((res = PQgetResult(conn)) != NULL)
	{
		ExecStatusType status = PQresultStatus(res);

		printf("%s: %s", label, PQresStatus(status));
		if (status == PGRES_TUPLES_OK)
			printf(" %d row(s), first \"%s\"", PQntuples(res),
				   PQntuples(res) > 0 ? PQgetvalue(res, 0, 0) : "");
		else if (status == PGRES_COMMAND_OK)
			printf(" \"%s\" nparams %d nfields %d", PQcmdStatus(res),
				   PQnparams(res), PQnfields(res));
		else
			printf(" \"%s\"", PQresultErrorMessage(res));
		printf("\n");
		PQclear(res);
		await_result(conn);
	}
	printf("%s: NULL PQerrorMessage \"%s\"\n", label, PQerrorMessage(conn));
}

static void
sent(const char *call, PGconn *conn, int ok)
{
	printf("%s %d PQerrorMessage \"%s\"\n", call, ok, PQerrorMessage(conn));
}

static void
notifications(PGconn *conn)
{
	PGnotify   *notify;

	while ((notify = PQnotifies(conn)) != NULL)
	{
		printf("PQnotifies %s \"%s\" from %s\n", notify->relname, notify->extra,
			   notify->be_pid == PQbackendPID(conn) ? "PQbackendPID" : "other");
		PQfreemem(notify);
	}
	printf("PQnotifies NULL\n");
}

int
main(int argc, char **argv)
{
	PGconn	   *conn;
	PGresult   *res;
	PGresult   *early;
	const char *values[1];
	char	   *big;
	size_t		len = 1024 * 1024;
	int			flushed;

	if (argc != 2)
	{
		fprintf(stderr, "usage: async CONNINFO\n");
		return 2;
	}
	conn = PQconnectdb(argv[1]);
	if (PQstatus(conn) != CONNECTION_OK)
	{
		fprintf(stderr, "%s", PQerrorMessage(conn));
		return 1;
	}

	/* The hooks: the defaults are installed, NULL changes nothing. */
	printf("PQsetNoticeProcessor %s\n",
		   PQsetNoticeProcessor(conn, processor, "one") ? "default" : "NULL");
	PQclear(PQexec(conn, "DO $$BEGIN RAISE NOTICE 'hello'; END$$"));
	default_receiver = PQsetNoticeReceiver(conn, receiver, "r");
	printf("PQsetNoticeReceiver %s\n", default_receiver ? "default" : "NULL");
	printf("PQsetNoticeReceiver NULL proc %s\n",
		   PQsetNoticeReceiver(conn, NULL, NULL) == receiver ? "receiver" : "other");
	printf("PQsetNoticeProcessor NULL proc %s\n",
		   PQsetNoticeProcessor(conn, NULL, NULL) == processor ? "processor" : "other");
	printf("NULL conn %s %s\n",
		   PQsetNoticeReceiver(NULL, receiver, NULL) ? "set" : "NULL",
		   PQsetNoticeProcessor(NULL, processor, NULL) ? "set" : "NULL");
	PQclear(PQexec(conn, "DO $$BEGIN RAISE WARNING 'careful'; END$$"));

	/* A result keeps the hooks it was made with. */
	early = PQexec(conn, "select 1");
	PQsetNoticeProcessor(conn, processor, "two");
	res = PQexec(conn, "select 2");
	printf("PQgetvalue %s\n", or_null(PQgetvalue(early, 5, 0)));
	printf("PQgetvalue %s\n", or_null(PQgetvalue(res, 0, 7)));
	PQclear(early);
	PQclear(res);

	/* A simple query of three statements; the error ends it. */
	sent("PQsendQuery", conn,
		 PQsendQuery(conn, "select 1; select 2/0; select 3"));
	drain("three", conn);

	/* A notice raised while the results are collected. */
	sent("PQsendQuery", conn,
		 PQsendQuery(conn, "DO $$BEGIN RAISE NOTICE 'async'; END$$"));
	drain("notice", conn);

	/* One command at a time: the error is kept, not cleared. */
	sent("PQsendQuery", conn, PQsendQuery(conn, "select 'a'"));
	sent("PQsendQuery busy", conn, PQsendQuery(conn, "select 'b'"));
	sent("PQsendPrepare busy", conn,
		 PQsendPrepare(conn, NULL, "select 'b'", 0, NULL));
	drain("busy", conn);

	/* The argument checks, each on an idle connection. */
	sent("PQsendQuery NULL", conn, PQsendQuery(conn, NULL));
	sent("PQsendQueryParams -1", conn,
		 PQsendQueryParams(conn, "select 1", -1, NULL, NULL, NULL, NULL, 0));
	sent("PQsendPrepare NULL name", conn,
		 PQsendPrepare(conn, NULL, "select 1", 0, NULL));
	sent("PQsendPrepare NULL query", conn,
		 PQsendPrepare(conn, "s", NULL, 0, NULL));
	sent("PQsendQueryPrepared NULL", conn,
		 PQsendQueryPrepared(conn, NULL, 0, NULL, NULL, NULL, 0));

	/* The extended-query calls. */
	values[0] = "41";
	sent("PQsendQueryParams", conn,
		 PQsendQueryParams(conn, "select $1::int4 + 1", 1, NULL, values, NULL, NULL, 0));
	drain("params", conn);
	sent("PQsendPrepare", conn,
		 PQsendPrepare(conn, "s1", "select $1::text || '!'", 1, NULL));
	drain("prepare", conn);
	values[0] = "hi";
	sent("PQsendQueryPrepared", conn,
		 PQsendQueryPrepared(conn, "s1", 1, values, NULL, NULL, 0));
	drain("prepared", conn);
	sent("PQsendDescribePrepared", conn, PQsendDescribePrepared(conn, "s1"));
	drain("describe s1", conn);
	sent("PQsendDescribePortal", conn, PQsendDescribePortal(conn, NULL));
	drain("describe portal", conn);

	/* Non-blocking: a query too big for the socket, flushed in turns. */
	printf("PQisnonblocking %d\n", PQisnonblocking(conn));
	printf("PQsetnonblocking 1: %d\n", PQsetnonblocking(conn, 1));
	printf("PQisnonblocking %d\n", PQisnonblocking(conn));
	printf("PQsetnonblocking 1 again: %d\n", PQsetnonblocking(conn, 1));
	big = malloc(len + 64);
	strcpy(big, "select length('");
	memset(big + strlen(big), 'x', len);
	strcpy(big + strlen("select length('") + len, "')");
	sent("PQsendQuery 1 MiB", conn, PQsendQuery(conn, big));
	free(big);
	while ((flushed = PQflush(conn)) == 1)
	{
		wait_socket(conn, 1);
		if (!PQconsumeInput(conn))
			break;
	}
	printf("PQflush %d\n", flushed);
	drain("big", conn);
	printf("PQsetnonblocking 0: %d\n", PQsetnonblocking(conn, 0));
	printf("PQisnonblocking %d PQflush %d\n", PQisnonblocking(conn), PQflush(conn));

	/* Notifications, from this session. */
	PQclear(PQexec(conn, "LISTEN ch"));
	PQclear(PQexec(conn, "NOTIFY ch, 'payload'"));
	sent("PQsendQuery", conn, PQsendQuery(conn, "NOTIFY ch"));
	drain("notify", conn);
	notifications(conn);

	/* The server ends the session while a command runs. */
	sent("PQsendQuery", conn,
		 PQsendQuery(conn, "select pg_terminate_backend(pg_backend_pid())"));
	drain("terminated", conn);
	printf("PQstatus %d PQsocket %d", PQstatus(conn), PQsocket(conn));
	printf(" PQisBusy %d PQflush %d", PQisBusy(conn), PQflush(conn));
	printf(" PQsetnonblocking %d PQisnonblocking %d\n",
		   PQsetnonblocking(conn, 1), PQisnonblocking(conn));
	sent("PQsendQuery", conn, PQsendQuery(conn, "select 1"));
	sent("PQconsumeInput", conn, PQconsumeInput(conn));
	printf("PQgetResult %s\n", PQgetResult(conn) ? "result" : "NULL");

	/* Again, reading until the socket closes before anything is parsed. */
	PQreset(conn);
	printf("PQreset PQstatus %d\n", PQstatus(conn));
	sent("PQsendQuery", conn,
		 PQsendQuery(conn, "select pg_terminate_backend(pg_backend_pid())"));
	do
		wait_socket(conn, 0);
	while (PQconsumeInput(conn));
	printf("PQconsumeInput 0 PQstatus %d\n", PQstatus(conn));
	drain("unparsed", conn);

	PQfinish(conn);
	return 0;
}
