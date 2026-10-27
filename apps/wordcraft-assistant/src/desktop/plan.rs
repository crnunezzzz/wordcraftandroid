//! The plan path: a message goes to the user's own Codex (ChatGPT plan) or Claude Code (Claude
//! plan) command-line tool, installed on this computer and signed in through the provider's
//! own flow. The app never sees the sign-in or its tokens; it starts the unmodified tool, gives
//! it the request on stdin and reads its JSON events.
//!
//! The tool gets no tools of its own (no shell, files, web or patches) and exactly one MCP
//! server: this app (`--assistant-mcp`, see [`super::bridge`]), with the same two document
//! tools as the API path.
#![forbid(unsafe_code)]

use super::bridge;
use crate::{
    attach::{Attachment, Kind},
    protocol::{self, Provider},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

/// Model rounds per message (Claude Code's `--max-turns`).
const MAX_TURNS: u32 = 40;
/// Attachments sent through a plan tool's stdin (bytes, base64 included).
pub const MAX_PLAN_FILES: usize = 9 * 1024 * 1024;

/// Windows: start console programs without a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows: a console window of its own (sign-in and install, which the user watches).
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

fn hidden(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, CREATE_NO_WINDOW);
    command
}

/// The tool's name as the user knows it.
pub fn tool(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenAi => "Codex",
        Provider::Anthropic => "Claude Code",
    }
}

fn exe_name(provider: Provider) -> &'static str {
    match (provider, cfg!(windows)) {
        (Provider::OpenAi, true) => "codex.exe",
        (Provider::OpenAi, false) => "codex",
        (Provider::Anthropic, true) => "claude.exe",
        (Provider::Anthropic, false) => "claude",
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Where the official installers and npm put the tool, besides PATH (an app started from the
/// Start menu may have an older PATH than a new terminal).
fn known_places(provider: Provider) -> Vec<PathBuf> {
    let mut places = Vec::new();
    let home = env_dir("USERPROFILE").or_else(|| env_dir("HOME"));
    match provider {
        Provider::Anthropic => {
            if let Some(home) = &home {
                places.push(home.join(".local").join("bin").join(exe_name(provider)));
                places.push(home.join(".claude").join("local").join(exe_name(provider)));
            }
        }
        Provider::OpenAi => {
            if let Some(local) = env_dir("LOCALAPPDATA") {
                places.push(local.join("Programs").join("OpenAI").join("Codex").join("bin").join("codex.exe"));
            }
            if let Some(roaming) = env_dir("APPDATA") {
                // npm installs a codex.cmd shim; the native program sits in the platform package.
                let modules = roaming.join("npm").join("node_modules").join("@openai");
                for (package, triple) in [("codex-win32-x64", "x86_64-pc-windows-msvc"), ("codex-win32-arm64", "aarch64-pc-windows-msvc")] {
                    for root in
                        [modules.join("codex").join("node_modules").join("@openai").join(package), modules.join(package), modules.join("codex")]
                    {
                        places.push(root.join("vendor").join(triple).join("bin").join("codex.exe"));
                        places.push(root.join("vendor").join(triple).join("codex").join("codex.exe"));
                    }
                }
            }
            if let Some(home) = &home {
                places.push(home.join(".local").join("bin").join(exe_name(provider)));
            }
        }
    }
    if !cfg!(windows) {
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            places.push(Path::new(dir).join(exe_name(provider)));
        }
        if let Some(home) = &home {
            places.push(home.join(".npm-global").join("bin").join(exe_name(provider)));
        }
    }
    places
}

/// The tool's program, if it is installed.
pub fn locate(provider: Provider) -> Option<PathBuf> {
    let on_path =
        std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join(exe_name(provider)));
    on_path.chain(known_places(provider)).find(|p| p.is_file())
}

