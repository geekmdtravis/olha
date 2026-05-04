mod client;
mod encryption;
mod output;

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::{CompleteEnv, Shell};

#[derive(Parser)]
#[command(name = "olha")]
#[command(about = "Query and manage notifications", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List notifications with optional filters
    List {
        /// Filter by app name
        #[arg(long)]
        app: Option<String>,

        /// Filter by urgency (low, normal, critical)
        #[arg(long)]
        urgency: Option<String>,

        /// Filter by status (unread, read, cleared)
        #[arg(long)]
        status: Option<String>,

        /// Filter by category
        #[arg(long)]
        category: Option<String>,

        /// Search in summary and body text
        #[arg(long)]
        search: Option<String>,

        /// Show notifications since this ISO 8601 timestamp
        #[arg(long)]
        since: Option<String>,

        /// Show notifications until this ISO 8601 timestamp
        #[arg(long)]
        until: Option<String>,

        /// Limit number of results
        #[arg(long, default_value = "50")]
        limit: u32,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Count notifications
    Count {
        /// Filter by app name
        #[arg(long)]
        app: Option<String>,

        /// Filter by urgency (low, normal, critical)
        #[arg(long)]
        urgency: Option<String>,

        /// Filter by status (unread, read, cleared)
        #[arg(long)]
        status: Option<String>,

        /// Filter by category
        #[arg(long)]
        category: Option<String>,

        /// Search in summary and body text
        #[arg(long)]
        search: Option<String>,

        /// Show notifications since this ISO 8601 timestamp
        #[arg(long)]
        since: Option<String>,

        /// Show notifications until this ISO 8601 timestamp
        #[arg(long)]
        until: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show a single notification by ID
    Show {
        /// Notification row ID
        id: u64,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Mark notifications as read
    MarkRead {
        /// Notification IDs (space-separated)
        ids: Vec<u64>,

        /// Mark all as read
        #[arg(long)]
        all: bool,
    },

    /// Clear (dismiss) notifications
    Clear {
        /// Notification IDs (space-separated)
        ids: Vec<u64>,

        /// Clear all
        #[arg(long)]
        all: bool,
    },

    /// Delete notifications permanently
    Delete {
        /// Notification IDs (space-separated)
        ids: Vec<u64>,

        /// Delete all
        #[arg(long)]
        all: bool,
    },

    /// Invoke an action on a notification
    Invoke {
        /// Notification ID
        id: u64,

        /// Action key
        action_key: String,
    },

    /// Subscribe to real-time notification events
    Subscribe {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show daemon status and statistics
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Query or toggle Do Not Disturb. With no action, prints current state.
    ///
    /// While DND is on, incoming notifications are still stored in
    /// history but popups are silenced. Critical urgency notifications
    /// only break through when `dnd.allow_critical = true` is set in
    /// config.toml.
    Dnd {
        /// Action to take. Omit to show current state.
        #[arg(value_enum)]
        action: Option<DndActionArg>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Unlock the daemon: derive the DEK via `pass show` and load
    /// the X25519 secret into daemon memory. Triggers a pinentry
    /// prompt unless gpg-agent has cached the passphrase. Subject
    /// to the idle auto-lock timer.
    Unlock,

    /// Zero the in-memory X25519 secret. Writes keep working (public
    /// key is always loaded); reads of encrypted rows return
    /// placeholders until the next unlock.
    Lock,

    /// Install shell tab completions
    InstallCompletion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,

        /// Custom output path (optional)
        #[arg(short, long, value_hint = ValueHint::FilePath)]
        output: Option<PathBuf>,
    },

    /// Manage at-rest encryption of stored notifications
    #[command(subcommand)]
    Encryption(EncryptionCmd),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DndActionArg {
    Status,
    On,
    Off,
    Toggle,
}

impl From<DndActionArg> for client::DndAction {
    fn from(a: DndActionArg) -> Self {
        match a {
            DndActionArg::Status => client::DndAction::Status,
            DndActionArg::On => client::DndAction::On,
            DndActionArg::Off => client::DndAction::Off,
            DndActionArg::Toggle => client::DndAction::Toggle,
        }
    }
}

#[derive(Subcommand)]
enum EncryptionCmd {
    /// Generate an X25519 keypair and seed a DEK in `pass`. Writes
    /// the wrapped secret + public key to the DB `meta` table so the
    /// daemon can seal writes even while locked.
    Init {
        #[arg(long, default_value = "olha/db-key")]
        pass_entry: String,

        /// Overwrite an existing keypair/pass entry. Rows sealed
        /// under the old key become permanently unreadable.
        #[arg(long)]
        force: bool,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,
    },

    /// Verify the DEK unlocks, wipe existing plaintext rows, and flip
    /// the `[encryption].enabled` flag in config.toml.
    Enable {
        #[arg(long, default_value = "olha/db-key")]
        pass_entry: String,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,

        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Disable encryption in config.toml. Refuses unless all
    /// encrypted rows are explicitly downgraded with
    /// `--rekey-to-plaintext`.
    Disable {
        #[arg(long, default_value = "olha/db-key")]
        pass_entry: String,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,

        #[arg(long, short = 'y')]
        yes: bool,

        /// Decrypt every encrypted row back to plaintext TEXT
        /// columns. Intentional downgrade — required when rows
        /// exist.
        #[arg(long)]
        rekey_to_plaintext: bool,
    },

    /// Report config, pass availability, keypair, and row counts.
    Status {
        #[arg(long, default_value = "")]
        pass_entry: String,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,
    },

    /// Re-wrap the X25519 secret under a new DEK. Fast — touches
    /// only the `meta` table. Rows stay sealed under the same pk,
    /// no row-level re-encryption.
    Rewrap {
        /// Current pass entry. Defaults to the one in config.
        #[arg(long, default_value = "olha/db-key")]
        old_pass_entry: String,

        /// New pass entry. Same as `old_pass_entry` means "regenerate
        /// the existing entry's contents in place". Different means
        /// "switch to a new entry name" (also flips the config).
        #[arg(long, default_value = "olha/db-key")]
        new_pass_entry: String,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,
    },

    /// Generate a new X25519 keypair and re-seal every row under it.
    /// Slow (O(rows)). Stop `olhad` first.
    RotateKey {
        #[arg(long, default_value = "olha/db-key")]
        pass_entry: String,

        #[arg(long, value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,

        #[arg(long, value_hint = ValueHint::FilePath)]
        db: Option<PathBuf>,

        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn main() {
    CompleteEnv::with_factory(Cli::command).complete();

    let runtime = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    runtime.block_on(async {
        if let Err(e) = run().await {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    });
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List {
            app,
            urgency,
            status,
            category,
            search,
            since,
            until,
            limit,
            json,
        } => {
            client::list(client::ListFilter {
                app,
                urgency,
                status,
                category,
                search,
                since,
                until,
                limit: limit as i64,
                json,
            })
            .await?;
        }

        Commands::Count {
            app,
            urgency,
            status,
            category,
            search,
            since,
            until,
            json,
        } => {
            client::count(client::CountFilter {
                app,
                urgency,
                status,
                category,
                search,
                since,
                until,
                json,
            })
            .await?;
        }

        Commands::Show { id, json } => {
            client::show(id, json).await?;
        }

        Commands::MarkRead { ids, all } => {
            client::mark_read(ids, all).await?;
        }

        Commands::Clear { ids, all } => {
            client::clear(ids, all).await?;
        }

        Commands::Delete { ids, all } => {
            client::delete(ids, all).await?;
        }

        Commands::Invoke { id, action_key } => {
            client::invoke(id, action_key).await?;
        }

        Commands::Subscribe { json } => {
            client::subscribe(json).await?;
        }

        Commands::Status { json } => {
            client::status(json).await?;
        }

        Commands::Dnd { action, json } => {
            let act = action
                .map(client::DndAction::from)
                .unwrap_or(client::DndAction::Status);
            client::dnd(act, json).await?;
        }

        Commands::InstallCompletion { shell, output } => {
            cmd_install_completion(shell, output)?;
        }

        Commands::Unlock => client::unlock().await?,
        Commands::Lock => client::lock().await?,

        Commands::Encryption(sub) => match sub {
            EncryptionCmd::Init {
                pass_entry,
                force,
                config,
                db,
            } => {
                encryption::init(&pass_entry, force, config.as_deref(), db.as_deref())?;
            }
            EncryptionCmd::Enable {
                pass_entry,
                config,
                db,
                yes,
            } => {
                encryption::enable(&pass_entry, config.as_deref(), db.as_deref(), yes)?;
            }
            EncryptionCmd::Disable {
                pass_entry,
                config,
                db,
                yes,
                rekey_to_plaintext,
            } => {
                encryption::disable(
                    &pass_entry,
                    config.as_deref(),
                    db.as_deref(),
                    yes,
                    rekey_to_plaintext,
                )
                .await?;
            }
            EncryptionCmd::Status {
                pass_entry,
                config,
                db,
            } => {
                encryption::status(&pass_entry, config.as_deref(), db.as_deref()).await?;
            }
            EncryptionCmd::Rewrap {
                old_pass_entry,
                new_pass_entry,
                config,
                db,
            } => {
                encryption::rewrap(
                    &old_pass_entry,
                    &new_pass_entry,
                    config.as_deref(),
                    db.as_deref(),
                )?;
            }
            EncryptionCmd::RotateKey {
                pass_entry,
                config,
                db,
                yes,
            } => {
                encryption::rotate_key(&pass_entry, config.as_deref(), db.as_deref(), yes).await?;
            }
        },
    }

    Ok(())
}

fn cmd_install_completion(
    shell: Shell,
    output: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let activation = match shell {
        Shell::Bash => "source <(COMPLETE=bash olha)".to_string(),
        Shell::Zsh => "source <(COMPLETE=zsh olha)".to_string(),
        Shell::Fish => "COMPLETE=fish olha | source".to_string(),
        Shell::PowerShell => {
            "$env:COMPLETE = \"powershell\"; olha | Out-String | Invoke-Expression".to_string()
        }
        Shell::Elvish => "eval (COMPLETE=elvish olha | slurp)".to_string(),
        _ => return Err("unsupported shell".into()),
    };

    let zsh_zstyle = "zstyle ':completion:*:*:olha:*:*' sort false";

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let contents = match shell {
            Shell::Fish => format!("{activation}\n"),
            Shell::Zsh => format!("# olha completion\n{activation}\n{zsh_zstyle}\n"),
            _ => format!("# olha completion\n{activation}\n"),
        };

        std::fs::write(&path, contents)?;
        println!("Completion script written to: {}", path.display());
        return Ok(());
    }

    match shell {
        Shell::Zsh => {
            println!("Add the following lines to ~/.zshrc AFTER compinit:");
            println!();
            println!("    {activation}");
            println!("    {zsh_zstyle}");
            println!();
            println!("For example:");
            println!();
            println!("    autoload -U compinit");
            println!("    compinit");
            println!("    {activation}");
            println!("    {zsh_zstyle}");
        }
        Shell::Bash => {
            println!("Add the following line to ~/.bashrc AFTER any bash-completion setup:");
            println!();
            println!("    {activation}");
        }
        Shell::Fish => {
            println!("Add the following line to ~/.config/fish/config.fish:");
            println!();
            println!("    {activation}");
        }
        Shell::PowerShell => {
            println!("Add the following line to your PowerShell profile:");
            println!();
            println!("    {activation}");
        }
        Shell::Elvish => {
            println!("Add the following line to ~/.elvish/rc.elv:");
            println!();
            println!("    {activation}");
        }
        _ => return Err("unsupported shell".into()),
    }

    println!();
    println!("Then restart your shell or source the config file.");

    Ok(())
}
