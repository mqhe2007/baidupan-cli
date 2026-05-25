use baidupan_cli::api::{PanClient, UploadRequest};
use baidupan_cli::auth::OAuthClient;
use baidupan_cli::cli::{BaidupanCli, Commands};
use baidupan_cli::config::{AppCredentials, TokenStore};
use baidupan_cli::transfer::{TransferPlanner, UploadResumeState, UploadStateStore};
use baidupan_cli::Result;
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
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            let entries = client.list_dir(&path).await?;

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
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            client.mkdir(&path).await?;
            println!("created {path}");
        }
        Commands::Rm { path } => {
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            client.delete(&path).await?;
            println!("removed {path}");
        }
        Commands::Mv { from, to } => {
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            client.move_path(&from, &to).await?;
            println!("moved {from} -> {to}");
        }
        Commands::Cp { from, to } => {
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            client.copy_path(&from, &to).await?;
            println!("copied {from} -> {to}");
        }
        Commands::Upload {
            local,
            remote,
            encrypt,
        } => {
            let plan = TransferPlanner::new()?;
            let upload_store = UploadStateStore::for_current_user()?;
            let session_remote = normalize_remote_for_session(&remote);
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
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
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
                        remote,
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
            let plan = TransferPlanner::new()?;
            let prepared = plan.prepare_download(&remote, &local, decrypt, force)?;
            let client = PanClient::new(AppCredentials::from_env()?, token_store.clone())?;
            let progress = (!cli.json).then(|| transfer_progress_bar("download", 0));
            let callback_progress = progress.clone();
            let mut result = match client
                .download_file(
                    &remote,
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
                        remote,
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
    }

    Ok(())
}

fn normalize_remote_for_session(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }

    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
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
