use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "baidupan")]
#[command(about = "Baidu Netdisk terminal client", version)]
pub struct BaidupanCli {
    #[arg(
        long,
        global = true,
        help = "Emit machine-readable JSON where supported"
    )]
    pub json: bool,

    #[arg(short, long, global = true, action = clap::ArgAction::Count, help = "Increase log verbosity")]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(about = "Authorize this CLI with OAuth device-code login")]
    Login,

    #[command(about = "Remove the locally stored OAuth token")]
    Logout,

    #[command(about = "Show current token scope and expiry")]
    Whoami,

    #[command(about = "List a remote directory")]
    Ls {
        #[arg(default_value = "/")]
        path: String,
    },

    #[command(about = "Create a remote directory")]
    Mkdir { path: String },

    #[command(about = "Remove a remote file or directory")]
    Rm { path: String },

    #[command(about = "Move or rename a remote file or directory")]
    Mv { from: String, to: String },

    #[command(about = "Copy a remote file or directory")]
    Cp { from: String, to: String },

    #[command(about = "Upload a local file")]
    Upload {
        local: PathBuf,
        remote: String,

        #[arg(long, help = "Encrypt locally before uploading")]
        encrypt: bool,

        #[arg(long, short, help = "Overwrite the remote file if it exists")]
        force: bool,
    },

    #[command(about = "Download a remote file")]
    Download {
        remote: String,
        local: PathBuf,

        #[arg(long, help = "Decrypt locally after downloading")]
        decrypt: bool,

        #[arg(long, short, help = "Overwrite the local destination if it exists")]
        force: bool,
    },

    #[command(about = "Run a batch of tasks from a JSON manifest")]
    Batch {
        file: PathBuf,

        #[arg(long, help = "Keep running remaining tasks after an error")]
        continue_on_error: bool,
    },
}
