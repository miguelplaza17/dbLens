//! dbLens for Windows and Linux: a Tauri 2 shell around database drivers that
//! mirror the macOS app's DBLensKit. See desktop/CONTRACT.md.

pub mod close_guard;
pub mod commands;
pub mod connection_problem;
pub mod connector;
pub mod drivers;
pub mod engine;
pub mod export;
pub mod history;
pub mod import_swift;
pub mod mcp;
pub mod menu;
pub mod pool;
pub mod session;
pub mod settings;
pub mod sql_script;
pub mod ssh;
pub mod store;
pub mod terminal;
pub mod updater;
pub mod url;
pub mod windows;
pub mod wire;

use tauri::webview::PageLoadEvent;
use tauri::{Manager, RunEvent, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // rustls needs one crypto provider for the process; every driver uses ring.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = tauri::Builder::default();
    // A second launch raises the running app (or opens a database file in a new
    // window) instead of starting another process. It must be the first plugin.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
        windows::second_launch(app, &args, &cwd)
    }));
    let app = builder
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(commands::AppState::default())
        .manage(windows::PendingOpen::default())
        .manage(close_guard::CloseGuard::default())
        .manage(settings::SettingsState::from_env())
        .manage(history::HistoryState::from_env())
        .manage(updater::UpdaterState::default())
        .setup(|app| {
            menu::install(app.handle())?;
            windows::queue_launch_args(app.handle());
            updater::start(app.handle());
            Ok(())
        })
        .on_menu_event(|app, event| menu::on_event(app, event.id().as_ref()))
        // A reload (or HMR) forgets every session id: close what that page opened.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started {
                let app = webview.app_handle().clone();
                let label = webview.label().to_string();
                app.state::<close_guard::CloseGuard>().forget(&label);
                tauri::async_runtime::spawn(async move {
                    app.state::<commands::AppState>()
                        .close_owned_by(&label)
                        .await;
                });
            }
        })
        .on_window_event(|window, event| {
            // Unsaved grid edits: the page decides (close_guard.rs).
            if let WindowEvent::CloseRequested { api, .. } = event {
                if close_guard::hold_close(window) {
                    api.prevent_close();
                }
            }
            if let WindowEvent::Destroyed = event {
                window
                    .state::<close_guard::CloseGuard>()
                    .forget(window.label());
                // Taken out of the map now (the label may be reused at once),
                // closed in the background so a slow server can't freeze the UI.
                let state = window.state::<commands::AppState>();
                for webview in window.webviews() {
                    state.close_owned_by_later(webview.label());
                }
                state.close_owned_by_later(window.label());
            }
        })
        .invoke_handler(tauri::generate_handler![
            menu::set_menu_state,
            menu::set_menu_labels,
            close_guard::close_guard_ready,
            close_guard::close_confirmed,
            windows::new_window,
            windows::take_pending_open,
            commands::detect_engine,
            commands::summarize_url,
            commands::mask_url,
            commands::connect,
            commands::disconnect,
            commands::cancel,
            commands::relations,
            commands::page,
            commands::page_block,
            commands::execute_stream,
            commands::transaction_open,
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
            terminal::connection_url_with_password,
            terminal::open_in_terminal,
            history::sql_history,
            history::add_sql_history,
            history::clear_sql_history,
            commands::set_agents_can_write,
            commands::set_agents_manage_connections,
            commands::move_connection,
            commands::add_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::set_folder_expanded,
            import_swift::import_swift_connections,
            settings::get_settings,
            settings::set_settings,
            settings::settings_info,
            settings::open_settings,
            settings::fit_settings,
            settings::show_data_folder,
            updater::check_for_updates,
            updater::install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building dbLens");
    // The process ends with exit() and managed state is never dropped, so the
    // sessions and their ssh tunnels are closed here, on the way out.
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            let state = app.state::<commands::AppState>();
            tauri::async_runtime::block_on(state.close_all());
            updater::on_exit(app);
        }
    });
}