/// Run a short command (version, sign-in status) and collect its output; None if it did not
/// start or took longer than `timeout`.
fn quick(exe: &Path, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut command = Command::new(exe);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    clean_env(&mut command);
    let mut child = hidden(&mut command).spawn().ok()?;
    let mut readers = Vec::new();
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)]
        .into_iter()
        .flatten()
    {
        readers.push(std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.take(64 * 1024).read_to_string(&mut text);
            text
        }));
    }
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if waited < timeout => {
                std::thread::sleep(step);
                waited += step;
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text: Vec<String> = readers.into_iter().filter_map(|r| r.join().ok()).collect();
    Some((status.success(), text.join("\n").trim().to_owned()))
}

/// Requests through the plan: an API key in the environment would take over (Claude Code uses
/// it first in print mode), and a cloud-provider switch would send them elsewhere.
fn clean_env(command: &mut Command) {
    for name in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
    ] {
        command.env_remove(name);
    }
}

/// Is the tool installed and signed in? Blocking (seconds): call from a worker thread.
pub fn check(provider: Provider) -> crate::assistant::PlanInfo {
    use crate::assistant::PlanInfo;
    let Some(exe) = locate(provider) else {
        return PlanInfo { checked: true, installed: false, logged_in: false, detail: String::new() };
    };
    let version = quick(&exe, &["--version"], Duration::from_secs(20))
        .map(|(_, text)| text.lines().next().unwrap_or("").chars().take(80).collect::<String>())
        .unwrap_or_default();
    let status = match provider {
        Provider::Anthropic => quick(&exe, &["auth", "status"], Duration::from_secs(20)),
        Provider::OpenAi => quick(&exe, &["login", "status"], Duration::from_secs(20)),
    };
    let (logged_in, how) = match (provider, status) {
        (_, None) => (false, "No se pudo comprobar la sesión.".to_owned()),
        (Provider::Anthropic, Some((ok, text))) => {
            let method = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["authMethod"].as_str().map(str::to_owned)).unwrap_or_default();
            match method.as_str() {
                "claude.ai" | "oauth_token" => (ok, "Sesión con tu cuenta de Claude.".to_owned()),
                "" | "none" => (false, String::new()),
                other => (ok, format!("Claude Code usa otra forma de acceso ({other}); para usar tu plan, inicia sesión con tu cuenta de Claude.")),
            }
        }
        (Provider::OpenAi, Some((ok, text))) => {
            let line = text.lines().find(|l| l.to_ascii_lowercase().contains("logged in")).unwrap_or("").trim().to_owned();
            let plan = line.to_ascii_lowercase().contains("chatgpt");
            if ok && !plan && !line.is_empty() {
                (true, format!("Codex: {line}. Para usar tu plan, inicia sesión con ChatGPT."))
            } else {
                (ok, if ok { "Sesión con tu cuenta de ChatGPT.".to_owned() } else { String::new() })
            }
        }
    };
    let detail = [version, how].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    PlanInfo { checked: true, installed: true, logged_in, detail }
}

/// Open the tool's own sign-in where the user can watch it (it continues in the browser).
pub fn open_login(provider: Provider) -> Result<(), String> {
    let exe = locate(provider).ok_or_else(|| format!("{} no está instalado.", tool(provider)))?;
    let args: &[&str] = match provider {
        Provider::Anthropic => &["auth", "login"],
        Provider::OpenAi => &["login"],
    };
    visible(&exe.to_string_lossy(), args)
}

/// Open a console that runs the provider's official installer.
pub fn open_install(provider: Provider) -> Result<(), String> {
    #[cfg(windows)]
    {
        let script = match provider {
            Provider::Anthropic => "irm https://claude.ai/install.ps1 | iex",
            Provider::OpenAi => "irm https://chatgpt.com/codex/install.ps1 | iex",
        };
        let message = format!(
            "Write-Host 'Instalando {} con el instalador oficial. Al terminar, vuelve a la app y pulsa Comprobar de nuevo.'; {script}",
            tool(provider)
        );
        visible("powershell.exe", &["-NoExit", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &message])
    }
    #[cfg(not(windows))]
    {
        // npm installs into ~/.local (where `locate` looks), so it needs no administrator.
        let script = match provider {
            // Ubuntu's desktop comes with wget but not always curl.
            Provider::Anthropic => {
                "if command -v curl >/dev/null; then curl -fsSL https://claude.ai/install.sh | bash; \
                 else wget -qO- https://claude.ai/install.sh | bash; fi"
            }
            Provider::OpenAi => {
                "if command -v npm >/dev/null; then npm install -g --prefix \"$HOME/.local\" @openai/codex; \
                 else echo 'Codex se instala con npm, que no está instalado.'; \
                 echo 'En Ubuntu: sudo apt install nodejs npm  y luego pulsa Instalar otra vez.'; fi"
            }
        };
        visible("sh", &["-c", script])
    }
}

