/*
 * exec.c
 *		Connects with argv[1], runs one PQexec per case and prints everything
 *		the result accessors rlibpq-ffi exports answer for it, through the
 *		vendored libpq-fe.h.
 *
 * What testlibpq.c does not reach: a NULL field, an empty query, a command
 * tag, a failed query and what it leaves in PQerrorMessage, several
 * statements in one PQexec, a notice from the server, a row or column out of
 * range, and a COPY result's nameless columns.
 */
#include <stdio.h>
#include <stdlib.h>

#include "libpq-fe.h"

static const char *
or_null(const char *text)
{
	return text ? text : "NULL";
}

static PGresult *
show(PGconn *conn, const char *query)
{
	PGresult   *res = PQexec(conn, query);
	int			i,
				j;

	printf("-- %s\n", query);
	printf("status %s cmdStatus \"%s\" ntuples %d nfields %d\n",
		   PQresStatus(PQresultStatus(res)), or_null(PQcmdStatus(res)),
		   PQntuples(res), PQnfields(res));
	for (j = 0; j < PQnfields(res); j++)
		printf("fname %d %s\n", j, or_null(PQfname(res, j)));
	for (i = 0; i < PQntuples(res); i++)
		for (j = 0; j < PQnfields(res); j++)
			printf("value %d %d \"%s\" length %d isnull %d\n", i, j,
				   PQgetvalue(res, i, j), PQgetlength(res, i, j),
				   PQgetisnull(res, i, j));
	printf("resultErrorMessage \"%s\"\n", PQresultErrorMessage(res));
	printf("errorMessage \"%s\"\n", PQerrorMessage(conn));
	return res;
}

int
main(int argc, char **argv)
{
	PGconn	   *conn;
	PGresult   *res;

	if (argc != 2)
	{
		fprintf(stderr, "usage: %s CONNINFO\n", argv[0]);
		return 2;
	}
	conn = PQconnectdb(argv[1]);
	if (PQstatus(conn) != CONNECTION_OK)
	{
		fprintf(stderr, "%s", PQerrorMessage(conn));
		PQfinish(conn);
		return 1;
	}
	printf("status %d errorMessage \"%s\"\n", PQstatus(conn), PQerrorMessage(conn));

	res = show(conn, "select 1 as a, null::text as b, 'xyz'::text as c");
	/* each out-of-range call raises a notice, on stderr, in this order */
	printf("out of range: %s", or_null(PQgetvalue(res, 1, 0)));
	printf(" %s", or_null(PQgetvalue(res, 0, 3)));
	printf(" %s", or_null(PQfname(res, -1)));
	printf(" %d", PQgetlength(res, -1, 0));
	printf(" %d\n", PQgetisnull(res, 0, 5));
	PQclear(res);

	PQclear(show(conn, ""));
	PQclear(show(conn, "create temp table t (x int)"));
	PQclear(show(conn, "insert into t values (1), (2)"));
	PQclear(show(conn, "select 1/0"));
	PQclear(show(conn, "select x from t order by x; select 'last'"));
	PQclear(show(conn, "select 1; select 1/0; select 2"));
	PQclear(show(conn, "do $$ begin raise notice 'from the server'; end $$"));
	PQclear(show(conn, "copy (select 1, 2) to stdout"));

	printf("status %d\n", PQstatus(conn));
	PQfinish(conn);
	return 0;
}
