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
//!
//! Kiro keeps its list with a `todo_list` tool (tasks by 0-based position)
//! or, in its newer engine, a task tool (tasks by id). Both take a
//! `command` (`create`, `complete`, `add`, `remove`), are recognized by
//! that shape, and work through the list in order, so the first task not
//! done is the one in progress. When the reply carries the whole list, it
//! is taken from there.
//!
//! A plan can also be a Markdown file of checkboxes the agent ticks as it
//! goes (see [`Plan::from_markdown`]).

use std::path::Path;

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
            _ if is_kiro_list(input) => self.kiro(input, response),
            _ => return false,
        }
        self.items != before
    }

    /// Kiro's to-do and task tools.
    fn kiro(&mut self, input: &Value, response: &Value) {
        // The reply's copy of the whole list, when it has one, is the
        // most reliable.
        if let Some(items) = listed_items(response) {
            self.items = items;
            self.mark_first_open();
            return;
        }
        let task = |task: &Value| task["task_description"].as_str().map(|text| clean(text, TEXT));
        let positions = |key: &str| -> Vec<usize> {
            input[key].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).collect()
        };
        let ids = |key: &str| -> Vec<String> { input[key].as_array().into_iter().flatten().filter_map(id_of).collect() };
        match input["command"].as_str().unwrap_or("") {
            "create" => {
                self.items = input["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(task)
                    .enumerate()
                    .map(|(index, text)| PlanItem { id: (index + 1).to_string(), text, active: None, status: ItemStatus::Pending })
                    .collect();
            }
            "complete" => {
                for position in positions("completed_indices") {
                    if let Some(item) = self.items.get_mut(position) {
                        item.status = ItemStatus::Done;
                    }
                }
                for id in ids("completed_task_ids") {
                    if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
                        item.status = ItemStatus::Done;
                    }
                }
            }
            "add" => {
                let new: Vec<String> = input["new_tasks"].as_array().into_iter().flatten().filter_map(task).collect();
                let at = positions("insert_indices");
                for (index, text) in new.into_iter().enumerate() {
                    let item = PlanItem { id: self.next_number().to_string(), text, active: None, status: ItemStatus::Pending };
                    match at.get(index) {
                        Some(&position) if position <= self.items.len() => self.items.insert(position, item),
                        _ => self.items.push(item),
                    }
                }
            }
            "remove" => {
                let mut at = positions("remove_indices");
                at.sort_unstable();
                for position in at.into_iter().rev() {
                    if position < self.items.len() {
                        self.items.remove(position);
                    }
                }
                let gone = ids("remove_task_ids");
                self.items.retain(|item| !gone.contains(&item.id));
            }
            // Another saved list was loaded, and its tasks are not known.
            "load" => self.items.clear(),
            _ => {}
        }
        self.mark_first_open();
    }

    /// Lists worked through in order have no "in progress" of their own:
    /// the first item not done is the one being worked on.
    fn mark_first_open(&mut self) {
        for item in &mut self.items {
            if item.status == ItemStatus::InProgress {
                item.status = ItemStatus::Pending;
            }
        }
        if let Some(item) = self.items.iter_mut().find(|item| item.status != ItemStatus::Done) {
            item.status = ItemStatus::InProgress;
        }
    }

    /// A plan from the checkboxes of a Markdown file: `- [ ]` to do,
    /// `- [x]` done, and `- [-]`, `- [~]` or `- [/]` in progress. Lines in
    /// code blocks are skipped. When none is marked in progress, the first
    /// item not done is taken to be. `None` when there are no checkboxes.
    pub fn from_markdown(text: &str) -> Option<Plan> {
        let mut items = Vec::new();
        let mut in_code = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                continue;
            }
            // A list marker ("-", "*", "+" or "1."), then "[?] ".
            let rest = trimmed
                .strip_prefix(['-', '*', '+'])
                .or_else(|| trimmed.split_once(". ").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map(|(_, rest)| rest))
                .map(str::trim_start);
            let Some(rest) = rest else { continue };
            let mut chars = rest.chars();
            let (Some('['), Some(mark), Some(']')) = (chars.next(), chars.next(), chars.next()) else { continue };
            let status = match mark {
                ' ' => ItemStatus::Pending,
                'x' | 'X' => ItemStatus::Done,
                '-' | '~' | '/' => ItemStatus::InProgress,
                _ => continue,
            };
            let text = chars.as_str().trim();
            if text.is_empty() {
                continue;
            }
            items.push(PlanItem { id: (items.len() + 1).to_string(), text: clean(text, TEXT), active: None, status });
        }
        if items.is_empty() {
            return None;
        }
        let mut plan = Plan { items };
        if plan.current().is_none() {
            plan.mark_first_open();
        }
        Some(plan)
    }

    /// The plan in a Markdown file, if it has checkboxes. Big files are
    /// not plans.
    pub fn from_file(path: &Path) -> Option<Plan> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > 512 * 1024 {
            return None;
        }
        Plan::from_markdown(&std::fs::read_to_string(path).ok()?)
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

