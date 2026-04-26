use pi_ai::{ContentBlock, Message, Role};

/// Summarises older messages into a single "context recap" message.
/// Upstream pi delegates compaction to the model itself; this implementation
/// performs an LLM-free heuristic compaction (keep last N turns + a textual
/// digest of older ones) so the loop never blocks waiting for a recap call.
pub struct Compactor {
    pub keep_last_turns: usize,
}

impl Default for Compactor {
    fn default() -> Self {
        Self { keep_last_turns: 6 }
    }
}

impl Compactor {
    pub fn compact(&self, messages: &[Message], instructions: Option<&str>) -> (Vec<Message>, String) {
        // Identify the boundary: keep the last `keep_last_turns` user messages
        // and everything after them.
        let user_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::User))
            .map(|(i, _)| i)
            .collect();
        let keep_from = if user_indices.len() <= self.keep_last_turns {
            0
        } else {
            user_indices[user_indices.len() - self.keep_last_turns]
        };

        let summary = build_summary(&messages[..keep_from], instructions);
        let mut out = Vec::with_capacity(messages.len() - keep_from + 1);
        if !summary.is_empty() {
            out.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!(
                        "<context_recap>\n{}\n</context_recap>",
                        summary
                    ),
                }],
            });
        }
        out.extend(messages[keep_from..].iter().cloned());
        (out, summary)
    }
}

fn build_summary(messages: &[Message], instructions: Option<&str>) -> String {
    if messages.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if let Some(ins) = instructions {
        out.push_str(&format!("Compaction instructions: {ins}\n\n"));
    }
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        };
        let mut text = String::new();
        for c in &m.content {
            match c {
                ContentBlock::Text { text: t } => {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(t);
                }
                ContentBlock::ToolUse { name, .. } => {
                    text.push_str(&format!(" [tool:{name}]"));
                }
                ContentBlock::ToolResult { is_error, .. } => {
                    text.push_str(if *is_error { " [tool_error]" } else { " [tool_ok]" });
                }
                _ => {}
            }
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let one_line: String = trimmed.replace('\n', " ").chars().take(280).collect();
        out.push_str(&format!("- {role}: {one_line}\n"));
    }
    out
}
