//! `EmbeddingRouter` — the cosine-similarity router that picks among
//! the configured `RouteEntry`s by max-pool similarity of the
//! prompt embedding against each route's exemplar embeddings.

use crate::router::engine::{
    default_embedding_model_path, EmbeddingEngine, OnnxEmbeddingEngine,
};
#[cfg(feature = "onnx-inference")]
use crate::router::engine::OnnxRealEngine;
use crate::router::exemplars::{load_routes_from_dir, resolve_router_dir};
use crate::router::text::{cosine_similarity, parse_thinking, resolve_force, router_input};
use crate::router::{
    RouteEntry, Router, RouterError, RoutingContext, RoutingDecision, ToolSpec,
};
use crate::router::exemplars::default_routes;
use pi_ai::Message;
use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct EmbeddingRouter {
    routes: Arc<Vec<RouteEntry>>,
    engine: Arc<dyn EmbeddingEngine>,
}

impl EmbeddingRouter {
    pub fn bundled() -> Result<Self, RouterError> {
        // Resolve routes: materialise bundled defaults on first run, then
        // load the merged set (user overrides ∪ bundled defaults).
        let routes = match resolve_router_dir(true) {
            Some(dir) => load_routes_from_dir(&dir),
            None => default_routes(),
        };

        let path = default_embedding_model_path();
        #[cfg(feature = "onnx-inference")]
        if path.exists() {
            let engine = OnnxRealEngine::new(path)?;
            return Ok(Self::with_engine(routes, Arc::new(engine)));
        }

        let engine = OnnxEmbeddingEngine::new(path)?;
        Ok(Self::with_engine(routes, Arc::new(engine)))
    }

    pub fn from_model_path(path: impl AsRef<Path>) -> Result<Self, RouterError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(RouterError::EmbeddingsUnavailable(format!(
                "{} not found; run `pi router fetch-embeddings`",
                path.display()
            )));
        }
        #[cfg(feature = "onnx-inference")]
        {
            let engine = OnnxRealEngine::new(path)?;
            return Ok(Self::with_engine(default_routes(), Arc::new(engine)));
        }
        #[cfg(not(feature = "onnx-inference"))]
        {
            let engine = OnnxEmbeddingEngine::new(path)?;
            Ok(Self::with_engine(default_routes(), Arc::new(engine)))
        }
    }

    pub fn with_engine(routes: Vec<RouteEntry>, engine: Arc<dyn EmbeddingEngine>) -> Self {
        Self {
            routes: Arc::new(routes),
            engine,
        }
    }

    pub fn resolve_route_id(&self, prompt: &str) -> Result<String, RouterError> {
        self.resolve_route(prompt)
            .map(|(route, _)| route.id.clone())
    }

    fn resolve_route(&self, prompt: &str) -> Result<(RouteEntry, f32), RouterError> {
        let prompt_embedding = self.engine.embed(&router_input(prompt, &[], &[]))?;
        let mut best: Option<(f32, &RouteEntry)> = None;
        for route in self.routes.iter() {
            let score = self.route_similarity(route, &prompt_embedding)?;
            match best {
                Some((best_score, _)) if score <= best_score => {}
                _ => best = Some((score, route)),
            }
        }
        let (score, route) = best.ok_or_else(|| RouterError::Config("no routes configured".into()))?;
        Ok((route.clone(), score))
    }

    fn route_similarity(
        &self,
        route: &RouteEntry,
        prompt_embedding: &[f32],
    ) -> Result<f32, RouterError> {
        let mut sims = Vec::with_capacity(route.examples.len().max(1));
        if route.examples.is_empty() {
            return Err(RouterError::Config(format!(
                "route {} has no examples",
                route.id
            )));
        }
        for example in &route.examples {
            let example_embedding = self.engine.embed(example)?;
            sims.push(cosine_similarity(prompt_embedding, &example_embedding));
        }
        sims.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
        Ok(*sims.first().unwrap_or(&0.0))
    }

    fn decision_for(&self, route_id: &str) -> Result<RoutingDecision, RouterError> {
        let route = self
            .routes
            .iter()
            .find(|r| r.id == route_id)
            .ok_or_else(|| RouterError::Config(format!("missing route {route_id}")))?;
        Ok(RoutingDecision {
            route_id: route.id.clone(),
            provider: route.provider.clone(),
            model: route.model.clone(),
            thinking: parse_thinking(&route.thinking),
        })
    }
}

impl Router for EmbeddingRouter {
    fn route(
        &self,
        prompt: &str,
        history: &[Message],
        tools: &[ToolSpec],
        ctx: &RoutingContext,
    ) -> Result<RoutingDecision, RouterError> {
        if let Some(force) = &ctx.force {
            return Ok(resolve_force(force));
        }
        let prompt = router_input(prompt, history, tools);
        let route_id = self.resolve_route_id(&prompt)?;
        let decision = self.decision_for(&route_id)?;
        let key = format!("{}/{}", decision.provider, decision.model);
        if ctx.registry.resolve(&key).is_none() && ctx.registry.resolve(&decision.model).is_none() {
            return Err(RouterError::UnknownModel(key));
        }
        Ok(decision)
    }
}
