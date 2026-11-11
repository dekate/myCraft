//! AI chat engine: user-supplied provider keys, model routing (failover,
//! round robin, auto small/big), and blocking HTTP calls run on worker
//! threads so the UI never stalls. UI lives in `ai_tab.rs`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::state::MyCraftState;

/// How long a route that answered 429/529 is pushed to the back of the queue.
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
/// Attached files are sent whole; bigger ones are refused rather than truncated.
const MAX_FILE_BYTES: u64 = 200_000;
/// Only the most recent turns are replayed to the model.
const HISTORY_LIMIT: usize = 20;

// ───────────────────────────── Providers & routes ─────────────────────────────

/// Request/response format. Most vendors (OpenAI, Gemini, DeepSeek, Groq,
/// OpenRouter, Ollama…) speak the OpenAI chat-completions format.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Wire {
    OpenAi,
    Anthropic,
}

pub struct Preset {
    pub name: &'static str,
    pub wire: Wire,
    pub base_url: &'static str,
    pub needs_key: bool,
    /// Default small / big model; empty means "pick one with ⟳ Models".
    pub small: &'static str,
    pub big: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "Anthropic", wire: Wire::Anthropic, base_url: "https://api.anthropic.com/v1", needs_key: true, small: "claude-haiku-5-5", big: "claude-opus-5-5" },
    Preset { name: "OpenAI", wire: Wire::OpenAi, base_url: "https://api.openai.com/v1", needs_key: true, small: "", big: "" },
    Preset { name: "Google Gemini", wire: Wire::OpenAi, base_url: "https://generativelanguage.googleapis.com/v1beta/openai", needs_key: true, small: "", big: "" },
    Preset { name: "DeepSeek", wire: Wire::OpenAi, base_url: "https://api.deepseek.com/v1", needs_key: true, small: "deepseek-chat", big: "deepseek-reasoner" },
    Preset { name: "OpenRouter", wire: Wire::OpenAi, base_url: "https://openrouter.ai/api/v1", needs_key: true, small: "", big: "" },
    Preset { name: "Groq", wire: Wire::OpenAi, base_url: "https://api.groq.com/openai/v1", needs_key: true, small: "", big: "" },
    Preset { name: "Ollama (local)", wire: Wire::OpenAi, base_url: "http://localhost:11434/v1", needs_key: false, small: "", big: "" },
    Preset { name: "Custom (OpenAI-compatible)", wire: Wire::OpenAi, base_url: "http://localhost:8000/v1", needs_key: false, small: "", big: "" },
];

/// One API key at one endpoint. Several entries may share a vendor
/// (e.g. two OpenAI keys) so round robin / failover can spread across them.
#[derive(Clone, Debug)]
pub struct Provider {
    pub id: u32,
    pub label: String,
    pub wire: Wire,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Tier {
    Small,
    Big,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Small => "small",
            Tier::Big => "big",
        }
    }
}

/// A (provider, model) pair in the combo. List order is priority order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub provider_id: u32,
    pub model: String,
    pub tier: Tier,
    pub enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum RouteMode {
    /// Try routes top to bottom, moving on after any error.
    #[default]
    Failover,
    /// Rotate the starting route per message, then fail over.
    RoundRobin,
    /// Classify the prompt, prefer routes of that tier, fall back to the other.
    AutoSize,
    /// One fixed route, no fallback.
    Single,
}

impl RouteMode {
    pub fn label(self) -> &'static str {
        match self {
            RouteMode::Failover => "Combo: Failover",
            RouteMode::RoundRobin => "Combo: Round robin",
            RouteMode::AutoSize => "Combo: Auto size",
            RouteMode::Single => "Single model",
        }
    }
}

pub struct AiConfig {
    pub providers: Vec<Provider>,
    pub routes: Vec<Route>,
    pub mode: RouteMode,
    /// (provider_id, model) used by `RouteMode::Single`.
    pub single: Option<(u32, String)>,
    /// Send a project overview (file tree + key manifests) with every message.
    pub auto_context: bool,
    /// Offer the model the `run_command` tool.
    pub allow_commands: bool,
    /// Run the model's commands without asking first.
    pub auto_run: bool,
    next_id: u32,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct StoredConfig {
    providers: Vec<StoredProvider>,
    routes: Vec<Route>,
    mode: RouteMode,
    single: Option<(u32, String)>,
    // Stored as "off" flags so a missing field means the safe default.
    no_auto_context: bool,
    no_commands: bool,
    auto_run: bool,
    next_id: u32,
}

#[derive(Serialize, Deserialize)]
struct StoredProvider {
    id: u32,
    label: String,
    wire: Wire,
    base_url: String,
    enabled: bool,
    /// Hex of the DPAPI-encrypted key (Windows); readable only by this user account.
    key_blob: String,
}

pub fn config_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("myCraft")
        .join("ai.json")
}

impl AiConfig {
    fn empty() -> Self {
        Self { providers: Vec::new(), routes: Vec::new(), mode: RouteMode::Failover, single: None, auto_context: true, allow_commands: true, auto_run: false, next_id: 1 }
    }

