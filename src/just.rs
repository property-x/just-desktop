use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct JustDump {
    #[serde(default)]
    pub first: Option<String>,
    #[serde(default)]
    pub recipes: BTreeMap<String, JustRecipe>,
    #[serde(default)]
    pub settings: JustSettings,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub warnings: Vec<Value>,
}

impl JustDump {
    pub fn default_recipe_name(&self) -> String {
        self.first
            .clone()
            .or_else(|| self.recipes.keys().next().cloned())
            .unwrap_or_default()
    }

    pub fn first_recipe_display(&self) -> String {
        self.first.clone().unwrap_or_else(|| "-".to_string())
    }

    pub fn display_title(&self) -> String {
        let path = Path::new(&self.source);
        if let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim_start_matches('.'))
            .filter(|s| !s.is_empty())
        {
            stem.to_string()
        } else if let Some(name) = path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
        {
            name.to_string()
        } else {
            "justfile".to_string()
        }
    }

    pub fn filter_recipes(&self, query: &str) -> Vec<JustRecipe> {
        let filter_query = query.trim().to_lowercase();
        self.recipes
            .values()
            .filter(|r| {
                if filter_query.is_empty() {
                    return true;
                }
                r.name.to_lowercase().contains(&filter_query)
                    || r.doc
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&filter_query)
                    || r.formatted_body_lines()
                        .iter()
                        .any(|line| line.to_lowercase().contains(&filter_query))
            })
            .cloned()
            .collect()
    }

    pub fn resolve_recipe(&self, selected: &str) -> Option<JustRecipe> {
        self.recipes
            .get(selected)
            .cloned()
            .or_else(|| self.recipes.values().next().cloned())
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct JustParameter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub export: bool,
}

