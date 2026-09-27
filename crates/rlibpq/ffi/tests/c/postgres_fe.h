/*
 * postgres_fe.h
 *		Stand-in for PostgreSQL's internal src/include/postgres_fe.h.
 *
 * The upstream test programs in this directory include "postgres_fe.h" for
 * the C library headers it drags in. The real header also pulls in c.h and
 * pg_config.h, which belong to a configured PostgreSQL source tree, not to
 * libpq's public API. This stand-in supplies only what the programs use, so
 * they compile verbatim against include/libpq-fe.h alone.
 */
#ifndef POSTGRES_FE_H
#define POSTGRES_FE_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#endif							/* POSTGRES_FE_H */
