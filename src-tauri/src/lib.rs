#[path = "activity/chat_tool.rs"]
pub mod activity_chat_tool;
#[path = "activity/matcher.rs"]
pub mod activity_matcher;
pub mod activity_service;
pub mod ai;
pub mod archive_cancel;
pub mod archive_contract;
pub mod archive_create_test;
pub mod archive_extract;
pub mod archive_jobs;
pub mod archive_multipart;
pub mod archive_preflight;
pub mod archive_rar;
pub mod archive_rar_tool;
pub mod archive_sevenz;
pub mod archive_zip;
pub mod automation;
pub mod branch_view;
pub mod codecs;
pub mod comments;
pub mod compare;
pub mod compare_archive;
pub mod copy_engine;
pub mod copy_native;
pub mod copy_pool;
pub mod disk_usage;
pub mod disk_usage_cache;
pub mod domain;
pub mod fast_scan;
pub mod file_tools;
pub mod fs_service;
pub mod hotlist;
pub mod indexer_client;
pub mod ipc;
pub mod media_meta;
pub mod media_protocol;
pub mod move_engine;
pub mod mutations;
pub mod native_dialog;
pub mod office_text;
pub mod ops;
pub mod path_guard;
pub mod path_suggest;
pub mod prefs;
pub mod preview;
pub mod preview_archive;
pub mod preview_image;
pub mod profile;
pub mod properties;
pub mod rename_batch;
pub mod search;
pub mod selection_file;
pub mod shell_open;
pub mod sync_baseline;
pub mod sync_engine;
pub mod sync_incremental;
pub mod tree_source;
pub mod watch;

/// Product name kept in sync with `src/lib/version.ts`.
pub const APP_NAME: &str = "Ferrum";

