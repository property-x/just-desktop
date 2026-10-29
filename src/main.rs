mod just;

use dioxus::prelude::*;
use just::{
    JustRecipe, RecipeAbortHandle, RecipeExecutionState, RecipeRunStatus, load_custom_justfile,
    load_initial_justfiles, pick_and_load_justfile, run_just_recipe_async,
};
use std::collections::BTreeMap;

const MODERN_STYLES: &str = include_str!("styles.css");
const APP_ICON_BYTES: &[u8] = include_bytes!("../assets/blueprint-j.png");

fn main() {
    if std::path::Path::new("/.flatpak-info").exists()
        && let Ok(exe) = std::env::current_exe()
    {
        let mut cmd = std::process::Command::new("flatpak-spawn");
        cmd.args(["--host", "--watch-bus"]);
        for (k, v) in std::env::vars() {
            if k.starts_with("DIOXUS_")
                || k.starts_with("RUST_")
                || matches!(
                    k.as_str(),
                    "IP" | "PORT" | "DISPLAY" | "WAYLAND_DISPLAY" | "XDG_RUNTIME_DIR"
                )
            {
                cmd.arg(format!("--env={k}={v}"));
            }
        }
        cmd.arg(exe);
        cmd.args(std::env::args_os().skip(1));
        if let Ok(status) = cmd.status() {
            std::process::exit(status.code().unwrap_or(0));
        }
    }

    let mut config = dioxus::desktop::Config::new()
        .with_window(dioxus::desktop::WindowBuilder::new().with_title("Just Desktop"))
        .with_menu(None);
    if let Ok(icon) = dioxus::desktop::icon_from_memory(APP_ICON_BYTES) {
        config = config.with_icon(icon);
    }

    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .launch(App);
}

fn app_icon_data_uri() -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    format!("data:image/png;base64,{}", STANDARD.encode(APP_ICON_BYTES))
}

fn schedule_recipe_run(
    justfile_src: String,
    run_name: String,
    run_args: Vec<String>,
    mut run_states: Signal<BTreeMap<(String, String), RecipeExecutionState>>,
    mut logs: Signal<Vec<String>>,
) {
    let key = (justfile_src.clone(), run_name.clone());
    let args_suffix = if run_args.is_empty() {
        String::new()
    } else {
        format!(" {}", run_args.join(" "))
    };

    if let Some(existing) = run_states().get(&key).cloned()
        && existing.status == RecipeRunStatus::Running
    {
        if let Some(handle) = &existing.abort_handle {
            handle.abort();
        }
        run_states.write().insert(
            key,
            RecipeExecutionState {
                status: RecipeRunStatus::Idle,
                output: format!("Aborted `just {run_name}{args_suffix}`."),
                log_expanded: existing.log_expanded,
                abort_handle: existing.abort_handle,
            },
        );
        return;
    }

    let abort_handle = RecipeAbortHandle::new();
    {
        let mut states = run_states.write();
        let prev_expanded = states.get(&key).map(|s| s.log_expanded).unwrap_or(false);
        states.insert(
            key.clone(),
            RecipeExecutionState {
                status: RecipeRunStatus::Running,
                output: format!("Running `just {run_name}{args_suffix}`..."),
                log_expanded: prev_expanded,
                abort_handle: Some(abort_handle.clone()),
            },
        );
    }
    logs.write().push(format!(
        "[start] Scheduled `just {}{}` in background...",
        run_name, args_suffix
    ));
    spawn(async move {
        let (summary_line, full_log, final_status) =
            run_just_recipe_async(justfile_src, run_name, run_args, abort_handle.clone()).await;
        {
            let mut states = run_states.write();
            let is_same_run = states
                .get(&key)
                .and_then(|s| s.abort_handle.as_ref())
                .is_some_and(|h| h == &abort_handle);
            if is_same_run {
                let prev_expanded = states.get(&key).map(|s| s.log_expanded).unwrap_or(false);
                states.insert(
                    key,
                    RecipeExecutionState {
                        status: final_status,
                        output: full_log,
                        log_expanded: prev_expanded,
                        abort_handle: None,
                    },
                );
            }
        }
        logs.write().push(summary_line);
    });
}

