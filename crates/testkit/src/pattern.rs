//! The regular-expression subset the stolen TAP tests need.
//!
//! `command_like` and `command_fails_like`
//! (`src/test/perl/PostgreSQL/Test/Utils.pm:1006` and `:1059`) take a Perl
//! `qr//` and hand it to `Test::More::like`. To keep those assertions verbatim
//! rather than degrading them into substring checks, the patterns are matched
//! here by a small engine built on the standard library alone: the `regex`
//! crate is not in the approved dependency list (AGENTS.md, "stdlib first"),
//! and a substring or glob approximation would quietly weaken every stolen
//! test that uses `\d+`, `.*`, a character class or an anchor.
//!
//! What is supported is exactly what the upstream `t/*.pl` patterns use:
//! literals and escapes, `.`, character classes with ranges and negation, the
//! perl classes `\d \D \w \W \s \S`, groups (capturing and `(?:)`, both plain
//! grouping here), alternation, the quantifiers `* + ? {n} {n,} {n,m}` with an
//! optional lazy `?`, and the anchors `^` and `$` with Perl's semantics. Perl
//! writes its flags after the pattern (`qr/^2$/m`); Rust has no such syntax, so
//! they are written as a leading inline group instead, `(?m)^2$`. Anything
//! outside the subset — backreferences, lookaround, `\b`, named groups, `/x` —
//! is a [`PatternError`] at construction, never a silent mismatch.
//!
//! The matcher is a Thompson NFA simulated over the whole thread set (the
//! construction in Thompson, CACM 1968), not a backtracker: a pattern such as
//! `(a*)*b` against a long line of `a`s costs `O(len * program)` here instead
//! of exponential time, so a stolen test can never hang the suite. Because
//! only a yes/no answer is needed, no submatches are tracked and greediness
//! does not affect the verdict; lazy quantifiers are accepted and treated as
//! their greedy twins.

use std::fmt;

/// Why a pattern could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    /// The pattern ended in the middle of a construct.
    #[error("pattern ends unexpectedly after `{0}`")]
    UnexpectedEnd(String),
    /// A `(` with no `)`, or a `)` with no `(`.
    #[error("unbalanced `{0}` in pattern")]
    UnbalancedParen(char),
    /// A `[` whose `]` never arrived.
    #[error("character class is not closed with `]`")]
    UnterminatedClass,
    /// `{2,1}`, or a repetition count that does not fit in a `u32`.
    #[error("invalid repetition `{0}`")]
    InvalidRepetition(String),
    /// A quantifier with nothing to repeat, such as a leading `*`.
    #[error("quantifier `{0}` has nothing to repeat")]
    NothingToRepeat(char),
    /// An escape this subset does not implement.
    #[error("escape `\\{0}` is not supported by the testkit pattern subset")]
    UnsupportedEscape(char),
    /// A group extension this subset does not implement.
    #[error("group `(?{0}` is not supported by the testkit pattern subset")]
    UnsupportedGroup(String),
    /// A flag letter this subset does not implement.
    #[error("flag `{0}` is not supported; only `i`, `m` and `s` are")]
    UnsupportedFlag(char),
    /// The compiled program would be larger than [`MAX_PROGRAM_LEN`].
    #[error("pattern expands to more than {MAX_PROGRAM_LEN} instructions")]
    TooLarge,
}

/// Ceiling on the compiled program, so `a{1,1000000}` is an error at
/// construction instead of a gigabyte of instructions at match time.
pub const MAX_PROGRAM_LEN: usize = 10_000;

/// Flags, written as a leading `(?ims)` because Perl's trailing form has no
/// Rust equivalent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Flags {
    /// `i`: ASCII-case-insensitive matching.
    ignore_case: bool,
    /// `m`: `^` and `$` also match at internal line boundaries.
    multiline: bool,
    /// `s`: `.` also matches `\n`.
    dotall: bool,
}

