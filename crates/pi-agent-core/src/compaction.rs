use pi_ai::{ContentBlock, GenerateRequest, Message, ModelInfo, Provider, Role, ThinkingLevel};

/// Two strategies live side by side:
///
/// * [`Compactor`] — fast heuristic, no network. Default in tests and CI.
/// * [`LlmCompactor`] — calls the model to produce a summary, mirroring
///   upstream pi's behaviour. Used when the user runs `/compact <prompt>`
///   or when auto-compaction triggers and an LLM is available.
pub struct Compactor {
    pub keep_last_turns: usize,
}

impl Default for Compactor {
    fn default() -> Self {
        Self { keep_last_turns: 6 }
    }
}

impl Compactor {
    pub fn compact(
        &self,
        messages: &[Message],
        instructions: Option<&str>,
    ) -> (Vec<Message>, String) {
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
        } else if self.keep_last_turns == 0 {
            // Drop *every* historical message — keep nothing of the original
            // transcript and let the recap stand on its own.
            messages.len()
        } else {
            user_indices[user_indices.len() - self.keep_last_turns]
        };

        let summary = build_summary(&messages[..keep_from], instructions);
        let mut out = Vec::with_capacity(messages.len() - keep_from + 1);
        if !summary.is_empty() {
            out.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("<context_recap>\n{}\n</context_recap>", summary),
                }],
            });
        }
        out.extend(messages[keep_from..].iter().cloned());
        (out, summary)
    }
}

/// LLM-driven compactor: asks the model itself to produce a recap.
pub struct LlmCompactor<'a> {
    pub keep_last_turns: usize,
    pub provider: &'a dyn Provider,
    pub model: &'a ModelInfo,
}

impl<'a> LlmCompactor<'a> {
    pub async fn compact(
        &self,
        messages: &[Message],
        instructions: Option<&str>,
    ) -> Result<(Vec<Message>, String), pi_ai::AiError> {
        let user_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::User))
            .map(|(i, _)| i)
            .collect();
        let keep_from = if user_indices.len() <= self.keep_last_turns {
            return Ok((messages.to_vec(), String::new()));
        } else {
            user_indices[user_indices.len() - self.keep_last_turns]
        };

        let recap_prompt = match instructions {
            Some(ins) => format!(
                "Summarise the following conversation history so the assistant can continue working. Follow these instructions: {ins}\n\nRespond with a tight bullet list of facts, decisions, file paths touched, and pending TODOs."
            ),
            None => "Summarise the following conversation history so the assistant can continue working. Respond with a tight bullet list of facts, decisions, file paths touched, and pending TODOs.".to_string(),
        };

        let history_text = build_summary(&messages[..keep_from], None);
        let req = GenerateRequest {
            model: self.model.id.clone(),
            system: Some(recap_prompt),
            messages: vec![Message::user_text(history_text)],
            tools: Vec::new(),
            thinking: ThinkingLevel::Off,
            temperature: Some(0.0),
            max_output_tokens: Some(2_048),
            extras: serde_json::Value::Null,
        };
        let resp = self.provider.generate(req, self.model).await?;
        let summary = resp.message.text();
        let mut out = Vec::with_capacity(messages.len() - keep_from + 1);
        if !summary.is_empty() {
            out.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("<context_recap>\n{summary}\n</context_recap>"),
                }],
            });
        }
        out.extend(messages[keep_from..].iter().cloned());
        Ok((out, summary))
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
                    text.push_str(if *is_error {
                        " [tool_error]"
                    } else {
                        " [tool_ok]"
                    });
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