impl JustParameter {
    pub fn default_string(&self) -> String {
        match &self.default {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct JustRecipe {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub namepath: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub body: Vec<Vec<Value>>,
    #[serde(default)]
    pub dependencies: Vec<Value>,
    #[serde(default)]
    pub parameters: Vec<JustParameter>,
    #[serde(default)]
    pub priors: usize,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub quiet: bool,
    #[serde(default)]
    pub shebang: bool,
}

impl JustRecipe {
    pub fn formatted_body_lines(&self) -> Vec<String> {
        self.body
            .iter()
            .map(|line| {
                line.iter()
                    .map(|fragment| match fragment {
                        Value::String(s) => s.clone(),
                        Value::Array(arr) => {
                            if let Some(Value::Array(inner)) = arr.first()
                                && inner.first().and_then(Value::as_str) == Some("variable")
                                && let Some(var_name) = inner.get(1).and_then(Value::as_str)
                            {
                                return format!("{{{{{var_name}}}}}");
                            }
                            fragment.to_string()
                        }
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    pub fn resolve_args(
        &self,
        justfile_source: &str,
        overrides: &BTreeMap<(String, String, String), String>,
    ) -> Vec<String> {
        let mut args = Vec::new();
        for param in &self.parameters {
            let key = (
                justfile_source.to_string(),
                self.name.clone(),
                param.name.clone(),
            );
            // An emptied field falls back to the default; skipping it would shift later positional args.
            let raw = overrides
                .get(&key)
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| param.default_string());
            if param.kind == "star" || param.kind == "plus" {
                for part in raw.split_whitespace() {
                    args.push(part.to_string());
                }
            } else if !raw.is_empty() {
                args.push(raw);
            }
        }
        args
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct JustSettings {
    #[serde(default)]
    pub dotenv_load: bool,
    #[serde(default)]
    pub export: bool,
    #[serde(default)]
    pub positional_arguments: bool,
    #[serde(default)]
    pub quiet: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JustfileProfile {
    pub label: String,
    pub path: String,
    pub dump: JustDump,
}

fn just_command() -> Command {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".cargo/bin/just"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/just"));
    candidates.push(PathBuf::from("/usr/bin/just"));

    let program = candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("just"));

    let mut cmd = Command::new(program);
    if let Some(home) = std::env::var_os("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin");
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::from(&cargo_bin);
        new_path.push(":");
        new_path.push(current_path);
        cmd.env("PATH", new_path);
    }
    cmd
}

pub fn load_just_dump_for_path(justfile_path: &str) -> (JustDump, String) {
    let mut cmd = just_command();
    cmd.args(["--justfile", justfile_path]);
    if let Some(parent) = Path::new(justfile_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        cmd.arg("--working-directory").arg(parent);
    }
    cmd.args(["--dump", "--dump-format", "json"]);

    let error = match cmd.output() {
        Ok(output) if output.status.success() => {
            match serde_json::from_slice::<JustDump>(&output.stdout) {
                Ok(mut parsed) => {
                    if parsed.source.is_empty() {
                        parsed.source = justfile_path.to_string();
                    }
                    let src = parsed.source.clone();
                    return (parsed, format!("[cmd] Loaded live dump from `{src}`"));
                }
                Err(err) => err.to_string(),
            }
        }
        Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_string(),
        Err(err) => err.to_string(),
    };

    // Empty dump keeps the path so the entry can be reloaded once the justfile is fixed.
    let dump = JustDump {
        source: justfile_path.to_string(),
        ..Default::default()
    };
    (
        dump,
        format!("[error] Could not load `{justfile_path}`: {error}"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeRunStatus {
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct RecipeAbortHandle {
    inner: Arc<Mutex<(bool, Option<u32>)>>,
}

impl PartialEq for RecipeAbortHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for RecipeAbortHandle {}

impl RecipeAbortHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_aborted(&self) -> bool {
        self.inner.lock().map(|g| g.0).unwrap_or(false)
    }

    pub fn abort(&self) {
        let pid = {
            let Ok(mut guard) = self.inner.lock() else {
                return;
            };
            guard.0 = true;
            guard.1
        };

        if let Some(pid) = pid {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}"), &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }

    fn register_pid(&self, pid: u32) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        if guard.0 {
            return false;
        }
        guard.1 = Some(pid);
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeExecutionState {
    pub status: RecipeRunStatus,
    pub output: String,
    pub log_expanded: bool,
    pub abort_handle: Option<RecipeAbortHandle>,
}

pub fn run_just_recipe(
    justfile_path: &str,
    recipe_name: &str,
    args: &[String],
    abort_handle: &RecipeAbortHandle,
) -> (String, String, RecipeRunStatus) {
    let working_dir = Path::new(justfile_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir_display = working_dir.display();
    let args_display = if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args.join(" "))
    };

    let mut cmd = just_command();
    if Path::new(justfile_path).is_file() {
        cmd.args(["--justfile", justfile_path]);
        cmd.arg("--working-directory").arg(working_dir);
    }
    cmd.arg(recipe_name)
        .args(args)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = match cmd.spawn() {
        Ok(mut child) => {
            if !abort_handle.register_pid(child.id()) {
                let _ = child.kill();
            }
            child
        }
        Err(err) => {
            let msg = format!("[exec] `{dir_display}$ just {recipe_name}{args_display}` ({err})");
            let full_log = format!("{dir_display}$ just {recipe_name}{args_display}\nError: {err}");
            return (msg, full_log, RecipeRunStatus::Failed);
        }
    };

    match child.wait_with_output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if abort_handle.is_aborted() {
                let summary =
                    format!("[abort] Aborted `{dir_display}$ just {recipe_name}{args_display}`");
                let mut full_log = format!(
                    "{dir_display}$ just {recipe_name}{args_display}\nStatus: Aborted by user"
                );
                if !stdout.is_empty() {
                    full_log.push_str(&format!("\n\n--- stdout ---\n{stdout}"));
                }
                if !stderr.is_empty() {
                    full_log.push_str(&format!("\n\n--- stderr ---\n{stderr}"));
                }
                return (summary, full_log, RecipeRunStatus::Idle);
            }

            let status_code = output.status.code().unwrap_or(-1);
            let detail = if !stdout.is_empty() {
                format!(" — {stdout}")
            } else if !stderr.is_empty() {
                format!(" — {stderr}")
            } else {
                String::new()
            };
            let summary = format!(
                "[exec] `{dir_display}$ just {recipe_name}{args_display}` exited with {status_code}{detail}"
            );

            let mut full_log = format!(
                "{dir_display}$ just {recipe_name}{args_display}\nExit code: {status_code}"
            );
            if !stdout.is_empty() {
                full_log.push_str(&format!("\n\n--- stdout ---\n{stdout}"));
            }
            if !stderr.is_empty() {
                full_log.push_str(&format!("\n\n--- stderr ---\n{stderr}"));
            }
            if stdout.is_empty() && stderr.is_empty() {
                full_log.push_str("\n(no output)");
            }

            let run_status = if output.status.success() {
                RecipeRunStatus::Done
            } else {
                RecipeRunStatus::Failed
            };

            (summary, full_log, run_status)
        }
        Err(err) => {
            let msg = format!("[exec] `{dir_display}$ just {recipe_name}{args_display}` ({err})");
            let full_log = format!("{dir_display}$ just {recipe_name}{args_display}\nError: {err}");
            (msg, full_log, RecipeRunStatus::Failed)
        }
    }
}

pub async fn run_just_recipe_async(
    justfile_path: String,
    recipe_name: String,
    args: Vec<String>,
    abort_handle: RecipeAbortHandle,
) -> (String, String, RecipeRunStatus) {
    struct BlockingTask<T> {
        state: Arc<Mutex<(Option<T>, Option<Waker>)>>,
    }

    impl<T> Future for BlockingTask<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut guard = self.state.lock().unwrap();
            if let Some(res) = guard.0.take() {
                Poll::Ready(res)
            } else {
                guard.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    let state = Arc::new(Mutex::new((None, None::<Waker>)));
    let thread_state = Arc::clone(&state);
    thread::spawn(move || {
        let out = run_just_recipe(&justfile_path, &recipe_name, &args, &abort_handle);
        let mut guard = thread_state.lock().unwrap();
        guard.0 = Some(out);
        if let Some(waker) = guard.1.take() {
            waker.wake();
        }
    });

    BlockingTask { state }.await
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex_str, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn resolve_justfile_paths(raw_args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut paths = Vec::new();
    for raw_arg in raw_args {
        let trimmed = raw_arg.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }

        let cleaned = if let Some(rest) = trimmed.strip_prefix("file://localhost") {
            percent_decode(rest)
        } else if let Some(rest) = trimmed.strip_prefix("file://") {
            percent_decode(rest)
        } else {
            trimmed.to_string()
        };

        let mut candidate = PathBuf::from(&cleaned);
        if candidate.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            candidate = cwd.join(&candidate);
        }
        if candidate.is_dir() {
            for name in ["justfile", "Justfile", ".justfile"] {
                let sub = candidate.join(name);
                if sub.is_file() {
                    candidate = sub;
                    break;
                }
            }
        }
        if let Ok(canon) = std::fs::canonicalize(&candidate) {
            candidate = canon;
        }
        if candidate.is_file() {
            let s = candidate.to_string_lossy().to_string();
            if !paths.contains(&s) {
                paths.push(s);
            }
        }
    }
    paths
}

pub fn load_initial_justfiles() -> (Vec<JustfileProfile>, String) {
    let paths = resolve_justfile_paths(std::env::args().skip(1));
    if paths.is_empty() {
        return (
            Vec::new(),
            "[ready] No justfiles loaded. Add a justfile to get started.".to_string(),
        );
    }

    let mut profiles = Vec::new();
    let mut first_msg = String::new();
    for (idx, path) in paths.iter().enumerate() {
        let (profile, msg) = load_custom_justfile(path);
        if idx == 0 {
            first_msg = msg;
        }
        profiles.push(profile);
    }
    (profiles, first_msg)
}

pub fn load_custom_justfile(justfile_path: &str) -> (JustfileProfile, String) {
    let trimmed = justfile_path.trim();
    let path_obj = Path::new(trimmed);
    let file_name = path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("justfile");
    let parent_name = path_obj
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let label = format!("{parent_name} / {file_name}");

    let (dump, msg) = load_just_dump_for_path(trimmed);

    (
        JustfileProfile {
            label,
            path: trimmed.to_string(),
            dump,
        },
        msg,
    )
}

pub fn pick_and_load_justfile() -> Option<(JustfileProfile, String)> {
    let selected = rfd::FileDialog::new()
        .set_title("Open Justfile")
        .pick_file()?;
    let path_str = selected.to_string_lossy().to_string();
    Some(load_custom_justfile(&path_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_args_keeps_positions_and_splits_variadic() {
        let param = |name: &str, default: &str, kind: &str| JustParameter {
            name: name.to_string(),
            default: Value::String(default.to_string()),
            kind: kind.to_string(),
            export: false,
        };
        let recipe = JustRecipe {
            name: "deploy".to_string(),
            namepath: "deploy".to_string(),
            doc: None,
            body: Vec::new(),
            dependencies: Vec::new(),
            parameters: vec![
                param("env", "dev", "singular"),
                param("region", "eu", "singular"),
                param("flags", "", "star"),
            ],
            priors: 0,
            private: false,
            quiet: false,
            shebang: false,
        };
        let key = |p: &str| ("jf".to_string(), "deploy".to_string(), p.to_string());
        let overrides = BTreeMap::from([
            (key("env"), String::new()),
            (key("flags"), "-a  -b".to_string()),
        ]);
        assert_eq!(
            recipe.resolve_args("jf", &overrides),
            ["dev", "eu", "-a", "-b"]
        );
    }
}