/// One item of a character class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassItem {
    Single(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl ClassItem {
    fn matches(self, c: char, ignore_case: bool) -> bool {
        match self {
            ClassItem::Single(want) => c == want || (ignore_case && c.eq_ignore_ascii_case(&want)),
            ClassItem::Range(lo, hi) => {
                let in_range = |c: char| (lo..=hi).contains(&c);
                in_range(c)
                    || (ignore_case
                        && (in_range(c.to_ascii_lowercase()) || in_range(c.to_ascii_uppercase())))
            }
            // Perl's \d, \w and \s are ASCII here: every pattern stolen from
            // the TAP suite applies them to ASCII tool output.
            ClassItem::Digit(positive) => c.is_ascii_digit() == positive,
            ClassItem::Word(positive) => (c.is_ascii_alphanumeric() || c == '_') == positive,
            // Perl's \s has included the vertical tab since 5.18;
            // `is_ascii_whitespace` still leaves it out.
            ClassItem::Space(positive) => (c.is_ascii_whitespace() || c == '\x0b') == positive,
        }
    }
}

/// A `[...]` class.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

impl Class {
    fn matches(&self, c: char, ignore_case: bool) -> bool {
        self.items.iter().any(|item| item.matches(c, ignore_case)) != self.negated
    }
}

/// The parsed shape of a pattern, before compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Empty,
    Literal(char),
    /// `.`
    AnyChar,
    Class(Class),
    /// `^`
    Start,
    /// `$`
    End,
    Concat(Vec<Node>),
    Alternate(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
    },
}

/// One NFA instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Inst {
    Char(char),
    Class(Class),
    /// `.` with `s` off: anything but `\n`.
    AnyButNewline,
    /// `.` with `s` on.
    AnyChar,
    /// Zero-width `^`.
    Start,
    /// Zero-width `$`.
    End,
    Split(usize, usize),
    Jump(usize),
    Match,
}

