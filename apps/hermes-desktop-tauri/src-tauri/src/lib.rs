mod branding;
mod commands;
mod hermes_backend;
mod hermes_ws_bridge;

use commands::*;
use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .plugin(tauri_plugin_websocket::init())
        .menu(|app| build_application_menu(app))
        .on_menu_event(|app, event| {
            let state = app.state::<AppState>();
            handle_menu_event(app, event, &state);
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::Focused(_) | WindowEvent::Resized(_)) {
                emit_window_state_changed(&window.app_handle(), None);
            }

            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                terminate_tracked_backend(&state);
                dispose_all_terminal_sessions(&state);

                let app_for_preview = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_for_preview.state::<AppState>();
                    stop_all_preview_file_watches(&state).await;
                });
            }
        })
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            let handle = app.handle();
            if maybe_handle_macos_first_launch(&handle) {
                return Ok(());
            }
            let menu = build_application_menu(&handle)?;
            handle.set_menu(menu)?;
            emit_window_state_changed(&handle, None);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Connection
            get_connection,
            get_connection_config,
            save_connection_config,
            apply_connection_config,
            test_connection_config,
            probe_connection_config,
            oauth_login_connection_config,
            oauth_logout_connection_config,
            get_gateway_ws_url,
            start_hermes,
            subscribe_task_stream,
            cancel_task_stream,
            // API proxy
            hermes_api,
            local_model_chat_completion,
            local_model_chat_completion_stream,
            // Boot
            get_boot_progress,
            get_bootstrap_state,
            reset_bootstrap,
            repair_bootstrap,
            cancel_bootstrap,
            // File
            read_file_data_url,
            read_file_text,
            read_dir,
            git_root,
            // Clipboard
            write_clipboard,
            // Notifications
            notify,
            // External
            open_external,
            // Updates
            check_updates,
            apply_updates,
            get_update_branch,
            set_update_branch,
            get_update_sources,
            set_update_sources,
            open_desktop_update_repository,
            set_preview_shortcut_active,
            // Context menu
            show_context_menu,
            // Version
            get_version,
            check_hermes_agent_update,
            install_hermes_agent,
            update_hermes_agent,
            repair_hermes_agent,
            install_hermes_service,
            uninstall_hermes_service,
            restart_hermes_service,
            // Logs
            reveal_logs,
            get_recent_logs,
            // Settings
            get_default_project_dir,
            set_default_project_dir,
            pick_default_project_dir,
            get_ui_preferences,
            set_ui_language,
            // Image
            save_image_from_url,
            save_image_buffer,
            save_clipboard_image,
            // Preview
            normalize_preview_target,
            watch_preview_file,
            stop_preview_file_watch,
            // Titlebar
            set_titlebar_theme,
            // Link
            fetch_link_title,
            // Terminal
            terminal_start,
            terminal_write,
            terminal_resize,
            terminal_dispose,
            // Paths
            select_paths,
            // Microphone
            request_microphone_access,
            // Account / billing
            sign_in_oauth,
            sign_in_email,
            sign_out,
            get_account,
            refresh_account_token,
            create_alipay_order,
            create_wechat_order,
            query_payment_status,
            register_push_token,
            unregister_push_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
