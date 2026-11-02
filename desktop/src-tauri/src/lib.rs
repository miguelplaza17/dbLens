//! dbLens for Windows and Linux: a Tauri 2 shell around database drivers that
//! mirror the macOS app's DBLensKit. See desktop/CONTRACT.md.

pub mod commands;
pub mod connection_problem;
pub mod connector;
pub mod drivers;
pub mod engine;
pub mod export;
pub mod session;
pub mod sql_script;
pub mod ssh;
pub mod store;
pub mod url;

use tauri::webview::PageLoadEvent;
use tauri::{Manager, RunEvent, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // rustls needs one crypto provider for the process; every driver uses ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = tauri::Builder::default()
        .manage(commands::AppState::default())
        // A reload (or HMR) forgets every session id: close what that page opened.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started {
                let app = webview.app_handle().clone();
                let label = webview.label().to_string();
                tauri::async_runtime::spawn(async move {
                    app.state::<commands::AppState>()
                        .close_owned_by(&label)
                        .await;
                });
            }
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                let state = window.state::<commands::AppState>();
                for webview in window.webviews() {
                    tauri::async_runtime::block_on(state.close_owned_by(webview.label()));
                }
                tauri::async_runtime::block_on(state.close_owned_by(window.label()));
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::detect_engine,
            commands::summarize_url,
            commands::mask_url,
            commands::connect,
            commands::disconnect,
            commands::cancel,
            commands::relations,
            commands::page,
            commands::count,
            commands::execute,
            commands::read_only,
            commands::rehearse,
            commands::execute_atomically,
            commands::update_row,
            commands::insert_row,
            commands::delete_row,
            commands::schema_graph,
            commands::qualified_name,
            commands::export_result,
            commands::list_connections,
            commands::save_connection,
            commands::delete_connection,
            commands::rename_connection,
            commands::connection_url,
            commands::move_connection,
            commands::add_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::set_folder_expanded,
        ])
        .build(tauri::generate_context!())
        .expect("error while building dbLens");
    // The process ends with exit() and managed state is never dropped, so the
    // sessions and their ssh tunnels are closed here, on the way out.
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            let state = app.state::<commands::AppState>();
            tauri::async_runtime::block_on(state.close_all());
        }
    });
}
