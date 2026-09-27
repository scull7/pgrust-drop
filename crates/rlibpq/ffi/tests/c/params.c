/*
 * params.c
 *		Connects with argv[1] and drives the extended-query calls and the
 *		result metadata rlibpq-ffi exports, through the vendored libpq-fe.h,
 *		printing what each answered.
 *
 * What testlibpq3.c does not reach: PQprepare, PQdescribePrepared,
 * PQexecPrepared, PQdescribePortal, a column's table, type, size and
 * modifier, PQfnumber's identifier folding, the tags PQcmdTuples and
 * PQoidValue read, an error's fields, the arguments C refuses before
 * sending, and a binary COPY.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "libpq-fe.h"

static const char *
or_null(const char *text)
{
	return text ? text : "NULL";
}

/* print what a result and the connection say, then free the result */
static void
done(PGconn *conn, const char *what, PGresult *res)
{
	printf("-- %s\n", what);
	if (!res)
	{
		printf("NULL errorMessage \"%s\"\n", PQerrorMessage(conn));
		return;
	}
	printf("status %s cmdStatus \"%s\" cmdTuples \"%s\" oidValue %u oidStatus \"%s\"\n",
		   PQresStatus(PQresultStatus(res)), PQcmdStatus(res), PQcmdTuples(res),
		   PQoidValue(res), PQoidStatus(res));
	printf("resultErrorMessage \"%s\"\n", PQresultErrorMessage(res));
	PQclear(res);
}

static void
describe(PGresult *res)
{
	int			i;

	printf("nparams %d", PQnparams(res));
	for (i = 0; i < PQnparams(res); i++)
		printf(" paramtype %d %u", i, PQparamtype(res, i));
	printf("\n");
	for (i = 0; i < PQnfields(res); i++)
		printf("field %d %s type %u size %d mod %d format %d\n", i,
			   or_null(PQfname(res, i)), PQftype(res, i), PQfsize(res, i),
			   PQfmod(res, i), PQfformat(res, i));
}

