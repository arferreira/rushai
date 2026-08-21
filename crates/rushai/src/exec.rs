use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use rushai_config::AuthStore;
use rushai_config::Config;
use rushai_core::agent::{Engine, EngineConfig};
use rushai_core::store::Store;
use rushai_core::tool::Tool;
use rushai_core::tools::{GlobTool, Grep, Ls, View};
use rushai_protocol::{Decision, Event, Op, PartDelta, TokenUsage, UserPart};
use rushai_provider::fake::{FakeProvider, Scripted};
use rushai_provider::{
    Anthropic, ClaudeBridge, Copilot, ModelInfo, OpenAiCompat, Provider, Retrying, catalog,
};

const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-5";
/// Request default, deliberately independent of a model's max_output:
/// catalog models allow 128k, which is not a sane default spend.
const DEFAULT_MAX_TOKENS: u64 = 8192;

pub async fn run(
    prompt: String,
    model: Option<String>,
    fake_script: Option<&Path>,
    yolo: bool,
) -> anyhow::Result<()> {
    let provider: Arc<dyn Provider> = Arc::from(build_provider(model, fake_script)?);
    let dir = crate::paths::data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let store = Store::open(dir.join("rushai.db"))?;
    let title: String = prompt.chars().take(60).collect();
    let session = store.create_session(title, None).await?;
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(View),
        Arc::new(Ls),
        Arc::new(GlobTool),
        Arc::new(Grep),
    ];

    let (engine, mut events) = Engine::spawn(EngineConfig {
        store,
        provider: provider.clone(),
        tools,
        cwd: std::env::current_dir()?,
        yolo,
        max_tokens: Some(DEFAULT_MAX_TOKENS),
    });
    engine.submit(Op::Prompt {
        session: session.id.clone(),
        parts: vec![UserPart::Text { text: prompt }],
    });

    let mut usage = TokenUsage::default();
    let mut failed = false;
    let mut out = std::io::stdout();
    while let Some(event) = events.recv().await {
        match event {
            Event::PartDelta {
                delta: PartDelta::Text { delta },
                ..
            } => {
                out.write_all(delta.as_bytes())?;
                out.flush()?;
            }
            Event::ToolStarted { name, input, .. } => {
                eprintln!("[tool] {name}: {}", compact(&input));
            }
            Event::ToolDone {
                output, is_error, ..
            } => {
                if is_error {
                    eprintln!("[tool error] {}", first_line(&output));
                }
            }
            Event::PermissionRequested { request } => {
                eprintln!(
                    "[denied without asking: {}; rerun with --yolo to allow tools]",
                    request.description
                );
                engine.submit(Op::PermissionDecision {
                    request: request.id,
                    decision: Decision::Deny,
                });
            }
            Event::Usage { usage: u, .. } => {
                usage.input += u.input;
                usage.output += u.output;
                usage.cache_read += u.cache_read;
                usage.cache_write += u.cache_write;
            }
            Event::Error { message } => {
                eprintln!("error: {message}");
                failed = true;
            }
            Event::RunFinished { reason, .. } => {
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
                if reason == rushai_protocol::FinishReason::Error {
                    failed = true;
                }
                break;
            }
            _ => {}
        }
    }
    if failed {
        bail!("run failed");
    }
    Ok(())
}

fn compact(input: &serde_json::Value) -> String {
    let text = input.to_string();
    if text.len() > 120 {
        format!("{}...", &text[..text.floor_char_boundary(120)])
    } else {
        text
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn build_provider(
    model: Option<String>,
    fake_script: Option<&Path>,
) -> anyhow::Result<Box<dyn Provider>> {
    if let Some(path) = fake_script {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let fake = match serde_json::from_str::<Vec<Vec<Scripted>>>(&text) {
            Ok(turns) => FakeProvider::turns(turns),
            Err(_) => FakeProvider::new(serde_json::from_str(&text)?),
        };
        return Ok(Box::new(fake));
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