/// True when the incoming path is a usable filesystem path: non-empty after
/// trimming, no NUL bytes, not composed only of separators/whitespace.
/// Deliberately permissive — Ferrum must be able to browse anywhere the OS
/// allows, so this only rejects structurally unusable input, it does not
/// sandbox.
pub fn is_plausible_path(path: &str) -> bool {
    !path.trim().is_empty()
        && !path.contains('\0')
        && path
            .trim()
            .chars()
            .any(|c| c != '/' && c != '\\' && !c.is_whitespace())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // FER-457: fail closed. A set but invalid test-profile variable must never fall back to the real profile.
    // FER-462 R1: the reason is also shown in a message box (a release build has no console).
    if let Err(profile::InvalidOverride(name)) = profile::init() {
        profile::report_invalid_override(name);
        std::process::exit(2);
    }
    let mut context = tauri::generate_context!();
    // FER-457: a test profile also owns the WebView2 user-data folder (localStorage, caches). The config's
    // `dataDirectory` only accepts paths relative to the app local dir, so with a profile the config windows are
    // not auto-created; `setup` builds them with an absolute data directory instead.
    let test_profile = profile::profile_dir();
    if test_profile.is_some() {
        for window in context.config_mut().app.windows.iter_mut() {
            window.create = false;
        }
    }
    media_protocol::register(tauri::Builder::default())
        .invoke_handler(tauri::generate_handler![
            ipc::list_dir,
            ipc::list_drives,
            ipc::drive_letters_mask,
            ipc::path_suggest,
            ipc::path_resolve,
            ipc::enqueue_dummy_op,
            ipc::cancel_operation,
            ipc::list_operations,
            ipc::copy_control_snapshot,
            ipc::copy_control_adjust,
            ipc::clear_finished_operations,
            ipc::retry_operation,
            ipc::create_entry,
            ipc::rename_entry,
            ipc::delete_entries,
            ipc::plan_copy,
            ipc::enqueue_copy,
            ipc::enqueue_move,
            ipc::archive_list,
            ipc::archive_read,
            ipc::archive_extract,
            ipc::archive_extract_preflight,
            ipc::archive_create,
            ipc::archive_test,
            ipc::archive_rar_tool_probe,
            ipc::archive_rar_tool_detect,
            ipc::archive_rar_tool_set_path,
            ipc::archive_rar_tool_status,
            ipc::archive_multipart_check,
            ipc::save_selection,
            ipc::load_selection,
            ipc::search_start,
            ipc::search_cancel,
            ipc::disk_usage_start,
            ipc::disk_usage_cancel,
            ipc::disk_usage_snapshot,
            ipc::disk_usage_cache_load,
            ipc::disk_usage_cache_list,
            ipc::disk_usage_cache_delete,
            ipc::disk_usage_cache_note_mutation,
            ipc::disk_usage_query_children,
            ipc::disk_usage_top_files,
            ipc::disk_usage_fast_eligibility,
            indexer_client::disk_usage_index_eligibility,
            indexer_client::indexer_status,
            indexer_client::indexer_set_mode,
            indexer_client::indexer_folder_sizes,
            ipc::read_preview,
            ipc::read_archive_member_preview,
            ipc::archive_cancel_request,
            ipc::read_image_preview,
            ipc::read_pdf_bytes,
            ipc::read_media_metadata,
            media_protocol::media_grant,
            media_protocol::media_revoke,
            ipc::compare_files,
            ipc::compare_dirs,
            ipc::validate_sync_roots,
            ipc::compare_cancel,
            ipc::compare_cache_clear,
            ipc::rename_batch,
            ipc::entry_properties,
            ipc::occupied_space,
            ipc::occupied_space_cancel,
            ipc::volume_info,
            ipc::system_info,
            ipc::set_attributes,
            ipc::branch_start,
            ipc::branch_cancel,
            ipc::verify_checksums,
            ipc::cancel_checksum_request,
            ipc::checksum_archive_member,
            ipc::enqueue_checksum_create,
            ipc::enqueue_split_file,
            ipc::enqueue_combine_parts,
            ipc::watch_start,
            ipc::watch_stop,
            ipc::load_prefs,
            ipc::save_prefs,
            ipc::activity_record_visit,
            ipc::activity_validate,
            ipc::activity_recent,
            ipc::activity_frequent,
            ipc::activity_search,
            ipc::activity_state,
            ipc::activity_set_recording,
            ipc::activity_forget,
            ipc::activity_exclude,
            ipc::activity_clear,
            ipc::activity_favorites,
            ipc::activity_add_favorite,
            ipc::activity_remove_favorite,
            ipc::activity_set_retention,
            ipc::activity_set_hidden,
            ipc::activity_ack_disclosure,
            ipc::activity_overview,
            ipc::activity_preview,
            ipc::activity_chat_query,
            ipc::activity_chat_locate,
            ipc::shell_open,
            ipc::open_external,
            ipc::open_terminal,
            ipc::associate_with,
            ipc::print_file,
            ipc::comment_get,
            ipc::comments_for_dir,
            ipc::comment_set,
            ipc::enqueue_encode_files,
            ipc::enqueue_decode_files,
            ai::commands::ai_plan_intent,
            ai::commands::ai_cancel_plan,
            ai::commands::ai_test_connection,
            ai::attach_commands::ai_attachment_inspect,
            ai::attach_commands::ai_ask,
            ai::agent_cli::commands::agent_cli_status,
            ai::agent_cli::commands::agent_cli_detect,
            ai::agent_cli::commands::agent_cli_set_default,
            ai::agent_cli::commands::agent_cli_acknowledge,
            ai::agent_cli::commands::agent_cli_set_enabled,
            ai::agent_cli::commands::agent_cli_set_terminal_default,
            ai::agent_cli::commands::agent_terminal_open,
            ai::agent_cli::commands::agent_ask,
            ai::credential_commands::ai_credential_status,
            ai::credential_commands::ai_credential_save,
            ai::credential_commands::ai_credential_clear,
            profile::test_profile_active,
            ai::setup_commands::ai_setup_load,
            ai::setup_commands::ai_setup_save,
            ai::setup_commands::ai_setup_clear,
            ai::exec_commands::ai_exec_reserve,
            ai::exec_commands::ai_exec_record_enqueued,
            ai::exec_commands::ai_exec_record_not_started,
            ai::exec_commands::ai_exec_mark_unknown,
            ai::exec_commands::ai_exec_resolve_unknown,
            ai::exec_commands::ai_exec_by_execution,
            ai::exec_commands::ai_exec_unresolved,
            ai::approval_commands::ai_register_approval,
            ai::approval_commands::ai_authorize_execute,
            ai::approval_commands::ai_revoke_approval,
            sync_baseline::sync_baseline_load,
            sync_baseline::sync_baseline_save,
            sync_baseline::sync_baseline_clear,
            hotlist::hotlist_load,
            hotlist::hotlist_save,
            hotlist::hotlist_check_location,
            hotlist::hotlist_set_location,
            hotlist::hotlist_import_preview,
            hotlist::hotlist_run_command,
            automation::commands::sync_profile_list,
            automation::commands::sync_profile_get,
            automation::commands::sync_profile_validate,
            automation::commands::sync_profile_save,
            automation::commands::sync_profile_delete,
            automation::commands::sync_lease_acquire,
            automation::commands::sync_lease_release
        ])
        .manage(media_protocol::MediaRegistry::new())
        .manage(hotlist::store::Store::new())
        .manage(ai::commands::AiState::default())
        .manage(ai::approval::ApprovalLedger::new())
        // A reload or crash recovery of the webview loses the JavaScript that holds manual sync
        // lease tokens; release them so folders do not stay locked (Automation P0.4). Started =
        // WebView2 ContentLoading: fires for every new document, including F5 / DevTools reload.
        .on_page_load(|webview, payload| {
            use tauri::Manager;
            if payload.event() == tauri::webview::PageLoadEvent::Started
                && let Some(state) = webview.try_state::<automation::commands::AutomationState>()
            {
                automation::commands::release_manual_leases_on_page_load(&state);
            }
        })
        .setup(move |app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            use tauri::Manager;
            if let Some(root) = &test_profile {
                for config in app.config().app.windows.clone() {
                    tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                        .title(format!("{} - Profil uji", config.title))
                        .data_directory(profile::webview_in(root))
                        .build()?;
                }
                eprintln!(
                    "ferrum: test profile active (FERRUM_PROFILE_DIR); real profile and AI key untouched"
                );
            }
            // Bridge the queue's progress sink to Tauri events. Built here
            // (not in .manage with a default) because emitting needs the app handle.
            let handle = app.handle().clone();
            let queue = ops::OperationQueue::new(Box::new(move |p| {
                use tauri::Emitter;
                let _ = handle.emit("operations://progress", p);
            }));
            app.handle().manage(queue);
            // FER-471: live copy-thread state for the progress dialog.
            let control_handle = app.handle().clone();
            copy_pool::set_state_sink(Box::new(move |state| {
                use tauri::Emitter;
                let _ = control_handle.emit("operations://copy-control", state);
            }));
            // Durable AI execution ledger (idempotency across restarts). If the journal cannot be
            // opened the ledger falls back to memory for this run and says so in the log.
            let journal = profile::data_dir(app.handle()).and_then(|dir| {
                std::fs::create_dir_all(&dir)
                    .ok()
                    .map(|()| dir.join("ai-exec.journal"))
            });
            let ledger = match journal.as_deref().map(ai::executor::ExecutionLedger::open) {
                Some(Ok(ledger)) => ledger,
                _ => {
                    log::warn!("AI execution journal unavailable; using an in-memory ledger");
                    ai::executor::ExecutionLedger::in_memory()
                }
            };
            app.handle().manage(ledger);
            // Sync baseline store (FER-414): one file per root pair under the app data dir.
            // If the dir is unusable the store reports "unavailable" honestly (load = first run).
            let baseline_store = match profile::data_dir(app.handle()) {
                Some(dir) => sync_baseline::SyncBaselineStore::open(&dir.join("sync-baselines")),
                None => sync_baseline::SyncBaselineStore::unavailable(),
            };
            app.handle().manage(baseline_store);
            // Activity store lives in OS app-local data; no temp fallback —
            // if it cannot open, recording stays off for this run (spec §7).
            // FERRUM_ACTIVITY_DIR / FERRUM_PROFILE_DIR redirect it for tests only
            // (the Windows Known-Folder API ignores LOCALAPPDATA env overrides).
            use crate::activity_service::{ActivityHandle, ActivityService};
            let activity_dir = profile::activity_dir(app.handle());
            let service = activity_dir.and_then(|dir| ActivityService::open(&dir).ok());
            if service.is_none() {
                log::warn!("activity store unavailable; activity recording disabled");
            }
            app.handle().manage(ActivityHandle(service));
            // Automation store (P0.3): SQLite under the local (non-roaming) data dir. Opening
            // failures are remembered in the state and reported per command; a newer store
            // schema opens read-only (LC-3).
            app.handle().manage(automation::commands::AutomationState::open());
            Ok(())
        })
        .run(context)
        .expect("error while building tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_matches_prototype() {
        assert_eq!(APP_NAME, "Ferrum");
    }

    #[test]
    fn rejects_empty_and_separator_only_paths() {
        assert!(!is_plausible_path(""));
        assert!(!is_plausible_path("   "));
        assert!(!is_plausible_path("/"));
        assert!(!is_plausible_path("\\\\"));
        assert!(!is_plausible_path(" / \\ "));
    }

    #[test]
    fn accepts_real_paths_without_sandboxing() {
        assert!(is_plausible_path("C:\\"));
        assert!(is_plausible_path("/home/arif"));
        assert!(is_plausible_path("C:\\Users\\you\\Downloads"));
        assert!(is_plausible_path("relative/path.txt"));
        assert!(is_plausible_path("\\\\server\\share")); // UNC
        assert!(is_plausible_path("D:/béhind thé scenes")); // non-ASCII
    }
}
