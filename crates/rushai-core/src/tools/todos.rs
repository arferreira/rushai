use std::fmt::Write;

use rushai_provider::ToolDef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::permission::PermissionSpec;
use crate::store::{Store, Todo};
use crate::tool::{RunToken, Tool, ToolCtx, ToolError, parse_input, schema_for};

#[derive(Deserialize, JsonSchema)]
struct Input {
    /// The full task list, replacing whatever was stored.
    todos: Vec<TodoInput>,
}

#[derive(Deserialize, JsonSchema)]
struct TodoInput {
    text: String,
    #[serde(default)]
    done: bool,
}

/// The todos tool persists to the session row, so it needs the store.
pub struct Todos {
    store: Store,
}

impl Todos {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Tool for Todos {
    fn name(&self) -> &'static str {
        "todos"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "todos".into(),
            description: include_str!("descriptions/todos.md").into(),
            input_schema: schema_for::<Input>(),
        }
    }

    fn permission(&self, _ctx: &ToolCtx, _input: &Value) -> Option<PermissionSpec> {
        None
    }

    async fn run(&self, ctx: &ToolCtx, input: Value, _run: RunToken) -> Result<String, ToolError> {
        let input: Input = parse_input(input)?;
        let todos: Vec<Todo> = input
            .todos
            .into_iter()
            .map(|t| Todo {
                text: t.text,
                done: t.done,
            })
            .collect();
        self.store
            .set_todos(&ctx.session, &todos)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(render(&todos))
    }
}

fn render(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "no todos".into();
    }
    let mut out = String::new();
    for todo in todos {
        let mark = if todo.done { "x" } else { " " };
        let _ = writeln!(out, "[{mark}] {}", todo.text);
    }
    out
}
