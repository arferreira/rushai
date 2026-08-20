use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use futures::StreamExt;
use rushai_config::AuthStore;
use rushai_config::Config;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::fake::{FakeProvider, Scripted};
use rushai_provider::{
    Anthropic, ChatMessage, ChatRequest, ClaudeBridge, Copilot, ModelInfo, OpenAiCompat, Provider,
    ProviderEvent, Retrying, catalog,
};

const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-5";
/// Request default, deliberately independent of a model's max_output:
/// catalog models allow 128k, which is not a sane default spend.
const DEFAULT_MAX_TOKENS: u64 = 8192;

pub async fn run(
    prompt: String,
    model: Option<String>,
    fake_script: Option<&Path>,
) -> anyhow::Result<()> {
    let provider = build_provider(model, fake_script)?;
    let request = ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            parts: vec![Part::Text { text: prompt }],
        }],
        max_tokens: Some(DEFAULT_MAX_TOKENS),
        ..Default::default()
    };

    let mut stream = provider.stream(&request).await?;
    let mut usage = TokenUsage::default();
    let mut out = std::io::stdout();
    while let Some(event) = stream.next().await {
        match event? {
            ProviderEvent::TextDelta(text) => {
                out.write_all(text.as_bytes())?;
                out.flush()?;
            }
            ProviderEvent::Usage(u) => {
                usage.input += u.input;
                usage.output += u.output;
                usage.cache_read += u.cache_read;
                usage.cache_write += u.cache_write;
            }
            ProviderEvent::Done { .. } => {
                out.write_all(b"\n")?;
                let est = provider
                    .model()
                    .cost
                    .map(|cost| format!(", est ${:.4}", cost.estimate(&usage)))
                    .unwrap_or_default();
                eprintln!(
                    "tokens: {} in, {} out, {} cache read, {} cache write{est}",
                    usage.input, usage.output, usage.cache_read, usage.cache_write
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn build_provider(
    model: Option<String>,
    fake_script: Option<&Path>,
) -> anyhow::Result<Box<dyn Provider>> {
    if let Some(path) = fake_script {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let script: Vec<Scripted> = serde_json::from_str(&text)?;
        return Ok(Box::new(FakeProvider::new(script)));
    }

    let cwd = std::env::current_dir()?;
    let config = Config::load(cwd)?;
    let selection = model
        .or(config.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.into());
    let (provider_id, model_id) = selection
        .split_once('/')
        .with_context(|| format!("model {selection:?} is not provider/model"))?;

    match provider_id {
        "anthropic" => {
            let api_key = config
                .providers
                .get("anthropic")
                .and_then(|p| p.api_key.clone())
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .context(
                    "no anthropic api key: set providers.anthropic.api_key or ANTHROPIC_API_KEY",
                )?;
            Ok(Box::new(Retrying::new(Anthropic::new(
                api_key,
                model_info(provider_id, model_id),
            ))))
        }
        "openai" | "openrouter" => {
            let env_key = if provider_id == "openai" {
                "OPENAI_API_KEY"
            } else {
                "OPENROUTER_API_KEY"
            };
            let entry = config.providers.get(provider_id);
            let api_key = entry
                .and_then(|p| p.api_key.clone())
                .or_else(|| std::env::var(env_key).ok())
                .with_context(|| {
                    format!(
                        "no {provider_id} api key: set providers.{provider_id}.api_key or {env_key}"
                    )
                })?;
            let base_url =
                entry
                    .and_then(|p| p.base_url.clone())
                    .unwrap_or_else(|| match provider_id {
                        "openai" => "https://api.openai.com/v1".into(),
                        _ => "https://openrouter.ai/api/v1".into(),
                    });
            Ok(Box::new(Retrying::new(OpenAiCompat::new(
                api_key,
                model_info(provider_id, model_id),
                base_url,
            ))))
        }
        "claude" => {
            let model = model_info(provider_id, model_id);
            Ok(Box::new(ClaudeBridge::discover(model)))
        }
        "copilot" => {
            let store = AuthStore::new(crate::paths::data_dir()?.join("auth.json"));
            let entry = store
                .load()?
                .copilot
                .context("copilot is not signed in: run rush login copilot")?;
            Ok(Box::new(Retrying::new(Copilot::new(
                entry.github_token,
                model_info(provider_id, model_id),
            ))))
        }
        // Any configured provider with a base_url speaks the compat protocol.
        other => match config.providers.get(other) {
            Some(entry) if entry.base_url.is_some() => {
                let api_key = entry.api_key.clone().unwrap_or_default();
                let base_url = entry.base_url.clone().unwrap();
                Ok(Box::new(Retrying::new(OpenAiCompat::new(
                    api_key,
                    model_info(provider_id, model_id),
                    base_url,
                ))))
            }
            _ => bail!(
                "unknown provider {other:?} (built in: anthropic, openai, openrouter; \
                 or configure providers.{other}.base_url for an openai-compatible endpoint)"
            ),
        },
    }
}

fn model_info(provider_id: &str, model_id: &str) -> ModelInfo {
    catalog::lookup(provider_id, model_id).unwrap_or_else(|| info(model_id))
}

fn info(model_id: &str) -> ModelInfo {
    ModelInfo {
        id: model_id.into(),
        context_window: 200_000,
        max_output: 8192,
        cost: None,
    }
}
