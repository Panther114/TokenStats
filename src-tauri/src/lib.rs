//! TokenStats: a unified token usage dashboard for local coding-agent harnesses
//! (Claude Code, Codex, OpenCode, DeepSeek Harness, Gemini CLI and others).
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod model;
pub mod opencode;
pub mod pricing;
pub mod providers;
pub mod report;
pub mod scan;

use pricing::Pricing;
use providers::ProviderConfig;
use scan::Scanner;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};

struct AppState {
    scanner: Mutex<Scanner>,
    pricing: Mutex<Pricing>,
    providers: Mutex<Vec<ProviderConfig>>,
    pricing_path: PathBuf,
    providers_path: PathBuf,
}

/// Scans the enabled providers (incrementally) and returns the dashboard report as JSON text.
#[tauri::command]
async fn get_report(app: AppHandle, full: Option<bool>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let enabled: Vec<ProviderConfig> = state.providers.lock().map_err(|e| e.to_string())?.iter().filter(|p| p.enabled).cloned().collect();
        let mut scanner = state.scanner.lock().map_err(|e| e.to_string())?;
        if full.unwrap_or(false) {
            scanner.clear();
        }
        let res = scanner.scan(&enabled);
        let data: Vec<(ProviderConfig, model::HarnessData)> = enabled.into_iter().zip(res.data).collect();
        let pricing = state.pricing.lock().map_err(|e| e.to_string())?;
        let report = report::build(&data, &pricing, res.millis, res.parsed_files, res.cached_files);
        serde_json::to_string(&report).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The configured providers plus the catalog of supported kinds.
#[tauri::command]
fn get_providers(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let list = state.providers.lock().map_err(|e| e.to_string())?.clone();
    Ok(serde_json::json!({ "providers": list, "catalog": providers::catalog() }))
}

/// Replaces the provider configuration (add, edit, reorder, enable/disable, delete).
#[tauri::command]
fn set_providers(state: State<'_, AppState>, list: Vec<ProviderConfig>) -> Result<(), String> {
    for p in &list {
        if providers::kind_info(&p.kind).is_none() {
            return Err(format!("unknown provider kind '{}'", p.kind));
        }
        if p.name.trim().is_empty() || p.id.trim().is_empty() {
            return Err("every provider needs an id and a name".into());
        }
    }
    let text = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    if let Some(dir) = state.providers_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&state.providers_path, text).map_err(|e| e.to_string())?;
    *state.providers.lock().map_err(|e| e.to_string())? = list;
    Ok(())
}

/// Restores the default provider list (built-in harnesses plus detected ones).
#[tauri::command]
fn reset_providers(state: State<'_, AppState>) -> Result<Vec<ProviderConfig>, String> {
    let list = providers::default_config();
    let _ = std::fs::remove_file(&state.providers_path);
    *state.providers.lock().map_err(|e| e.to_string())? = list.clone();
    Ok(list)
}

/// Replaces the pricing table with a snapshot produced by the dashboard's daily refresh.
#[tauri::command]
fn set_pricing(state: State<'_, AppState>, snapshot: String) -> Result<String, String> {
    let p = Pricing::from_json(&snapshot)?;
    let updated = p.updated.clone();
    if let Some(dir) = state.pricing_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&state.pricing_path, &snapshot).map_err(|e| e.to_string())?;
    *state.pricing.lock().map_err(|e| e.to_string())? = p;
    Ok(updated)
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Called by the page once its first frame is ready, so the window never flashes blank.
#[tauri::command]
fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn load_providers(path: &PathBuf) -> Vec<ProviderConfig> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<ProviderConfig>>(&t).ok())
        .map(|list| list.into_iter().filter(|p| providers::kind_info(&p.kind).is_some()).collect())
        .unwrap_or_else(providers::default_config)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .setup(|app| {
            let dir = app.path().app_local_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("TokenStats"));
            let pricing_path = dir.join("pricing.json");
            let providers_path = dir.join("providers.json");
            let pricing = std::fs::read_to_string(&pricing_path)
                .ok()
                .and_then(|t| Pricing::from_json(&t).ok())
                .filter(|p| p.updated >= Pricing::embedded().updated)
                .unwrap_or_else(Pricing::embedded);
            app.manage(AppState {
                scanner: Mutex::new(Scanner::new(Some(dir.join("scan-cache.bin")))),
                pricing: Mutex::new(pricing),
                providers: Mutex::new(load_providers(&providers_path)),
                pricing_path,
                providers_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_report, get_providers, set_providers, reset_providers, set_pricing, app_version, app_ready])
        .run(tauri::generate_context!())
        .expect("error while running TokenStats");
}