/// A compiled pattern.
///
/// Construct it from the same text the Perl test carries between the `qr//`
/// slashes, with the trailing flags moved to a leading `(?…)` group.
#[derive(Debug, Clone)]
pub struct Pattern {
    source: String,
    flags: Flags,
    program: Vec<Inst>,
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl PartialEq for Pattern {
    /// Two patterns are equal when they were written the same way; this exists
    /// so [`crate::Violation`] can derive `PartialEq` for its unit tests.
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Pattern {}

impl Pattern {
    /// Compile `source`.
    ///
    /// # Errors
    /// [`PatternError`] when the pattern is malformed or uses a construct
    /// outside the supported subset.
    pub fn new(source: &str) -> Result<Self, PatternError> {
        let mut parser = Parser::new(source);
        let flags = parser.leading_flags()?;
        let node = parser.parse_alternation()?;
        if let Some(c) = parser.peek() {
            debug_assert_eq!(c, ')');
            return Err(PatternError::UnbalancedParen(')'));
        }
        let mut program = Vec::new();
        compile(&node, flags, &mut program)?;
        program.push(Inst::Match);
        Ok(Self {
            source: source.to_owned(),
            flags,
            program,
        })
    }

    /// The text the pattern was written as.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Does `text` contain a match, the way Perl's `=~` asks it?
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let mut current = ThreadList::new(self.program.len());
        let mut next = ThreadList::new(self.program.len());
        for pos in 0..=chars.len() {
            // Unanchored search: a fresh attempt may start at any position.
            self.add_thread(&mut current, 0, pos, &chars);
            let mut index = 0;
            while index < current.pcs.len() {
                let pc = current.pcs[index];
                index += 1;
                let consumed = match &self.program[pc] {
                    Inst::Match => return true,
                    Inst::Char(want) => chars.get(pos).is_some_and(|&c| {
                        c == *want || (self.flags.ignore_case && c.eq_ignore_ascii_case(want))
                    }),
                    Inst::Class(class) => chars
                        .get(pos)
                        .is_some_and(|&c| class.matches(c, self.flags.ignore_case)),
                    Inst::AnyButNewline => chars.get(pos).is_some_and(|&c| c != '\n'),
                    Inst::AnyChar => pos < chars.len(),
                    // Already followed by add_thread; they are in the list
                    // only so the same state is never expanded twice.
                    Inst::Split(..) | Inst::Jump(_) | Inst::Start | Inst::End => false,
                };
                if consumed {
                    self.add_thread(&mut next, pc + 1, pos + 1, &chars);
                }
            }
            std::mem::swap(&mut current, &mut next);
            next.clear();
        }
        false
    }

    /// Follow every zero-width instruction reachable from `pc` at `pos`, so the
    /// thread list holds only instructions that consume a character (or
    /// `Match`). Iterative: a pattern may nest deeply enough to blow a
    /// recursive closure's stack.
    fn add_thread(&self, list: &mut ThreadList, pc: usize, pos: usize, chars: &[char]) {
        let mut stack = vec![pc];
        while let Some(pc) = stack.pop() {
            if !list.insert(pc) {
                continue;
            }
            match &self.program[pc] {
                Inst::Jump(target) => stack.push(*target),
                Inst::Split(a, b) => {
                    // Order is irrelevant: the verdict is a yes/no, so
                    // greediness cannot change it.
                    stack.push(*b);
                    stack.push(*a);
                }
                // A failed assertion simply kills the thread: the state stays
                // in the list, so it is never expanded again at this position.
                Inst::Start if self.at_start(pos, chars) => stack.push(pc + 1),
                Inst::End if self.at_end(pos, chars) => stack.push(pc + 1),
                _ => {}
            }
        }
    }

    /// Perl `^`: the start of the text, and after any `\n` under `/m` — but
    /// not after a `\n` that ends the text. Perl's `MBOL` opcode requires
    /// `!NEXTCHR_IS_EOS` (`regexec.c`), so `"a\n" =~ /^$/m` does not match;
    /// without that guard a stolen `/m`-anchored empty-line pattern would pass
    /// on output Perl rejects, which is exactly the fidelity this module
    /// promises.
    fn at_start(&self, pos: usize, chars: &[char]) -> bool {
        pos == 0 || (self.flags.multiline && pos < chars.len() && chars[pos - 1] == '\n')
    }

    /// Perl `$`: the end of the text, or just before a newline that ends the
    /// text; under `/m`, before any `\n`. The "before the final newline" case
    /// is not a nicety — every stolen pattern anchored with `$` is matched
    /// against tool output that ends in one.
    fn at_end(&self, pos: usize, chars: &[char]) -> bool {
        if pos == chars.len() {
            return true;
        }
        if chars[pos] != '\n' {
            return false;
        }
        self.flags.multiline || pos + 1 == chars.len()
    }
}

/// The set of NFA states alive at one position, deduplicated in `O(1)`.
struct ThreadList {
    pcs: Vec<usize>,
    seen: Vec<bool>,
}

impl ThreadList {
    fn new(len: usize) -> Self {
        Self {
            pcs: Vec::with_capacity(len),
            seen: vec![false; len],
        }
    }

    /// Add `pc` unless it is already in the list; `true` when it was added.
    fn insert(&mut self, pc: usize) -> bool {
        if self.seen[pc] {
            return false;
        }
        self.seen[pc] = true;
        self.pcs.push(pc);
        true
    }

