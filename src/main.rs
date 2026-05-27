use baidupan_cli::api::{PanClient, UploadRequest};
use baidupan_cli::auth::OAuthClient;
use baidupan_cli::batch::{load_batch_tasks, BatchReport, BatchTask, BatchTaskResult};
use baidupan_cli::cli::{BaidupanCli, Commands};
use baidupan_cli::config::{AppCredentials, TokenStore};
use baidupan_cli::transfer::{TransferPlanner, UploadResumeState, UploadStateStore};
use baidupan_cli::{Error, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = BaidupanCli::parse();
    init_tracing(cli.verbose);

    let token_store = TokenStore::for_current_user()?;

    match cli.command {
        Commands::Login => {
            let credentials = AppCredentials::from_env()?;
            let oauth = OAuthClient::new()?;
            let device_code = oauth.request_device_code(&credentials).await?;

            println!("App key: {}", credentials.masked_app_key());
            println!("Open this URL: {}", device_code.verification_url);
            println!("User code: {}", device_code.user_code);
            println!("QR URL: {}", device_code.qrcode_url);
            println!(
                "Waiting for authorization, polling every {} seconds...",
                device_code.interval.max(5)
            );

            let token = oauth.poll_for_token(&credentials, &device_code).await?;
            token_store.save(&token)?;
            println!("login succeeded");
        }
        Commands::Logout => {
            token_store.remove()?;
            println!("logged out");
        }
        Commands::Whoami => {
            let token = token_store.load()?;
            println!(
                "scope: {}",
                token.scope.unwrap_or_else(|| "unknown".to_string())
            );
            println!("expires_at: {}", token.expires_at);
        }
        Commands::Ls { path } => {
            let credentials = AppCredentials::from_env()?;
            let remote_path = resolve_remote_path(&credentials, &path)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            let entries = client.list_dir(&remote_path).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                for entry in entries {
                    let kind = if entry.isdir == 1 { "dir" } else { "file" };
                    println!(
                        "{kind}\t{}\t{}\t{}",
                        entry.size, entry.path, entry.server_filename
                    );
                }
            }
        }
        Commands::Mkdir { path } => {
            let credentials = AppCredentials::from_env()?;
            let remote_path = resolve_remote_path(&credentials, &path)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.mkdir(&remote_path).await?;
            println!("created {remote_path}");
        }
        Commands::Rm { path } => {
            let credentials = AppCredentials::from_env()?;
            let remote_path = resolve_remote_path(&credentials, &path)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.delete(&remote_path).await?;
            println!("removed {remote_path}");
        }
        Commands::Mv { from, to } => {
            let credentials = AppCredentials::from_env()?;
            let from_path = resolve_remote_path(&credentials, &from)?;
            let to_path = resolve_remote_path(&credentials, &to)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.move_path(&from_path, &to_path).await?;
            println!("moved {from_path} -> {to_path}");
        }
        Commands::Cp { from, to } => {
            let credentials = AppCredentials::from_env()?;
            let from_path = resolve_remote_path(&credentials, &from)?;
            let to_path = resolve_remote_path(&credentials, &to)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.copy_path(&from_path, &to_path).await?;
            println!("copied {from_path} -> {to_path}");
        }
        Commands::Upload {
            local,
            remote,
            encrypt,
            force,
        } => {
            let credentials = AppCredentials::from_env()?;
            let plan = TransferPlanner::new()?;
            let upload_store = UploadStateStore::for_current_user()?;
            let session_remote = resolve_upload_remote_path(&credentials, &local, &remote)?;
            let session_key = upload_store.session_key(&local, &session_remote, encrypt)?;
            let cache_path = encrypt.then(|| upload_store.cache_path(&session_key));
            let prepared =
                plan.prepare_upload_with_cache(&local, encrypt, cache_path.as_deref())?;
            let block_list = plan.block_list(&prepared.materialized)?;
            let resume_state = upload_store.load(&session_key)?;
            let resume_uploadid = resume_state
                .as_ref()
                .filter(|state| {
                    state.is_compatible(
                        &prepared.source,
                        &session_remote,
                        &prepared.materialized,
                        prepared.size,
                        prepared.encrypted,
                        &block_list,
                    )
                })
                .map(|state| state.uploadid.as_str());
            let client = PanClient::new(credentials, token_store.clone())?;
            let progress = (!cli.json).then(|| transfer_progress_bar("upload", prepared.size));
            let callback_progress = progress.clone();
            let callback_store = upload_store.clone();
            let callback_session_key = session_key.clone();
            let callback_source = prepared.source.clone();
            let callback_remote = session_remote.clone();
            let callback_materialized = prepared.materialized.clone();
            let callback_size = prepared.size;
            let callback_encrypted = prepared.encrypted;
            let callback_block_list = block_list.clone();
            let result = client
                .upload_file(
                    UploadRequest {
                        local_path: &prepared.materialized,
                        remote_path: &session_remote,
                        size: prepared.size,
                        block_list: &block_list,
                        encrypted: prepared.encrypted,
                        resume_uploadid,
                        ondup: if force {
                            baidupan_cli::api::ONDUP_OVERWRITE
                        } else {
                            baidupan_cli::api::ONDUP_FAIL
                        },
                    },
                    move |uploadid| {
                        let state = UploadResumeState {
                            session_key: callback_session_key.clone(),
                            source_path: callback_source.clone(),
                            remote_path: callback_remote.clone(),
                            materialized_path: callback_materialized.clone(),
                            size: callback_size,
                            encrypted: callback_encrypted,
                            block_list: callback_block_list.clone(),
                            uploadid: uploadid.to_string(),
                        };
                        if let Err(error) = callback_store.save(&state) {
                            eprintln!("warning: failed to persist upload resume state: {error}");
                        }
                    },
                    move |current, total| {
                        if let Some(progress) = callback_progress.as_ref() {
                            update_transfer_progress(progress, current, total);
                        }
                    },
                )
                .await;

            let result = match result {
                Ok(result) => {
                    upload_store.cleanup_success(
                        &session_key,
                        &prepared.materialized,
                        prepared.encrypted,
                    )?;
                    if let Some(progress) = progress.as_ref() {
                        progress.finish_with_message("upload complete");
                    }
                    result
                }
                Err(error) => {
                    if let Some(progress) = progress.as_ref() {
                        progress.abandon_with_message("upload failed");
                    }
                    return Err(baidupan_cli::Error::Api(format!(
                        "upload {} -> {} failed: {}",
                        local.display(),
                        session_remote,
                        error
                    )));
                }
            };

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("uploaded {} -> {}", local.display(), result.path);
                println!("size: {} bytes, parts: {}", result.size, result.parts);
                if result.encrypted {
                    println!("remote payload is encrypted");
                }
            }
        }
        Commands::Download {
            remote,
            local,
            decrypt,
            force,
        } => {
            let credentials = AppCredentials::from_env()?;
            let remote_path = resolve_remote_path(&credentials, &remote)?;
            let plan = TransferPlanner::new()?;
            let prepared = plan.prepare_download(&remote_path, &local, decrypt, force)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            let progress = (!cli.json).then(|| transfer_progress_bar("download", 0));
            let callback_progress = progress.clone();
            let mut result = match client
                .download_file(
                    &remote_path,
                    &prepared.temp_path,
                    prepared.resume_from,
                    move |current, total| {
                        if let Some(progress) = callback_progress.as_ref() {
                            update_transfer_progress(progress, current, total);
                        }
                    },
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    if let Some(progress) = progress.as_ref() {
                        progress.abandon_with_message("download interrupted; rerun to resume");
                    }
                    return Err(baidupan_cli::Error::Api(format!(
                        "download {} -> {} failed: {}",
                        remote_path,
                        local.display(),
                        error
                    )));
                }
            };

            plan.finalize_download(&prepared, force)?;
            result.local_path = local.clone();
            result.decrypted = decrypt;

            if let Some(progress) = progress.as_ref() {
                progress.finish_with_message("download complete");
            }

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("downloaded {} -> {}", result.remote_path, local.display());
                println!("size: {} bytes", result.size);
                if result.decrypted {
                    println!("local payload has been decrypted");
                }
            }
        }
        Commands::Batch {
            file,
            continue_on_error,
        } => {
            let tasks = load_batch_tasks(&file)?;
            let report = run_batch_tasks(&tasks, &token_store, cli.json, continue_on_error).await?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "batch complete: {} succeeded, {} failed, {} total",
                    report.succeeded, report.failed, report.total
                );
            }

            if report.failed > 0 {
                return Err(baidupan_cli::Error::Api(format!(
                    "batch completed with {} failure(s)",
                    report.failed
                )));
            }
        }
    }

    Ok(())
}

