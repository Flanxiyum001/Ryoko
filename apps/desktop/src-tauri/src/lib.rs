mod code_run;
mod completion;
mod connectors;
mod debug_adapter;
mod git_status;
mod github_queue;
mod inline_edit;
mod lsp;
mod lsp_frame;
mod marketplace;
mod mission;
mod model_runtime;
mod paths;
mod review;
mod run;
mod search;
mod servers;
mod sessions;
mod sessions_projection;
mod settings;
mod terminal;
mod terminal_decode;
mod workbench;
mod workspace;

use tauri::Manager;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(workspace::WorkspaceState::default())
        .manage(github_queue::GitHubQueueState::default())
        .manage(lsp::LspState::default())
        .manage(run::RunState::default())
        .manage(completion::CompletionState::default())
        .manage(code_run::CodeRunState::default())
        .manage(terminal::TerminalState::default())
        .manage(debug_adapter::DebugState::default())
        .invoke_handler(tauri::generate_handler![
            workspace::open_workspace,
            workspace::list_files,
            workspace::read_file,
            workspace::write_file,
            workbench::git_changes,
            workbench::workspace_search,
            workbench::workspace_replace_preview,
            workbench::workspace_replace_apply,
            workbench::inline_edit,
            workbench::settings_read,
            workbench::settings_save_providers,
            workbench::settings_save_lsp_servers,
            workbench::workbench_protocol_version,
            lsp::lsp_start,
            lsp::lsp_send,
            lsp::lsp_registry,
            lsp::lsp_stop,
            marketplace::marketplace_catalog,
            mission::list_mission_logs,
            mission::read_mission_log,
            github_queue::pair_github_queue,
            github_queue::disconnect_github_queue,
            github_queue::list_github_repositories,
            github_queue::get_github_queue,
            sessions::list_agent_sessions,
            review::list_review_hunks,
            review::stage_review_hunks,
            review::reject_review_hunks,
            review::read_review_file,
            run::sandbox_init,
            run::start_run,
            run::validate_review,
            run::run_status,
            run::pause_run,
            run::resume_run,
            run::cancel_run,
            code_run::code_run_start,
            code_run::code_run_status,
            code_run::code_run_stop,
            completion::fim_complete,
            completion::fim_cancel,
            terminal::terminal_start,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_status,
            terminal::terminal_stop,
            connectors::list_connectors,
            connectors::add_connector,
            connectors::set_connector_enabled,
            connectors::set_connector_tools,
            connectors::inspect_connector,
            debug_adapter::dap_start,
            debug_adapter::dap_send,
            debug_adapter::dap_status,
            debug_adapter::dap_stop,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Ryoko desktop shell");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<lsp::LspState>() {
                lsp::shutdown(&state);
            }
            if let Some(state) = handle.try_state::<run::RunState>() {
                run::shutdown(&state);
            }
            if let Some(state) = handle.try_state::<code_run::CodeRunState>() {
                code_run::shutdown(&state);
            }
            if let Some(state) = handle.try_state::<terminal::TerminalState>() {
                terminal::shutdown(&state);
            }
            if let Some(state) = handle.try_state::<debug_adapter::DebugState>() {
                debug_adapter::shutdown(&state);
            }
        }
    });
}
