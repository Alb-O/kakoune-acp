use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(author, version, about = "Agent Client Protocol bridge for Kakoune")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start the background daemon that manages an ACP agent connection.
    Daemon(DaemonOptions),
    /// Send a prompt to the daemon and render the response.
    Prompt(PromptOptions),
    /// Query the daemon for diagnostic information.
    Status(StatusOptions),
    /// Ask the daemon to shut down.
    Shutdown(ShutdownOptions),
}

#[derive(Args, Debug)]
#[command(trailing_var_arg = true)]
pub struct DaemonOptions {
    /// Path to the unix socket used for daemon communication.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Kakoune session identifier. Used to derive default socket paths.
    #[arg(long, env = "kak_session")]
    pub session: Option<String>,
    /// Working directory for the agent session.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Command used to launch the agent process (program followed by args).
    #[arg(required = true)]
    pub agent: Vec<OsString>,
}

#[derive(Args, Debug)]
pub struct PromptOptions {
    /// Path to the unix socket used for daemon communication.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Explicit prompt text. If omitted, stdin is read instead.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Read the prompt from a file on disk.
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    /// Additional snippets of context that should be appended to the prompt.
    #[arg(long)]
    pub context: Vec<String>,
    /// Kakoune session to send responses back to.
    #[arg(long, env = "kak_session")]
    pub session: Option<String>,
    /// Kakoune client to target when emitting commands.
    #[arg(long, env = "kak_client")]
    pub client: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = PromptOutput::Plain)]
    pub output: PromptOutput,
    /// Optional title used when rendering Kakoune commands.
    #[arg(long, default_value = "Agent Response")]
    pub title: String,
    /// Emit Kakoune commands directly instead of printing plain text.
    #[arg(long)]
    pub send_to_kak: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum PromptOutput {
    Plain,
    Json,
    KakCommands,
}

#[derive(Args, Debug)]
pub struct StatusOptions {
    /// Path to the unix socket used for daemon communication.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Kakoune session identifier. Used to derive default socket paths.
    #[arg(long, env = "kak_session")]
    pub session: Option<String>,
    /// Render the status response as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ShutdownOptions {
    /// Path to the unix socket used for daemon communication.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Kakoune session identifier. Used to derive default socket paths.
    #[arg(long, env = "kak_session")]
    pub session: Option<String>,
}