    /// Load from disk. Returns a warning when the file is unreadable or a key
    /// could not be decrypted (e.g. the file was copied from another account).
    pub fn load() -> (Self, Option<String>) {
        let path = config_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return (Self::empty(), None),
        };
        let stored: StoredConfig = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => return (Self::empty(), Some(format!("AI config unreadable ({}): {}", path.display(), e))),
        };
        let mut warning = None;
        let providers = stored
            .providers
            .into_iter()
            .map(|p| {
                let api_key = if p.key_blob.is_empty() {
                    String::new()
                } else {
                    hex_decode(&p.key_blob).and_then(|b| secret::unprotect(&b)).unwrap_or_else(|| {
                        warning = Some(format!("API key for '{}' could not be decrypted; re-enter it.", p.label));
                        String::new()
                    })
                };
                Provider { id: p.id, label: p.label, wire: p.wire, base_url: p.base_url, api_key, enabled: p.enabled }
            })
            .collect::<Vec<_>>();
        let next_id = stored.next_id.max(providers.iter().map(|p| p.id + 1).max().unwrap_or(1));
        let cfg = Self {
            providers,
            routes: stored.routes,
            mode: stored.mode,
            single: stored.single,
            auto_context: !stored.no_auto_context,
            allow_commands: !stored.no_commands,
            auto_run: stored.auto_run,
            next_id,
        };
        (cfg, warning)
    }

    pub fn save(&self) -> Result<(), String> {
        let providers = self
            .providers
            .iter()
            .map(|p| {
                let key_blob = if p.api_key.is_empty() {
                    String::new()
                } else {
                    hex_encode(&secret::protect(&p.api_key).ok_or("could not encrypt API key")?)
                };
                Ok(StoredProvider { id: p.id, label: p.label.clone(), wire: p.wire, base_url: p.base_url.clone(), enabled: p.enabled, key_blob })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let stored = StoredConfig {
            providers,
            routes: self.routes.clone(),
            mode: self.mode,
            single: self.single.clone(),
            no_auto_context: !self.auto_context,
            no_commands: !self.allow_commands,
            auto_run: self.auto_run,
            next_id: self.next_id,
        };
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {}", path.display(), e))
    }

    /// Add a provider from a preset, plus routes for its default models.
    pub fn add_preset(&mut self, preset: &Preset) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let same = self.providers.iter().filter(|p| p.label.starts_with(preset.name)).count();
        let label = if same == 0 { preset.name.to_string() } else { format!("{} #{}", preset.name, same + 1) };
        self.providers.push(Provider { id, label, wire: preset.wire, base_url: preset.base_url.to_string(), api_key: String::new(), enabled: true });
        for (model, tier) in [(preset.small, Tier::Small), (preset.big, Tier::Big)] {
            if !model.is_empty() {
                self.routes.push(Route { provider_id: id, model: model.to_string(), tier, enabled: true });
            }
        }
        id
    }

    pub fn remove_provider(&mut self, id: u32) {
        self.providers.retain(|p| p.id != id);
        self.routes.retain(|r| r.provider_id != id);
        if self.single.as_ref().is_some_and(|(pid, _)| *pid == id) {
            self.single = None;
        }
    }

    pub fn provider(&self, id: u32) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn route_label(&self, r: &Route) -> String {
        let p = self.provider(r.provider_id).map(|p| p.label.as_str()).unwrap_or("?");
        format!("{} · {}", p, r.model)
    }

    fn usable(&self, r: &Route) -> bool {
        r.enabled && !r.model.trim().is_empty() && self.provider(r.provider_id).is_some_and(|p| p.enabled)
    }

    /// Route indices to try, in order, for one message.
    pub fn plan(&self, tier: Tier, rr_cursor: usize, is_cooling: impl Fn(&Route) -> bool) -> Vec<usize> {
        let usable: Vec<usize> = (0..self.routes.len()).filter(|&i| self.usable(&self.routes[i])).collect();
        let mut order = match self.mode {
            RouteMode::Failover => usable,
            RouteMode::RoundRobin => {
                let mut v = usable;
                if !v.is_empty() {
                    let k = rr_cursor % v.len();
                    v.rotate_left(k);
                }
                v
            }
            RouteMode::AutoSize => {
                let (mut first, rest): (Vec<usize>, Vec<usize>) = usable.into_iter().partition(|&i| self.routes[i].tier == tier);
                first.extend(rest);
                first
            }
            RouteMode::Single => match &self.single {
                Some((pid, model)) => usable.into_iter().filter(|&i| self.routes[i].provider_id == *pid && self.routes[i].model == *model).take(1).collect(),
                None => Vec::new(),
            },
        };
        // Rate-limited routes go last instead of being dropped: a cooling key
        // is still better than no answer. Stable sort keeps priority otherwise.
        order.sort_by_key(|&i| is_cooling(&self.routes[i]));
        order
    }
}

// ───────────────────────────── Task-size classifier ─────────────────────────────

const BIG_TASK_WORDS: &[&str] = &[
    "refactor", "architect", "design", "implement", "debug", "migrat", "optimi", "analy",
    "review", "rewrite", "step by step", "explain why", "build ", "create a", "write a", "generate",
];

/// Cheap local heuristic deciding whether a prompt needs the big model.
/// Returns the tier and a short reason shown in the UI.
pub fn classify(prompt: &str, context_chars: usize) -> (Tier, &'static str) {
    let p = prompt.to_lowercase();
    if context_chars > 4_000 {
        (Tier::Big, "large attached context")
    } else if prompt.chars().count() > 1_200 {
        (Tier::Big, "long prompt")
    } else if p.contains("```") || prompt.lines().count() > 12 {
        (Tier::Big, "contains code")
    } else if BIG_TASK_WORDS.iter().any(|w| p.contains(w)) {
        (Tier::Big, "complex task")
    } else {
        (Tier::Small, "short question")
    }
}

// ───────────────────────────── Model calls & agent loop (worker thread) ─────────────────────────────

/// Upper bound on model ↔ tool round trips for one user message.
const MAX_STEPS: usize = 12;
/// A command still running after this is killed (dev servers, watchers…).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// Command output beyond this is cut, and the model is told so.
const MAX_OUTPUT_CHARS: usize = 20_000;