async fn run_batch_tasks(
    tasks: &[BatchTask],
    token_store: &TokenStore,
    json_mode: bool,
    continue_on_error: bool,
) -> Result<BatchReport> {
    let mut results = Vec::with_capacity(tasks.len());

    for (index, task) in tasks.iter().enumerate() {
        let task_result = execute_batch_task(task, token_store, json_mode).await;

        match task_result {
            Ok(output) => {
                if !json_mode {
                    println!(
                        "[{}/{}] ok: {}",
                        index + 1,
                        tasks.len(),
                        describe_batch_task(task)
                    );
                }
                results.push(BatchTaskResult {
                    task: task.clone(),
                    ok: true,
                    output: Some(output),
                    error: None,
                });
            }
            Err(error) => {
                if !json_mode {
                    eprintln!(
                        "[{}/{}] failed: {}: {}",
                        index + 1,
                        tasks.len(),
                        describe_batch_task(task),
                        error
                    );
                }
                results.push(BatchTaskResult {
                    task: task.clone(),
                    ok: false,
                    output: None,
                    error: Some(error.to_string()),
                });
                if !continue_on_error {
                    break;
                }
            }
        }
    }

    let succeeded = results.iter().filter(|result| result.ok).count();
    let failed = results.len().saturating_sub(succeeded);

    Ok(BatchReport {
        total: tasks.len(),
        succeeded,
        failed,
        results,
    })
}

