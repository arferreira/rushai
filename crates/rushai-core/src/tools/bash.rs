use std::process::Stdio;
use std::time::Duration;

use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::permission::PermissionSpec;
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;
const MAX_OUTPUT: usize = 128 * 1024;

/// Provider credentials and tokens are stripped from the child's environment
/// so a command cannot exfiltrate them.
const SCRUBBED_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "COPILOT_TOKEN",
    "CLAUDE_BIN",
];

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// Command line to run through the shell.
    command: String,
    /// Timeout in seconds (default 120, max 600).
    timeout: Option<u64>,
}

pub struct Bash {
    /// Explicit shell path, or None to resolve `$SHELL` at run time. Tests set
    /// this so they do not depend on the developer's login shell.
    shell: Option<String>,
}

impl Bash {
    pub fn new() -> Self {
        Self { shell: None }
    }

    pub fn with_shell(shell: impl Into<String>) -> Self {
        Self {
            shell: Some(shell.into()),
        }
    }

    fn shell(&self) -> Option<String> {
        self.shell.clone().or_else(|| std::env::var("SHELL").ok())
    }
}

impl Default for Bash {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash".into(),
            description: include_str!("descriptions/bash.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _ctx: &ToolCtx, input: &Value) -> Option<PermissionSpec> {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Discriminate the grant by the exact command so a grant for one
        // command never authorizes another. The command lives in `action`
        // (not `path`, which dispatch would try to absolutize). `persistable`
        // is false: an "always" here only lasts the session.
        let mut spec = PermissionSpec::new(
            "bash",
            format!("execute:{}", normalize(command)),
            None,
            format!("run: {command}"),
        );
        spec.persistable = false;
        Some(spec)
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        if let Some(reason) = banned(&input.command) {
            return Err(ToolError::Failed(format!("refused: {reason}")));
        }
        let timeout = Duration::from_secs(
            input
                .timeout
                .unwrap_or(DEFAULT_TIMEOUT)
                .clamp(1, MAX_TIMEOUT),
        );

        let (mut command, shell) = shell_command(self, &input.command);
        command
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for var in SCRUBBED_ENV {
            command.env_remove(var);
        }
        put_in_process_group(&mut command);

        let mut child = command
            .spawn()
            .map_err(|e| ToolError::Failed(format!("failed to run shell {shell}: {e}")))?;
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        // Drain both pipes in one loop, stopping the moment total output
        // passes the cap. Reading whichever pipe is ready avoids the deadlock
        // where one pipe fills (blocking the child) while we wait on the
        // other to reach EOF, which it never will.
        let mut merged = Vec::new();
        let read = async {
            let mut ob = [0u8; 8192];
            let mut eb = [0u8; 8192];
            let mut out_open = true;
            let mut err_open = true;
            let mut capped = false;
            while out_open || err_open {
                tokio::select! {
                    r = stdout.read(&mut ob), if out_open => match r? {
                        0 => out_open = false,
                        n => merged.extend_from_slice(&ob[..n]),
                    },
                    r = stderr.read(&mut eb), if err_open => match r? {
                        0 => err_open = false,
                        n => merged.extend_from_slice(&eb[..n]),
                    },
                }
                if merged.len() > MAX_OUTPUT {
                    capped = true;
                    break;
                }
            }
            Ok::<_, std::io::Error>(capped)
        };

        let collected = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                kill_group(&mut child).await;
                return Err(ToolError::Canceled);
            }
            timed = tokio::time::timeout(timeout, read) => timed,
        };

        let capped = match collected {
            Ok(Ok(capped)) => capped,
            Ok(Err(io)) => {
                kill_group(&mut child).await;
                return Err(io.into());
            }
            Err(_) => {
                kill_group(&mut child).await;
                return Err(ToolError::Failed(format!(
                    "command timed out after {}s",
                    timeout.as_secs()
                )));
            }
        };

        let status = if capped {
            // We stopped reading at the cap; the child may block on a full
            // pipe, so kill the group rather than wait for it.
            kill_group(&mut child).await;
            None
        } else {
            // Pipes hit EOF, so the child is finishing; reap it.
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .ok()
                .and_then(Result::ok)
                .or_else(|| {
                    let _ = child.start_kill();
                    None
                })
        };

        let mut out = String::from_utf8_lossy(&merged).into_owned();
        if out.len() > MAX_OUTPUT {
            let mut cut = MAX_OUTPUT;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
        }
        if capped {
            out.push_str("\n... output truncated; command was stopped\n");
        }
        if let Some(status) = status
            && !status.success()
        {
            let code = status.code().map_or("signal".to_owned(), |c| c.to_string());
            out.push_str(&format!("\n[exit {code}]"));
        }
        Ok(out)
    }
}

/// Collapse whitespace so trivial spacing differences share one grant.
fn normalize(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A small tripwire against obviously catastrophic commands. Not a sandbox:
/// permission prompts are the real gate, this only blocks the worst typos.
fn banned(command: &str) -> Option<&'static str> {
    let c = normalize(command);
    let patterns = [
        ("rm -rf /", "recursive delete of root"),
        ("rm -rf /*", "recursive delete of root"),
        ("mkfs", "filesystem format"),
        ("dd of=/dev/", "raw write to a device"),
        ("shutdown", "system shutdown"),
        ("reboot", "system reboot"),
        (":(){:|:&};:", "fork bomb"),
    ];
    patterns
        .iter()
        .find(|(needle, _)| c.contains(needle))
        .map(|(_, reason)| *reason)
}

#[cfg(unix)]
fn shell_command(tool: &Bash, command: &str) -> (Command, String) {
    let shell = tool.shell().unwrap_or_else(|| "/bin/sh".into());
    let mut c = Command::new(&shell);
    c.arg("-c").arg(command);
    (c, shell)
}

#[cfg(windows)]
fn shell_command(tool: &Bash, command: &str) -> (Command, String) {
    let shell = tool.shell().unwrap_or_else(|| "cmd".into());
    let mut c = Command::new(&shell);
    c.arg("/C").arg(command);
    (c, shell)
}

#[cfg(unix)]
fn put_in_process_group(command: &mut Command) {
    // New process group so a timeout or cancel kills the whole tree.
    command.process_group(0);
}

#[cfg(not(unix))]
fn put_in_process_group(_command: &mut Command) {}

#[cfg(unix)]
async fn kill_group(child: &mut tokio::process::Child) {
    // Signal the whole group (negative pid) so grandchildren die too.
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait().await;
}

#[cfg(not(unix))]
async fn kill_group(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}