int
main(int argc, char **argv)
{
	PGconn	   *conn;
	PGresult   *res;
	Oid			types[2] = {23, 0};
	const char *values[2];
	int			lengths[2];
	int			formats[2];
	const char *table_oid;

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

	/* a prepared statement: one type given, one left to the server */
	res = PQprepare(conn, "s1", "select $1 + $2::int8 as \"Sum\", $2::text as two",
					2, types);
	done(conn, "prepare s1", res);

	res = PQdescribePrepared(conn, "s1");
	printf("-- describe s1\n");
	describe(res);
	/*
	 * out of range: each raises a notice, on stderr, so each is its own
	 * statement to fix the order
	 */
	printf("paramtype 2 %u", PQparamtype(res, 2));
	printf(" paramtype -1 %u", PQparamtype(res, -1));
	printf(" ftype 2 %u\n", PQftype(res, 2));
	/* PQfnumber folds its argument as an SQL identifier */
	printf("fnumber Sum %d \"Sum\" %d sum %d two %d TWO %d \"TWO\" %d \"\" %d NULL %d\n",
		   PQfnumber(res, "Sum"), PQfnumber(res, "\"Sum\""), PQfnumber(res, "sum"),
		   PQfnumber(res, "two"), PQfnumber(res, "TWO"), PQfnumber(res, "\"TWO\""),
		   PQfnumber(res, ""), PQfnumber(res, NULL));
	done(conn, "describe s1", res);

	values[0] = "40";
	values[1] = "2";
	res = PQexecPrepared(conn, "s1", 2, values, NULL, NULL, 0);
	printf("-- exec s1 text\n");
	describe(res);
	printf("value \"%s\" \"%s\" binaryTuples %d\n", PQgetvalue(res, 0, 0),
		   PQgetvalue(res, 0, 1), PQbinaryTuples(res));
	done(conn, "exec s1 text", res);

	/* a NULL value, and binary results */
	values[1] = NULL;
	res = PQexecPrepared(conn, "s1", 2, values, NULL, NULL, 1);
	printf("-- exec s1 binary\n");
	describe(res);
	printf("isnull %d %d length %d %d binaryTuples %d\n", PQgetisnull(res, 0, 0),
		   PQgetisnull(res, 0, 1), PQgetlength(res, 0, 0), PQgetlength(res, 0, 1),
		   PQbinaryTuples(res));
	done(conn, "exec s1 binary", res);

	/* a binary parameter */
	values[0] = "\0\0\0\7";
	lengths[0] = 4;
	formats[0] = 1;
	res = PQexecParams(conn, "select $1::int4 * 6 as answer", 1, NULL, values,
					   lengths, formats, 0);
	printf("-- params binary int4\n");
	describe(res);
	printf("value \"%s\"\n", PQgetvalue(res, 0, 0));
	done(conn, "params binary int4", res);

	/* a table's columns, and the tags of the commands that change it */
	done(conn, "create",
		 PQexec(conn, "create temp table pt (id int4, name varchar(10), price numeric(5,2))"));
	res = PQexec(conn, "select 'pt'::regclass::oid");
	table_oid = PQgetvalue(res, 0, 0);
	values[0] = "a";
	values[1] = "b";
	done(conn, "insert",
		 PQexecParams(conn, "insert into pt values (1, $1, 1.5), (2, $2, 2.5)",
					  2, NULL, values, NULL, NULL, 0));
	{
		PGresult   *sel = PQexecParams(conn, "select name, id, price, id + 1 as next from pt",
									   0, NULL, NULL, NULL, NULL, 0);
		char		oid[16];
		int			i;

		printf("-- select pt\n");
		describe(sel);
		for (i = 0; i < PQnfields(sel); i++)
		{
			snprintf(oid, sizeof(oid), "%u", PQftable(sel, i));
			printf("field %d table %s tablecol %d\n", i,
				   strcmp(oid, table_oid) == 0 ? "pt" : oid, PQftablecol(sel, i));
		}
		printf("ftable 4 %u", PQftable(sel, 4));
		printf(" ftablecol 4 %d\n", PQftablecol(sel, 4));
		done(conn, "select pt", sel);
	}
	PQclear(res);
	done(conn, "update", PQexec(conn, "update pt set price = price + 1"));
	done(conn, "delete", PQexec(conn, "delete from pt where id = 2"));

	/* a failed command, broken into its fields */
	values[0] = "0";
	res = PQexecParams(conn, "select 1 / $1::int4", 1, NULL, values, NULL, NULL, 0);
	printf("-- division by zero\n");
	printf("severity %s nonlocalized %s sqlstate %s primary %s schema %s\n",
		   or_null(PQresultErrorField(res, PG_DIAG_SEVERITY)),
		   or_null(PQresultErrorField(res, PG_DIAG_SEVERITY_NONLOCALIZED)),
		   or_null(PQresultErrorField(res, PG_DIAG_SQLSTATE)),
		   or_null(PQresultErrorField(res, PG_DIAG_MESSAGE_PRIMARY)),
		   or_null(PQresultErrorField(res, PG_DIAG_SCHEMA_NAME)));
	done(conn, "division by zero", res);
	res = PQexec(conn, "select 1");
	printf("-- no error\nsqlstate %s\n",
		   or_null(PQresultErrorField(res, PG_DIAG_SQLSTATE)));
	PQclear(res);
	done(conn, "exec nosuch", PQexecPrepared(conn, "nosuch", 0, NULL, NULL, NULL, 0));

	/* arguments refused before anything is sent */
	done(conn, "params NULL command",
		 PQexecParams(conn, NULL, 0, NULL, NULL, NULL, NULL, 0));
	done(conn, "params -1", PQexecParams(conn, "select 1", -1, NULL, NULL, NULL, NULL, 0));
	done(conn, "params 65536",
		 PQexecParams(conn, "select 1", 65536, NULL, NULL, NULL, NULL, 0));
	done(conn, "prepare NULL name", PQprepare(conn, NULL, "select 1", 0, NULL));
	done(conn, "prepare NULL query", PQprepare(conn, "s2", NULL, 0, NULL));
	done(conn, "exec NULL name", PQexecPrepared(conn, NULL, 0, NULL, NULL, NULL, 0));
	values[0] = "\0\0\0\7";
	done(conn, "exec binary without length",
		 PQexecPrepared(conn, "s1", 1, values, NULL, formats, 0));
	/* the connection is still in step */
	done(conn, "select 1", PQexec(conn, "select 1"));

	/* a cursor's portal */
	PQclear(PQexec(conn, "begin"));
	PQclear(PQexec(conn, "declare c cursor for select 1::int2 as x, 'y'::name as y"));
	res = PQdescribePortal(conn, "c");
	printf("-- describe c\n");
	describe(res);
	done(conn, "describe c", res);
	done(conn, "describe NULL", PQdescribePortal(conn, NULL));
	PQclear(PQexec(conn, "end"));

	/* a binary COPY: PQbinaryTuples is 1 */
	res = PQexec(conn, "copy (select 1) to stdout (format binary)");
	printf("-- copy binary\nbinaryTuples %d nfields %d fformat %d\n",
		   PQbinaryTuples(res), PQnfields(res), PQfformat(res, 0));
	PQclear(res);

	PQfinish(conn);
	return 0;
}
