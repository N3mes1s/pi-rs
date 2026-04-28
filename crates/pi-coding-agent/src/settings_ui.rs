//! Settings picker UI helpers.
//!
//! Provides a structured list of editable settings fields (with options for
//! each) and a mutation helper so the interactive `/settings` picker flow can
//! apply a chosen value directly to a [`Settings`] struct.

use pi_agent_core::settings::{QueueMode, Settings, ThinkingSetting, Transport};

/// One editable setting field exposed to the picker UI.
pub struct SettingsField {
    /// Machine-readable field name (used as the key in [`apply`]).
    pub name: &'static str,
    /// The field's current value serialised to a display string.
    pub current: String,
    /// All valid values for this field (shown in the second picker).
    pub options: Vec<String>,
}

/// Build the list of editable [`SettingsField`]s for the given `settings`.
///
/// The `themes` slice is used to populate options for the `theme` field.
pub fn fields(settings: &Settings, themes: &[String]) -> Vec<SettingsField> {
    vec![
        SettingsField {
            name: "thinking",
            current: thinking_label(settings.thinking).to_string(),
            options: vec!["off".into(), "low".into(), "medium".into(), "high".into()],
        },
        SettingsField {
            name: "steering_mode",
            current: queue_mode_label(settings.steering_mode).to_string(),
            options: vec!["one-at-a-time".into(), "all".into()],
        },
        SettingsField {
            name: "follow_up_mode",
            current: queue_mode_label(settings.follow_up_mode).to_string(),
            options: vec!["one-at-a-time".into(), "all".into()],
        },
        SettingsField {
            name: "transport",
            current: transport_label(settings.transport).to_string(),
            options: vec!["sse".into(), "websocket".into(), "auto".into()],
        },
        SettingsField {
            name: "scoped_models",
            current: settings.scoped_models.to_string(),
            options: vec!["false".into(), "true".into()],
        },
        SettingsField {
            name: "theme",
            current: settings.theme.clone(),
            options: themes.to_vec(),
        },
    ]
}

/// Mutate `settings` in-place by setting `field` to `value`.
///
/// Returns `Err` if `field` is unknown or `value` is not a valid option for
/// that field.
pub fn apply(settings: &mut Settings, field: &str, value: &str) -> Result<(), String> {
    match field {
        "thinking" => {
            settings.thinking = parse_thinking(value)
                .ok_or_else(|| format!("invalid thinking value: {:?}", value))?;
        }
        "steering_mode" => {
            settings.steering_mode = parse_queue_mode(value)
                .ok_or_else(|| format!("invalid steering_mode value: {:?}", value))?;
        }
        "follow_up_mode" => {
            settings.follow_up_mode = parse_queue_mode(value)
                .ok_or_else(|| format!("invalid follow_up_mode value: {:?}", value))?;
        }
        "transport" => {
            settings.transport = parse_transport(value)
                .ok_or_else(|| format!("invalid transport value: {:?}", value))?;
        }
        "scoped_models" => {
            settings.scoped_models = parse_bool(value)
                .ok_or_else(|| format!("invalid scoped_models value: {:?}", value))?;
        }
        "theme" => {
            if value.is_empty() {
                return Err("theme value must not be empty".into());
            }
            settings.theme = value.to_string();
        }
        other => {
            return Err(format!("unknown settings field: {:?}", other));
        }
    }
    Ok(())
}

// ─── private helpers ─────────────────────────────────────────────────────────

fn thinking_label(t: ThinkingSetting) -> &'static str {
    match t {
        ThinkingSetting::Off => "off",
        ThinkingSetting::Low => "low",
        ThinkingSetting::Medium => "medium",
        ThinkingSetting::High => "high",
        ThinkingSetting::XHigh => "xhigh",
    }
}

fn parse_thinking(s: &str) -> Option<ThinkingSetting> {
    match s {
        "off" => Some(ThinkingSetting::Off),
        "low" => Some(ThinkingSetting::Low),
        "medium" => Some(ThinkingSetting::Medium),
        "high" => Some(ThinkingSetting::High),
        "xhigh" => Some(ThinkingSetting::XHigh),
        _ => None,
    }
}

fn queue_mode_label(q: QueueMode) -> &'static str {
    match q {
        QueueMode::OneAtATime => "one-at-a-time",
        QueueMode::All => "all",
    }
}

fn parse_queue_mode(s: &str) -> Option<QueueMode> {
    match s {
        "one-at-a-time" => Some(QueueMode::OneAtATime),
        "all" => Some(QueueMode::All),
        _ => None,
    }
}

fn transport_label(t: Transport) -> &'static str {
    match t {
        Transport::Sse => "sse",
        Transport::Websocket => "websocket",
        Transport::Auto => "auto",
    }
}

fn parse_transport(s: &str) -> Option<Transport> {
    match s {
        "sse" => Some(Transport::Sse),
        "websocket" => Some(Transport::Websocket),
        "auto" => Some(Transport::Auto),
        _ => None,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}