/// The Markdown files a prompt names, as written: `@PLAN.md`,
/// `docs/plan.md`, `".kiro/specs/x/tasks.md"`.
pub fn markdown_files_named(prompt: &str) -> Vec<String> {
    prompt
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .map(|word| {
            word.trim_start_matches(['@', '"', '\'', '`'])
                .trim_end_matches(['"', '\'', '`', '.', ',', ';', ':', '!', '?'])
        })
        .filter(|word| word.len() > 3 && word.to_ascii_lowercase().ends_with(".md"))
        .map(str::to_owned)
        .collect()
}

/// Whether a tool call looks like Kiro's to-do or task tool: a `command`
/// it knows, with that tool's own fields.
fn is_kiro_list(input: &Value) -> bool {
    let known = ["tasks", "completed_indices", "completed_task_ids", "new_tasks", "remove_indices", "remove_task_ids", "load_id"];
    matches!(input["command"].as_str(), Some("create" | "complete" | "add" | "remove" | "load"))
        && known.iter().any(|key| input.get(*key).is_some())
}

/// A whole list in a reply: the first array of objects that each have a
/// description and say whether they are done.
fn listed_items(response: &Value) -> Option<Vec<PlanItem>> {
    let items = |list: &Vec<Value>| -> Option<Vec<PlanItem>> {
        let parsed: Vec<PlanItem> = list
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let text = ["task_description", "description", "content", "subject"].iter().find_map(|k| entry[*k].as_str())?;
                let done = entry["completed"].as_bool().or_else(|| entry["status"].as_str().map(|s| s == "completed"))?;
                Some(PlanItem {
                    id: id_of(&entry["id"]).unwrap_or_else(|| (index + 1).to_string()),
                    text: clean(text, TEXT),
                    active: None,
                    status: if done { ItemStatus::Done } else { ItemStatus::Pending },
                })
            })
            .collect::<Option<_>>()?;
        (!parsed.is_empty()).then_some(parsed)
    };
    match response {
        Value::Array(list) => items(list).or_else(|| list.iter().find_map(listed_items)),
        Value::Object(map) => map.values().find_map(listed_items),
        // Replies are often JSON inside a string.
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            listed_items(&serde_json::from_str(text).ok()?)
        }
        _ => None,
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
    fn kiros_todo_list_works_through_positions() {
        let mut plan = Plan::default();
        let task = |text: &str| json!({ "task_description": text });
        let create = json!({ "command": "create", "todo_list_description": "pricing",
            "tasks": [task("Read the styles"), task("Fix the heights"), task("Add a test")] });
        assert!(plan.apply("todo_list", &create, &json!(null)));
        // In order, so the first is the one in progress.
        assert_eq!(plan.current().map(|i| i.text.as_str()), Some("Read the styles"));
        plan.apply("todo_list", &json!({ "command": "complete", "completed_indices": [0], "context_update": "", "current_id": "x" }), &json!(null));
        assert_eq!(plan.progress(), Some((1, 3)));
        assert_eq!(plan.current().map(|i| i.text.as_str()), Some("Fix the heights"));
        plan.apply("todo_list", &json!({ "command": "add", "new_tasks": [task("Update docs")], "insert_indices": [3], "current_id": "x" }), &json!(null));
        plan.apply("todo_list", &json!({ "command": "remove", "remove_indices": [2], "current_id": "x" }), &json!(null));
        let texts: Vec<&str> = plan.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Read the styles", "Fix the heights", "Update docs"]);
        // Looking the lists up changes nothing; other tools are not lists.
        assert!(!plan.apply("todo_list", &json!({ "command": "lookup" }), &json!(null)));
        assert!(!plan.apply("fs_write", &json!({ "command": "create", "path": "/x" }), &json!(null)));
    }

    #[test]
    fn kiros_task_tool_works_by_id_and_trusts_a_listed_reply() {
        let mut plan = Plan::default();
        let create = json!({ "command": "create", "task_list_description": "x",
            "tasks": [{ "task_description": "One" }, { "task_description": "Two" }] });
        plan.apply("task", &create, &json!({}));
        plan.apply("task", &json!({ "command": "complete", "completed_task_ids": ["1"], "context_update": "" }), &json!({}));
        assert_eq!(plan.progress(), Some((1, 2)));
        // A reply with the whole list (here as JSON in a string) wins.
        let reply = json!(r#"{"tasks":[{"id":"a","task_description":"One","completed":true},{"id":"b","task_description":"Two","completed":true},{"id":"c","task_description":"Three","completed":false}]}"#);
        plan.apply("task", &json!({ "command": "add", "new_tasks": [{ "task_description": "Three" }] }), &reply);
        assert_eq!(plan.progress(), Some((2, 3)));
        assert_eq!(plan.current().map(|i| i.id.as_str()), Some("c"));
    }

    #[test]
    fn markdown_checkboxes_make_a_plan() {
        let text = "# Pricing plan\n\n- [x] Read the styles\n- [ ] Fix the heights\n  * [ ] nested step\n1. [X] numbered\n```\n- [ ] not a task, code\n```\n- [] not a box\nplain text\n";
        let plan = Plan::from_markdown(text).unwrap();
        let texts: Vec<&str> = plan.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Read the styles", "Fix the heights", "nested step", "numbered"]);
        assert_eq!(plan.progress(), Some((2, 4)));
        // None marked in progress: the first open one is.
        assert_eq!(plan.current().map(|i| i.text.as_str()), Some("Fix the heights"));
        // An explicit marker wins.
        let marked = Plan::from_markdown("- [ ] a\n- [-] b\n").unwrap();
        assert_eq!(marked.current().map(|i| i.text.as_str()), Some("b"));
        assert_eq!(Plan::from_markdown("# no boxes\n- item\n"), None);
    }

    #[test]
    fn markdown_files_are_found_in_a_prompt() {
        let prompt = "implement @PLAN.md, then check `docs/notes.md`. See .kiro/specs/x/tasks.md! (not README)";
        assert_eq!(markdown_files_named(prompt), ["PLAN.md", "docs/notes.md", ".kiro/specs/x/tasks.md"]);
        assert!(markdown_files_named("no files here.").is_empty());
    }

    #[test]
    fn hostile_text_is_cleaned() {
        let mut plan = Plan::default();
        let input = json!({ "todos": [{ "content": "x\u{1b}]0;pwned\u{7}y", "status": "pending" }] });
        plan.apply("TodoWrite", &input, &json!({}));
        assert!(!plan.items[0].text.contains('\u{1b}'));
    }
}
