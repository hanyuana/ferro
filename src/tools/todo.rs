use super::*;
use crate::session::{render_todos, Todo};
use serde_json::json;

pub struct TodoWrite;

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }
    fn description(&self) -> &'static str {
        "创建并维护当前任务的结构化待办清单（每次传入完整列表，覆盖旧列表）。\n\
         - 任务需要 3 步以上时使用，帮助规划并让用户看到进度\n\
         - 同一时间只保持一个 in_progress；完成一项后立即标记 completed，不要攒着批量更新\n\
         - 简单的单步任务不需要使用"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn describe(&self, input: &Value, _ctx: &ToolCtx) -> Invocation {
        let n = input["todos"].as_array().map(|a| a.len()).unwrap_or(0);
        Invocation { title: format!("{} 项", n), ..Default::default() }
    }
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput> {
        let todos: Vec<Todo> = serde_json::from_value(input["todos"].clone())
            .map_err(|e| anyhow!("todos 参数格式错误: {}", e))?;
        let rendered = render_todos(&todos);
        *ctx.todos.lock().unwrap() = todos;
        Ok(ToolOutput { content: format!("待办清单已更新：\n{}", rendered), display: Some(rendered) })
    }
}