const TOOL_NAME: &str = "run_command";
const TOOL_DESCRIPTION: &str = "Run a Windows cmd.exe command in the project folder and return its \
stdout, stderr and exit code. Non-interactive: stdin is closed, and commands that wait for input or \
never exit (dev servers, watchers) are killed after 120 seconds. Use it to inspect the project \
(dir, type, findstr, git status/log) and to run tools such as composer, npm, php artisan or tests.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A command the model ran; `meta` holds the command, `text` its output.
    Tool,
    Error,
}

pub struct ChatMsg {
    pub role: Role,
    pub text: String,
    /// Header line: who answered, tier, timing, failover trail.
    pub meta: String,
    pub time: String,
}

#[derive(Clone)]
struct Target {
    provider_id: u32,
    label: String,
    wire: Wire,
    base_url: String,
    api_key: String,
    model: String,
}

pub struct Attempt {
    pub provider_id: u32,
    pub label: String,
    pub model: String,
    pub error: Option<String>,
    pub rate_limited: bool,
}

struct Outcome {
    reply: Option<String>,
    attempts: Vec<Attempt>,
    elapsed: Duration,
    commands: usize,
}

/// Worker → UI messages for one request.
enum Event {
    /// Text the model wrote alongside a tool call.
    Note(String),
    /// The model wants to run a command; reply true to allow.
    Ask(String, mpsc::Sender<bool>),
    Ran { command: String, output: String },
    Done(Outcome),
}

#[derive(Clone)]
struct ToolCall {
    id: String,
    command: String,
}

/// Provider-neutral conversation so a failover can switch wire formats mid-task.
enum Turn {
    User(String),
    /// `raw` keeps Anthropic's content blocks (incl. thinking) to send back unchanged.
    Assistant { text: String, calls: Vec<ToolCall>, raw: Option<Value> },
    Results(Vec<(String, String)>),
}

struct Reply {
    text: String,
    calls: Vec<ToolCall>,
    raw: Option<Value>,
}

struct CallError {
    status: Option<u16>,
    msg: String,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}

fn send(req: ureq::Request, body: Option<Value>) -> Result<Value, CallError> {
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(r) => r.into_json::<Value>().map_err(|e| CallError { status: None, msg: format!("invalid JSON response: {}", e) }),
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            Err(CallError { status: Some(code), msg: format!("HTTP {}: {}", code, api_error_message(&body)) })
        }
        Err(ureq::Error::Transport(t)) => Err(CallError { status: None, msg: t.to_string() }),
    }
}

fn api_error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    parsed
        .as_ref()
        .and_then(|v| v["error"]["message"].as_str().or_else(|| v["error"].as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(200).collect())
}

fn authed(req: ureq::Request, t: &Target) -> ureq::Request {
    if t.api_key.is_empty() {
        return req;
    }
    match t.wire {
        Wire::Anthropic => req.set("x-api-key", &t.api_key),
        Wire::OpenAi => req.set("Authorization", &format!("Bearer {}", t.api_key)),
    }
}

fn anthropic_messages(turns: &[Turn]) -> Vec<Value> {
    turns
        .iter()
        .map(|t| match t {
            Turn::User(text) => json!({"role": "user", "content": text}),
            Turn::Assistant { raw: Some(raw), .. } => json!({"role": "assistant", "content": raw}),
            Turn::Assistant { text, calls, raw: None } => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in calls {
                    blocks.push(json!({"type": "tool_use", "id": c.id, "name": TOOL_NAME, "input": {"command": c.command}}));
                }
                json!({"role": "assistant", "content": blocks})
            }
            Turn::Results(results) => {
                let blocks: Vec<Value> = results.iter().map(|(id, out)| json!({"type": "tool_result", "tool_use_id": id, "content": out})).collect();
                json!({"role": "user", "content": blocks})
            }
        })
        .collect()
}

fn openai_messages(system: &str, turns: &[Turn]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for t in turns {
        match t {
            Turn::User(text) => out.push(json!({"role": "user", "content": text})),
            Turn::Assistant { text, calls, .. } => {
                let mut m = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    m["tool_calls"] = calls
                        .iter()
                        .map(|c| json!({"id": c.id, "type": "function", "function": {"name": TOOL_NAME, "arguments": json!({"command": c.command}).to_string()}}))
                        .collect();
                }
                out.push(m);
            }
            Turn::Results(results) => {
                for (id, output) in results {
                    out.push(json!({"role": "tool", "tool_call_id": id, "content": output}));
                }
            }
        }
    }
    out
}

fn call_model(agent: &ureq::Agent, t: &Target, system: &str, turns: &[Turn], tools: bool) -> Result<Reply, CallError> {
    let schema = json!({"type": "object", "properties": {"command": {"type": "string", "description": "The cmd.exe command line to run"}}, "required": ["command"]});
    match t.wire {
        Wire::Anthropic => {
            let mut body = json!({"model": t.model, "max_tokens": 16000, "system": system, "messages": anthropic_messages(turns)});
            if tools {
                body["tools"] = json!([{"name": TOOL_NAME, "description": TOOL_DESCRIPTION, "input_schema": schema}]);
            }
            let req = authed(agent.post(&endpoint(&t.base_url, "messages")), t).set("anthropic-version", "2026-10-21");
            let v = send(req, Some(body))?;
            if v["stop_reason"] == "refusal" {
                let cat = v["stop_details"]["category"].as_str().unwrap_or("unspecified");
                return Err(CallError { status: None, msg: format!("model declined (category: {})", cat) });
            }
            let blocks = v["content"].as_array().cloned().unwrap_or_default();
            let text: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            let calls: Vec<ToolCall> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| ToolCall { id: b["id"].as_str().unwrap_or_default().to_string(), command: b["input"]["command"].as_str().unwrap_or_default().to_string() })
                .collect();
            if text.is_empty() && calls.is_empty() {
                return Err(CallError { status: None, msg: "empty response".into() });
            }
            Ok(Reply { text: text.join("\n"), calls, raw: Some(v["content"].clone()) })
        }
        Wire::OpenAi => {
            let mut body = json!({"model": t.model, "messages": openai_messages(system, turns)});
            if tools {
                body["tools"] = json!([{"type": "function", "function": {"name": TOOL_NAME, "description": TOOL_DESCRIPTION, "parameters": schema}}]);
            }
            let v = send(authed(agent.post(&endpoint(&t.base_url, "chat/completions")), t), Some(body))?;
            let msg = &v["choices"][0]["message"];
            let text = msg["content"].as_str().unwrap_or_default().to_string();
            let calls: Vec<ToolCall> = msg["tool_calls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            let args: Value = serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
                            ToolCall { id: c["id"].as_str().unwrap_or_default().to_string(), command: args["command"].as_str().unwrap_or_default().to_string() }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if text.trim().is_empty() && calls.is_empty() {
                return Err(CallError { status: None, msg: "empty response".into() });
            }
            Ok(Reply { text, calls, raw: None })
        }
    }
}