#[component]
fn Collapsible(title: String, children: Element) -> Element {
    let mut expanded = use_signal(|| false);
    rsx! {
        div {
            class: "collapsible-section",
            button {
                class: "collapsible-header",
                onclick: move |_| expanded.set(!expanded()),
                span { if expanded() { "▾ {title}" } else { "▸ {title}" } }
                span {
                    class: "cmd-tag",
                    if expanded() { "Hide" } else { "Show" }
                }
            }
            if expanded() {
                {children}
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResizeSide {
    Left,
    Right,
}

#[component]
fn App() -> Element {
    let initial: (Vec<just::JustfileProfile>, String) = use_hook(load_initial_justfiles);
    let initial_first = initial
        .0
        .first()
        .map(|p| p.dump.default_recipe_name())
        .unwrap_or_default();

    let mut justfiles = use_signal(move || initial.0.clone());
    let mut active_justfile_idx = use_signal(|| 0usize);
    let mut selected_recipe = use_signal(move || initial_first);
    let mut left_width = use_signal(|| 280.0_f64);
    let mut right_width = use_signal(|| 330.0_f64);
    let mut drag_state = use_signal(|| None::<(ResizeSide, f64, f64)>);
    let mut filter_input = use_signal(String::new);
    let mut param_values = use_signal(BTreeMap::<(String, String, String), String>::new);
    let mut run_states = use_signal(BTreeMap::<(String, String), RecipeExecutionState>::new);
    let mut logs = use_signal(move || {
        vec![
            "[ready] Cuvk Justfile Inspector initialized".to_string(),
            initial.1.clone(),
        ]
    });

    let profiles = justfiles();
    let current_idx = active_justfile_idx().min(profiles.len().saturating_sub(1));
    let dump = profiles
        .get(current_idx)
        .map(|p| p.dump.clone())
        .unwrap_or_default();

    let filtered_recipes: Vec<JustRecipe> = dump.filter_recipes(&filter_input());
    let active_recipe = dump.resolve_recipe(&selected_recipe());
    let first_recipe_name = dump.first_recipe_display();
    let justfile_title = if profiles.is_empty() {
        "No Justfile Loaded".to_string()
    } else {
        dump.display_title()
    };
    let brand_icon_uri = use_hook(app_icon_data_uri);
    let active_drag_side = drag_state().map(|(side, _, _)| side);

    rsx! {
        style { "{MODERN_STYLES}" }

        div {
            class: if active_drag_side.is_some() {
                "app-shell antialiased is-resizing"
            } else {
                "app-shell antialiased"
            },
            style: "grid-template-columns: {left_width()}px 6px minmax(260px, 1fr) 6px {right_width()}px;",
            onmousemove: move |evt| {
                if let Some((side, start_x, start_w)) = drag_state() {
                    let dx = evt.client_coordinates().x - start_x;
                    match side {
                        ResizeSide::Left => {
                            left_width.set((start_w + dx).clamp(180.0, 600.0));
                        }
                        ResizeSide::Right => {
                            right_width.set((start_w - dx).clamp(220.0, 650.0));
                        }
                    }
                }
            },
            onmouseup: move |_| {
                if drag_state().is_some() {
                    drag_state.set(None);
                }
            },
            onmouseleave: move |_| {
                if drag_state().is_some() {
                    drag_state.set(None);
                }
            },

            // 1. Left Sidebar (List of Justfiles to switch between)
            aside {
                class: "sidebar-left",
                div {
                    class: "brand-header",
                    div {
                        class: "brand-badge",
                        img {
                            class: "brand-badge-icon",
                            src: "{brand_icon_uri}",
                            alt: "Just Desktop icon",
                        }
                    }
                    div {
                        h1 { class: "brand-title", "Just Desktop" }
                        p { class: "brand-subtitle", "Select and run justfile recipes" }
                    }
                }

                div {
                    div {
                        class: "sidebar-section-header",
                        p { class: "section-label", "Justfiles ({profiles.len()})" }
                        button {
                            class: "run-cmd-btn",
                            title: "Open a justfile via file dialog",
                            onclick: move |_| {
                                if let Some((profile, log_msg)) = pick_and_load_justfile() {
                                    let default_rec = profile.dump.default_recipe_name();
                                    justfiles.write().push(profile);
                                    let new_idx = justfiles().len().saturating_sub(1);
                                    active_justfile_idx.set(new_idx);
                                    selected_recipe.set(default_rec);
                                    logs.write().push(log_msg);
                                }
                            },
                            "+ Add Justfile"
                        }
                    }

                    div {
                        class: "cmd-list",
                        for (idx, profile) in profiles.iter().enumerate() {
                            {
                                let is_active = current_idx == idx;
                                let label = profile.label.clone();
                                let path = profile.path.clone();
                                let remove_path = profile.path.clone();
                                let recipe_count = profile.dump.recipes.len();
                                let first_recipe = profile.dump.default_recipe_name();

                                rsx! {
                                    div {
                                        key: "{idx}-{path}",
                                        class: if is_active { "cmd-btn active" } else { "cmd-btn" },
                                        onclick: move |_| {
                                            active_justfile_idx.set(idx);
                                            selected_recipe.set(first_recipe.clone());
                                            logs.write().push(format!("[switch] Switched to justfile '{}'", path));
                                        },
                                        div {
                                            class: "cmd-btn-header",
                                            span { "{label}" }
                                            div {
                                                class: "justfile-item-actions",
                                                span {
                                                    class: if is_active { "cmd-tag first" } else { "cmd-tag" },
                                                    "{recipe_count} recipes"
                                                }
                                                button {
                                                    class: "trash-btn",
                                                    title: "Remove justfile",
                                                    onclick: move |evt| {
                                                        evt.stop_propagation();
                                                        let mut list = justfiles.write();
                                                        if idx < list.len() {
                                                            list.remove(idx);
                                                        }
                                                        let remaining = list.len();
                                                        let next_idx = if remaining == 0 {
                                                            0
                                                        } else if current_idx >= remaining {
                                                            remaining - 1
                                                        } else if idx < current_idx {
                                                            current_idx - 1
                                                        } else {
                                                            current_idx
                                                        };
                                                        let next_default = list
                                                            .get(next_idx)
                                                            .map(|p| p.dump.default_recipe_name())
                                                            .unwrap_or_default();
                                                        drop(list);
                                                        active_justfile_idx.set(next_idx);
                                                        selected_recipe.set(next_default);
                                                        logs.write().push(format!("[remove] Removed justfile '{}'", remove_path));
                                                    },
                                                    "🗑"
                                                }
                                            }
                                        }
                                        span { class: "cmd-desc", "{profile.path}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Left Resize Handle
            div {
                class: if active_drag_side == Some(ResizeSide::Left) {
                    "resize-handle active"
                } else {
                    "resize-handle"
                },
                title: "Drag to resize left panel (double-click to reset)",
                onmousedown: move |evt| {
                    drag_state.set(Some((
                        ResizeSide::Left,
                        evt.client_coordinates().x,
                        left_width(),
                    )));
                },
                ondoubleclick: move |_| left_width.set(280.0),
            }

            // 2. Center Component (Filled with `just --dump --dump-format json` information)
            main {
                class: "main-panel",
                header {
                    class: "main-header",
                    div {
                        p { class: "main-title", "{justfile_title}" }
                        if !dump.source.is_empty() {
                            p { class: "brand-subtitle", "Source: {dump.source}" }
                        }
                    }
                    div {
                        class: "header-actions",
                        span {
                            class: "status-pill",
                            span { class: "status-dot" }
                            "{dump.recipes.len()} Recipes Loaded"
                        }
                        button {
                            title: if profiles.is_empty() {
                                "Open a justfile via file dialog"
                            } else {
                                "Reload the loaded justfiles"
                            },
                            class: "send-btn",
                            onclick: move |_| {
                                let current_list = justfiles();
                                if current_list.is_empty() {
                                    if let Some((profile, msg)) = pick_and_load_justfile() {
                                        selected_recipe.set(profile.dump.default_recipe_name());
                                        justfiles.set(vec![profile]);
                                        active_justfile_idx.set(0);
                                        logs.write().push(msg);
                                    }
                                } else {
                                    let mut updated = Vec::with_capacity(current_list.len());
                                    let mut active_msg = String::new();
                                    for (i, prof) in current_list.iter().enumerate() {
                                        let (reloaded_prof, msg) = load_custom_justfile(&prof.path);
                                        if i == current_idx {
                                            active_msg = msg;
                                        }
                                        updated.push(reloaded_prof);
                                    }
                                    justfiles.set(updated);
                                    logs.write().push(active_msg);
                                }
                            },
                            if profiles.is_empty() { "Open Justfile" } else { "Reload Justfile" }
                        }
                    }
                }

                div {
                    class: "center-viewport",

                    // Collapsible Summary Metrics from Top-Level Dump Info (collapsed by default)
                    Collapsible {
                        title: "Justfile Summary",
                        div {
                            class: "summary-strip",
                            div {
                                class: "metric-box",
                                span { class: "metric-label", "First / Entry Recipe" }
                                span { class: "metric-value", "{first_recipe_name}" }
                            }
                            div {
                                class: "metric-box",
                                span { class: "metric-label", "Total Recipes" }
                                span { class: "metric-value", "{dump.recipes.len()}" }
                            }
                            div {
                                class: "metric-box",
                                span { class: "metric-label", "Justfile Source" }
                                span { class: "metric-value", "{dump.source}" }
                            }
                        }
                    }

                    // Available Commands Grid
                    div {
                        p {
                            class: "section-label",
                            "Available Commands ({filtered_recipes.len()})"
                        }
                        div {
                            class: "composer-bar",
                            input {
                                class: "composer-input",
                                placeholder: "Filter recipes by name, doc, or command body (e.g., cargo, dx serve)...",
                                value: "{filter_input}",
                                oninput: move |evt| filter_input.set(evt.value()),
                            }
                            if !filter_input().is_empty() {
                                button {
                                    class: "send-btn",
                                    onclick: move |_| filter_input.set(String::new()),
                                    "Clear"
                                }
                            }
                        }
                        div {
                            class: "recipe-grid",
                            if profiles.is_empty() {
                                p {
                                    class: "cmd-desc",
                                    "No justfile loaded. Use + Add Justfile to open one."
                                }
                            }
                            for recipe in filtered_recipes {
                                {
                                    let r_name = recipe.name.clone();
                                    let run_name = recipe.name.clone();
                                    let toggle_name = recipe.name.clone();
                                    let justfile_src = dump.source.clone();
                                    let toggle_src = dump.source.clone();
                                    let exec_state = run_states()
                                        .get(&(justfile_src.clone(), r_name.clone()))
                                        .cloned();
                                    let status = exec_state.as_ref().map(|s| s.status);
                                    let log_expanded = exec_state
                                        .as_ref()
                                        .map(|s| s.log_expanded)
                                        .unwrap_or(false);
                                    let log_output = exec_state
                                        .as_ref()
                                        .map(|s| s.output.clone())
                                        .unwrap_or_else(|| "Not executed yet.".to_string());
                                    let log_output_id = format!("recipe-log-content-{r_name}");
                                    let is_selected = selected_recipe() == r_name;
                                    let is_first = dump.first.as_deref() == Some(r_name.as_str());
                                    let resolved_args = recipe.resolve_args(&justfile_src, &param_values());
                                    let recipe_params = recipe.parameters.clone();
                                    let body_preview = recipe
                                        .formatted_body_lines()
                                        .join(" && ");
                                    let btn_class = match status {
                                        Some(RecipeRunStatus::Running) => "run-cmd-btn running",
                                        Some(RecipeRunStatus::Done) => "run-cmd-btn done",
                                        Some(RecipeRunStatus::Failed) => "run-cmd-btn failed",
                                        Some(RecipeRunStatus::Idle) | None => "run-cmd-btn",
                                    };
                                    let btn_label = match status {
                                        Some(RecipeRunStatus::Running) => "⏳ Running...",
                                        Some(RecipeRunStatus::Done) => "✓ Done",
                                        Some(RecipeRunStatus::Failed) => "✕ Failed",
                                        Some(RecipeRunStatus::Idle) | None => "▶ Run",
                                    };
                                    rsx! {
                                        div {
                                            key: "{r_name}",
                                            class: if is_selected { "recipe-card selected" } else { "recipe-card" },
                                            onclick: move |_| {
                                                selected_recipe.set(r_name.clone());
                                                logs.write().push(format!("[select] Focused recipe '{}'", r_name));
                                            },
                                            div {
                                                class: "cmd-btn-header",
                                                div {
                                                    class: "recipe-card-title-group",
                                                    span { "{recipe.name}" }
                                                    for p in &recipe_params {
                                                        span {
                                                            key: "{p.name}",
                                                            class: "cmd-tag",
                                                            "{p.name}"
                                                        }
                                                    }
                                                    if is_first {
                                                        span { class: "cmd-tag first", "first" }
                                                    }
                                                }
                                                button {
                                                    class: "{btn_class}",
                                                    title: if matches!(status, Some(RecipeRunStatus::Running)) {
                                                        "Click to abort running recipe".to_string()
                                                    } else {
                                                        format!("Run `just {}` in justfile directory", recipe.name)
                                                    },
                                                    onclick: move |evt| {
                                                        evt.stop_propagation();
                                                        selected_recipe.set(run_name.clone());
                                                        schedule_recipe_run(
                                                            justfile_src.clone(),
                                                            run_name.clone(),
                                                            resolved_args.clone(),
                                                            run_states,
                                                            logs,
                                                        );
                                                    },
                                                    "{btn_label}"
                                                }
                                            }
                                            p {
                                                class: "cmd-desc",
                                                "{recipe.doc.as_deref().unwrap_or(\"No description\")}"
                                            }
                                            div {
                                                class: "code-block",
                                                span { "{body_preview}" }
                                            }
                                            div {
                                                class: "recipe-log-collapsible",
                                                onclick: move |evt| evt.stop_propagation(),
                                                button {
                                                    class: "recipe-log-toggle",
                                                    onclick: move |evt| {
                                                        evt.stop_propagation();
                                                        let key = (toggle_src.clone(), toggle_name.clone());
                                                        let mut states = run_states.write();
                                                        if let Some(existing) = states.get_mut(&key) {
                                                            existing.log_expanded = !existing.log_expanded;
                                                        } else {
                                                            states.insert(
                                                                key,
                                                                RecipeExecutionState {
                                                                    status: RecipeRunStatus::Idle,
                                                                    output: "Not executed yet.".to_string(),
                                                                    log_expanded: true,
                                                                    abort_handle: None,
                                                                },
                                                            );
                                                        }
                                                    },
                                                    span {
                                                        if log_expanded {
                                                            "▾ Execution Log"
                                                        } else {
                                                            "▸ Execution Log"
                                                        }
                                                    }
                                                    span {
                                                        class: "cmd-tag",
                                                        if log_expanded { "Hide" } else { "Show" }
                                                    }
                                                }
                                                if log_expanded {
                                                    div {
                                                        class: "recipe-log-output",
                                                        div {
                                                            id: "{log_output_id}",
                                                            class: "recipe-log-content",
                                                            onmounted: move |_| {
                                                                let element_id = serde_json::to_string(&log_output_id)
                                                                    .unwrap_or_else(|_| "\"\"".to_string());
                                                                let script = format!(
                                                                    r#"
                                                                    const element = document.getElementById({element_id});
                                                                    if (element) {{
                                                                        const scrollToEnd = () => {{
                                                                            element.scrollTop = element.scrollHeight;
                                                                        }};
                                                                        scrollToEnd();
                                                                        const observer = new MutationObserver(scrollToEnd);
                                                                        observer.observe(element, {{ childList: true, subtree: true, characterData: true }});
                                                                    }}
                                                                    "#
                                                                );
                                                                let _ = document::eval(&script);
                                                            },
                                                            "{log_output}"
                                                        }
                                                        button {
                                                            class: "copy-log-btn",
                                                            title: "Copy execution log",
                                                            onclick: move |evt| {
                                                                evt.stop_propagation();
                                                                let output = log_output.clone();
                                                                async move {
                                                                    let output = serde_json::to_string(&output)
                                                                        .unwrap_or_else(|_| "\"\"".to_string());
                                                                    let script = format!(
                                                                        "try {{ await navigator.clipboard.writeText({output}); return true; }} catch {{ return false; }}"
                                                                    );
                                                                    let _ = document::eval(&script).await;
                                                                }
                                                            },
                                                            "Copy"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Right Resize Handle
            div {
                class: if active_drag_side == Some(ResizeSide::Right) {
                    "resize-handle active"
                } else {
                    "resize-handle"
                },
                title: "Drag to resize right panel (double-click to reset)",
                onmousedown: move |evt| {
                    drag_state.set(Some((
                        ResizeSide::Right,
                        evt.client_coordinates().x,
                        right_width(),
                    )));
                },
                ondoubleclick: move |_| right_width.set(330.0),
            }

            // 3. Right Sidebar (Parameters / Settings / Logs Area)
            aside {
                class: "sidebar-right",
                div {
                    class: "brand-header",
                    div {
                        h1 { class: "brand-title", "Context & Parameters" }
                        p { class: "brand-subtitle", "Recipe Attributes & Settings" }
                    }
                }

                if let Some(recipe) = &active_recipe {
                    // Recipe Parameter Editor (Adapt parameters of each recipe)
                    div {
                        class: "info-card",
                        div {
                            class: "cmd-btn-header",
                            h3 { class: "info-card-title", "Recipe Parameters ({recipe.name})" }
                            span {
                                class: "cmd-tag",
                                "{recipe.parameters.len()} params"
                            }
                        }
                        if recipe.parameters.is_empty() {
                            p {
                                class: "cmd-desc",
                                "No parameters defined for `{recipe.name}`."
                            }
                        } else {
                            for param in &recipe.parameters {
                                {
                                    let p_name = param.name.clone();
                                    let p_kind = if param.kind.is_empty() {
                                        "singular".to_string()
                                    } else {
                                        param.kind.clone()
                                    };
                                    let default_val = param.default_string();
                                    let placeholder = if default_val.is_empty() {
                                        format!("Enter value for {p_name}...")
                                    } else {
                                        format!("Default: {default_val}")
                                    };
                                    let param_key = (
                                        dump.source.clone(),
                                        recipe.name.clone(),
                                        p_name.clone(),
                                    );
                                    let enter_src = dump.source.clone();
                                    let enter_recipe = recipe.clone();
                                    let current_val = param_values()
                                        .get(&param_key)
                                        .cloned()
                                        .unwrap_or_else(|| default_val.clone());

                                    rsx! {
                                        div {
                                            key: "{recipe.name}-{p_name}",
                                            class: "param-edit-group",
                                            div {
                                                class: "param-edit-header",
                                                span { class: "param-edit-name", "{p_name}" }
                                                span { class: "cmd-tag", "{p_kind}" }
                                            }
                                            input {
                                                class: "param-input",
                                                placeholder: "{placeholder}",
                                                value: "{current_val}",
                                                oninput: move |evt| {
                                                    param_values.write().insert(param_key.clone(), evt.value());
                                                },
                                                onkeydown: move |evt| {
                                                    if evt.key() == Key::Enter {
                                                        let args = enter_recipe.resolve_args(&enter_src, &param_values());
                                                        schedule_recipe_run(
                                                            enter_src.clone(),
                                                            enter_recipe.name.clone(),
                                                            args,
                                                            run_states,
                                                            logs,
                                                        );
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Collapsible Selected Command Parameters (collapsed by default)
                    Collapsible {
                        title: "Selected Command Parameters",
                        div {
                            class: "collapsible-card-body",
                            div {
                                class: "param-row",
                                span { class: "param-key", "Namepath" }
                                span { class: "param-val", "{recipe.namepath}" }
                            }
                            div {
                                class: "param-row",
                                span { class: "param-key", "Parameters" }
                                span { class: "param-val", "{recipe.parameters.len()}" }
                            }
                            div {
                                class: "param-row",
                                span { class: "param-key", "Dependencies" }
                                span { class: "param-val", "{recipe.dependencies.len()}" }
                            }
                            div {
                                class: "param-row",
                                span { class: "param-key", "Priors" }
                                span { class: "param-val", "{recipe.priors}" }
                            }
                            div {
                                class: "param-row",
                                span { class: "param-key", "Private / Quiet" }
                                span { class: "param-val", "{recipe.private} / {recipe.quiet}" }
                            }
                            div {
                                class: "param-row",
                                span { class: "param-key", "Shebang" }
                                span { class: "param-val", "{recipe.shebang}" }
                            }
                        }
                    }
                }

                // Collapsible Justfile Settings (collapsed by default)
                Collapsible {
                    title: "Justfile Settings",
                    div {
                        class: "collapsible-card-body",
                        div {
                            class: "param-row",
                            span { class: "param-key", "dotenv_load" }
                            span { class: "param-val", "{dump.settings.dotenv_load}" }
                        }
                        div {
                            class: "param-row",
                            span { class: "param-key", "export" }
                            span { class: "param-val", "{dump.settings.export}" }
                        }
                        div {
                            class: "param-row",
                            span { class: "param-key", "positional_arguments" }
                            span { class: "param-val", "{dump.settings.positional_arguments}" }
                        }
                        div {
                            class: "param-row",
                            span { class: "param-key", "quiet" }
                            span { class: "param-val", "{dump.settings.quiet}" }
                        }
                        div {
                            class: "param-row",
                            span { class: "param-key", "Warnings" }
                            span { class: "param-val", "{dump.warnings.len()}" }
                        }
                    }
                }

                // Collapsible Activity Logs (collapsed by default)
                Collapsible {
                    title: "Activity Logs",
                    div {
                        class: "collapsible-card-body",
                        for (idx, entry) in logs().into_iter().enumerate() {
                            div {
                                key: "{idx}",
                                class: "log-entry",
                                "{entry}"
                            }
                        }
                    }
                }
            }
        }
    }
}
