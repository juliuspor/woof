use reqwest::Method;
use serde_json::{json, Map, Value};
use woof_llm::{ChatTool, FunctionDefinition, FunctionToolCall};

const TIME_REPORT_PERIODS: [&str; 7] = [
    "today",
    "yesterday",
    "this_week",
    "last_week",
    "this_month",
    "last_7_days",
    "last_30_days",
];

#[derive(Clone, Debug, PartialEq)]
pub struct DaemonToolRequest {
    pub method: Method,
    pub path: String,
    pub body: Option<Value>,
}

pub fn definitions() -> Vec<ChatTool> {
    vec![
        tool(
            "search_memory",
            "Search local captured activity using lexical and semantic recall.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 30, "default": 20}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "get_recent_activity",
            "Get the immediate foreground activity timeline.",
            json!({
                "type": "object",
                "properties": {
                    "minutes": {"type": "integer", "minimum": 1, "maximum": 1440, "default": 60},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 12}
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "get_snapshots",
            "Read full local snapshots after identifying their IDs.",
            json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": 20
                    }
                },
                "required": ["ids"],
                "additionalProperties": false
            }),
        ),
        tool(
            "get_chronicle",
            "Read a pre-generated hour, day, week, month, or year summary.",
            json!({
                "type": "object",
                "properties": {
                    "level": {"type": "string", "enum": ["hour", "day", "week", "month", "year"]},
                    "period": {"type": "string"}
                },
                "required": ["level", "period"],
                "additionalProperties": false
            }),
        ),
        tool(
            "get_working_memory",
            "Read the most recently active local snapshots.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 40}
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "search_wiki",
            "Search the local personal knowledge wiki.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 10}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        tool(
            "get_wiki_page",
            "Read one local wiki page by slug.",
            json!({
                "type": "object",
                "properties": {"slug": {"type": "string"}},
                "required": ["slug"],
                "additionalProperties": false
            }),
        ),
        tool(
            "list_wiki",
            "List local wiki pages, optionally filtered by entity type.",
            json!({
                "type": "object",
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["person", "project", "topic", "tool", "org"]
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "get_time_report",
            "Get locally classified activity time.",
            json!({
                "type": "object",
                "properties": {
                    "period": {"type": "string", "enum": TIME_REPORT_PERIODS},
                    "from": {"type": "string", "description": "Start date as YYYY-MM-DD."},
                    "to": {"type": "string", "description": "Inclusive end date as YYYY-MM-DD."}
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "list_time_rules",
            "List local automatic project-classification rules.",
            empty_schema(),
        ),
        tool(
            "get_capture_status",
            "Check whether local Accessibility capture is running or paused.",
            empty_schema(),
        ),
        tool(
            "list_capture_blacklist",
            "List apps, domains, window titles, and patterns excluded from capture.",
            empty_schema(),
        ),
        tool(
            "get_identity",
            "Read the locally stored preferred name.",
            empty_schema(),
        ),
        tool(
            "get_followups",
            "List commitments, blockers, decisions, and open questions extracted locally.",
            json!({
                "type": "object",
                "properties": {
                    "status": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 50}
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "get_statistics",
            "Read local capture and memory summary statistics.",
            empty_schema(),
        ),
        tool(
            "list_scheduled_rules",
            "List user-created local reminders and recurring check-ins.",
            empty_schema(),
        ),
    ]
}

pub fn daemon_request(call: &FunctionToolCall) -> Result<DaemonToolRequest, String> {
    let arguments = call
        .arguments_json()
        .map_err(|_| "tool arguments are not valid JSON")?;
    let arguments = arguments
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_string())?;

    let request = match call.name.as_str() {
        "search_memory" => get(
            "/search",
            &[
                ("q", required_string(arguments, "query")?),
                ("limit", integer(arguments, "limit", 20, 1, 30)?.to_string()),
            ],
        ),
        "get_recent_activity" => get(
            "/recent-activity",
            &[
                (
                    "minutes",
                    integer(arguments, "minutes", 60, 1, 1_440)?.to_string(),
                ),
                ("limit", integer(arguments, "limit", 12, 1, 20)?.to_string()),
            ],
        ),
        "get_snapshots" => {
            let ids = string_array(arguments, "ids", 20)?;
            get("/snapshots", &[("ids", ids.join(","))])
        }
        "get_chronicle" => {
            let level = required_string(arguments, "level")?;
            if !["hour", "day", "week", "month", "year"].contains(&level.as_str()) {
                return Err("invalid chronicle level".into());
            }
            get(
                "/chronicle",
                &[
                    ("level", level),
                    ("period", required_string(arguments, "period")?),
                ],
            )
        }
        "get_working_memory" => get(
            "/working-memory",
            &[(
                "limit",
                integer(arguments, "limit", 40, 1, 200)?.to_string(),
            )],
        ),
        "search_wiki" => get(
            "/wiki/search",
            &[
                ("q", required_string(arguments, "query")?),
                (
                    "limit",
                    integer(arguments, "limit", 10, 1, 100)?.to_string(),
                ),
            ],
        ),
        "get_wiki_page" => get(
            "/wiki/page",
            &[("slug", required_string(arguments, "slug")?)],
        ),
        "list_wiki" => {
            let mut pairs = vec![(
                "limit",
                integer(arguments, "limit", 50, 1, 200)?.to_string(),
            )];
            if let Some(page_type) = optional_string(arguments, "type")? {
                if !["person", "project", "topic", "tool", "org"].contains(&page_type.as_str()) {
                    return Err("invalid wiki page type".into());
                }
                pairs.push(("type", page_type));
            }
            get("/wiki/list", &pairs)
        }
        "get_time_report" => {
            let mut pairs = Vec::new();
            for key in ["period", "from", "to"] {
                if let Some(value) = optional_string(arguments, key)? {
                    if key == "period" && !TIME_REPORT_PERIODS.contains(&value.as_str()) {
                        return Err("invalid time report period".into());
                    }
                    pairs.push((key, value));
                }
            }
            get("/time/report", &pairs)
        }
        "list_time_rules" => get("/time/rules", &[]),
        "get_capture_status" => get("/capture/status", &[]),
        "list_capture_blacklist" => get("/capture/blacklist", &[]),
        "get_identity" => get("/identity", &[]),
        "get_followups" => {
            let mut pairs = vec![(
                "limit",
                integer(arguments, "limit", 50, 1, 100)?.to_string(),
            )];
            if let Some(status) = optional_string(arguments, "status")? {
                pairs.push(("status", status));
            }
            get("/chronicle/followups", &pairs)
        }
        "get_statistics" => get("/stats/overview", &[]),
        "list_scheduled_rules" => get("/rules", &[]),
        _ => return Err("unknown local tool".into()),
    };
    Ok(request)
}

fn tool(name: &str, description: &str, parameters: Value) -> ChatTool {
    ChatTool::function(FunctionDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        strict: Some(false),
    })
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

fn get(path: &str, pairs: &[(&str, String)]) -> DaemonToolRequest {
    let path = if pairs.is_empty() {
        path.to_string()
    } else {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            serializer.append_pair(key, value);
        }
        format!("{path}?{}", serializer.finish())
    };
    DaemonToolRequest {
        method: Method::GET,
        path,
        body: None,
    }
}

fn required_string(arguments: &Map<String, Value>, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn optional_string(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn integer(
    arguments: &Map<String, Value>,
    key: &str,
    default: i64,
    minimum: i64,
    maximum: i64,
) -> Result<i64, String> {
    match arguments.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .filter(|value| (minimum..=maximum).contains(value))
            .ok_or_else(|| format!("{key} is outside the allowed range")),
    }
}

fn string_array(
    arguments: &Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let values = arguments
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key} must be an array"))?;
    if values.is_empty() || values.len() > maximum {
        return Err(format!("{key} has an invalid item count"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must contain non-empty strings"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> FunctionToolCall {
        FunctionToolCall {
            id: "call_fixture".into(),
            name: name.into(),
            arguments: serde_json::to_string(&arguments).unwrap(),
        }
    }

    #[test]
    fn exposes_only_read_only_memory_surfaces() {
        let definitions = definitions();
        let names = definitions
            .iter()
            .map(|tool| tool.function.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "search_memory",
                "get_recent_activity",
                "get_snapshots",
                "get_chronicle",
                "get_working_memory",
                "search_wiki",
                "get_wiki_page",
                "list_wiki",
                "get_time_report",
                "list_time_rules",
                "get_capture_status",
                "list_capture_blacklist",
                "get_identity",
                "get_followups",
                "get_statistics",
                "list_scheduled_rules",
            ]
        );
        assert!(definitions.iter().all(|tool| {
            tool.function.parameters["additionalProperties"] == Value::Bool(false)
        }));
    }

    #[test]
    fn routes_and_percent_encodes_read_tools() {
        let request = daemon_request(&call(
            "search_memory",
            json!({"query": "boxer & roadmap", "limit": 7}),
        ))
        .unwrap();
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.path, "/search?q=boxer+%26+roadmap&limit=7");
        assert!(request.body.is_none());
    }

    #[test]
    fn time_report_uses_the_daemon_period_vocabulary() {
        let definition = definitions()
            .into_iter()
            .find(|tool| tool.function.name == "get_time_report")
            .unwrap();
        assert_eq!(
            definition.function.parameters["properties"]["period"]["enum"],
            json!(TIME_REPORT_PERIODS)
        );
        let request =
            daemon_request(&call("get_time_report", json!({"period": "this_week"}))).unwrap();
        assert_eq!(request.path, "/time/report?period=this_week");
        assert!(daemon_request(&call("get_time_report", json!({"period": "week"}))).is_err());
    }

    #[test]
    fn rejects_adversarial_mutating_tool_calls_without_dispatching() {
        for name in [
            "save_time_rule",
            "assign_project",
            "pause_capture",
            "resume_capture",
            "set_capture_blacklist",
            "set_identity_name",
            "save_scheduled_rule",
            "delete_scheduled_rule",
            "delete_all_data",
        ] {
            assert!(
                daemon_request(&call(name, json!({"blacklist": [], "name": "attacker"}))).is_err(),
                "mutating tool {name} must never produce a daemon request"
            );
        }
    }
}