    fn clear(&mut self) {
        for &pc in &self.pcs {
            self.seen[pc] = false;
        }
        self.pcs.clear();
    }
}

/// Emit `node` into `program`.
fn compile(node: &Node, flags: Flags, program: &mut Vec<Inst>) -> Result<(), PatternError> {
    if program.len() > MAX_PROGRAM_LEN {
        return Err(PatternError::TooLarge);
    }
    match node {
        Node::Empty => {}
        Node::Literal(c) => program.push(Inst::Char(*c)),
        Node::AnyChar => program.push(if flags.dotall {
            Inst::AnyChar
        } else {
            Inst::AnyButNewline
        }),
        Node::Class(class) => program.push(Inst::Class(class.clone())),
        Node::Start => program.push(Inst::Start),
        Node::End => program.push(Inst::End),
        Node::Concat(nodes) => {
            for node in nodes {
                compile(node, flags, program)?;
            }
        }
        Node::Alternate(branches) => compile_alternate(branches, flags, program)?,
        Node::Repeat { node, min, max } => compile_repeat(node, *min, *max, flags, program)?,
    }
    if program.len() > MAX_PROGRAM_LEN {
        return Err(PatternError::TooLarge);
    }
    Ok(())
}

fn compile_alternate(
    branches: &[Node],
    flags: Flags,
    program: &mut Vec<Inst>,
) -> Result<(), PatternError> {
    let mut jumps = Vec::new();
    for (index, branch) in branches.iter().enumerate() {
        let last = index + 1 == branches.len();
        if last {
            compile(branch, flags, program)?;
        } else {
            let split = program.len();
            program.push(Inst::Split(0, 0));
            let body = program.len();
            compile(branch, flags, program)?;
            jumps.push(program.len());
            program.push(Inst::Jump(0));
            let rest = program.len();
            program[split] = Inst::Split(body, rest);
        }
    }
    let end = program.len();
    for jump in jumps {
        program[jump] = Inst::Jump(end);
    }
    Ok(())
}

fn compile_repeat(
    node: &Node,
    min: u32,
    max: Option<u32>,
    flags: Flags,
    program: &mut Vec<Inst>,
) -> Result<(), PatternError> {
    // The mandatory copies.
    for _ in 0..min {
        compile(node, flags, program)?;
    }
    match max {
        // `x{n,}`: the last copy loops.
        None => {
            let split = program.len();
            program.push(Inst::Split(0, 0));
            let body = program.len();
            compile(node, flags, program)?;
            program.push(Inst::Jump(split));
            let rest = program.len();
            program[split] = Inst::Split(body, rest);
        }
        // `x{n,m}`: m - n optional copies, each able to skip the rest.
        Some(max) => {
            let optional = max.saturating_sub(min);
            let mut splits = Vec::new();
            for _ in 0..optional {
                splits.push(program.len());
                program.push(Inst::Split(0, 0));
                let body = program.len();
                compile(node, flags, program)?;
                let split = splits[splits.len() - 1];
                program[split] = Inst::Split(body, 0);
                if program.len() > MAX_PROGRAM_LEN {
                    return Err(PatternError::TooLarge);
                }
            }
            let end = program.len();
            for split in splits {
                if let Inst::Split(body, _) = program[split] {
                    program[split] = Inst::Split(body, end);
                }
            }
        }
    }
    Ok(())
}

/// Recursive-descent parser over the pattern text.
struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn consumed(&self) -> String {
        self.chars[..self.pos].iter().collect()
    }

    /// `(?ims)` at the very start, standing in for Perl's trailing `/ims`.
    fn leading_flags(&mut self) -> Result<Flags, PatternError> {
        let mut flags = Flags::default();
        if self.peek() != Some('(') || self.chars.get(1) != Some(&'?') {
            return Ok(flags);
        }
        let mut scan = 2;
        let mut parsed = Flags::default();
        while let Some(&c) = self.chars.get(scan) {
            match c {
                'i' => parsed.ignore_case = true,
                'm' => parsed.multiline = true,
                's' => parsed.dotall = true,
                ')' => {
                    self.pos = scan + 1;
                    flags = parsed;
                    return Ok(flags);
                }
                // `(?:` and friends are groups, not a flag block; leave them.
                ':' | '=' | '!' | '<' | '\'' | 'P' | '#' => return Ok(flags),
                _ => return Err(PatternError::UnsupportedFlag(c)),
            }
            scan += 1;
        }
        Ok(flags)
    }

