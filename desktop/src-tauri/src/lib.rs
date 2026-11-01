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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // rustls needs one crypto provider for the process; every driver uses ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tauri::Builder::default()
        .manage(commands::AppState::default())
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
        .run(tauri::generate_context!())
        .expect("error while running dbLens");
}