struct Job {
    targets: Vec<Target>,
    system: String,
    history: Vec<(Role, String)>,
    tools: bool,
    auto_run: bool,
    project: PathBuf,
}

/// Call the model, run any commands it asks for (with approval), feed the
/// output back, and repeat until it answers in plain text. Each model call
/// fails over along the route plan; a route that worked keeps the task.
fn run_agent(job: Job, events: mpsc::Sender<Event>) {
    let started = Instant::now();
    let agent = agent();
    let mut attempts = Vec::new();
    let mut commands = 0;
    let mut turns: Vec<Turn> = job
        .history
        .into_iter()
        .map(|(r, text)| if r == Role::User { Turn::User(text) } else { Turn::Assistant { text, calls: Vec::new(), raw: None } })
        .collect();
    let mut ti = 0;
    let done = |reply: Option<String>, attempts: Vec<Attempt>, commands: usize| Event::Done(Outcome { reply, attempts, elapsed: started.elapsed(), commands });

    for step in 0..MAX_STEPS {
        // Last step: no tools, so the model has to wrap up in text.
        let tools = job.tools && step + 1 < MAX_STEPS;
        let reply = loop {
            let Some(t) = job.targets.get(ti) else {
                let _ = events.send(done(None, attempts, commands));
                return;
            };
            match call_model(&agent, t, &job.system, &turns, tools) {
                Ok(r) => break r,
                Err(e) => {
                    attempts.push(Attempt { provider_id: t.provider_id, label: t.label.clone(), model: t.model.clone(), error: Some(e.msg), rate_limited: matches!(e.status, Some(429) | Some(529)) });
                    ti += 1;
                }
            }
        };

        if reply.calls.is_empty() {
            let t = &job.targets[ti];
            attempts.push(Attempt { provider_id: t.provider_id, label: t.label.clone(), model: t.model.clone(), error: None, rate_limited: false });
            let _ = events.send(done(Some(reply.text), attempts, commands));
            return;
        }

        if !reply.text.trim().is_empty() && events.send(Event::Note(reply.text.clone())).is_err() {
            return; // UI stopped this request
        }
        let calls = reply.calls.clone();
        turns.push(Turn::Assistant { text: reply.text, calls: reply.calls, raw: reply.raw });

        let mut results = Vec::new();
        for c in calls {
            let allowed = if c.command.trim().is_empty() {
                false
            } else if job.auto_run {
                true
            } else {
                let (tx, rx) = mpsc::channel();
                if events.send(Event::Ask(c.command.clone(), tx)).is_err() {
                    return;
                }
                rx.recv().unwrap_or(false)
            };
            let output = if allowed {
                commands += 1;
                let out = run_command(&c.command, &job.project);
                if events.send(Event::Ran { command: c.command.clone(), output: out.clone() }).is_err() {
                    return;
                }
                out
            } else {
                "The user declined to run this command.".to_string()
            };
            results.push((c.id, output));
        }
        turns.push(Turn::Results(results));
    }
}

/// Run one command line through cmd.exe in `dir`, without a console window.
fn run_command(command: &str, dir: &Path) -> String {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("cmd");
    #[cfg(windows)]
    {
        // /S strips the outer quotes and keeps the rest verbatim, so the
        // command line reaches cmd exactly as the model wrote it.
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(format!("/D /S /C \"{}\"", command));
    }
    #[cfg(not(windows))]
    cmd.arg("/C").arg(command);
    cmd.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::hide_console(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("failed to start cmd.exe: {}", e),
    };
    // Read pipes on threads: a full pipe would otherwise block the child, and
    // a leftover grandchild can hold them open after cmd exits.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut pipe = pipe;
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    drop(tx);

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break None,
        }
    };
    let mut output = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(2)) {
        output.extend(chunk);
    }
    let mut text = String::from_utf8_lossy(&output).trim_end().to_string();
    let total = text.chars().count();
    if total > MAX_OUTPUT_CHARS {
        text = text.chars().take(MAX_OUTPUT_CHARS).collect();
        text.push_str(&format!("\n[output cut: showing {} of {} characters]", MAX_OUTPUT_CHARS, total));
    }
    let footer = match status {
        Some(s) => format!("[exit code {}]", s.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into())),
        None => format!("[killed after {}s timeout]", COMMAND_TIMEOUT.as_secs()),
    };
    if text.is_empty() {
        footer
    } else {
        format!("{}\n{}", text, footer)
    }
}

