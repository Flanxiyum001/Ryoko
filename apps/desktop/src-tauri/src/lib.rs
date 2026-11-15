mod github_queue;
mod lsp;
mod lsp_frame;
mod mission;
mod paths;
mod workspace;

use tauri::Manager;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(workspace::WorkspaceState::default())
        .manage(github_queue::GitHubQueueState::default())
        .manage(lsp::LspState::default())
        .invoke_handler(tauri::generate_handler![
            workspace::open_workspace,
            workspace::list_files,
            workspace::read_file,
            workspace::write_file,
            lsp::lsp_start,
            lsp::lsp_send,
            lsp::lsp_stop,
            mission::list_mission_logs,
            mission::read_mission_log,
            github_queue::pair_github_queue,
            github_queue::disconnect_github_queue,
            github_queue::list_github_repositories,
            github_queue::get_github_queue,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Ryoko desktop shell");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<lsp::LspState>() {
                lsp::shutdown(&state);
            }
        }
    });
}
