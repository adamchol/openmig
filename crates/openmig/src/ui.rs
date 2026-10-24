//! User interaction, kept behind a trait so flows can be tested without a terminal.

use openmig_core::{Plan, RenameCandidate};

pub trait Ui: Send + Sync {
    /// Whether prompts can be answered (stdin and stderr are terminals).
    fn is_interactive(&self) -> bool;

    fn info(&self, message: &str);

    fn warn(&self, message: &str);

    /// Print a plan for review (comments, statements, destructive markers).
    fn show_plan(&self, title: &str, plan: &Plan);

    /// Ask a yes/no question. Non-interactive implementations return `false`.
    fn confirm(&self, prompt: &str) -> bool;

    /// Ask which candidates are renames. Returns the accepted ones.
    fn choose_renames(&self, candidates: &[RenameCandidate]) -> Vec<RenameCandidate>;
}