fn list_models(t: Target) -> Result<Vec<String>, String> {
    let agent = agent();
    let mut req = authed(agent.get(&endpoint(&t.base_url, "models")), &t);
    if t.wire == Wire::Anthropic {
        req = req.set("anthropic-version", "2026-10-21").query("limit", "1000");
    }
    let v = send(req, None).map_err(|e| e.msg)?;
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

// ───────────────────────────── Project overview ─────────────────────────────

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "vendor", "target", "storage", "bootstrap", ".idea", ".vscode", "dist", "build", ".next", "__pycache__", ".venv", "venv"];
/// Manifests that explain how a project is built and run. `.env` is never read.
const KEY_FILES: &[&str] = &["README.md", "composer.json", "package.json", "Cargo.toml", "go.mod", "requirements.txt", "pyproject.toml", "docker-compose.yml", ".env.example"];
const MAX_TREE_ENTRIES: usize = 200;
const MAX_KEY_FILE_CHARS: usize = 6_000;

/// File tree (two levels, noise folders skipped) plus key manifest files, sent
/// as context so the model knows the project without being asked.
pub fn project_overview(dir: &Path) -> String {
    fn entries(dir: &Path) -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| (e.file_name().to_string_lossy().to_string(), e.file_type().is_ok_and(|t| t.is_dir()))).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        v
    }

    let mut tree = Vec::new();
    let mut more = 0;
    for (name, is_dir) in entries(dir) {
        if tree.len() >= MAX_TREE_ENTRIES {
            more += 1;
            continue;
        }
        tree.push(format!("{}{}", name, if is_dir { "/" } else { "" }));
        if is_dir && !SKIP_DIRS.contains(&name.as_str()) {
            for (sub, sub_dir) in entries(&dir.join(&name)) {
                if tree.len() >= MAX_TREE_ENTRIES {
                    more += 1;
                    continue;
                }
                tree.push(format!("  {}{}", sub, if sub_dir { "/" } else { "" }));
            }
        }
    }
    let mut s = format!("Project file tree (two levels; contents of {} not listed):\n{}", SKIP_DIRS.join(", "), tree.join("\n"));
    if more > 0 {
        s.push_str(&format!("\n… and {} more entries", more));
    }

    for name in KEY_FILES {
        let Ok(content) = std::fs::read_to_string(dir.join(name)) else { continue };
        let total = content.chars().count();
        let shown: String = content.chars().take(MAX_KEY_FILE_CHARS).collect();
        s.push_str(&format!("\n\n--- {} ---\n{}", name, shown));
        if total > MAX_KEY_FILE_CHARS {
            s.push_str(&format!("\n[cut: showing {} of {} characters]", MAX_KEY_FILE_CHARS, total));
        }
    }
    s
}

// ───────────────────────────── Chat session (UI state) ─────────────────────────────

pub struct ContextFile {
    pub rel: String,
    pub content: String,
}

struct Pending {
    rx: Receiver<Event>,
    tier_note: String,
    approval: Option<(String, mpsc::Sender<bool>)>,
}

pub struct AiChat {
    pub config: AiConfig,
    pub messages: Vec<ChatMsg>,
    pub input: String,
    pub project: String,
    pub context_files: Vec<ContextFile>,
    pub attach_input: String,
    pub notice: Option<String>,
    pub setup_open: bool,
    pub show_context: bool,
    pub add_preset_idx: usize,
    /// Model ids fetched per provider id (for the model dropdowns).
    pub models: HashMap<u32, Vec<String>>,
    pending: Option<Pending>,
    model_fetches: Vec<(u32, Receiver<Result<Vec<String>, String>>)>,
    fetch_tried: std::collections::HashSet<u32>,
    cooldown: HashMap<(u32, String), Instant>,
    rr_cursor: usize,
}

