//! The terminal implementation of [`Ui`].

use crate::ui::Ui;
use openmig_core::{Plan, RenameCandidate};
use owo_colors::OwoColorize;
use std::io::{IsTerminal, Write};

pub struct TerminalUi {
    color: bool,
    interactive: bool,
    quiet: bool,
}

impl TerminalUi {
    pub fn new(color: bool, quiet: bool) -> Self {
        Self {
            color,
            interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            quiet,
        }
    }

    /// Whether colors should be used on stderr/stdout for the `--color` choice.
    pub fn detect_color(choice: crate::cli::ColorChoice) -> bool {
        match choice {
            crate::cli::ColorChoice::Always => true,
            crate::cli::ColorChoice::Never => false,
            crate::cli::ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
            }
        }
    }

    fn paint<'a>(&self, text: &'a str, f: impl Fn(&'a str) -> String) -> String {
        if self.color { f(text) } else { text.to_owned() }
    }
}

impl Ui for TerminalUi {
    fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn info(&self, message: &str) {
        if !self.quiet {
            eprintln!("{message}");
        }
    }

    fn warn(&self, message: &str) {
        let label = self.paint("warning:", |t| t.yellow().bold().to_string());
        eprintln!("{label} {message}");
    }

    fn show_plan(&self, title: &str, plan: &Plan) {
        let mut out = std::io::stdout().lock();
        if !title.is_empty() {
            let _ = writeln!(out, "{}", self.paint(title, |t| t.bold().to_string()));
        }
        for warning in &plan.warnings {
            let _ = writeln!(
                out,
                "-- {} {warning}",
                self.paint("warning:", |t| t.yellow().to_string())
            );
        }
        for s in &plan.statements {
            if let Some(reason) = &s.destructive {
                let line = format!("-- DESTRUCTIVE: {reason}");
                let _ = writeln!(out, "{}", self.paint(&line, |t| t.red().bold().to_string()));
            }
            if !s.comment.is_empty() {
                let line = format!("-- {}", s.comment);
                let _ = writeln!(out, "{}", self.paint(&line, |t| t.dimmed().to_string()));
            }
            let _ = writeln!(out, "{};", s.sql);
        }
    }

    fn confirm(&self, prompt: &str) -> bool {
        if !self.interactive {
            return false;
        }
        dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()
            .unwrap_or(false)
    }

    fn choose_renames(&self, candidates: &[RenameCandidate]) -> Vec<RenameCandidate> {
        if !self.interactive {
            return Vec::new();
        }
        candidates
            .iter()
            .filter(|c| {
                self.confirm(&format!(
                    "Was {} renamed to `{}`? ({})",
                    c.from,
                    c.to.name(),
                    c.reason
                ))
            })
            .cloned()
            .collect()
    }
}

/// A [`Ui`] that never prompts and records messages; for tests and `--json`.
#[derive(Default)]
pub struct RecordingUi {
    pub messages: std::sync::Mutex<Vec<String>>,
    pub accept_renames: bool,
    pub confirm_answer: bool,
}

impl Ui for RecordingUi {
    fn is_interactive(&self) -> bool {
        false
    }

    fn info(&self, message: &str) {
        self.messages.lock().unwrap().push(message.to_owned());
    }

    fn warn(&self, message: &str) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("warning: {message}"));
    }

    fn show_plan(&self, title: &str, plan: &Plan) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("{title}\n{}", plan.to_sql()));
    }

    fn confirm(&self, _prompt: &str) -> bool {
        self.confirm_answer
    }

    fn choose_renames(&self, candidates: &[RenameCandidate]) -> Vec<RenameCandidate> {
        if self.accept_renames {
            candidates.to_vec()
        } else {
            Vec::new()
        }
    }
}