/// Start a program in a window of its own.
fn visible(program: &str, args: &[&str]) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut command = Command::new(program);
        command.args(args);
        clean_env(&mut command);
        std::os::windows::process::CommandExt::creation_flags(&mut command, CREATE_NEW_CONSOLE);
        command.spawn().map(|_| ()).map_err(|e| format!("No se pudo abrir la ventana: {e}"))
    }
    #[cfg(target_os = "macos")]
    {
        let line = std::iter::once(program).chain(args.iter().copied()).map(shell_quote).collect::<Vec<_>>().join(" ");
        let script = format!("tell application \"Terminal\" to do script \"{}\"", line.replace('\\', "\\\\").replace('"', "\\\""));
        Command::new("osascript")
            .args(["-e", &script, "-e", "tell application \"Terminal\" to activate"])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let line = std::iter::once(program).chain(args.iter().copied()).map(shell_quote).collect::<Vec<_>>().join(" ");
        // `sh` is dash on Ubuntu: plain `read`, no bash options.
        let script = format!("{line}; echo; printf 'Pulsa Intro para cerrar esta ventana. '; read _");
        // Ubuntu's terminals first (GNOME Terminal; Ptyxis from 25.10), then the usual others.
        let terminals: [(&str, &[&str]); 6] = [
            ("gnome-terminal", &["--"]),
            ("ptyxis", &["--"]),
            ("x-terminal-emulator", &["-e"]),
            ("konsole", &["-e"]),
            ("xfce4-terminal", &["-x"]),
            ("xterm", &["-e"]),
        ];
        let started = terminals.iter().any(|(terminal, flag)| {
            let mut command = Command::new(terminal);
            command.args(*flag).args(["sh", "-c", &script]);
            clean_env(&mut command);
            command.spawn().is_ok()
        });
        if started { Ok(()) } else { Err(format!("No se encontró una terminal. Abre una y ejecuta: {line}")) }
    }
}

#[cfg(not(windows))]
fn shell_quote(word: &str) -> String {
    if word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@".contains(&b)) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// One message through a plan tool.
pub struct Request<'a> {
    pub provider: Provider,
    pub exe: &'a Path,
    /// None: the tool's (plan's) default model.
    pub model: Option<&'a str>,
    pub effort: &'a str,
    pub system: &'a str,
    /// The user message with the document snapshot.
    pub text: &'a str,
    pub files: &'a [Attachment],
    /// The tool's own conversation to continue.
    pub session: Option<&'a str>,
    /// This app's executable (the MCP server) and the bridge file it reads.
    pub app: &'a Path,
    pub bridge: &'a Path,
    /// A folder of the app's own to run in: no project files or instructions are picked up.
    pub work: &'a Path,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The tool's conversation id, to continue it with the next message.
    Session(String),
    /// Text the model wrote to the user.
    Text(String),
    /// The run ended; Some: what went wrong.
    Done(Option<String>),
}

pub struct Run {
    child: Child,
    pub events: Receiver<Event>,
    /// Files written for this run (images Codex reads), removed with it.
    temp: Vec<PathBuf>,
}