impl AiChat {
    pub fn new(www_root: &str) -> Self {
        let (config, warning) = AiConfig::load();
        Self {
            config,
            messages: Vec::new(),
            input: String::new(),
            project: www_root.to_string(),
            context_files: Vec::new(),
            attach_input: String::new(),
            notice: warning,
            setup_open: false,
            show_context: false,
            add_preset_idx: 0,
            models: HashMap::new(),
            pending: None,
            model_fetches: Vec::new(),
            fetch_tried: Default::default(),
            cooldown: HashMap::new(),
            rr_cursor: 0,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The command waiting for the user's approval, if any.
    pub fn pending_approval(&self) -> Option<&str> {
        self.pending.as_ref()?.approval.as_ref().map(|(c, _)| c.as_str())
    }

    pub fn answer_approval(&mut self, allow: bool) {
        if let Some((_, tx)) = self.pending.as_mut().and_then(|p| p.approval.take()) {
            let _ = tx.send(allow);
        }
    }

    /// Abandon the running request. A model call already in flight finishes
    /// in the background and is discarded; no further commands run.
    pub fn stop(&mut self) {
        if let Some(p) = self.pending.take() {
            drop(p);
            self.messages.push(ChatMsg { role: Role::Error, text: "Stopped.".into(), meta: String::new(), time: MyCraftState::current_time_str() });
        }
    }

    pub fn is_fetching(&self, provider_id: u32) -> bool {
        self.model_fetches.iter().any(|(id, _)| *id == provider_id)
    }

    pub fn cooling_secs(&self, r: &Route) -> Option<u64> {
        self.cooldown
            .get(&(r.provider_id, r.model.clone()))
            .and_then(|until| until.checked_duration_since(Instant::now()))
            .map(|d| d.as_secs() + 1)
    }

    fn context_chars(&self) -> usize {
        self.context_files.iter().map(|f| f.content.len()).sum()
    }

    fn plan_for(&self, prompt: &str) -> (Vec<usize>, Tier, &'static str) {
        let (tier, reason) = classify(prompt, self.context_chars());
        let plan = self.config.plan(tier, self.rr_cursor, |r| self.cooling_secs(r).is_some());
        (plan, tier, reason)
    }

    /// Where the current draft would go, for the composer hint.
    pub fn preview(&self) -> String {
        let (plan, tier, reason) = self.plan_for(&self.input);
        let Some(&first) = plan.first() else {
            return "No usable model — add a provider key and a route in 🔑 Providers.".into();
        };
        let target = self.config.route_label(&self.config.routes[first]);
        let fallbacks = plan.len() - 1;
        let size = if self.config.mode == RouteMode::AutoSize { format!("{} task ({}) → ", tier.label(), reason) } else { String::new() };
        format!("{}{}{}", size, target, if fallbacks > 0 { format!("  (+{} fallback)", fallbacks) } else { String::new() })
    }

    /// Send the composer text. Returns a line for the activity log.
    pub fn send(&mut self) -> Result<String, String> {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.is_busy() {
            return Err("Nothing to send.".into());
        }
        let (plan, tier, reason) = self.plan_for(&prompt);
        if plan.is_empty() {
            return Err("No usable model route. Open 🔑 Providers, add a key and enable a route.".into());
        }
        let targets: Vec<Target> = plan
            .iter()
            .filter_map(|&i| {
                let r = &self.config.routes[i];
                let p = self.config.provider(r.provider_id)?;
                Some(Target { provider_id: p.id, label: p.label.clone(), wire: p.wire, base_url: p.base_url.clone(), api_key: p.api_key.clone(), model: r.model.clone() })
            })
            .collect();

        self.messages.push(ChatMsg { role: Role::User, text: prompt, meta: String::new(), time: MyCraftState::current_time_str() });
        self.input.clear();

        let job = Job {
            targets,
            system: self.system_prompt(),
            history: self.history(),
            tools: self.config.allow_commands,
            auto_run: self.config.auto_run,
            project: PathBuf::from(&self.project),
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || run_agent(job, tx));
        let tier_note = if self.config.mode == RouteMode::AutoSize { format!("auto: {} ({})", tier.label(), reason) } else { self.config.mode.label().to_string() };
        let first = self.config.route_label(&self.config.routes[plan[0]]);
        self.pending = Some(Pending { rx, tier_note: tier_note.clone(), approval: None });
        Ok(format!("AI request → {} [{}]", first, tier_note))
    }

    fn system_prompt(&self) -> String {
        let mut s = format!(
            "You are the AI assistant built into myCraft, a local Windows full-stack dev box \
             (web server, PHP, MySQL, Node.js, Redis, Mailpit). Help the developer with their project. \
             Be concise and practical; put code in fenced code blocks.\n\nProject folder: {}\n",
            self.project
        );
        if self.config.allow_commands {
            s.push_str(
                "\nYou can run commands on the developer's machine with the run_command tool \
                 (cmd.exe, working directory = the project folder). Use it to look at the project \
                 yourself instead of asking the developer to paste output. The developer may have to \
                 approve each command. Prefer read-only commands; before anything that deletes, \
                 overwrites or installs, say what it will do.\n",
            );
        }
        if self.config.auto_context {
            s.push('\n');
            s.push_str(&project_overview(Path::new(&self.project)));
            s.push('\n');
        }
        for f in &self.context_files {
            s.push_str(&format!("\n--- file: {} ---\n{}\n--- end of {} ---\n", f.rel, f.content, f.rel));
        }
        s
    }

    /// Recent user/assistant turns, errors and command logs dropped and
    /// same-role runs merged (a failed request leaves two user turns in a row).
    fn history(&self) -> Vec<(Role, String)> {
        let mut out: Vec<(Role, String)> = Vec::new();
        for m in self.messages.iter().filter(|m| matches!(m.role, Role::User | Role::Assistant)) {
            match out.last_mut() {
                Some((r, text)) if *r == m.role => {
                    text.push_str("\n\n");
                    text.push_str(&m.text);
                }
                _ => out.push((m.role, m.text.clone())),
            }
        }
        let skip = out.len().saturating_sub(HISTORY_LIMIT);
        let mut out: Vec<_> = out.into_iter().skip(skip).collect();
        // Both APIs expect the conversation to start with a user turn.
        while out.first().is_some_and(|(r, _)| *r != Role::User) {
            out.remove(0);
        }
        out
    }

    /// Collect finished background work. Returns (success, log line) pairs.
    pub fn poll(&mut self) -> Vec<(bool, String)> {
        let mut logs = Vec::new();

        loop {
            let Some(p) = &mut self.pending else { break };
            match p.rx.try_recv() {
                Ok(Event::Note(text)) => self.messages.push(ChatMsg { role: Role::Assistant, text, meta: "working…".into(), time: MyCraftState::current_time_str() }),
                Ok(Event::Ask(command, tx)) => {
                    crate::notify::send("myCraft AI", &format!("Waiting for approval to run: {}", command));
                    p.approval = Some((command, tx));
                }
                Ok(Event::Ran { command, output }) => {
                    logs.push((true, format!("AI ran: {}", command)));
                    self.messages.push(ChatMsg { role: Role::Tool, text: output, meta: command, time: MyCraftState::current_time_str() });
                }
                Ok(Event::Done(outcome)) => {
                    let has_reply = outcome.reply.is_some();
                    let reply_preview = outcome.reply.as_deref().and_then(|t| {
                        t.lines().find(|l| !l.trim().is_empty())
                    }).map(|l| l.chars().take(90).collect::<String>());

                    let tier_note = self.pending.take().map(|p| p.tier_note).unwrap_or_default();
                    logs.push(self.finish(outcome, &tier_note));

                    let body = match reply_preview {
                        Some(preview) => format!("{preview}…\nAI is ready for your next prompt."),
                        None if has_reply => "AI finished responding. Ready for your next prompt.".into(),
                        None => "AI request completed. Ready for your next prompt.".into(),
                    };
                    crate::notify::send("myCraft AI", &body);
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.messages.push(ChatMsg { role: Role::Error, text: "Request worker stopped unexpectedly.".into(), meta: "error".into(), time: MyCraftState::current_time_str() });
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let mut i = 0;
        while i < self.model_fetches.len() {
            match self.model_fetches[i].1.try_recv() {
                Ok(res) => {
                    let (id, _) = self.model_fetches.remove(i);
                    let label = self.config.provider(id).map(|p| p.label.clone()).unwrap_or_default();
                    match res {
                        Ok(models) => {
                            self.notice = Some(format!("{}: {} models available.", label, models.len()));
                            logs.push((true, format!("AI provider '{}' connected ({} models).", label, models.len())));
                            self.models.insert(id, models);
                        }
                        Err(e) => {
                            self.notice = Some(format!("{}: {}", label, e));
                            logs.push((false, format!("AI provider '{}' check failed: {}", label, e)));
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.model_fetches.remove(i);
                }
                Err(TryRecvError::Empty) => i += 1,
            }
        }
        logs
    }

    fn finish(&mut self, outcome: Outcome, tier_note: &str) -> (bool, String) {
        let now = Instant::now();
        for a in outcome.attempts.iter().filter(|a| a.rate_limited) {
            self.cooldown.insert((a.provider_id, a.model.clone()), now + RATE_LIMIT_COOLDOWN);
        }
        self.cooldown.retain(|_, until| *until > now);
        if self.config.mode == RouteMode::RoundRobin {
            self.rr_cursor = self.rr_cursor.wrapping_add(1);
        }

        let failed: Vec<String> = outcome
            .attempts
            .iter()
            .filter_map(|a| a.error.as_ref().map(|e| format!("{} · {}: {}", a.label, a.model, e)))
            .collect();
        let secs = outcome.elapsed.as_secs_f32();
        let time = MyCraftState::current_time_str();
        let cmds = if outcome.commands > 0 { format!(" · {} command(s)", outcome.commands) } else { String::new() };

        match (outcome.reply, outcome.attempts.last()) {
            (Some(text), Some(winner)) => {
                let mut meta = format!("{} · {} · {} · {:.1}s{}", winner.model, winner.label, tier_note, secs, cmds);
                if !failed.is_empty() {
                    meta.push_str(&format!(" · failover after {} error(s)", failed.len()));
                }
                let log = format!("AI reply from {} via {} in {:.1}s{}", winner.model, winner.label, secs, if failed.is_empty() { String::new() } else { format!(" (skipped: {})", failed.join("; ")) });
                self.messages.push(ChatMsg { role: Role::Assistant, text, meta, time });
                (true, log)
            }
            _ => {
                let text = format!("Every route failed:\n{}", failed.iter().map(|f| format!("• {}", f)).collect::<Vec<_>>().join("\n"));
                self.messages.push(ChatMsg { role: Role::Error, text, meta: format!("{} · {:.1}s", tier_note, secs), time });
                (false, format!("AI request failed on all {} route(s).", failed.len()))
            }
        }
    }

    pub fn fetch_models(&mut self, provider_id: u32) {
        let Some(p) = self.config.provider(provider_id) else { return };
        let target = Target { provider_id, label: p.label.clone(), wire: p.wire, base_url: p.base_url.clone(), api_key: p.api_key.clone(), model: String::new() };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(list_models(target));
        });
        self.model_fetches.push((provider_id, rx));
    }

    /// Load model lists for every provider that can answer (has a key, or is a
    /// local server) and hasn't been loaded yet. Failures are shown once and
    /// not retried until the user presses ⟳ Test.
    pub fn autofetch_models(&mut self) {
        let ids: Vec<u32> = self
            .config
            .providers
            .iter()
            .filter(|p| p.enabled && (!p.api_key.is_empty() || p.base_url.contains("localhost") || p.base_url.contains("127.0.0.1")))
            .map(|p| p.id)
            .filter(|id| !self.models.contains_key(id) && !self.fetch_tried.contains(id) && !self.is_fetching(*id))
            .collect();
        for id in ids {
            self.fetch_tried.insert(id);
            self.fetch_models(id);
        }
    }

    pub fn set_project(&mut self, dir: String) {
        if dir != self.project {
            self.project = dir;
            self.context_files.clear();
        }
    }

    /// Attach a project file (relative path) to the prompt context.
    pub fn attach(&mut self) -> Result<String, String> {
        let rel = self.attach_input.trim().trim_start_matches(['/', '\\']).to_string();
        if rel.is_empty() {
            return Err("Type a path relative to the project, e.g. routes/api.php".into());
        }
        let root = Path::new(&self.project).canonicalize().map_err(|e| format!("Project folder unavailable: {}", e))?;
        let full = root.join(&rel).canonicalize().map_err(|_| format!("Not found: {}", rel))?;
        if !full.starts_with(&root) {
            return Err("Only files inside the selected project can be attached.".into());
        }
        let meta = std::fs::metadata(&full).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!("Not a file: {}", rel));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("{} is {} KB; the limit is {} KB. Attach a smaller file.", rel, meta.len() / 1024, MAX_FILE_BYTES / 1024));
        }
        let content = String::from_utf8(std::fs::read(&full).map_err(|e| e.to_string())?).map_err(|_| format!("{} is not a UTF-8 text file.", rel))?;
        let rel = rel.replace('\\', "/");
        self.context_files.retain(|f| f.rel != rel);
        self.context_files.push(ContextFile { rel: rel.clone(), content });
        self.attach_input.clear();
        Ok(rel)
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Project choices: the www root itself plus each folder inside it.
pub fn project_dirs(www_root: &str) -> Vec<String> {
    let mut dirs = vec![www_root.to_string()];
    if let Ok(rd) = std::fs::read_dir(www_root) {
        let mut sub: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();
        sub.sort_by_key(|s| s.to_lowercase());
        dirs.extend(sub);
    }
    dirs
}

// ───────────────────────────── Key storage ─────────────────────────────

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// API keys are encrypted with Windows DPAPI (bound to the current user
/// account) before they touch disk.
#[cfg(windows)]
mod secret {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    fn run(input: &[u8], protect: bool) -> Option<Vec<u8>> {
        let blob_in = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
        let mut blob_out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: blob_in points at `input`, which outlives the call; DPAPI
        // allocates blob_out with LocalAlloc, which we copy and free below.
        let ok = unsafe {
            if protect {
                CryptProtectData(&blob_in, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut blob_out)
            } else {
                CryptUnprotectData(&blob_in, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut blob_out)
            }
        };
        if ok == 0 || blob_out.pbData.is_null() {
            return None;
        }
        let out = unsafe { std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize).to_vec() };
        unsafe { LocalFree(blob_out.pbData as _) };
        Some(out)
    }

    pub fn protect(s: &str) -> Option<Vec<u8>> {
        run(s.as_bytes(), true)
    }

    pub fn unprotect(b: &[u8]) -> Option<String> {
        run(b, false).and_then(|v| String::from_utf8(v).ok())
    }
}

#[cfg(not(windows))]
mod secret {
    pub fn protect(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }
    pub fn unprotect(b: &[u8]) -> Option<String> {
        String::from_utf8(b.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: RouteMode) -> AiConfig {
        let mut c = AiConfig::empty();
        c.mode = mode;
        let a = c.add_preset(&PRESETS[0]); // haiku (small), opus (big)
        let d = c.add_preset(&PRESETS[3]); // deepseek-chat (small), deepseek-reasoner (big)
        assert_eq!((a, d), (1, 2));
        c
    }

    #[test]
    fn classify_sizes() {
        assert_eq!(classify("what port is mysql on?", 0).0, Tier::Small);
        assert_eq!(classify("refactor the checkout controller", 0).0, Tier::Big);
        assert_eq!(classify("hi", 10_000).0, Tier::Big);
        assert_eq!(classify("look:\n```php\necho 1;\n```", 0).0, Tier::Big);
    }

    #[test]
    fn failover_keeps_list_order_and_cooling_goes_last() {
        let c = cfg(RouteMode::Failover);
        assert_eq!(c.plan(Tier::Small, 0, |_| false), vec![0, 1, 2, 3]);
        assert_eq!(c.plan(Tier::Small, 0, |r| r.model == "claude-haiku-5-5"), vec![1, 2, 3, 0]);
    }

    #[test]
    fn auto_size_prefers_tier_then_falls_back() {
        let c = cfg(RouteMode::AutoSize);
        assert_eq!(c.plan(Tier::Small, 0, |_| false), vec![0, 2, 1, 3]);
        assert_eq!(c.plan(Tier::Big, 0, |_| false), vec![1, 3, 0, 2]);
    }

    #[test]
    fn round_robin_rotates() {
        let c = cfg(RouteMode::RoundRobin);
        assert_eq!(c.plan(Tier::Small, 1, |_| false), vec![1, 2, 3, 0]);
        assert_eq!(c.plan(Tier::Small, 5, |_| false), vec![1, 2, 3, 0]);
    }

    #[test]
    fn disabled_provider_and_single_mode() {
        let mut c = cfg(RouteMode::Single);
        c.single = Some((2, "deepseek-reasoner".into()));
        assert_eq!(c.plan(Tier::Small, 0, |_| false), vec![3]);
        c.providers[1].enabled = false;
        assert!(c.plan(Tier::Small, 0, |_| false).is_empty());
        c.mode = RouteMode::Failover;
        assert_eq!(c.plan(Tier::Small, 0, |_| false), vec![0, 1]);
    }

    #[cfg(windows)]
    #[test]
    fn run_command_captures_output_and_exit_code() {
        let dir = std::env::temp_dir();
        let out = run_command("echo hello & echo oops 1>&2 & exit /b 3", &dir);
        assert!(out.contains("hello") && out.contains("oops"), "{}", out);
        assert!(out.ends_with("[exit code 3]"), "{}", out);
    }

    #[test]
    fn overview_lists_tree_and_manifests_but_never_env() {
        let dir = std::env::temp_dir().join(format!("mycraft-ai-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::write(dir.join("src/main.php"), "").unwrap();
        std::fs::write(dir.join("composer.json"), "{\"name\":\"demo\"}").unwrap();
        std::fs::write(dir.join(".env"), "DB_PASSWORD=secret").unwrap();
        let s = project_overview(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(s.contains("src/") && s.contains("  main.php"));
        assert!(s.contains("node_modules/") && !s.contains("pkg"));
        assert!(s.contains("\"name\":\"demo\""));
        assert!(!s.contains("secret"));
    }

    #[test]
    fn key_encryption_roundtrip() {
        let blob = secret::protect("sk-test-123").unwrap();
        assert_ne!(blob, b"sk-test-123");
        assert_eq!(secret::unprotect(&blob).as_deref(), Some("sk-test-123"));
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex_decode(&hex_encode(&[0, 15, 255])), Some(vec![0, 15, 255]));
        assert_eq!(hex_decode("abc"), None);
    }
}