    fn parse_alternation(&mut self) -> Result<Node, PatternError> {
        let mut branches = vec![self.parse_concat()?];
        while self.eat('|') {
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alternate(branches)
        })
    }

    fn parse_concat(&mut self) -> Result<Node, PatternError> {
        let mut nodes = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            nodes.push(self.parse_repeat()?);
        }
        Ok(match nodes.len() {
            0 => Node::Empty,
            1 => nodes.pop().unwrap_or(Node::Empty),
            _ => Node::Concat(nodes),
        })
    }

    fn parse_repeat(&mut self) -> Result<Node, PatternError> {
        let atom = self.parse_atom()?;
        let (min, max) = match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => match self.parse_counted()? {
                Some(bounds) => bounds,
                // Perl treats a `{` that starts no valid quantifier as a
                // literal; `qr/:\{\?VERBOSITY} /` relies on the same for `}`.
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        if matches!(atom, Node::Start | Node::End) {
            // `^*` is a Perl warning and a nonsense assertion; refuse it.
            return Err(PatternError::NothingToRepeat(
                if matches!(atom, Node::Start) {
                    '^'
                } else {
                    '$'
                },
            ));
        }
        // A lazy or possessive marker changes which match is found, never
        // whether one exists, and only the latter is asked here.
        self.eat('?');
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
        })
    }

    /// `{n}`, `{n,}` or `{n,m}`. `Ok(None)` means "this `{` is a literal".
    fn parse_counted(&mut self) -> Result<Option<(u32, Option<u32>)>, PatternError> {
        let open = self.pos;
        self.pos += 1;
        let min_text = self.take_digits();
        if min_text.is_empty() {
            self.pos = open;
            return Ok(None);
        }
        let comma = self.eat(',');
        let max_text = if comma {
            self.take_digits()
        } else {
            min_text.clone()
        };
        if !self.eat('}') {
            self.pos = open;
            return Ok(None);
        }
        let text: String = self.chars[open..self.pos].iter().collect();
        let min: u32 = min_text
            .parse()
            .map_err(|_| PatternError::InvalidRepetition(text.clone()))?;
        let max = if comma && max_text.is_empty() {
            None
        } else {
            let max: u32 = max_text
                .parse()
                .map_err(|_| PatternError::InvalidRepetition(text.clone()))?;
            if max < min {
                return Err(PatternError::InvalidRepetition(text));
            }
            Some(max)
        };
        Ok(Some((min, max)))
    }

    fn take_digits(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn parse_atom(&mut self) -> Result<Node, PatternError> {
        match self.next() {
            None => Ok(Node::Empty),
            Some('.') => Ok(Node::AnyChar),
            Some('^') => Ok(Node::Start),
            Some('$') => Ok(Node::End),
            Some('(') => self.parse_group(),
            Some(')') => Err(PatternError::UnbalancedParen(')')),
            Some('[') => Ok(Node::Class(self.parse_class()?)),
            Some('\\') => self.parse_escape(),
            Some(c @ ('*' | '+' | '?')) => Err(PatternError::NothingToRepeat(c)),
            Some(c) => Ok(Node::Literal(c)),
        }
    }

    fn parse_group(&mut self) -> Result<Node, PatternError> {
        if self.eat('?') {
            // `(?:…)` is the only extension in the subset: capturing is not
            // modelled, so it and a bare group compile identically.
            if !self.eat(':') {
                let rest: String = self.chars[self.pos..].iter().take(2).collect::<String>();
                return Err(PatternError::UnsupportedGroup(rest));
            }
        }
        let inner = self.parse_alternation()?;
        if !self.eat(')') {
            return Err(PatternError::UnbalancedParen('('));
        }
        Ok(inner)
    }

    fn parse_class(&mut self) -> Result<Class, PatternError> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        // A `]` first is a literal `]`, as in Perl.
        if self.eat(']') {
            items.push(ClassItem::Single(']'));
        }
        loop {
            let Some(c) = self.next() else {
                return Err(PatternError::UnterminatedClass);
            };
            if c == ']' {
                return Ok(Class { negated, items });
            }
            let low = if c == '\\' {
                match self.class_escape()? {
                    ClassEscape::Item(item) => {
                        items.push(item);
                        continue;
                    }
                    ClassEscape::Literal(c) => c,
                }
            } else {
                c
            };
            // A `-` before `]` is a literal `-`.
            if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                self.pos += 1;
                let Some(high) = self.next() else {
                    return Err(PatternError::UnterminatedClass);
                };
                let high = if high == '\\' {
                    match self.class_escape()? {
                        ClassEscape::Literal(c) => c,
                        ClassEscape::Item(_) => {
                            return Err(PatternError::InvalidRepetition("[a-\\d]".to_owned()));
                        }
                    }
                } else {
                    high
                };
                if high < low {
                    return Err(PatternError::InvalidRepetition(format!("[{low}-{high}]")));
                }
                items.push(ClassItem::Range(low, high));
            } else {
                items.push(ClassItem::Single(low));
            }
        }
    }

    fn class_escape(&mut self) -> Result<ClassEscape, PatternError> {
        let Some(c) = self.next() else {
            return Err(PatternError::UnexpectedEnd(self.consumed()));
        };
        Ok(match c {
            'd' => ClassEscape::Item(ClassItem::Digit(true)),
            'D' => ClassEscape::Item(ClassItem::Digit(false)),
            'w' => ClassEscape::Item(ClassItem::Word(true)),
            'W' => ClassEscape::Item(ClassItem::Word(false)),
            's' => ClassEscape::Item(ClassItem::Space(true)),
            'S' => ClassEscape::Item(ClassItem::Space(false)),
            other => ClassEscape::Literal(control_escape(other)?),
        })
    }

    fn parse_escape(&mut self) -> Result<Node, PatternError> {
        let Some(c) = self.next() else {
            return Err(PatternError::UnexpectedEnd(self.consumed()));
        };
        let item = match c {
            'd' => ClassItem::Digit(true),
            'D' => ClassItem::Digit(false),
            'w' => ClassItem::Word(true),
            'W' => ClassItem::Word(false),
            's' => ClassItem::Space(true),
            'S' => ClassItem::Space(false),
            other => return Ok(Node::Literal(control_escape(other)?)),
        };
        Ok(Node::Class(Class {
            negated: false,
            items: vec![item],
        }))
    }
}