impl Run {
    pub fn cancel(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.cancel();
        }
        for file in &self.temp {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// A TOML basic string (JSON's escaping is valid TOML).
fn toml(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// The command line and the stdin text for a request (separate so it can be tested).
pub fn command(request: &Request, temp: &mut Vec<PathBuf>) -> Result<(Vec<OsString>, String), String> {
    let mut args: Vec<OsString> = Vec::new();
    let mut arg = |a: &str| args.push(a.into());
    let weight: usize = request.files.iter().map(Attachment::weight).sum();
    if weight > MAX_PLAN_FILES {
        return Err(format!(
            "Con el plan, los adjuntos de un mensaje pueden ocupar hasta {}; estos ocupan {}. Quita algún archivo.",
            crate::attach::size(MAX_PLAN_FILES),
            crate::attach::size(weight)
        ));
    }
    match request.provider {
        Provider::Anthropic => {
            let config = json!({"mcpServers":{bridge::SERVER:{
                "type":"stdio",
                "command": request.app.to_string_lossy(),
                "args":[bridge::MCP_FLAG],
                "env":{bridge::BRIDGE_ENV: request.bridge.to_string_lossy()}
            }}});
            let config_file = request.work.join(format!("mcp-{}.json", std::process::id()));
            std::fs::write(&config_file, config.to_string()).map_err(|e| e.to_string())?;
            temp.push(config_file.clone());
            for a in ["-p", "--output-format", "stream-json", "--verbose", "--input-format", "stream-json"] {
                arg(a);
            }
            arg("--system-prompt");
            arg(request.system);
            // No built-in tools (shell, files, web): only the document's.
            arg("--tools");
            arg("");
            arg("--mcp-config");
            arg(&config_file.to_string_lossy());
            arg("--strict-mcp-config");
            arg("--allowedTools");
            arg(&format!("mcp__{0}__list_commands,mcp__{0}__execute_command", bridge::SERVER));
            arg("--permission-mode");
            arg("dontAsk");
            arg("--max-turns");
            arg(&MAX_TURNS.to_string());
            if let Some(model) = request.model {
                arg("--model");
                arg(model);
            }
            if let Some(session) = request.session {
                arg("--resume");
                arg(session);
            }
            let content = protocol::user_content(Provider::Anthropic, request.text, request.files);
            let line = json!({"type":"user","message":{"role":"user","content":content}});
            Ok((args, format!("{line}\n")))
        }
        Provider::OpenAi => {
            arg("exec");
            for a in ["--json", "--skip-git-repo-check", "--ignore-user-config", "--sandbox", "read-only", "--color", "never", "-C"] {
                arg(a);
            }
            arg(&request.work.to_string_lossy());
            if let Some(model) = request.model {
                arg("--model");
                arg(model);
            }
            let effort = if ["low", "medium", "high"].contains(&request.effort) { request.effort } else { "medium" };
            let mut config = vec![
                format!("developer_instructions={}", toml(request.system)),
                format!("model_reasoning_effort={}", toml(effort)),
                "approval_policy=\"never\"".to_owned(),
                "web_search=\"disabled\"".to_owned(),
                format!("mcp_servers.{}.command={}", bridge::SERVER, toml(&request.app.to_string_lossy())),
                format!("mcp_servers.{}.args=[{}]", bridge::SERVER, toml(bridge::MCP_FLAG)),
                format!("mcp_servers.{}.env={{{}={}}}", bridge::SERVER, bridge::BRIDGE_ENV, toml(&request.bridge.to_string_lossy())),
                format!("mcp_servers.{}.default_tools_approval_mode=\"approve\"", bridge::SERVER),
                format!("mcp_servers.{}.tool_timeout_sec=150", bridge::SERVER),
            ];
            // No tools of Codex's own: no shell, patches, images, browser or plugins.
            for feature in [
                "shell_tool",
                "unified_exec",
                "view_image",
                "js_repl",
                "apps",
                "plugins",
                "browser_use",
                "computer_use",
                "image_generation",
                "memory_tool",
            ] {
                config.push(format!("features.{feature}=false"));
            }
            for c in &config {
                arg("-c");
                arg(c);
            }
            // Images go as files; PDFs and documents go as their text (Codex reads images only).
            let mut images = Vec::new();
            let mut text = String::new();
            for (i, file) in request.files.iter().enumerate() {
                match file.kind {
                    Kind::Image => {
                        let extension = file.media_type.rsplit('/').next().unwrap_or("png");
                        let path = request.work.join(format!("adjunto-{}-{i}.{extension}", std::process::id()));
                        std::fs::write(&path, &file.data).map_err(|e| e.to_string())?;
                        temp.push(path.clone());
                        images.push(path);
                    }
                    Kind::Text => {
                        text.push_str(&format!("<attached_file name=\"{}\">\n{}\n</attached_file>\n\n", file.name.replace('"', "'"), file.text))
                    }
                    Kind::Pdf => {
                        return Err(format!(
                            "Codex no lee archivos PDF. Con el plan de ChatGPT, copia el texto de «{}» o adjunta capturas de sus páginas; con una clave de API de OpenAI el PDF se envía entero.",
                            file.name
                        ));
                    }
                }
            }
            if let Some(session) = request.session {
                arg("resume");
                arg(session);
            }
            for image in &images {
                arg("--image");
                arg(&image.to_string_lossy());
            }
            arg("-");
            text.push_str(request.text);
            Ok((args, text))
        }
    }
}

/// Start a request; events arrive on [`Run::events`].
pub fn start(request: &Request) -> Result<Run, String> {
    std::fs::create_dir_all(request.work).map_err(|e| e.to_string())?;
    let mut temp = Vec::new();
    let (args, input) = command(request, &mut temp)?;
    let mut command = Command::new(request.exe);
    command.args(&args).current_dir(request.work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    clean_env(&mut command);
    let mut child = hidden(&mut command).spawn().map_err(|e| format!("No se pudo iniciar {}: {e}", tool(request.provider)))?;
    let (tx, events) = mpsc::channel();
    let stderr = Arc::new(Mutex::new(String::new()));
    if let Some(pipe) = child.stderr.take() {
        let tail = stderr.clone();
        let _ = std::thread::Builder::new().name("craft-plan-stderr".into()).spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if let Ok(mut tail) = tail.lock() {
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 8000 {
                        let mut cut = tail.len() - 6000;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail.replace_range(..cut, "");
                    }
                }
            }
        });
    }
    if let Some(mut pipe) = child.stdin.take() {
        // Written from a thread: a large request must not block the UI while the tool starts.
        let _ = std::thread::Builder::new().name("craft-plan-stdin".into()).spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
    }
    let provider = request.provider;
    let Some(stdout) = child.stdout.take() else { return Err("Sin salida del proceso".into()) };
    std::thread::Builder::new()
        .name("craft-plan-events".into())
        .spawn(move || read_events(provider, stdout, &tx, &stderr))
        .map_err(|e| e.to_string())?;
    Ok(Run { child, events, temp })
}

fn read_events(provider: Provider, stdout: impl Read, tx: &Sender<Event>, stderr: &Arc<Mutex<String>>) {
    let mut done = false;
    let mut last_error = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        for event in parse(provider, &event, &mut last_error) {
            done |= matches!(event, Event::Done(_));
            if tx.send(event).is_err() {
                return;
            }
        }
        if done {
            return;
        }
    }
    let tail = stderr.lock().map(|s| s.clone()).unwrap_or_default();
    let _ = tx.send(Event::Done(Some(last_error.unwrap_or_else(|| failure(provider, &tail)))));
}

