use crate::client::Client;
use crate::commands::{auth, read};
use crate::config::Config;
use crate::error::{AppError, Result};
use crate::output::Output;
use clap::{Parser, Subcommand};

const LLM_GUIDE: &str = include_str!("llm.md");

#[derive(Parser)]
#[command(
    name = "ntn",
    version,
    about = "Read-only, agent-friendly Notion CLI",
    after_help = "Output is JSON when stdout is not a terminal. Run `ntn llm` for an agent guide."
)]
struct Cli {
    /// Account profile, user id, name or email
    #[arg(long, short = 'a', global = true, env = "NTN_ACCOUNT")]
    account: Option<String>,
    /// Workspace (space) id; defaults to the one captured at login
    #[arg(long, global = true, env = "NTN_SPACE")]
    space: Option<String>,
    /// Force JSON output
    #[arg(long, global = true, conflicts_with = "text")]
    json: bool,
    /// Force human text output
    #[arg(long, global = true)]
    text: bool,
    /// Keep only these comma-separated fields (dot paths) of each listed item
    #[arg(long, global = true)]
    fields: Option<String>,
    /// Log API calls to stderr
    #[arg(long, short = 'v', global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage stored browser sessions
    Auth {
        #[command(subcommand)]
        cmd: auth::AuthCmd,
    },
    /// Search pages and databases in a workspace
    Search(read::SearchArgs),
    /// Read pages
    Page {
        #[command(subcommand)]
        cmd: read::PageCmd,
    },
    /// Query databases
    Db {
        #[command(subcommand)]
        cmd: read::DbCmd,
    },
    /// List the workspaces your account belongs to
    Spaces,
    /// Print a usage guide written for LLM agents
    Llm,
}

pub struct Ctx {
    pub out: Output,
    pub account: Option<String>,
    pub space: Option<String>,
    pub verbose: bool,
}

impl Ctx {
    pub fn client(&self) -> Result<Client> {
        let (_, acct) = Config::load()?.select(self.account.as_deref())?;
        let space = self.space.clone().or(acct.space_id);
        Client::new(acct.session, space, self.verbose)
    }
}

pub fn run() -> i32 {
    if std::env::args().nth(1).as_deref() == Some("--llm") {
        print!("{LLM_GUIDE}");
        return 0;
    }
    let cli = Cli::parse();
    let ctx = Ctx {
        out: Output::new(cli.json, cli.text, cli.fields),
        account: cli.account,
        space: cli.space,
        verbose: cli.verbose,
    };
    let result = match cli.command {
        Command::Auth { cmd } => auth::run(&ctx, cmd),
        Command::Search(args) => read::search(&ctx, args),
        Command::Page { cmd } => read::page(&ctx, cmd),
        Command::Db { cmd } => read::db(&ctx, cmd),
        Command::Spaces => read::spaces(&ctx),
        Command::Llm => {
            print!("{LLM_GUIDE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => 0,
        Err(err) => {
            ctx.out.error(&err);
            err.exit_code()
        }
    }
}

pub fn read_limit(limit: usize, max: usize) -> Result<usize> {
    if limit == 0 || limit > max {
        return Err(AppError::input(format!(
            "--limit must be between 1 and {max}"
        )));
    }
    Ok(limit)
}