/// What a backslash inside a class turned out to be.
enum ClassEscape {
    Item(ClassItem),
    Literal(char),
}

/// The one-letter escapes, plus "a backslashed punctuation mark is itself".
fn control_escape(c: char) -> Result<char, PatternError> {
    Ok(match c {
        'a' => '\x07',
        'e' => '\x1b',
        'f' => '\x0c',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\x0b',
        '0' => '\0',
        // Backreferences, \b, \Q…\E and the rest are outside the subset: they
        // must fail loudly rather than match something almost right.
        c if c.is_ascii_alphanumeric() => return Err(PatternError::UnsupportedEscape(c)),
        c => c,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, text: &str) -> bool {
        Pattern::new(pattern)
            .unwrap_or_else(|err| panic!("compile {pattern}: {err}"))
            .is_match(text)
    }

    #[test]
    fn a_literal_matches_anywhere_in_the_text() {
        assert!(matches(
            "Copyright",
            "initdb (PostgreSQL) 18.6\nCopyright (c)\n"
        ));
        assert!(!matches("Copyright", "initdb (PostgreSQL) 18.6\n"));
    }

    #[test]
    fn an_escaped_metacharacter_is_a_literal() {
        // qr/public\.tab1/ must not accept "publicXtab1".
        assert!(matches(r"public\.tab1", "table public.tab1 "));
        assert!(!matches(r"public\.tab1", "table publicXtab1 "));
    }

    #[test]
    fn dot_matches_any_character_but_newline() {
        assert!(matches("foo.*bar", "fooXYbar"));
        assert!(!matches("foo.*bar", "foo\nbar"));
        // qr/foo.*bar/s
        assert!(matches("(?s)foo.*bar", "foo\nbar"));
    }

    #[test]
    fn digits_and_spaces_have_their_perl_meaning() {
        // qr/^Time: \d+[.,]\d\d\d ms/m
        let pattern = r"(?m)^Time: \d+[.,]\d\d\d ms";
        assert!(matches(pattern, "SELECT 1\nTime: 12,345 ms\n"));
        assert!(matches(pattern, "SELECT 1\nTime: 0.000 ms\n"));
        assert!(!matches(pattern, "SELECT 1\nTime: ms\n"));
        // qr/DROP PUBLICATION\s+some_publication /
        assert!(matches(
            r"DROP PUBLICATION\s+some_publication ",
            "DROP PUBLICATION   some_publication ;"
        ));
    }

    #[test]
    fn a_class_range_and_its_negation() {
        assert!(matches("[a-f]+", "xxdeadbeef"));
        assert!(!matches("[a-f]+", "xyz"));
        assert!(matches("[^0-9]", "a"));
        assert!(!matches("[^0-9]", "7"));
        // A `-` just before `]` is a literal.
        assert!(matches("[a-]", "-"));
    }

    #[test]
    fn quantifiers_cover_star_plus_question_and_counts() {
        // qr/plpg\a?sql\./ — the \a is BEL, optional.
        assert!(matches(r"plpg\a?sql\.", "plpgsql."));
        assert!(matches(r"plpg\a?sql\.", "plpg\u{7}sql."));
        assert!(matches("ab{2,3}c", "abbc"));
        assert!(matches("ab{2,3}c", "abbbc"));
        assert!(!matches("ab{2,3}c", "abc"));
        assert!(!matches("ab{2,3}c", "abbbbc"));
        assert!(matches("ab{2}c", "abbc"));
        assert!(matches("ab{2,}c", "abbbbbbc"));
    }

    #[test]
    fn alternation_and_groups() {
        assert!(matches("(cat|dog)s?", "two dogs"));
        assert!(matches("(?:ab)+c", "ababc"));
        assert!(!matches("(cat|dog)s", "two birds"));
    }

    #[test]
    fn anchors_follow_perl_not_rust() {
        // qr/^123$/ against output that ends in a newline: Perl's `$` matches
        // before that final newline, so this must hold.
        assert!(matches("^123$", "123\n"));
        assert!(matches("^123$", "123"));
        assert!(!matches("^123$", "x123\n"));
        // Without /m an interior line does not anchor.
        assert!(!matches("^2$", "1\n2\n3\n"));
        // qr/^2$/m does.
        assert!(matches("(?m)^2$", "1\n2\n3\n"));
        assert!(matches("(?m)^WORK_MEM = ", "a\nWORK_MEM = 512\n"));
    }

    #[test]
    fn a_trailing_anchor_after_a_number() {
        // qr/unexpected PQresultStatus: 8$/
        assert!(matches(
            "unexpected PQresultStatus: 8$",
            "client: unexpected PQresultStatus: 8\n"
        ));
        assert!(!matches(
            "unexpected PQresultStatus: 8$",
            "client: unexpected PQresultStatus: 80\n"
        ));
    }

    #[test]
    fn multiline_start_refuses_the_position_after_a_trailing_newline() {
        // perl -e 'exit("a\n" =~ /^$/m ? 0 : 1)' is nomatch: /m anchors at the
        // start of a *line*, and the position past the final newline starts no
        // line. Matching there would let an empty-line pattern pass on output
        // Perl rejects.
        assert!(!matches("(?m)^$", "a\n"));
        assert!(!matches(r"(?m)^\s*$", "a\n"));
        // A genuinely empty line in the middle still matches, both ways.
        assert!(matches("(?m)^$", "a\n\nb\n"));
        assert!(matches("(?m)^$", "a\n\n"));
        // And an ordinary /m anchor is unaffected.
        assert!(matches("(?m)^b", "a\nb\n"));
        assert!(matches("(?m)^a", "a\n"));
        // Without /m the empty pattern still matches at position 0.
        assert!(matches("^$", "\n"));
    }

    #[test]
    fn perl_whitespace_includes_the_vertical_tab() {
        // perl -e 'exit("\x0b" =~ /\s/ ? 0 : 1)' matches.
        assert!(matches(r"\s", "\u{b}"));
        assert!(matches(r"[\s]", "\u{b}"));
        assert!(!matches(r"\S", "\u{b}"));
        // The rest of the ASCII set is unchanged.
        for space in [" ", "\t", "\n", "\r", "\u{c}"] {
            assert!(matches(r"\s", space), "{space:?}");
        }
        assert!(!matches(r"\s", "x"));
    }

    #[test]
    fn ignore_case_is_opt_in() {
        assert!(!matches("work_mem", "WORK_MEM = 512"));
        assert!(matches("(?i)work_mem", "WORK_MEM = 512"));
        assert!(matches("(?i)[a-z]+", "ABC"));
    }

    #[test]
    fn a_brace_that_starts_no_quantifier_is_a_literal() {
        // qr/:\{\?VERBOSITY} /
        assert!(matches(r":\{\?VERBOSITY} ", ":{?VERBOSITY} "));
        assert!(matches("a{b", "a{b"));
    }

    #[test]
    fn an_empty_pattern_matches_anything() {
        // qr// is used upstream as "no expectation".
        assert!(matches("", "whatever"));
        assert!(matches("", ""));
    }

    #[test]
    fn nested_quantifiers_do_not_blow_up() {
        // A backtracker needs exponential time here; the NFA simulation is
        // linear, so this test finishing at all is the assertion.
        let text = "a".repeat(2_000);
        assert!(!matches("(a*)*b", &text));
        assert!(matches("(a*)*b", &format!("{text}b")));
    }

    #[test]
    fn unsupported_constructs_are_errors_not_silent_mismatches() {
        assert_eq!(
            Pattern::new(r"(a)\1"),
            Err(PatternError::UnsupportedEscape('1'))
        );
        assert_eq!(
            Pattern::new(r"\bword"),
            Err(PatternError::UnsupportedEscape('b'))
        );
        assert!(matches!(
            Pattern::new("(?=ahead)"),
            Err(PatternError::UnsupportedGroup(_))
        ));
        assert_eq!(
            Pattern::new("(?x)spaced"),
            Err(PatternError::UnsupportedFlag('x'))
        );
    }

    #[test]
    fn malformed_patterns_are_errors() {
        assert_eq!(
            Pattern::new("(unclosed"),
            Err(PatternError::UnbalancedParen('('))
        );
        assert_eq!(
            Pattern::new("closed)"),
            Err(PatternError::UnbalancedParen(')'))
        );
        assert_eq!(Pattern::new("[a-z"), Err(PatternError::UnterminatedClass));
        assert_eq!(
            Pattern::new("*star"),
            Err(PatternError::NothingToRepeat('*'))
        );
        assert_eq!(
            Pattern::new("a{3,2}"),
            Err(PatternError::InvalidRepetition("{3,2}".to_owned()))
        );
        assert!(matches!(
            Pattern::new(r"trailing\"),
            Err(PatternError::UnexpectedEnd(_))
        ));
    }

    #[test]
    fn a_huge_repetition_is_refused_at_compile_time() {
        assert_eq!(Pattern::new("a{1,999999}"), Err(PatternError::TooLarge));
    }

    #[test]
    fn a_pattern_prints_as_it_was_written() {
        let pattern = Pattern::new(r"(?m)^Time: \d+ ms").expect("compiles");
        assert_eq!(pattern.to_string(), r"(?m)^Time: \d+ ms");
        assert_eq!(pattern.as_str(), r"(?m)^Time: \d+ ms");
    }
}
