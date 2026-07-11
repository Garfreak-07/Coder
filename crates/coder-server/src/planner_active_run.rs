#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveRunMessageIntent {
    Status,
    Cancel,
    Supplement,
}

pub(crate) fn active_run_message_intent(message: &str) -> Option<ActiveRunMessageIntent> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if requests_future_task(&lower, trimmed) {
        return None;
    }
    if requests_cancel(&lower, trimmed) {
        return Some(ActiveRunMessageIntent::Cancel);
    }
    if requests_status(&lower, trimmed) {
        return Some(ActiveRunMessageIntent::Status);
    }
    if supplements_current_run(&lower, trimmed) {
        return Some(ActiveRunMessageIntent::Supplement);
    }
    None
}

fn requests_future_task(lower: &str, message: &str) -> bool {
    ["next task", "later task", "follow-up", "after this task"]
        .iter()
        .any(|needle| lower.contains(needle))
        || [
            "\u{4e0b}\u{4e00}\u{4e2a}\u{4efb}\u{52a1}",
            "\u{4e4b}\u{540e}\u{7684}\u{4efb}\u{52a1}",
            "\u{4ee5}\u{540e}\u{518d}\u{505a}",
            "\u{540e}\u{7eed}\u{4efb}\u{52a1}",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn requests_cancel(lower: &str, message: &str) -> bool {
    if ["do not stop", "don't stop", "do not cancel", "don't cancel"]
        .iter()
        .any(|needle| lower.contains(needle))
        || [
            "\u{4e0d}\u{8981}\u{505c}\u{6b62}",
            "\u{4e0d}\u{8981}\u{53d6}\u{6d88}",
            "\u{522b}\u{505c}",
            "\u{4e0d}\u{7528}\u{4e2d}\u{65ad}",
        ]
        .iter()
        .any(|needle| message.contains(needle))
    {
        return false;
    }
    matches!(lower, "stop" | "cancel" | "abort")
        || [
            "stop the task",
            "stop the current task",
            "stop current task",
            "cancel the task",
            "cancel the current task",
            "cancel current task",
            "abort the task",
            "stop the workflow",
            "cancel the workflow",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        || matches!(
            message,
            "\u{505c}" | "\u{505c}\u{6b62}" | "\u{53d6}\u{6d88}"
        )
        || [
            "\u{505c}\u{6b62}\u{4efb}\u{52a1}",
            "\u{505c}\u{6b62}\u{5f53}\u{524d}\u{4efb}\u{52a1}",
            "\u{505c}\u{6b62}\u{6267}\u{884c}",
            "\u{53d6}\u{6d88}\u{4efb}\u{52a1}",
            "\u{4e2d}\u{65ad}\u{4efb}\u{52a1}",
            "\u{7ec8}\u{6b62}\u{4efb}\u{52a1}",
            "\u{505c}\u{6389}\u{5f53}\u{524d}\u{4efb}\u{52a1}",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn requests_status(lower: &str, message: &str) -> bool {
    [
        "task status",
        "run status",
        "workflow status",
        "progress",
        "how is it going",
        "is it still running",
        "is it done",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || [
            "\u{4efb}\u{52a1}\u{72b6}\u{6001}",
            "\u{6267}\u{884c}\u{72b6}\u{6001}",
            "\u{8fdb}\u{5ea6}",
            "\u{600e}\u{4e48}\u{6837}\u{4e86}",
            "\u{5230}\u{54ea}\u{4e86}",
            "\u{8fd8}\u{5728}\u{8fd0}\u{884c}",
            "\u{5b8c}\u{6210}\u{4e86}\u{5417}",
            "\u{7ed3}\u{675f}\u{4e86}\u{5417}",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

fn supplements_current_run(lower: &str, message: &str) -> bool {
    [
        "current task",
        "active task",
        "while it runs",
        "while the task runs",
        "also add",
        "make sure",
        "change it to",
        "stop after",
        "do not stop",
        "don't stop",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || [
            "\u{5f53}\u{524d}\u{4efb}\u{52a1}",
            "\u{6b63}\u{5728}\u{6267}\u{884c}",
            "\u{8865}\u{5145}",
            "\u{53e6}\u{5916}",
            "\u{518d}\u{52a0}",
            "\u{8bb0}\u{5f97}",
            "\u{6539}\u{6210}",
            "\u{505a}\u{5230}",
            "\u{505a}\u{5b8c}",
            "\u{7ee7}\u{7eed}\u{6267}\u{884c}",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_active_run_messages_without_capturing_future_plans() {
        assert_eq!(
            active_run_message_intent(
                "\u{4efb}\u{52a1}\u{8fdb}\u{5ea6}\u{600e}\u{4e48}\u{6837}\u{4e86}"
            ),
            Some(ActiveRunMessageIntent::Status)
        );
        assert_eq!(
            active_run_message_intent("\u{505c}\u{6b62}\u{5f53}\u{524d}\u{4efb}\u{52a1}"),
            Some(ActiveRunMessageIntent::Cancel)
        );
        assert_eq!(
            active_run_message_intent("\u{8865}\u{5145}\u{4e00}\u{70b9}\u{ff1a}\u{8bb0}\u{5f97}\u{589e}\u{52a0}\u{97f3}\u{6548}"),
            Some(ActiveRunMessageIntent::Supplement)
        );
        assert_eq!(
            active_run_message_intent("Plan one follow-up improvement for a later task"),
            None
        );
        assert_eq!(
            active_run_message_intent(
                "\u{4e0d}\u{8981}\u{505c}\u{6b62}\u{ff0c}\u{7ee7}\u{7eed}\u{6267}\u{884c}"
            ),
            Some(ActiveRunMessageIntent::Supplement)
        );
    }
}