async fn execute_batch_task(
    task: &BatchTask,
    token_store: &TokenStore,
    json_mode: bool,
) -> Result<serde_json::Value> {
    let credentials = AppCredentials::from_env()?;

    match task {
        BatchTask::Mkdir { path } => {
            let remote_path = resolve_remote_path(&credentials, path)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.mkdir(&remote_path).await?;
            Ok(serde_json::json!({"path": remote_path}))
        }
        BatchTask::Rm { path } => {
            let remote_path = resolve_remote_path(&credentials, path)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.delete(&remote_path).await?;
            Ok(serde_json::json!({"path": remote_path}))
        }
        BatchTask::Mv { from, to } => {
            let from_path = resolve_remote_path(&credentials, from)?;
            let to_path = resolve_remote_path(&credentials, to)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.move_path(&from_path, &to_path).await?;
            Ok(serde_json::json!({"from": from_path, "to": to_path}))
        }
        BatchTask::Cp { from, to } => {
            let from_path = resolve_remote_path(&credentials, from)?;
            let to_path = resolve_remote_path(&credentials, to)?;
            let client = PanClient::new(credentials, token_store.clone())?;
            client.copy_path(&from_path, &to_path).await?;
            Ok(serde_json::json!({"from": from_path, "to": to_path}))
        }
        BatchTask::Upload {
            local,
            remote,
            encrypt,
            force,
        } => {
            let summary = run_upload(
                local,
                remote,
                *encrypt,
                *force,
                &credentials,
                token_store,
                json_mode,
            )
            .await?;
            Ok(serde_json::to_value(summary)?)
        }
        BatchTask::Download {
            remote,
            local,
            decrypt,
            force,
        } => {
            let summary = run_download(
                remote,
                local,
                *decrypt,
                *force,
                &credentials,
                token_store,
                json_mode,
            )
            .await?;
            Ok(serde_json::to_value(summary)?)
        }
    }
}

fn describe_batch_task(task: &BatchTask) -> String {
    match task {
        BatchTask::Mkdir { path } => format!("mkdir {}", path),
        BatchTask::Rm { path } => format!("rm {}", path),
        BatchTask::Mv { from, to } => format!("mv {} -> {}", from, to),
        BatchTask::Cp { from, to } => format!("cp {} -> {}", from, to),
        BatchTask::Upload { local, remote, .. } => {
            format!("upload {} -> {}", local.display(), remote)
        }
        BatchTask::Download { remote, local, .. } => {
            format!("download {} -> {}", remote, local.display())
        }
    }
}

