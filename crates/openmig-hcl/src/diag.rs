use crate::source::{SourceMap, Span};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One problem found while loading or decoding a schema.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// Primary location.
    pub span: Option<Span>,
    /// Text shown under the primary location.
    pub label: Option<String>,
    pub help: Option<String>,
    /// Secondary locations, e.g. the earlier declaration of a duplicate block.
    pub related: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span: None,
            label: None,
            help: None,
            related: Vec::new(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(message)
        }
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_related(mut self, span: Span, label: impl Into<String>) -> Self {
        self.related.push((span, label.into()));
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter().filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Warning)
    }

    /// Render all diagnostics with source snippets.
    pub fn render(&self, sources: &SourceMap, color: bool) -> String {
        let handler = if color {
            miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::unicode())
        } else {
            miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::unicode_nocolor())
        };
        let mut out = String::new();
        for d in &self.items {
            let report = Rendered::new(d, sources);
            if handler.render_report(&mut out, &report).is_err() {
                let _ = writeln!(out, "{}", d.message);
            }
        }
        out
    }
}

/// Adapter that presents a [`Diagnostic`] to miette.
#[derive(Debug)]
struct Rendered {
    message: String,
    severity: Severity,
    help: Option<String>,
    source: Option<miette::NamedSource<String>>,
    labels: Vec<miette::LabeledSpan>,
}

impl Rendered {
    fn new(d: &Diagnostic, sources: &SourceMap) -> Self {
        let mut labels = Vec::new();
        let mut source = None;
        let mut message = d.message.clone();
        if let Some(span) = d.span {
            let file = sources.get(span.file);
            source = Some(miette::NamedSource::new(
                file.path.display().to_string(),
                file.content.to_string(),
            ));
            labels.push(miette::LabeledSpan::new_primary_with_span(
                d.label.clone(),
                span.to_miette(),
            ));
            for (rel, text) in &d.related {
                if rel.file == span.file {
                    labels.push(miette::LabeledSpan::new_with_span(
                        Some(text.clone()),
                        rel.to_miette(),
                    ));
                } else {
                    let (line, col) = sources.line_col(rel.file, rel.start);
                    let _ = write!(
                        message,
                        "\n  {text}: {}:{line}:{col}",
                        sources.path(rel.file).display()
                    );
                }
            }
        }
        Self {
            message,
            severity: d.severity,
            help: d.help.clone(),
            source,
            labels,
        }
    }
}

impl std::fmt::Display for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Rendered {}

impl miette::Diagnostic for Rendered {
    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.help
            .as_ref()
            .map(|h| Box::new(h) as Box<dyn std::fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.source.as_ref().map(|s| s as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        if self.labels.is_empty() {
            None
        } else {
            Some(Box::new(self.labels.iter().cloned()))
        }
    }
}
