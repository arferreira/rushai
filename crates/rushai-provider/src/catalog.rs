//! Vendored model catalog: context windows, output limits, and prices.
//!
//! Numbers are verified against provider pricing pages when the snapshot
//! is updated. `lookup` returns None for unknown models and call sites
//! keep their defaults.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::{ModelCost, ModelInfo};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEntry {
    aliases: BTreeMap<String, String>,
    models: BTreeMap<String, ModelEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    context_window: u64,
    max_output: u64,
    cost: ModelCost,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("catalog.json")).expect("vendored catalog is valid")
    })
}

/// Resolve a model (or one of its aliases) within a provider.
/// The returned ModelInfo carries the canonical model id.
pub fn lookup(provider: &str, model: &str) -> Option<ModelInfo> {
    let provider = provider.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    let entry = catalog().providers.get(&provider)?;
    let id = entry
        .aliases
        .get(&model)
        .map(String::as_str)
        .unwrap_or(&model);
    let m = entry.models.get(id)?;
    Some(ModelInfo {
        id: id.to_owned(),
        context_window: m.context_window,
        max_output: m.max_output,
        cost: Some(m.cost),
    })
}

#[cfg(test)]
mod tests {
    use super::lookup;

    #[test]
    fn aliases_resolve_to_canonical_ids() {
        let sonnet = lookup("anthropic", "sonnet").unwrap();
        assert_eq!(sonnet.id, "claude-sonnet-5");
        assert_eq!(sonnet.context_window, 1_000_000);
        let cost = sonnet.cost.unwrap();
        assert_eq!(cost.input, 2.0);
        assert_eq!(cost.output, 10.0);

        let haiku = lookup("anthropic", "haiku").unwrap();
        assert_eq!(haiku.id, "claude-haiku-4-5-20251001");
        assert_eq!(haiku.context_window, 200_000);
    }

    #[test]
    fn canonical_ids_and_full_slugs_resolve() {
        assert!(lookup("openai", "gpt-5.6-terra").is_some());
        assert!(lookup("openrouter", "deepseek/deepseek-v4-flash").is_some());
    }

    #[test]
    fn unknown_models_and_providers_are_none() {
        assert!(lookup("anthropic", "claude-2").is_none());
        assert!(lookup("mistral", "large").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(lookup("Anthropic", "Sonnet").unwrap().id, "claude-sonnet-5");
    }

    #[test]
    fn every_alias_points_at_a_real_model() {
        let raw: serde_json::Value = serde_json::from_str(include_str!("catalog.json")).unwrap();
        for (provider, entry) in raw["providers"].as_object().unwrap() {
            let models = entry["models"].as_object().unwrap();
            for (alias, target) in entry["aliases"].as_object().unwrap() {
                let target = target.as_str().unwrap();
                assert!(
                    models.contains_key(target),
                    "{provider} alias {alias} points at missing model {target}"
                );
            }
        }
    }

    #[test]
    fn cost_estimate_matches_hand_math() {
        let cost = lookup("anthropic", "sonnet").unwrap().cost.unwrap();
        let usage = rushai_protocol::TokenUsage {
            input: 1_000_000,
            output: 100_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        // 2.0 + 1.0 + 0.1 + 0.5
        assert!((cost.estimate(&usage) - 3.6).abs() < 1e-9);
    }
}