async fn run_upload(
    local: &std::path::Path,
    remote: &str,
    encrypt: bool,
    force: bool,
    credentials: &AppCredentials,
    token_store: &TokenStore,
    json_mode: bool,
) -> Result<baidupan_cli::api::UploadSummary> {
    let plan = TransferPlanner::new()?;
    let upload_store = UploadStateStore::for_current_user()?;
    let session_remote = resolve_upload_remote_path(credentials, local, remote)?;
    let session_key = upload_store.session_key(local, &session_remote, encrypt)?;
    let cache_path = encrypt.then(|| upload_store.cache_path(&session_key));
    let prepared = plan.prepare_upload_with_cache(local, encrypt, cache_path.as_deref())?;
    let block_list = plan.block_list(&prepared.materialized)?;
    let resume_state = upload_store.load(&session_key)?;
    let resume_uploadid = resume_state
        .as_ref()
        .filter(|state| {
            state.is_compatible(
                &prepared.source,
                &session_remote,
                &prepared.materialized,
                prepared.size,
                prepared.encrypted,
                &block_list,
            )
        })
        .map(|state| state.uploadid.as_str());
    let client = PanClient::new(credentials.clone(), token_store.clone())?;
    let progress = (!json_mode).then(|| transfer_progress_bar("upload", prepared.size));
    let callback_progress = progress.clone();
    let callback_store = upload_store.clone();
    let callback_session_key = session_key.clone();
    let callback_source = prepared.source.clone();
    let callback_remote = session_remote.clone();
    let callback_materialized = prepared.materialized.clone();
    let callback_size = prepared.size;
    let callback_encrypted = prepared.encrypted;
    let callback_block_list = block_list.clone();
    let result = client
        .upload_file(
            UploadRequest {
                local_path: &prepared.materialized,
                remote_path: &session_remote,
                size: prepared.size,
                block_list: &block_list,
                encrypted: prepared.encrypted,
                resume_uploadid,
                ondup: if force {
                    baidupan_cli::api::ONDUP_OVERWRITE
                } else {
                    baidupan_cli::api::ONDUP_FAIL
                },
            },
            move |uploadid| {
                let state = UploadResumeState {
                    session_key: callback_session_key.clone(),
                    source_path: callback_source.clone(),
                    remote_path: callback_remote.clone(),
                    materialized_path: callback_materialized.clone(),
                    size: callback_size,
                    encrypted: callback_encrypted,
                    block_list: callback_block_list.clone(),
                    uploadid: uploadid.to_string(),
                };
                if let Err(error) = callback_store.save(&state) {
                    eprintln!("warning: failed to persist upload resume state: {error}");
                }
            },
            move |current, total| {
                if let Some(progress) = callback_progress.as_ref() {
                    update_transfer_progress(progress, current, total);
                }
            },
        )
        .await;

    match result {
        Ok(result) => {
            upload_store.cleanup_success(
                &session_key,
                &prepared.materialized,
                prepared.encrypted,
            )?;
            if let Some(progress) = progress.as_ref() {
                progress.finish_with_message("upload complete");
            }
            Ok(result)
        }
        Err(error) => {
            if let Some(progress) = progress.as_ref() {
                progress.abandon_with_message("upload failed");
            }
            Err(baidupan_cli::Error::Api(format!(
                "upload {} -> {} failed: {}",
                local.display(),
                session_remote,
                error
            )))
        }
    }
}

async fn run_download(
    remote: &str,
    local: &std::path::Path,
    decrypt: bool,
    force: bool,
    credentials: &AppCredentials,
    token_store: &TokenStore,
    json_mode: bool,
) -> Result<baidupan_cli::api::DownloadSummary> {
    let remote_path = resolve_remote_path(credentials, remote)?;
    let plan = TransferPlanner::new()?;
    let prepared = plan.prepare_download(&remote_path, local, decrypt, force)?;
    let client = PanClient::new(credentials.clone(), token_store.clone())?;
    let progress = (!json_mode).then(|| transfer_progress_bar("download", 0));
    let callback_progress = progress.clone();
    let mut result = match client
        .download_file(
            &remote_path,
            &prepared.temp_path,
            prepared.resume_from,
            move |current, total| {
                if let Some(progress) = callback_progress.as_ref() {
                    update_transfer_progress(progress, current, total);
                }
            },
        )
        .await
    {
        Ok(result) => result,
        Err(error) => {
            if let Some(progress) = progress.as_ref() {
                progress.abandon_with_message("download interrupted; rerun to resume");
            }
            return Err(baidupan_cli::Error::Api(format!(
                "download {} -> {} failed: {}",
                remote_path,
                local.display(),
                error
            )));
        }
    };

    plan.finalize_download(&prepared, force)?;
    result.local_path = local.to_path_buf();
    result.decrypted = decrypt;

    if let Some(progress) = progress.as_ref() {
        progress.finish_with_message("download complete");
    }

    Ok(result)
}

