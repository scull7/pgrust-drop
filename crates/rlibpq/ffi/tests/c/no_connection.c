/*
 * no_connection.c
 *		Calls every function rlibpq-ffi exports through the vendored
 *		libpq-fe.h, with no connection, and prints what each answered.
 *
 * Compiled against the unmodified header, so a shim whose signature drifted
 * from libpq-fe.h fails to compile rather than misbehaving at run time.
 */
#include <stdio.h>
#include <stdlib.h>

#include "libpq-fe.h"

/*
 * libpq-fe.h:629 turns PQfreeNotify into PQfreemem; the exported symbol is
 * reached the way fe-exec.c:4076-4077 declares it.
 */
#undef PQfreeNotify
extern void PQfreeNotify(PGnotify *notify);

static int
hook(char *buf, int size, PGconn *conn)
{
	return 1;
}

static const char *
null_or_set(const void *ptr)
{
	return ptr ? "set" : "NULL";
}

/* How many rows a PQconninfoOption array has, and how many hold a value. */
static void
print_conninfo(PQconninfoOption *options)
{
	int			rows = 0;
	int			set = 0;

	if (!options)
	{
		printf("PQconninfo NULL\n");
		return;
	}
	for (PQconninfoOption *option = options; option->keyword; option++)
	{
		rows++;
		if (option->val)
			set++;
	}
	printf("PQconninfo %d rows, %d set\n", rows, set);
	PQconninfoFree(options);
}

static void
print_accessors(PGconn *conn)
{
	printf("PQdb %s PQuser %s PQoptions %s\n", null_or_set(PQdb(conn)),
		   null_or_set(PQuser(conn)), null_or_set(PQoptions(conn)));
	printf("PQpass %s PQhost %s PQport %s PQtty %s\n", PQpass(conn) ? PQpass(conn) : "NULL",
		   PQhost(conn) ? PQhost(conn) : "NULL", PQport(conn) ? PQport(conn) : "NULL",
		   PQtty(conn) ? PQtty(conn) : "NULL");
	printf("PQtransactionStatus %d PQparameterStatus %s PQserverVersion %d\n",
		   PQtransactionStatus(conn),
		   null_or_set(PQparameterStatus(conn, "server_version")),
		   PQserverVersion(conn));
	printf("PQsocket %d PQbackendPID %d PQconnectPoll %d\n", PQsocket(conn),
		   PQbackendPID(conn), PQconnectPoll(conn));
	print_conninfo(PQconninfo(conn));
}

