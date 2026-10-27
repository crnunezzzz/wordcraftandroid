//! The two tools the model gets, run against the open document. Shared by the API path (the
//! host's own agent loop) and the plan path (Codex or Claude Code calling back through the
//! tool bridge): both see the same commands and the same limits.
#![forbid(unsafe_code)]

use crate::protocol;
use serde_json::{Value, json};

/// The editor as the assistant drives it.
pub trait Editor {
    /// The command registry, as `registry.describe()` lists it.
    fn catalog(&self) -> Value;
    /// Run one engine command.
    fn run(&mut self, id: &str, params: Value) -> Result<Value, String>;
    /// `file.save` with empty params: save the open document where the host keeps it.
    fn save(&mut self) -> Result<Value, String>;
    /// The document snapshot attached to each user message.
    fn context(&mut self) -> Value;
    /// The interface size as a zoom over the system's scale `native` (1.0: as the system draws).
    fn interface_scale(&self, _native: f32) -> f32 {
        1.0
    }
    fn set_interface_scale(&mut self, _native: f32, _scale: f32) {}
}

/// Run one tool call; returns the step's label (in the user's language) and its result.
pub fn run_tool(editor: &mut dyn Editor, name: &str, args: &Value, over_limit: bool) -> (String, Result<Value, String>) {
    if over_limit {
        return ("Límite de acciones".into(), Err("Tool call limit reached for this request".into()));
    }
    if args.get("__invalid_json").is_some() {
        return ("Instrucción ilegible".into(), Err("Arguments were not valid JSON; send a JSON object".into()));
    }
    match name {
        "list_commands" => {
            let search = args["search"].as_str().unwrap_or("").to_lowercase();
            let offset = args["offset"].as_u64().unwrap_or(0).min(100_000) as usize;
            let matching: Vec<Value> = editor
                .catalog()
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["id"].as_str().is_some_and(protocol::command_allowed) && c.to_string().to_lowercase().contains(&search))
                .cloned()
                .map(|mut command| {
                    if command["id"] == "file.save" {
                        command["params"] = json!("{}: save the open document; no path or other params allowed");
                    }
                    command
                })
                .collect();
            let label = if search.is_empty() { "Consultó las órdenes disponibles".to_owned() } else { format!("Buscó órdenes: «{search}»") };
            (label, Ok(json!({"total":matching.len(),"offset":offset,"commands":matching.into_iter().skip(offset).take(30).collect::<Vec<_>>()})))
        }
        "execute_command" => {
            let id = args["command"].as_str().unwrap_or("").to_owned();
            let params = args.get("params").cloned().unwrap_or(json!({}));
            let catalog = editor.catalog();
            let entry = catalog.as_array().into_iter().flatten().find(|c| c["id"] == id.as_str());
            let label = entry.and_then(|c| c["label"].as_str()).map_or_else(|| id.clone(), |l| format!("{l} ({id})"));
            let result = if entry.is_none() || !protocol::command_allowed(&id) || !params.is_object() {
                Err(format!("Command not available: {id}. Use list_commands to find the exact id."))
            } else if let Err(e) = protocol::validate_params(&params, 0) {
                Err(e)
            } else if id == "file.save" {
                if params.as_object().is_some_and(|p| p.is_empty()) { editor.save() } else { Err("file.save only accepts {}".into()) }
            } else {
                editor.run(&id, params)
            };
            (label, result)
        }
        other => (other.to_owned(), Err(format!("Unknown tool {other}"))),
    }
}

/// A tool result as the model receives it: the value, or an error object; never more than
/// [`protocol::MAX_RESULT`] bytes. Returns (content, error detail for the step).
pub fn result_content(result: &Result<Value, String>) -> (String, String) {
    match result {
        Ok(v) => {
            let text = v.to_string();
            if text.len() <= protocol::MAX_RESULT {
                (text, String::new())
            } else {
                (json!({"error":"Result too large; inspect a smaller range"}).to_string(), String::new())
            }
        }
        Err(e) => (json!({"error": e}).to_string(), e.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        saved: bool,
        ran: Vec<String>,
    }
    impl Editor for Fake {
        fn catalog(&self) -> Value {
            json!([{"id":"format.bold","label":"Bold"},{"id":"file.save","label":"Save"},{"id":"file.open","label":"Open"}])
        }
        fn run(&mut self, id: &str, _params: Value) -> Result<Value, String> {
            self.ran.push(id.into());
            Ok(json!({"ok":true}))
        }
        fn save(&mut self) -> Result<Value, String> {
            self.saved = true;
            Ok(json!({"saved":true}))
        }
        fn context(&mut self) -> Value {
            json!({})
        }
    }

    #[test]
    fn tools_reach_only_document_commands() {
        let mut e = Fake { saved: false, ran: vec![] };
        let (_, listed) = run_tool(&mut e, "list_commands", &json!({}), false);
        let listed = listed.unwrap();
        assert_eq!(listed["total"], 2, "file.open is not offered: {listed}");
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"file.open","params":{"path":"/etc/passwd"}}), false).1.is_err());
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"format.bold","params":{"filePath":"x"}}), false).1.is_err());
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"file.save","params":{"path":"x.docx"}}), false).1.is_err());
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"file.save","params":{}}), false).1.is_ok());
        assert!(e.saved);
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"format.bold","params":{}}), false).1.is_ok());
        assert_eq!(e.ran, ["format.bold"]);
        assert!(run_tool(&mut e, "execute_command", &json!({"command":"format.bold","params":{}}), true).1.is_err());
        let (content, detail) = result_content(&Ok(json!({"x":"y".repeat(protocol::MAX_RESULT)})));
        assert!(content.contains("too large") && detail.is_empty());
    }
}
