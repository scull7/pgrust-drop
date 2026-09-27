//! The `\if` stack: `src/fe_utils/conditional.c` and
//! `src/include/fe_utils/conditional.h`.
//!
//! Upstream keeps a linked list of `IfStackElem`; a `Vec` whose last element
//! is the head is the same stack. Every function is pure state, so the whole
//! module is unit-tested without a lexer or a server.

use crate::scan::LexStateSave;

/// `ifState` (`conditional.h:29`): the state of one level of `\if` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfState {
    /// `IFSTATE_NONE`: not currently in an `\if` block.
    None,
    /// `IFSTATE_TRUE`: in an `\if` or `\elif` that is true and all parent
    /// branches (if any) are true.
    True,
    /// `IFSTATE_FALSE`: in an `\if` or `\elif` that is false but no true
    /// branch has yet been seen, and all parent branches (if any) are true.
    False,
    /// `IFSTATE_IGNORED`: in an `\elif` that follows a true branch, or the
    /// whole `\if` is a child of a false parent branch.
    Ignored,
    /// `IFSTATE_ELSE_TRUE`: in an `\else` that is true and all parent
    /// branches (if any) are true.
    ElseTrue,
    /// `IFSTATE_ELSE_FALSE`: in an `\else` that is false or ignored.
    ElseFalse,
}

/// `IfStackElem` (`conditional.h:57`), without the obsolete `paren_depth`
/// (`:61`), which the lexer state now carries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IfStackElem {
    /// `if_state`
    if_state: IfState,
    /// `query_len`: length of the query buffer at the last branch start;
    /// upstream's `-1` is `None`.
    query_len: Option<usize>,
    /// `lex_state`: lexer state at the last branch start.
    lex_state: Option<LexStateSave>,
}

/// `ConditionalStackData` (`conditional.h:66`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalStack {
    /// The last element is upstream's `head`.
    elems: Vec<IfStackElem>,
}

impl ConditionalStack {
    /// `conditional_stack_create()` (`conditional.c:18`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `conditional_stack_reset()` (`conditional.c:30`).
    pub fn reset(&mut self) {
        self.elems.clear();
    }

    /// `conditional_stack_depth()` (`conditional.c:87`).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.elems.len()
    }

    /// `conditional_stack_push()` (`conditional.c:53`).
    pub fn push(&mut self, new_state: IfState) {
        self.elems.push(IfStackElem {
            if_state: new_state,
            query_len: None,
            lex_state: None,
        });
    }

    /// `conditional_stack_pop()` (`conditional.c:70`): false if there was no
    /// branch to end.
    pub fn pop(&mut self) -> bool {
        self.elems.pop().is_some()
    }

    /// `conditional_stack_peek()` (`conditional.c:109`).
    #[must_use]
    pub fn peek(&self) -> IfState {
        self.elems.last().map_or(IfState::None, |e| e.if_state)
    }

    /// `conditional_stack_poke()` (`conditional.c:121`): false if there was
    /// no branch state to set.
    pub fn poke(&mut self, new_state: IfState) -> bool {
        match self.elems.last_mut() {
            Some(head) => {
                head.if_state = new_state;
                true
            }
            None => false,
        }
    }

    /// `conditional_stack_empty()` (`conditional.c:133`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elems.is_empty()
    }

    /// `conditional_active()` (`conditional.c:143`): should commands run
    /// normally — the current branch is active, or there is no open `\if`?
    #[must_use]
    pub fn active(&self) -> bool {
        matches!(
            self.peek(),
            IfState::None | IfState::True | IfState::ElseTrue
        )
    }

    /// `conditional_stack_set_query_len()` (`conditional.c:154`). Upstream
    /// asserts a non-empty stack; here an empty one is a no-op.
    pub fn set_query_len(&mut self, len: usize) {
        if let Some(head) = self.elems.last_mut() {
            head.query_len = Some(len);
        }
    }

    /// `conditional_stack_get_query_len()` (`conditional.c:165`): `None` if
    /// the stack is empty or the length was never saved.
    #[must_use]
    pub fn query_len(&self) -> Option<usize> {
        self.elems.last().and_then(|e| e.query_len)
    }

    /// `conditional_stack_set_lex_state()` (`conditional.c:179`).
    pub fn set_lex_state(&mut self, lex_state: LexStateSave) {
        if let Some(head) = self.elems.last_mut() {
            head.lex_state = Some(lex_state);
        }
    }

    /// `conditional_stack_get_lex_state()` (`conditional.c:193`).
    #[must_use]
    pub fn lex_state(&self) -> Option<&LexStateSave> {
        self.elems.last().and_then(|e| e.lex_state.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_stack_is_active_and_peeks_none() {
        let stack = ConditionalStack::new();
        assert!(stack.is_empty());
        assert!(stack.active());
        assert_eq!(stack.peek(), IfState::None);
        assert_eq!(stack.depth(), 0);
    }

    #[test]
    fn only_true_and_else_true_are_active() {
        // `conditional.c:147`.
        let mut stack = ConditionalStack::new();
        for (state, active) in [
            (IfState::True, true),
            (IfState::False, false),
            (IfState::Ignored, false),
            (IfState::ElseTrue, true),
            (IfState::ElseFalse, false),
        ] {
            stack.push(state);
            assert_eq!(stack.active(), active, "{state:?}");
            stack.pop();
        }
    }

    #[test]
    fn pop_and_poke_report_an_empty_stack() {
        let mut stack = ConditionalStack::new();
        assert!(!stack.pop());
        assert!(!stack.poke(IfState::True));
        stack.push(IfState::False);
        assert!(stack.poke(IfState::ElseTrue));
        assert_eq!(stack.peek(), IfState::ElseTrue);
        assert!(stack.pop());
        assert!(stack.is_empty());
    }

    #[test]
    fn the_saved_query_length_belongs_to_the_top_entry() {
        let mut stack = ConditionalStack::new();
        assert_eq!(stack.query_len(), None);
        stack.push(IfState::True);
        // A fresh entry has not saved one: upstream's -1 (`conditional.c:58`).
        assert_eq!(stack.query_len(), None);
        stack.set_query_len(7);
        stack.push(IfState::Ignored);
        assert_eq!(stack.query_len(), None);
        stack.set_query_len(3);
        assert_eq!(stack.query_len(), Some(3));
        stack.pop();
        assert_eq!(stack.query_len(), Some(7));
    }

    #[test]
    fn reset_pops_everything() {
        let mut stack = ConditionalStack::new();
        stack.push(IfState::True);
        stack.push(IfState::False);
        assert_eq!(stack.depth(), 2);
        stack.reset();
        assert!(stack.is_empty());
    }
}
