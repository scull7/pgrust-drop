-- tests for \if ... \endif

\if true
  select 'okay';
  select 'still okay';
\else
  not okay;
  still not okay
\endif

-- at this point query buffer should still have last valid line
\g

-- \if should work okay on part of a query
select
  \if true
    42
  \else
    (bogus
  \endif
  forty_two;

select \if false \\ (bogus \else \\ 42 \endif \\ forty_two;

-- test a large nested if using a variety of true-equivalents
\if true
	\if 1
		\if yes
			\if on
				\echo 'all true'
			\else
				\echo 'should not print #1-1'
			\endif
		\else
			\echo 'should not print #1-2'
		\endif
	\else
		\echo 'should not print #1-3'
	\endif
\else
	\echo 'should not print #1-4'
\endif

-- test a variety of false-equivalents in an if/elif/else structure
\if false
	\echo 'should not print #2-1'
\elif 0
	\echo 'should not print #2-2'
\elif no
	\echo 'should not print #2-3'
\elif off
	\echo 'should not print #2-4'
\else
	\echo 'all false'
\endif

-- test true-false elif after initial true branch
\if true
	\echo 'should print #2-5'
\elif true
	\echo 'should not print #2-6'
\elif false
	\echo 'should not print #2-7'
\else
	\echo 'should not print #2-8'
\endif

-- test simple true-then-else
\if true
	\echo 'first thing true'
\else
	\echo 'should not print #3-1'
\endif

-- test simple false-true-else
\if false
	\echo 'should not print #4-1'
\elif true
	\echo 'second thing true'
\else
	\echo 'should not print #5-1'
\endif

-- invalid boolean expressions are false
\if invalid boolean expression
	\echo 'will not print #6-1'
\else
	\echo 'will print anyway #6-2'
\endif

-- test un-matched endif
\endif

-- test un-matched else
\else

-- test un-matched elif
\elif

-- test double-else error
\if true
\else
\else
\endif

-- test elif out-of-order
\if false
\else
\elif
\endif

-- test if-endif matching in a false branch
\if false
    \if false
        \echo 'should not print #7-1'
    \else
        \echo 'should not print #7-2'
    \endif
    \echo 'should not print #7-3'
\else
    \echo 'should print #7-4'
\endif

-- show that vars and backticks are not expanded when ignoring extra args
\set foo bar
\echo :foo :'foo' :"foo"
\pset fieldsep | `nosuchcommand` :foo :'foo' :"foo"

-- show that vars and backticks are not expanded and commands are ignored
-- when in a false if-branch
\set try_to_quit '\\q'
\if false
	:try_to_quit
	\echo `nosuchcommand` :foo :'foo' :"foo"
	\pset fieldsep | `nosuchcommand` :foo :'foo' :"foo"
	\a
	SELECT $1 \bind 1 \g
	\bind_named stmt1 1 2 \g
	\C arg1
	\c arg1 arg2 arg3 arg4
	\cd arg1
	\close_prepared stmt1
	\conninfo
	\copy arg1 arg2 arg3 arg4 arg5 arg6
	\copyright
	SELECT 1 as one, 2, 3 \crosstabview
	\dt arg1
	\e arg1 arg2
	\ef whole_line
	\ev whole_line
	\echo arg1 arg2 arg3 arg4 arg5
	\echo arg1
	\encoding arg1
	\endpipeline
	\errverbose
	\f arg1
	\flush
	\flushrequest
	\g arg1
	\gx arg1
	\gexec
	\getresults
	SELECT 1 AS one \gset
	\h
	\?
	\html
	\i arg1
	\ir arg1
	\l arg1
	\lo arg1 arg2
	\lo_list
	\o arg1
	\p
	SELECT 1 \parse
	\password arg1
	\prompt arg1 arg2
	\pset arg1 arg2
	\q
	\reset
	\restrict test
	\s arg1
	\sendpipeline
	\set arg1 arg2 arg3 arg4 arg5 arg6 arg7
	\setenv arg1 arg2
	\sf whole_line
	\sv whole_line
	\startpipeline
	\syncpipeline
	\t arg1
	\T arg1
	\timing arg1
	\unrestrict not_valid
	\unset arg1
	\w arg1
	\watch arg1 arg2
	\x arg1
	-- \else here is eaten as part of OT_FILEPIPE argument
	\w |/no/such/file \else
	-- \endif here is eaten as part of whole-line argument
	\! whole_line \endif
	\z
\else
	\echo 'should print #8-1'
\endif

-- test that begin/end matching ignores to-be-ignored text
create function silly_function(int) returns int
begin atomic select $1;
\if false
end
\endif
;
end;
\sf silly_function(int)
drop function silly_function(int);

-- :{?...} defined variable test
\set i 1
\if :{?i}
  \echo '#9-1 ok, variable i is defined'
\else
  \echo 'should not print #9-2'
\endif

\if :{?no_such_variable}
  \echo 'should not print #10-1'
\else
  \echo '#10-2 ok, variable no_such_variable is not defined'
\endif

SELECT :{?i} AS i_is_defined;

SELECT NOT :{?no_such_var} AS no_such_var_is_not_defined;
