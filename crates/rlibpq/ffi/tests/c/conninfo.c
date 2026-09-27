/*
 * conninfo.c
 *		Prints every field of every PQconninfoOption row PQconndefaults and
 *		PQconninfoParse return, through the vendored libpq-fe.h, then the
 *		error contract of PQconninfoParse.
 *
 * With an argument, it parses that string; without, it prints the defaults.
 * Each row is "keyword|envvar|compiled|val|label|dispchar|dispsize", with
 * NULL spelled "(null)", so the Rust side can compare the struct layout
 * field by field with rlibpq's PQconninfoOptions[] table.
 */
#include <stdio.h>
#include <stdlib.h>

#include "libpq-fe.h"

static const char *
or_null(const char *s)
{
	return s ? s : "(null)";
}

int
main(int argc, char *argv[])
{
	PQconninfoOption *opts;
	PQconninfoOption *opt;
	char	   *errmsg = (char *) "untouched";

	if (argc > 2)
		return 2;

	if (argc == 2)
	{
		opts = PQconninfoParse(argv[1], &errmsg);
		if (opts == NULL)
		{
			printf("error: %s", errmsg ? errmsg : "(null)\n");
			PQfreemem(errmsg);
			/* A NULL errmsg is allowed: the error is simply not reported. */
			printf("without errmsg: %s\n",
				   PQconninfoParse(argv[1], NULL) ? "set" : "NULL");
			return 1;
		}
		printf("errmsg: %s\n", or_null(errmsg));
	}
	else
		opts = PQconndefaults();

	if (opts == NULL)
		return 3;

	for (opt = opts; opt->keyword; opt++)
		printf("%s|%s|%s|%s|%s|%s|%d\n", opt->keyword, or_null(opt->envvar),
			   or_null(opt->compiled), or_null(opt->val), or_null(opt->label),
			   or_null(opt->dispchar), opt->dispsize);

	PQconninfoFree(opts);
	PQconninfoFree(NULL);
	printf("freed\n");
	return 0;
}
