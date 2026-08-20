use anyhow::bail;
use rushai_config::{AuthStore, CopilotAuthEntry};
use rushai_provider::CopilotAuth;

use crate::paths;

pub async fn run(provider: &str) -> anyhow::Result<()> {
    match provider {
        "copilot" => login_copilot().await,
        other => bail!("unknown provider {other:?} (supported: copilot)"),
    }
}

async fn login_copilot() -> anyhow::Result<()> {
    let auth = CopilotAuth::new();
    let code = auth.start().await?;
    println!(
        "Open {} and enter code: {}",
        code.verification_uri, code.user_code
    );
    println!("Waiting for approval...");
    let github_token = auth.poll(&code).await?;

    let store = AuthStore::new(paths::data_dir()?.join("auth.json"));
    let mut stored = store.load()?;
    stored.copilot = Some(CopilotAuthEntry { github_token });
    store.save(&stored)?;
    println!("Signed in. Use it with: rush exec --model copilot/gpt-5.2 -p \"...\"");
    Ok(())
}
