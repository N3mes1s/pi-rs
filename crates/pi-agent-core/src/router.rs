use anyhow::{anyhow, Result};
use pi_ai::{FinishReason, Message, ModelRegistry, ThinkingLevel, ToolSpec};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    pub provider: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    pub max_tokens: Option<u32>,
    pub route_id: String,
    pub similarity: f32,
    pub fallback_chain: Vec<(String, String, ThinkingLevel)>,
    pub use_tale: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForceOverride {
    pub provider: String,
    pub model: String,
    pub thinking: ThinkingLevel,
}

#[derive(Debug, Clone)]
pub struct RoutingContext<'a> {
    pub registry: &'a ModelRegistry,
    pub user_lambda: f64,
    pub force: Option<ForceOverride>,
    pub session_id: &'a str,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub cost_usd: f64,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub stop_reason: FinishReason,
    pub tool_call_parse_ok: bool,
    pub max_tokens_overrun: bool,
    pub retry_count: u8,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub quality_score: Option<f32>,
    pub final_provider_error: Option<String>,
}

pub trait Router: Send + Sync {
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision>;

    fn observe(&self, _decision: &RoutingDecision, _outcome: &Outcome) {}
}

#[derive(Debug, Clone)]
pub struct StaticRouter {
    default: RoutingDecision,
    configured: Option<RoutingDecision>,
}

impl StaticRouter {
    pub fn new(
        default_provider: impl Into<String>,
        default_model: impl Into<String>,
        thinking: ThinkingLevel,
    ) -> Self {
        let default = RoutingDecision {
            provider: default_provider.into(),
            model: default_model.into(),
            thinking,
            max_tokens: None,
            route_id: "static".to_string(),
            similarity: 1.0,
            fallback_chain: Vec::new(),
            use_tale: false,
        };
        let configured = match RouterConfig::load() {
            Ok(Some(cfg)) => cfg.static_decision().ok().flatten(),
            Ok(None) | Err(_) => None,
        };
        Self {
            default,
            configured,
        }
    }

    pub fn from_paths(
        default_provider: impl Into<String>,
        default_model: impl Into<String>,
        thinking: ThinkingLevel,
        user_path: impl AsRef<Path>,
        repo_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let default = RoutingDecision {
            provider: default_provider.into(),
            model: default_model.into(),
            thinking,
            max_tokens: None,
            route_id: "static".to_string(),
            similarity: 1.0,
            fallback_chain: Vec::new(),
            use_tale: false,
        };
        let configured = RouterConfig::load_from_paths(user_path.as_ref(), repo_path.as_ref())?
            .map(|cfg| cfg.static_decision())
            .transpose()?
            .flatten();
        Ok(Self {
            default,
            configured,
        })
    }

    fn decide(&self, ctx: &RoutingContext<'_>) -> Result<RoutingDecision> {
        if let Some(force) = &ctx.force {
            Self::resolve_target(ctx.registry, &force.provider, &force.model)?;
            return Ok(RoutingDecision {
                provider: force.provider.clone(),
                model: force.model.clone(),
                thinking: force.thinking,
                max_tokens: None,
                route_id: "forced".to_string(),
                similarity: 1.0,
                fallback_chain: Vec::new(),
                use_tale: false,
            });
        }

        let decision = self
            .configured
            .clone()
            .unwrap_or_else(|| self.default.clone());
        Self::resolve_target(ctx.registry, &decision.provider, &decision.model)?;
        Ok(decision)
    }

    fn resolve_target(registry: &ModelRegistry, provider: &str, model: &str) -> Result<()> {
        let target = format!("{provider}/{model}");
        registry
            .resolve(&target)
            .ok_or_else(|| anyhow!("unknown model: {target}"))?;
        Ok(())
    }
}

impl Router for StaticRouter {
    fn route(
        &self,
        _prompt: &str,
        _history: &[Message],
        _tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision> {
        self.decide(ctx)
    }
}

#[derive(Debug, Deserialize)]
struct RouterConfig {
    #[serde(default)]
    route: Vec<RouteConfig>,
}

impl RouterConfig {
    fn load() -> Result<Option<Self>> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::load_from_paths(
            &home.join(".pi").join("agent").join("router.toml"),
            &PathBuf::from(".pi").join("router.toml"),
        )
    }

    fn load_from_paths(user_path: &Path, repo_path: &Path) -> Result<Option<Self>> {
        let path = if repo_path.exists() {
            repo_path
        } else if user_path.exists() {
            user_path
        } else {
            return Ok(None);
        };
        let raw = fs::read_to_string(path)?;
        let parsed = toml::from_str::<Self>(&raw)?;
        Ok(Some(parsed))
    }

    fn static_decision(&self) -> Result<Option<RoutingDecision>> {
        let Some(route) = self.route.first() else {
            return Ok(None);
        };
        Ok(Some(RoutingDecision {
            provider: route.provider.clone(),
            model: route.model.clone(),
            thinking: parse_thinking(&route.thinking)?,
            max_tokens: route.max_tokens,
            route_id: route.id.clone().unwrap_or_else(|| "static".to_string()),
            similarity: 1.0,
            fallback_chain: Vec::new(),
            use_tale: route.use_tale.unwrap_or(false),
        }))
    }
}

#[derive(Debug, Deserialize)]
struct RouteConfig {
    id: Option<String>,
    provider: String,
    model: String,
    #[serde(default = "default_thinking")]
    thinking: String,
    max_tokens: Option<u32>,
    use_tale: Option<bool>,
}

fn default_thinking() -> String {
    "off".to_string()
}

fn parse_thinking(value: &str) -> Result<ThinkingLevel> {
    Ok(match value {
        "off" => ThinkingLevel::Off,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::XHigh,
        other => return Err(anyhow!("unknown thinking level: {other}")),
    })
}
