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

int
main(void)
{
	char		buf[16];
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
	return 0;
}
