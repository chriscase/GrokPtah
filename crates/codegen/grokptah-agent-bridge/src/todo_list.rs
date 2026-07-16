//! In-session todo list for the Build agent (parity with upstream todo tools).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "in_progress" | "in-progress" | "doing" | "active" => Self::InProgress,
            "completed" | "done" | "complete" => Self::Completed,
            "cancelled" | "canceled" | "dropped" => Self::Cancelled,
            _ => Self::Pending,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

impl TodoList {
    /// Merge updates: items with matching id update; new ids append; full replace if `replace_all`.
    pub fn apply_update(&mut self, items: Vec<TodoItem>, merge: bool) {
        if !merge {
            self.items = items;
            return;
        }
        for it in items {
            if let Some(existing) = self.items.iter_mut().find(|e| e.id == it.id) {
                if !it.content.is_empty() {
                    existing.content = it.content;
                }
                existing.status = it.status;
            } else {
                self.items.push(it);
            }
        }
    }

    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "(empty todo list)".into();
        }
        let mut out = String::from("Todo list:\n");
        for (i, it) in self.items.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{}] {} ({})\n",
                i + 1,
                it.status.as_str(),
                it.content,
                it.id
            ));
        }
        out
    }

    /// Parse from tool JSON: array of {id?, content, status?} or {todos: [...], merge?: bool}
    pub fn from_tool_args(v: &serde_json::Value) -> anyhow::Result<(Vec<TodoItem>, bool)> {
        let merge = v
            .get("merge")
            .and_then(|m| m.as_bool())
            .unwrap_or(true);
        let arr = if let Some(a) = v.get("todos").and_then(|t| t.as_array()) {
            a.clone()
        } else if let Some(a) = v.as_array() {
            a.clone()
        } else if v.get("content").is_some() {
            vec![v.clone()]
        } else {
            anyhow::bail!("todo_write expects todos array or content field");
        };
        let mut items = Vec::new();
        for (i, el) in arr.iter().enumerate() {
            let content = el
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() {
                continue;
            }
            let id = el
                .get("id")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("t{}", i + 1));
            let status = el
                .get("status")
                .and_then(|c| c.as_str())
                .map(TodoStatus::parse)
                .unwrap_or(TodoStatus::Pending);
            items.push(TodoItem {
                id,
                content,
                status,
            });
        }
        if items.is_empty() {
            anyhow::bail!("no todo items provided");
        }
        Ok((items, merge))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_updates_status() {
        let mut list = TodoList::default();
        list.apply_update(
            vec![TodoItem {
                id: "1".into(),
                content: "A".into(),
                status: TodoStatus::Pending,
            }],
            false,
        );
        list.apply_update(
            vec![TodoItem {
                id: "1".into(),
                content: String::new(),
                status: TodoStatus::Completed,
            }],
            true,
        );
        assert_eq!(list.items[0].status, TodoStatus::Completed);
        assert_eq!(list.items[0].content, "A");
    }
}
