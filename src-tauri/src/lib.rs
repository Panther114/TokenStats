//! TokenStats: unified token usage dashboard for Claude Code, Codex, OpenCode and
//! DeepSeek Harness.
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod model;
pub mod opencode;
pub mod pricing;
pub mod report;
pub mod scan;

use pricing::Pricing;
use scan::Scanner;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

struct AppState {
    scanner: Mutex<Scanner>,
    pricing: Mutex<Pricing>,
    pricing_path: PathBuf,
}

/// Scans (incrementally) and returns the dashboard report as JSON text.
#[tauri::command]
async fn get_report(app: AppHandle, full: Option<bool>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut scanner = state.scanner.lock().map_err(|e| e.to_string())?;
        if full.unwrap_or(false) {
            scanner.clear();
        }
        let res = scanner.scan();
        let pricing = state.pricing.lock().map_err(|e| e.to_string())?;
        let report = report::build(&res.data, &pricing, res.millis, res.parsed_files, res.cached_files);
        serde_json::to_string(&report).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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
fn pricing_updated(state: State<'_, AppState>) -> String {
    state.pricing.lock().map(|p| p.updated.clone()).unwrap_or_default()
}

/// Called by the page once its first frame is ready, so the window never flashes blank.
#[tauri::command]
fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_main(app)))
        .setup(|app| {
            let dir = app.path().app_local_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("TokenStats"));
            let pricing_path = dir.join("pricing.json");
            let pricing = std::fs::read_to_string(&pricing_path)
                .ok()
                .and_then(|t| Pricing::from_json(&t).ok())
                .filter(|p| p.updated >= Pricing::embedded().updated)
                .unwrap_or_else(Pricing::embedded);
            app.manage(AppState {
                scanner: Mutex::new(Scanner::new(Some(dir.join("scan-cache.bin")))),
                pricing: Mutex::new(pricing),
                pricing_path,
            });

            let open = MenuItem::with_id(app, "open", "Open TokenStats", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &refresh, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("bundled icon"))
                .tooltip("TokenStats")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_main(app),
                    "refresh" => {
                        show_main(app);
                        let _ = app.emit("tokenstats://refresh", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing hides to the tray; Quit from the tray menu exits.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![get_report, set_pricing, pricing_updated, app_ready])
        .run(tauri::generate_context!())
        .expect("error while running TokenStats");
}
