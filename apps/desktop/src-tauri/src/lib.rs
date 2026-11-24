mod agent_chat;
mod agent_review;
mod code_run;
mod completion;
mod connectors;
mod context_mentions;
mod credential_store;
mod debug_adapter;
mod extensions;
mod git_status;
mod github_queue;
mod inline_edit;
mod lsp;
mod lsp_frame;
mod marketplace;
mod menu;
mod mission;
mod model_runtime;
mod onboarding;
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
mod updater;
mod workbench;
mod workspace;
mod worktree_ops;

use tauri::Manager;

pub fn run() {
    let context = tauri::generate_context!();
    let updates_configured = updater::is_configured(&context.config().plugins.0);
    let builder = updater::register(tauri::Builder::default(), updates_configured);
    #[cfg(target_os = "macos")]
    let builder = builder
        .menu(|handle| menu::build(handle))
        .on_menu_event(|handle, event| menu::forward(handle, event.id().as_ref()));
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .manage(updater::UpdaterState::new(updates_configured))
        .manage(workspace::WorkspaceState::default())
        .manage(agent_chat::AgentChatState::default())
        .manage(github_queue::GitHubQueueState::default())
        .manage(lsp::LspState::default())
        .manage(run::RunState::default())
        .manage(completion::CompletionState::default())
        .manage(code_run::CodeRunState::default())
        .manage(terminal::TerminalState::default())
        .manage(debug_adapter::DebugState::default())
        .invoke_handler(tauri::generate_handler![
            workspace::open_workspace,
            workspace::pick_workspace_folder,
            onboarding::onboarding_state,
            onboarding::onboarding_configure,
            onboarding::onboarding_remember_folder,
            onboarding::clone_repository,
            workspace::list_files,
            workspace::read_file,
            workspace::write_file,
            agent_chat::agent_chat,
            agent_chat::agent_chat_cancel,
            context_mentions::resolve_context_mention,
            context_mentions::search_context_symbols,
            agent_review::read_agent_candidate,
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
            worktree_ops::list_agent_session_changes,
            worktree_ops::read_agent_session_file,
            worktree_ops::release_agent_session,
            worktree_ops::discard_agent_session,
            worktree_ops::merge_agent_session,
            extensions::list_installed_extensions,
            extensions::install_wasi_extension,
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
            debug_adapter::dap_restart,
            updater::update_status,
            updater::update_check,
            updater::update_install,
        ])
        .build(context)
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
