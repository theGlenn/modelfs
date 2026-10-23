//! Renders removal plans and outcomes for the terminal.

use super::{Action, Outcome, Plan, Step};
use crate::report::human_bytes;
use std::fmt::Write as _;

/// The plan as people read it: what goes, how, and what blocks it.
pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    for model in &plan.models {
        let scope = if model.everywhere {
            "from every app and the store"
        } else {
            "from the selected app(s)"
        };
        let _ = writeln!(
            out,
            "Remove {} ({}) {scope}",
            model.name,
            human_bytes(model.size)
        );
    }
    for step in &plan.steps {
        let _ = writeln!(out, "  {}", describe(step));
    }
    if !plan.collateral.is_empty() {
        let _ = writeln!(out, "Also removed:");
        for item in &plan.collateral {
            let _ = writeln!(
                out,
                "  {} ({}): {}",
                item.path.display(),
                human_bytes(item.size),
                item.reason
            );
        }
    }
    if !plan.notes.is_empty() {
        let _ = writeln!(out, "Notes:");
        for note in &plan.notes {
            let _ = writeln!(out, "  {}: {}", note.path.display(), note.message);
        }
    }
    if !plan.refusals.is_empty() {
        let _ = writeln!(out, "Blocked:");
        for refusal in &plan.refusals {
            let _ = writeln!(out, "  {}: {}", refusal.path.display(), refusal.message);
        }
    }
    let _ = writeln!(out, "{}", frees_sentence(plan.frees));
    let _ = writeln!(out, "Plan {}", plan.id);
    out
}

/// What ran, what failed, and what was kept.
pub fn render_outcome(plan: &Plan, outcome: &Outcome) -> String {
    let mut out = String::new();
    for &index in &outcome.completed {
        let _ = writeln!(out, "done    {}", describe(&plan.steps[index]));
    }
    if let Some((index, reason)) = &outcome.failure {
        let step = plan.steps.get(*index).map(describe).unwrap_or_default();
        let _ = writeln!(out, "FAILED  {step}: {reason}");
    }
    for warning in &outcome.warnings {
        let _ = writeln!(out, "warning: {warning}");
    }
    let finished = outcome.completed.len() == plan.steps.len();
    let _ = writeln!(
        out,
        "{} {} of {} step(s){}",
        if outcome.stopped {
            "Stopped after"
        } else {
            "Ran"
        },
        outcome.completed.len(),
        plan.steps.len(),
        if finished {
            "."
        } else {
            "; nothing after the first unfinished step ran."
        }
    );
    out
}

/// One sentence on the space freed, naming only what is not zero.
fn frees_sentence(frees: super::Frees) -> String {
    if frees.now == 0 && frees.after_trash == 0 {
        return if frees.pinned > 0 {
            "Frees about 0 B: other copies share these bytes.".to_string()
        } else {
            "Frees about 0 B.".to_string()
        };
    }
    let mut parts = Vec::new();
    if frees.now > 0 {
        parts.push(format!("{} now", human_bytes(frees.now)));
    }
    if frees.after_trash > 0 {
        parts.push(format!(
            "{} once the Trash is emptied",
            human_bytes(frees.after_trash)
        ));
    }
    let mut sentence = format!("Frees about {}", parts.join(" and "));
    if frees.pinned > 0 {
        let _ = write!(
            sentence,
            "; {} stays in use by other copies",
            human_bytes(frees.pinned)
        );
    }
    sentence + "."
}

fn describe(step: &Step) -> String {
    let what = match &step.action {
        Action::OllamaDelete { tag } => format!("delete Ollama tag {tag}"),
        Action::Unlink => "unlink".to_string(),
        Action::Delete => "delete".to_string(),
        Action::DeleteFolder => "delete folder".to_string(),
        Action::Trash => "move to Trash".to_string(),
        Action::TrashFolder => "move folder to Trash".to_string(),
        Action::RemoveStoredCopy => "delete stored copy".to_string(),
    };
    format!("{:<12} {what}  {}", step.provider, step.path.display())
}
