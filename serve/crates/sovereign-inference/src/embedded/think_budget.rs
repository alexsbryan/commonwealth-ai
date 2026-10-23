//! Budget forcing for a thinking turn: count the tokens of the open
//! `<think>` block and, at the request's `think_budget` (default
//! [`THINK_BUDGET`]), close it so the model answers from the reasoning it
//! has. The MTP decode path reads it; the sync and stream paths in
//! model_slot.rs still carry their own copy, which never seeds from the
//! turn opener (see [`ThinkBudget::new`]).

use sovereign_contracts::types::CompletionRequest;

use super::chat_template::think_budget_applies;
use super::gates::push_sliding_tail;
use super::prompt_helpers::THINK_BUDGET;

/// What the decode loop writes when the budget is spent.
pub(crate) const CLOSE: &str = "\n</think>\n\n";

/// Whether the prompt left a think block open (`Prompt::turn_opener`): a
/// template that opens it in the generation prompt never has the model write
/// the tag, so the turn starts inside it.
pub(crate) fn opens_think(turn_opener: &str) -> bool {
    turn_opener.trim_end().ends_with("<think>")
}

/// A schema constraint's wait for the think block the prompt opened: the
/// reasoning is free, and what follows `</think>` is held to the schema.
/// Masked from the first token instead, the model wrote its answer inside the
/// block and `content` came back empty (note 068f6b1f).
#[derive(Default)]
pub(crate) struct ThinkWait {
    tail: String,
}

impl ThinkWait {
    /// Whether the block has closed once `bytes`, one accepted token's, are
    /// seen; the tag may span tokens.
    pub(crate) fn closes(&mut self, bytes: &[u8]) -> bool {
        push_sliding_tail(&mut self.tail, &String::from_utf8_lossy(bytes));
        self.tail.contains("</think>")
    }
}

pub(crate) struct ThinkBudget {
    tail: String,
    in_think: bool,
    tokens: usize,
    budget: usize,
    applies: bool,
    fired: bool,
}

impl ThinkBudget {
    /// `turn_opener` is the prompt's own open `<think>` (`Prompt::turn_opener`):
    /// a template that opens the block in the generation prompt never has
    /// the model write the tag, so the turn starts inside it.
    pub(crate) fn new(request: &CompletionRequest, turn_opener: &str) -> Self {
        Self {
            tail: String::with_capacity(32),
            in_think: opens_think(turn_opener),
            tokens: 0,
            budget: request.think_budget.unwrap_or(THINK_BUDGET),
            applies: think_budget_applies(request),
            fired: false,
        }
    }

    /// One decoded piece of the model's own text.
    pub(crate) fn observe(&mut self, piece: &str) {
        push_sliding_tail(&mut self.tail, piece);
        if !self.in_think && self.tail.contains("<think>") {
            self.in_think = true;
        } else if self.in_think && self.tail.contains("</think>") {
            self.in_think = false;
        }
        if self.in_think {
            self.tokens += 1;
        }
    }

    /// Whether the loop must write [`CLOSE`] now. Fires at most once per turn.
    pub(crate) fn due(&mut self) -> bool {
        if self.applies && self.in_think && !self.fired && self.tokens >= self.budget {
            self.fired = true;
            self.in_think = false;
            return true;
        }
        false
    }

    pub(crate) fn tokens(&self) -> usize {
        self.tokens
    }

    pub(crate) fn budget(&self) -> usize {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::PromptShape;

    /// Failing input, named: note 068f6b1f, a schema request with thinking
    /// on returned `content: ""` because the mask held the think block to the
    /// schema. The constraint waits only for a block the prompt opened, and
    /// arms on its close however the tag is tokenized.
    #[test]
    fn the_constraint_waits_for_the_think_block_the_prompt_opened() {
        assert!(opens_think("<|im_start|>assistant\n<think>\n"));
        assert!(!opens_think(""));
        assert!(!opens_think("<think>\n\n</think>\n\n"));
        let closes = |pieces: &[&[u8]]| {
            let mut wait = ThinkWait::default();
            pieces.iter().map(|p| wait.closes(p)).collect::<Vec<_>>()
        };
        assert_eq!(closes(&[b"the deal is won", b"</think>"]), [false, true]);
        assert_eq!(closes(&[b"</th", b"ink>\n\n"]), [false, true]);
        assert_eq!(closes(&[b"<think>", b"a tag: </thin"]), [false, false]);
    }

    fn request(budget: Option<usize>) -> CompletionRequest {
        CompletionRequest {
            think_budget: budget,
            ..CompletionRequest::default()
        }
    }

    #[test]
    fn a_template_opened_think_block_is_closed_at_its_budget() {
        let mut t = ThinkBudget::new(&request(Some(3)), "<think>\n");
        t.observe("a");
        t.observe("b");
        assert!(!t.due(), "two tokens are under a budget of three");
        t.observe("c");
        assert!(t.due(), "the third token spends the budget");
        assert!(!t.due(), "it fires once");
    }

    #[test]
    fn a_block_the_model_opens_itself_is_counted_from_its_tag() {
        let mut t = ThinkBudget::new(&request(Some(2)), "");
        t.observe("answer ");
        assert!(!t.due());
        t.observe("<think>");
        t.observe("x");
        assert!(t.due());
    }

    #[test]
    fn a_closed_block_is_never_forced() {
        let mut t = ThinkBudget::new(&request(Some(2)), "<think>\n");
        t.observe("x");
        t.observe("</think>");
        t.observe("y");
        t.observe("z");
        assert!(!t.due(), "the model closed it; the answer is not thinking");
    }

    #[test]
    fn a_conversation_runs_its_thinking_unbounded() {
        let mut r = request(Some(1));
        r.prompt_shape = Some(PromptShape::Conversation { messages: vec![] });
        let mut t = ThinkBudget::new(&r, "<think>\n");
        t.observe("x");
        t.observe("y");
        assert!(!t.due());
    }
}
