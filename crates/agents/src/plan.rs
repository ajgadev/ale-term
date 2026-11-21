//! An agent's plan: the to-do list it keeps while it works through a task,
//! so the app can show the item in progress and how far along it is.
//!
//! Claude Code keeps the list with one of two tools, and both are followed
//! here from the status hooks (after each successful call):
//!
//! * `TodoWrite` sends the whole list every time: `todos`, each with
//!   `content`, `activeForm` ("Adding the tests") and `status`.
//! * The task tools change it one item at a time: `TaskCreate` (`subject`,
//!   `activeForm`; its reply carries the new `task.id`) and `TaskUpdate`
//!   (`taskId` and whatever changed, `status: "deleted"` to drop one).
//!
//! Statuses are `pending`, `in_progress` and `completed`.

use serde_json::Value;

use crate::session::clean;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Pending,
    InProgress,
    Done,
}

impl ItemStatus {
    fn parse(value: &Value) -> Option<ItemStatus> {
        match value.as_str()? {
            "pending" => Some(ItemStatus::Pending),
            "in_progress" => Some(ItemStatus::InProgress),
            "completed" => Some(ItemStatus::Done),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    /// What is to be done: "Add the tests".
    pub text: String,
    /// The same while it is being done: "Adding the tests".
    pub active: Option<String>,
    pub status: ItemStatus,
}

impl PlanItem {
    /// How the item reads while in progress.
    pub fn label(&self) -> &str {
        self.active.as_deref().filter(|active| !active.is_empty()).unwrap_or(&self.text)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

/// Longest item text kept.
const TEXT: usize = 160;

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items done and items in all, when there is a plan.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let done = self.items.iter().filter(|item| item.status == ItemStatus::Done).count();
        (!self.items.is_empty()).then_some((done, self.items.len()))
    }

    /// The item being worked on.
    pub fn current(&self) -> Option<&PlanItem> {
        self.items.iter().find(|item| item.status == ItemStatus::InProgress)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Follows a successful call of `tool` with `input`, which produced
    /// `response`. Returns whether the plan changed.
    pub fn apply(&mut self, tool: &str, input: &Value, response: &Value) -> bool {
        let before = self.items.clone();
        match tool {
            "TodoWrite" => self.write(input),
            "TaskCreate" => self.create(input, response),
            "TaskUpdate" => self.update(input),
            _ => return false,
        }
        self.items != before
    }

    fn write(&mut self, input: &Value) {
        let Some(todos) = input["todos"].as_array() else { return };
        self.items = todos
            .iter()
            .enumerate()
            .filter_map(|(index, todo)| {
                Some(PlanItem {
                    id: (index + 1).to_string(),
                    text: clean(todo["content"].as_str()?, TEXT),
                    active: todo["activeForm"].as_str().map(|active| clean(active, TEXT)),
                    status: ItemStatus::parse(&todo["status"]).unwrap_or(ItemStatus::Pending),
                })
            })
            .collect();
    }

    fn create(&mut self, input: &Value, response: &Value) {
        let Some(subject) = input["subject"].as_str() else { return };
        // The new task's id is in the reply; if it is not, tasks are
        // numbered from 1, so the next number is the best guess.
        let id = id_of(&response["task"]["id"])
            .or_else(|| id_of(&response["id"]))
            .unwrap_or_else(|| self.next_number().to_string());
        let item = PlanItem {
            id: id.clone(),
            text: clean(subject, TEXT),
            active: input["activeForm"].as_str().map(|active| clean(active, TEXT)),
            status: ItemStatus::Pending,
        };
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(existing) => *existing = item,
            None => self.items.push(item),
        }
    }

    fn update(&mut self, input: &Value) {
        let Some(id) = id_of(&input["taskId"]) else { return };
        if input["status"] == "deleted" {
            self.items.retain(|item| item.id != id);
            return;
        }
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else { return };
        if let Some(status) = ItemStatus::parse(&input["status"]) {
            item.status = status;
        }
        if let Some(subject) = input["subject"].as_str() {
            item.text = clean(subject, TEXT);
        }
        if let Some(active) = input["activeForm"].as_str() {
            item.active = Some(clean(active, TEXT));
        }
    }

    fn next_number(&self) -> u64 {
        self.items.iter().filter_map(|item| item.id.parse::<u64>().ok()).max().unwrap_or(0) + 1
    }
}

/// An id given as a string or a number.
fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_written_list_replaces_the_plan() {
        let mut plan = Plan::default();
        let input = json!({ "todos": [
            { "content": "Read the styles", "activeForm": "Reading the styles", "status": "completed" },
            { "content": "Add the tests", "activeForm": "Adding the tests", "status": "in_progress" },
            { "content": "Update the docs", "activeForm": "Updating the docs", "status": "pending" },
        ]});
        assert!(plan.apply("TodoWrite", &input, &json!({})));
        assert_eq!(plan.progress(), Some((1, 3)));
        assert_eq!(plan.current().map(PlanItem::label), Some("Adding the tests"));
        // The same list again changes nothing.
        assert!(!plan.apply("TodoWrite", &input, &json!({})));
        // An empty list ends the plan.
        assert!(plan.apply("TodoWrite", &json!({ "todos": [] }), &json!({})));
        assert_eq!(plan.progress(), None);
    }

    #[test]
    fn tasks_are_created_updated_and_deleted_one_at_a_time() {
        let mut plan = Plan::default();
        let create = |subject: &str, active: &str| json!({ "subject": subject, "description": "", "activeForm": active });
        plan.apply("TaskCreate", &create("Add the tests", "Adding the tests"), &json!({ "task": { "id": "1", "subject": "Add the tests" } }));
        // A reply without the id: the next number.
        plan.apply("TaskCreate", &create("Update the docs", "Updating the docs"), &json!({}));
        plan.apply("TaskCreate", &create("Ship it", "Shipping it"), &json!({ "task": { "id": 3 } }));
        let ids: Vec<&str> = plan.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
        assert_eq!(plan.progress(), Some((0, 3)));

        assert!(plan.apply("TaskUpdate", &json!({ "taskId": "1", "status": "in_progress" }), &json!({})));
        assert_eq!(plan.current().map(PlanItem::label), Some("Adding the tests"));
        plan.apply("TaskUpdate", &json!({ "taskId": "1", "status": "completed" }), &json!({}));
        plan.apply("TaskUpdate", &json!({ "taskId": "2", "status": "in_progress", "activeForm": "Rewriting the docs" }), &json!({}));
        assert_eq!(plan.progress(), Some((1, 3)));
        assert_eq!(plan.current().map(PlanItem::label), Some("Rewriting the docs"));
        plan.apply("TaskUpdate", &json!({ "taskId": "3", "status": "deleted" }), &json!({}));
        assert_eq!(plan.progress(), Some((1, 2)));
        // Unknown tasks and other tools are ignored.
        assert!(!plan.apply("TaskUpdate", &json!({ "taskId": "9", "status": "completed" }), &json!({})));
        assert!(!plan.apply("Bash", &json!({ "command": "ls" }), &json!({})));
    }

    #[test]
    fn hostile_text_is_cleaned() {
        let mut plan = Plan::default();
        let input = json!({ "todos": [{ "content": "x\u{1b}]0;pwned\u{7}y", "status": "pending" }] });
        plan.apply("TodoWrite", &input, &json!({}));
        assert!(!plan.items[0].text.contains('\u{1b}'));
    }
}