int
main(void)
{
	char		buf[16];
	PGconn	   *conn;
	const char *const *names = PQsslAttributeNames(NULL);

	printf("PQlibVersion %d\n", PQlibVersion());
	printf("PQisthreadsafe %d\n", PQisthreadsafe());

	PQinitSSL(1);
	PQinitOpenSSL(1, 1);
	printf("PQsslInUse %d\n", PQsslInUse(NULL));
	printf("PQgetssl %s\n", null_or_set(PQgetssl(NULL)));
	printf("PQsslStruct %s\n", null_or_set(PQsslStruct(NULL, "OpenSSL")));
	printf("PQsslAttribute %s\n", null_or_set(PQsslAttribute(NULL, "library")));
	printf("PQsslAttributeNames %s\n", names && !names[0] ? "{NULL}" : "other");

	PQsetSSLKeyPassHook_OpenSSL(hook);
	printf("PQgetSSLKeyPassHook_OpenSSL %s\n",
		   PQgetSSLKeyPassHook_OpenSSL() ? "set" : "NULL");
	printf("PQdefaultSSLKeyPassHook_OpenSSL %d\n",
		   PQdefaultSSLKeyPassHook_OpenSSL(buf, sizeof(buf), NULL));

	printf("PQgssEncInUse %d\n", PQgssEncInUse(NULL));
	printf("PQgetgssctx %s\n", null_or_set(PQgetgssctx(NULL)));

	PQfreemem(NULL);
	PQfreemem(malloc(16));
	PQfreeNotify(malloc(sizeof(PGnotify)));
	printf("freed\n");

	/* a NULL PGconn and a NULL PGresult */
	printf("PQstatus %d\n", PQstatus(NULL));
	printf("PQerrorMessage %s", PQerrorMessage(NULL));
	printf("PQexec %s\n", null_or_set(PQexec(NULL, "select 1")));
	printf("PQresultStatus %s\n", PQresStatus(PQresultStatus(NULL)));
	printf("PQresStatus %s|%s|%s\n", PQresStatus(PGRES_EMPTY_QUERY),
		   PQresStatus(PGRES_TUPLES_CHUNK), PQresStatus(PGRES_TUPLES_CHUNK + 1));
	printf("PQresultErrorMessage \"%s\"\n", PQresultErrorMessage(NULL));
	printf("PQntuples %d PQnfields %d\n", PQntuples(NULL), PQnfields(NULL));
	printf("PQfname %s\n", null_or_set(PQfname(NULL, 0)));
	printf("PQcmdStatus %s\n", null_or_set(PQcmdStatus(NULL)));
	printf("PQgetvalue %s PQgetlength %d PQgetisnull %d\n",
		   null_or_set(PQgetvalue(NULL, 0, 0)), PQgetlength(NULL, 0, 0),
		   PQgetisnull(NULL, 0, 0));
	printf("PQbinaryTuples %d PQfnumber %d\n", PQbinaryTuples(NULL),
		   PQfnumber(NULL, "a"));
	printf("PQftable %u PQftablecol %d PQfformat %d\n", PQftable(NULL, 0),
		   PQftablecol(NULL, 0), PQfformat(NULL, 0));
	printf("PQftype %u PQfsize %d PQfmod %d\n", PQftype(NULL, 0),
		   PQfsize(NULL, 0), PQfmod(NULL, 0));
	printf("PQoidStatus \"%s\" PQoidValue %u PQcmdTuples \"%s\"\n",
		   PQoidStatus(NULL), PQoidValue(NULL), PQcmdTuples(NULL));
	printf("PQnparams %d PQparamtype %u\n", PQnparams(NULL), PQparamtype(NULL, 0));
	printf("PQresultErrorField %s\n",
		   null_or_set(PQresultErrorField(NULL, PG_DIAG_SQLSTATE)));
	printf("PQexecParams %s PQprepare %s PQexecPrepared %s\n",
		   null_or_set(PQexecParams(NULL, "select 1", 0, NULL, NULL, NULL, NULL, 0)),
		   null_or_set(PQprepare(NULL, "s", "select 1", 0, NULL)),
		   null_or_set(PQexecPrepared(NULL, "s", 0, NULL, NULL, NULL, 0)));
	printf("PQdescribePrepared %s PQdescribePortal %s\n",
		   null_or_set(PQdescribePrepared(NULL, "s")),
		   null_or_set(PQdescribePortal(NULL, "p")));
	printf("PQsendQuery %d PQsendQueryParams %d PQsendPrepare %d\n",
		   PQsendQuery(NULL, "select 1"),
		   PQsendQueryParams(NULL, "select 1", 0, NULL, NULL, NULL, NULL, 0),
		   PQsendPrepare(NULL, "s", "select 1", 0, NULL));
	printf("PQsendQueryPrepared %d PQsendDescribePrepared %d PQsendDescribePortal %d\n",
		   PQsendQueryPrepared(NULL, "s", 0, NULL, NULL, NULL, 0),
		   PQsendDescribePrepared(NULL, "s"), PQsendDescribePortal(NULL, "p"));
	printf("PQgetResult %s PQconsumeInput %d PQisBusy %d PQnotifies %s\n",
		   null_or_set(PQgetResult(NULL)), PQconsumeInput(NULL), PQisBusy(NULL),
		   null_or_set(PQnotifies(NULL)));
	printf("PQsetnonblocking %d PQisnonblocking %d PQflush %d\n",
		   PQsetnonblocking(NULL, 1), PQisnonblocking(NULL), PQflush(NULL));
	printf("PQsetNoticeReceiver %s PQsetNoticeProcessor %s\n",
		   PQsetNoticeReceiver(NULL, NULL, NULL) ? "set" : "NULL",
		   PQsetNoticeProcessor(NULL, NULL, NULL) ? "set" : "NULL");
	print_accessors(NULL);
	printf("PQparameterStatus %s PQresetStart %d PQresetPoll %d\n",
		   null_or_set(PQparameterStatus(NULL, NULL)), PQresetStart(NULL),
		   PQresetPoll(NULL));
	PQreset(NULL);
	PQclear(NULL);
	PQfinish(NULL);

	/* a PGconn whose conninfo does not parse: no socket is ever opened */
	conn = PQconnectdb("bogus");
	printf("PQconnectdb %s\n", null_or_set(conn));
	printf("PQstatus %d\n", PQstatus(conn));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	printf("PQexec %s\n", null_or_set(PQexec(conn, "select 1")));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	/* PQsendQueryStart refuses before any argument is looked at */
	printf("PQexecParams %s\n",
		   null_or_set(PQexecParams(conn, NULL, -1, NULL, NULL, NULL, NULL, 0)));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	printf("PQdescribePortal %s\n", null_or_set(PQdescribePortal(conn, NULL)));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	printf("PQsendQuery %d\n", PQsendQuery(conn, NULL));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	/* pqReadData finds no socket, and the error is added to the last */
	printf("PQconsumeInput %d\n", PQconsumeInput(conn));
	printf("PQerrorMessage %s", PQerrorMessage(conn));
	printf("PQgetResult %s PQisBusy %d PQnotifies %s\n", null_or_set(PQgetResult(conn)),
		   PQisBusy(conn), null_or_set(PQnotifies(conn)));
	printf("PQsetnonblocking %d PQisnonblocking %d PQflush %d\n",
		   PQsetnonblocking(conn, 1), PQisnonblocking(conn), PQflush(conn));
	printf("PQsetNoticeReceiver %s PQsetNoticeProcessor %s\n",
		   PQsetNoticeReceiver(conn, NULL, NULL) ? "set" : "NULL",
		   PQsetNoticeProcessor(conn, NULL, NULL) ? "set" : "NULL");
	print_accessors(conn);
	printf("PQparameterStatus %s\n", null_or_set(PQparameterStatus(conn, NULL)));
	/* the options were never valid, so a reset fails with no message */
	PQreset(conn);
	printf("PQreset PQstatus %d PQerrorMessage \"%s\"\n", PQstatus(conn),
		   PQerrorMessage(conn));
	printf("PQresetStart %d PQresetPoll %d\n", PQresetStart(conn), PQresetPoll(conn));
	PQfinish(conn);

	/* the other ways in, with options that do not parse */
	conn = PQconnectStart("bogus");
	printf("PQconnectStart PQstatus %d PQconnectPoll %d PQerrorMessage %s",
		   PQstatus(conn), PQconnectPoll(conn), PQerrorMessage(conn));
	PQfinish(conn);
	{
		const char *const keywords[] = {"port", "bogus", NULL};
		const char *const values[] = {"1", "x", NULL};

		conn = PQconnectdbParams(keywords, values, 0);
		printf("PQconnectdbParams PQstatus %d PQerrorMessage %s", PQstatus(conn),
			   PQerrorMessage(conn));
		PQfinish(conn);
	}
	{
		const char *const keywords[] = {"dbname", NULL};
		const char *const values[] = {"port=1 bogus=x", NULL};

		conn = PQconnectStartParams(keywords, values, 1);
		printf("PQconnectStartParams PQstatus %d PQerrorMessage %s",
			   PQstatus(conn), PQerrorMessage(conn));
		PQfinish(conn);
	}
	conn = PQsetdbLogin(NULL, NULL, NULL, NULL, "host=h bogus=x", NULL, NULL);
	printf("PQsetdbLogin PQstatus %d PQerrorMessage %s", PQstatus(conn),
		   PQerrorMessage(conn));
	PQfinish(conn);
	return 0;
}
