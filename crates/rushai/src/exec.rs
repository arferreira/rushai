use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use futures::StreamExt;
use rushai_config::Config;
use rushai_protocol::{Part, Role, TokenUsage};
use rushai_provider::fake::{FakeProvider, Scripted};
use rushai_provider::{Anthropic, ChatMessage, ChatRequest, ModelInfo, Provider, ProviderEvent};

const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-5";

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
        ..Default::default()
    };

    let mut stream = provider.stream(request).await?;
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
                eprintln!(
                    "tokens: {} in, {} out, {} cache read, {} cache write",
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
            Ok(Box::new(Anthropic::new(
                api_key,
                ModelInfo {
                    id: model_id.into(),
                    context_window: 200_000,
                    max_output: 8192,
                },
            )))
        }
        other => bail!("unknown provider {other:?} (supported: anthropic)"),
    }
}