/// Events from one line of the tool's JSON output.
pub fn parse(provider: Provider, event: &Value, last_error: &mut Option<String>) -> Vec<Event> {
    let mut out = Vec::new();
    let kind = event["type"].as_str().unwrap_or("");
    match provider {
        Provider::Anthropic => match kind {
            "system" if event["subtype"] == "init" => {
                if let Some(id) = event["session_id"].as_str() {
                    out.push(Event::Session(id.into()));
                }
            }
            "assistant" => {
                let text: String = event["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                if !text.trim().is_empty() {
                    out.push(Event::Text(text));
                }
            }
            "result" => {
                if let Some(id) = event["session_id"].as_str() {
                    out.push(Event::Session(id.into()));
                }
                let error = event["is_error"] == true || event["subtype"].as_str().is_some_and(|s| s.starts_with("error"));
                out.push(Event::Done(error.then(|| {
                    let said = event["result"].as_str().unwrap_or("").trim().to_owned();
                    match event["subtype"].as_str().unwrap_or("") {
                        "error_max_turns" => "Se alcanzó el límite de pasos de esta petición. Los cambios aplicados se pueden deshacer.".to_owned(),
                        _ if !said.is_empty() => describe(provider, &said),
                        other => format!("Claude Code terminó con un error ({other})."),
                    }
                })));
            }
            _ => {}
        },
        Provider::OpenAi => match kind {
            "thread.started" => {
                if let Some(id) = event["thread_id"].as_str() {
                    out.push(Event::Session(id.into()));
                }
            }
            "item.completed" if event["item"]["type"] == "agent_message" => {
                if let Some(text) = event["item"]["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    out.push(Event::Text(text.into()));
                }
            }
            "item.completed" if event["item"]["type"] == "error" => {
                *last_error = event["item"]["message"].as_str().map(str::to_owned);
            }
            "error" => *last_error = event["message"].as_str().map(str::to_owned),
            "turn.completed" => out.push(Event::Done(None)),
            "turn.failed" => {
                let message = event["error"]["message"].as_str().map(str::to_owned).or_else(|| last_error.clone()).unwrap_or_default();
                out.push(Event::Done(Some(describe(provider, &message))));
            }
            _ => {}
        },
    }
    out
}

