//! Administrative subcommands: `db`, `key`, `audit` and `config`.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum DbCmd {
    /// Print the database path
    Path,
    /// Create the database (encrypted unless --insecure-plaintext) and apply migrations
    Init {
        /// Encrypt the new database (the default; plaintext needs --insecure-plaintext)
        #[arg(long)]
        encrypt: bool,
    },
    /// Encrypt an existing plaintext database in place (verified, then the plaintext is wiped)
    Encrypt,
    /// Replace every key that can open the database with a new one (see also `wherebox key`)
    Rekey {
        /// New key: ssh, file, env (WHEREBOX_NEW_KEY) or passphrase (default: key_source setting)
        #[arg(long, value_name = "SOURCE")]
        to: Option<String>,
        /// Also re-encrypt the data under a fresh data key
        #[arg(long)]
        rotate_dek: bool,
    },
    /// Apply pending migrations (or show status)
    Migrate {
        /// Only report migration status
        #[arg(long)]
        status: bool,
    },
    /// Copy the database to FILE
    Backup { dest: PathBuf },
    /// Rebuild the database file to reclaim space
    Vacuum,
    /// Run integrity and consistency checks
    Check,
    /// Row counts and schema version
    Info,
    /// Merge repeat purchases of each consumable (same ASIN or product ref at a
    /// property) into one item with a lot per purchase; safe to run again
    RegroupConsumables {
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuditCmd {
    /// Show (and verify) the audit log
    Log {
        /// Only the newest N entries
        #[arg(long, short = 'n')]
        limit: Option<usize>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show configuration (file values, or all resolved values with --effective)
    Show {
        /// Show every setting with its resolved value and source layer
        #[arg(long, short)]
        effective: bool,
    },
    /// Set a value in the config file
    Set { key: String, value: String },
    /// Remove a value from the config file
    Unset { key: String },
    /// Print the config file path
    Path,
    /// List available settings
    Keys,
}

#[derive(Debug, Subcommand)]
pub enum KeyCmd {
    /// List the keys that can open the database and how to recover it
    Status,
    /// Add an SSH public key (an OpenSSH line or a path to a .pub file)
    AddSsh { public_key: String },
    /// Add a passphrase (from WHEREBOX_NEW_KEY, or asked twice)
    AddPassphrase,
    /// Add a key file under the config directory
    AddFile,
    /// Remove key SLOT (its number in `wherebox key status`)
    Remove { slot: usize },
}
