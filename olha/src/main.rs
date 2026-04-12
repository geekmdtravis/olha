mod client;
mod output;

use clap::{Parser, Subcommand};

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

        Commands::Count { app, urgency, status, category, search, since, until, json } => {
            client::count(client::CountFilter {
                app,
                urgency,
                status,
                category,
                search,
                since,
                until,
                json,
            }).await?;
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
    }

    Ok(())
}