/// A tool's error in the user's language, with its own words after.
fn describe(provider: Provider, message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let name = tool(provider);
    let lead = if lower.contains("not logged in")
        || lower.contains("login")
        || lower.contains("log in")
        || lower.contains("authenticat")
        || lower.contains("401")
    {
        format!("{name} no tiene una sesión válida. Inicia sesión en Ajustes del asistente.")
    } else if lower.contains("usage limit") || lower.contains("rate limit") || lower.contains("429") || lower.contains("limit reached") {
        "Has alcanzado el límite de uso de tu plan por ahora. Espera a que se renueve o usa una clave de API.".to_owned()
    } else if lower.contains("model")
        && (lower.contains("not supported") || lower.contains("not found") || lower.contains("does not exist") || lower.contains("not available"))
    {
        format!("Tu plan no permite este modelo con {name}. Elige otro en el menú de modelos.")
    } else {
        format!("{name} no pudo completar la petición.")
    };
    let mut detail: String = message.trim().chars().take(400).collect();
    if detail.len() < message.trim().len() {
        detail.push('…');
    }
    if detail.is_empty() { lead } else { format!("{lead}\n{detail}") }
}

/// The run ended without a result: say why from what the tool printed.
fn failure(provider: Provider, stderr: &str) -> String {
    let last: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = last.iter().rev().take(3).rev().copied().collect::<Vec<_>>().join("\n");
    if tail.is_empty() { format!("{} se cerró sin responder.", tool(provider)) } else { describe(provider, &tail) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(provider: Provider, files: &'a [Attachment], session: Option<&'a str>, work: &'a Path) -> Request<'a> {
        Request {
            provider,
            exe: Path::new("tool"),
            model: Some("m-1"),
            effort: "high",
            system: "Sistema \"con\" comillas\ny líneas",
            text: "Hola\n<document_snapshot>{}</document_snapshot>",
            files,
            session,
            app: Path::new("C:\\Program Files\\WordCraft IA\\wordcraft.exe"),
            bridge: Path::new("C:\\Users\\Ana\\AppData\\bridge.json"),
            work,
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn claude_code_gets_only_the_document_tools_and_reads_stdin() {
        let work = std::env::temp_dir().join(format!("craft-plan-test-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let mut temp = Vec::new();
        let (args, input) = command(&request(Provider::Anthropic, &[], Some("ses-1"), &work), &mut temp).unwrap();
        let args = strings(&args);
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone()).unwrap();
        assert_eq!(at("--tools"), "");
        assert_eq!(at("--allowedTools"), "mcp__craft__list_commands,mcp__craft__execute_command");
        assert_eq!(at("--resume"), "ses-1");
        assert_eq!(at("--model"), "m-1");
        assert!(args.contains(&"--strict-mcp-config".to_owned()));
        let config: Value = serde_json::from_slice(&std::fs::read(at("--mcp-config")).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["craft"]["args"][0], "--assistant-mcp");
        assert_eq!(config["mcpServers"]["craft"]["env"]["CRAFT_ASSISTANT_BRIDGE"], "C:\\Users\\Ana\\AppData\\bridge.json");
        let line: Value = serde_json::from_str(input.trim()).unwrap();
        assert_eq!(line["type"], "user");
        assert_eq!(line["message"]["content"][0]["text"], "Hola\n<document_snapshot>{}</document_snapshot>");
        assert!(!args.iter().any(|a| a.contains("snapshot")), "the document goes on stdin, not the command line");
        for f in temp {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn codex_runs_read_only_without_its_own_tools() {
        let work = std::env::temp_dir();
        let png = {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::new(2, 2))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        let files = [crate::attach::prepare("foto.png", &png).unwrap(), crate::attach::prepare("notas.txt", b"datos").unwrap()];
        let mut temp = Vec::new();
        let (args, input) = command(&request(Provider::OpenAi, &files, Some("th-9"), &work), &mut temp).unwrap();
        let args = strings(&args);
        assert_eq!(args[0], "exec");
        assert!(args.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(args.contains(&"features.shell_tool=false".to_owned()));
        assert!(args.contains(&"approval_policy=\"never\"".to_owned()));
        assert!(args.iter().any(|a| a == "developer_instructions=\"Sistema \\\"con\\\" comillas\\ny líneas\""), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("mcp_servers.craft.env={CRAFT_ASSISTANT_BRIDGE=\"C:\\\\Users")), "{args:?}");
        let resume = args.iter().position(|a| a == "resume").unwrap();
        assert_eq!(args[resume + 1], "th-9");
        assert_eq!(args[resume + 2], "--image");
        assert_eq!(args.last().unwrap(), "-");
        assert!(input.starts_with("<attached_file name=\"notas.txt\">\ndatos") && input.ends_with("</document_snapshot>"), "{input}");
        assert_eq!(temp.len(), 1);
        assert_eq!(std::fs::read(&temp[0]).unwrap(), png);
        for f in temp {
            let _ = std::fs::remove_file(f);
        }
        let pdf = [crate::attach::prepare("tema.pdf", b"%PDF-1.4 x").unwrap()];
        assert!(command(&request(Provider::OpenAi, &pdf, None, &work), &mut Vec::new()).unwrap_err().contains("PDF"));
    }

    #[test]
    fn events_become_text_sessions_and_failures() {
        let mut last = None;
        let claude = |e: Value, last: &mut Option<String>| parse(Provider::Anthropic, &e, last);
        assert_eq!(claude(json!({"type":"system","subtype":"init","session_id":"s1"}), &mut last), [Event::Session("s1".into())]);
        assert_eq!(
            claude(json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hecho"},{"type":"tool_use","name":"x"}]}}), &mut last),
            [Event::Text("Hecho".into())]
        );
        let ok = claude(json!({"type":"result","subtype":"success","is_error":false,"result":"Hecho","session_id":"s1"}), &mut last);
        assert_eq!(ok.last(), Some(&Event::Done(None)));
        let bad = claude(json!({"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}), &mut last);
        assert!(matches!(bad.last(), Some(Event::Done(Some(e))) if e.contains("sesión")));

        let codex = |e: Value, last: &mut Option<String>| parse(Provider::OpenAi, &e, last);
        assert_eq!(codex(json!({"type":"thread.started","thread_id":"t1"}), &mut last), [Event::Session("t1".into())]);
        assert_eq!(
            codex(json!({"type":"item.completed","item":{"id":"i","type":"agent_message","text":"Listo"}}), &mut last),
            [Event::Text("Listo".into())]
        );
        assert!(codex(json!({"type":"item.completed","item":{"type":"reasoning","text":"x"}}), &mut last).is_empty());
        assert!(codex(json!({"type":"error","message":"You've hit your usage limit"}), &mut last).is_empty());
        let failed = codex(json!({"type":"turn.failed","error":{"message":"You've hit your usage limit"}}), &mut last);
        assert!(matches!(failed.last(), Some(Event::Done(Some(e))) if e.contains("límite de uso")));
        assert_eq!(codex(json!({"type":"turn.completed","usage":{}}), &mut last), [Event::Done(None)]);
    }
}