fn resolve_remote_path(credentials: &AppCredentials, path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidRemotePath(path.to_string()));
    }

    let app_root = credentials.app_root();
    if trimmed == "/" {
        return Ok(app_root);
    }

    if trimmed.starts_with("/apps/") {
        return Err(Error::InvalidRemotePath(format!(
            "{path} (do not include the /apps/<app_name> prefix; commands are scoped to {})",
            credentials.app_root()
        )));
    }

    let relative = trimmed.trim_start_matches('/');
    let explicit_app_prefix = format!("apps/{}", credentials.app_name);
    if relative == explicit_app_prefix || relative.starts_with(&format!("{explicit_app_prefix}/")) {
        return Err(Error::InvalidRemotePath(format!(
            "{path} (do not include the /apps/<app_name> prefix; commands are scoped to {})",
            credentials.app_root()
        )));
    }

    Ok(format!("{}/{}", credentials.app_root(), relative))
}

fn resolve_upload_remote_path(
    credentials: &AppCredentials,
    local: &std::path::Path,
    remote: &str,
) -> Result<String> {
    let trimmed = remote.trim();
    let remote_path = resolve_remote_path(credentials, remote)?;

    if trimmed == "/" || trimmed.ends_with('/') {
        let file_name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::InvalidRemotePath(local.display().to_string()))?;
        return Ok(format!("{}/{file_name}", remote_path.trim_end_matches('/')));
    }

    Ok(remote_path)
}

fn init_tracing(verbose: u8) {
    let default_filter = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .without_time()
        .init();
}

fn transfer_progress_bar(prefix: &'static str, total: u64) -> ProgressBar {
    let progress = ProgressBar::new(total);
    let style = ProgressStyle::with_template(
        "{prefix:>10} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})",
    )
    .expect("valid progress style")
    .progress_chars("##-");

    progress.set_style(style);
    progress.set_prefix(prefix.to_string());
    progress
}

fn update_transfer_progress(progress: &ProgressBar, current: u64, total: u64) {
    if progress.length() != Some(total) {
        progress.set_length(total);
    }
    progress.set_position(current.min(total));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> AppCredentials {
        AppCredentials {
            app_key: "key".to_string(),
            app_secret: "secret".to_string(),
            app_name: "demo-app".to_string(),
        }
    }

    #[test]
    fn resolves_root_to_app_root() {
        assert_eq!(
            resolve_remote_path(&credentials(), "/").expect("root"),
            "/apps/demo-app"
        );
    }

    #[test]
    fn resolves_relative_paths_inside_app_root() {
        assert_eq!(
            resolve_remote_path(&credentials(), "docs/file.txt").expect("relative"),
            "/apps/demo-app/docs/file.txt"
        );
        assert_eq!(
            resolve_remote_path(&credentials(), "/docs/file.txt").expect("leading slash"),
            "/apps/demo-app/docs/file.txt"
        );
    }

    #[test]
    fn rejects_explicit_apps_prefix() {
        let error = resolve_remote_path(&credentials(), "/apps/demo-app/docs/file.txt")
            .expect_err("should reject full path");

        assert!(error
            .to_string()
            .contains("do not include the /apps/<app_name> prefix"));
    }

    #[test]
    fn resolves_upload_root_to_local_file_name() {
        assert_eq!(
            resolve_upload_remote_path(&credentials(), std::path::Path::new("files/test.txt"), "/")
                .expect("upload root"),
            "/apps/demo-app/test.txt"
        );
    }

    #[test]
    fn resolves_upload_directory_to_local_file_name() {
        assert_eq!(
            resolve_upload_remote_path(
                &credentials(),
                std::path::Path::new("files/test.txt"),
                "docs/"
            )
            .expect("upload directory"),
            "/apps/demo-app/docs/test.txt"
        );
    }

    #[test]
    fn resolves_explicit_upload_file_path() {
        assert_eq!(
            resolve_upload_remote_path(
                &credentials(),
                std::path::Path::new("files/test.txt"),
                "docs/renamed.txt"
            )
            .expect("explicit file path"),
            "/apps/demo-app/docs/renamed.txt"
        );
    }
}
