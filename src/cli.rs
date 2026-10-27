//! Command-line interface definition (clap derive).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::output::Format;

pub use crate::cli_admin::{AuditCmd, ConfigCmd, DbCmd, KeyCmd};
pub use crate::cli_lists::{CohortCmd, EntryFlags, ListCmd, MatchArgs, MatcherFlags};

#[derive(Debug, Parser)]
#[command(
    name = "wherebox",
    version,
    about = "Personal inventory: what you own, where it is, what it is worth",
    long_about = "Personal inventory: what you own, where it is, who owns it, what it is worth, \
        and the paperwork behind it.\n\n\
        Data lives in a local, encrypted FrankenSQLite (fsqlite) database, one per property \
        (--profile). Import and export speak Homebox CSV. Every command can emit table, json, \
        jsonl, csv or tsv output; JSON output (--json) uses the versioned `wherebox/v1` \
        envelope documented in docs/json-schema.md.",
    after_help = "Exit codes: 0 ok, 1 error, 2 usage, 3 not found, 4 invalid data, 5 database, 6 io, 7 config, 8 key (database locked: missing or wrong encryption key)."
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Args, Default)]
pub struct GlobalOpts {
    /// Config file (default: wherebox-cli/config.toml under $XDG_CONFIG_HOME, ~/.config,
    /// or %APPDATA% on Windows; env WHEREBOX_CONFIG)
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Database file (env WHEREBOX_DB)
    #[arg(long, global = true, value_name = "FILE")]
    pub db: Option<PathBuf>,
    /// Property profile, a separate database per property (env WHEREBOX_PROFILE; see `profile ls`)
    #[arg(short = 'P', long, global = true, value_name = "NAME")]
    pub profile: Option<String>,
    /// Output format (for `import`: the input format)
    #[arg(short = 'f', long, global = true, value_enum, ignore_case = true)]
    pub format: Option<Format>,
    /// JSON output (`wherebox/v1` envelope); same as --format json, but `import` keeps --format for its input
    #[arg(long, global = true)]
    pub json: bool,
    /// Write output to FILE instead of stdout
    #[arg(short = 'o', long, global = true, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Decimal places for text output (table/csv/tsv/markdown/html/org)
    #[arg(long, global = true, value_name = "N")]
    pub precision: Option<String>,
    /// strftime date format for text output (table/csv/tsv/markdown/html/org) (also accepted on import)
    #[arg(long, global = true, value_name = "FMT")]
    pub date_format: Option<String>,
    /// Time zone: local, UTC, IANA name or +HH:MM
    #[arg(long = "tz", global = true, value_name = "TZ")]
    pub timezone: Option<String>,
    /// Colour: auto, always, never
    #[arg(long, global = true, value_name = "WHEN")]
    pub color: Option<String>,
    /// CSV delimiter character (or 'tab')
    #[arg(long, global = true, value_name = "CHAR")]
    pub delimiter: Option<String>,
    /// CSV quote character
    #[arg(long, global = true, value_name = "CHAR")]
    pub quote: Option<String>,
    /// Omit the header row in csv/tsv output
    #[arg(long, global = true)]
    pub no_header: bool,
    /// Text for missing values in text output (table/csv/tsv/markdown/html/org)
    #[arg(long, global = true, value_name = "TEXT")]
    pub null: Option<String>,
    /// Comma-separated list of columns to output (text formats)
    #[arg(long, global = true, value_name = "COLS", value_delimiter = ',')]
    pub columns: Option<Vec<String>>,
    /// Suppress informational messages
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Print extra diagnostics to stderr
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Allow an UNENCRYPTED database (create one, or open a legacy plaintext one)
    #[arg(long, global = true)]
    pub insecure_plaintext: bool,
    /// Encrypt --output (or stdout) as an ASCII-armored age file
    #[arg(long, global = true)]
    pub encrypt_output: bool,
    /// age X25519 recipient (age1...) for --encrypt-output (repeatable; default: passphrase)
    #[arg(long = "recipient", global = true, value_name = "AGE_PUBKEY")]
    pub recipients: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add an item
    Add(AddArgs),
    /// Change an item's fields, labels or custom fields
    Edit(EditArgs),
    /// Move items to another location
    Mv(MvArgs),
    /// Mark an item gone (disposed, sold, gifted, lost); --purge deletes it
    Rm(RmArgs),
    /// Everything about one item: fields, labels, lots, attachments, history
    Show {
        /// Item: id, asset id, import ref or unique name
        item: String,
    },
    /// List items
    Ls(LsArgs),
    /// Search name, description, serial, model and notes
    Find(FindArgs),
    /// Location tree: properties, rooms, containers, shelves
    #[command(subcommand, alias = "location")]
    Loc(LocCmd),
    /// Labels
    #[command(subcommand)]
    Label(LabelCmd),
    /// Attach a file (photo, receipt, manual, warranty) to an item
    Attach(AttachArgs),
    /// List, extract or remove attachments
    #[command(subcommand)]
    Attachment(AttachmentCmd),
    /// An item's history (created, moved, edited, status changes, valuations)
    History { item: String },
    /// Record a replacement value
    Value(ValueArgs),
    /// Lots of a consumable (each purchase, with its own dates)
    #[command(subcommand)]
    Lot(LotCmd),
    /// Log use of a consumable, oldest lot first
    Use(ConsumeArgs),
    /// Log eating food, oldest lot first
    Eat(ConsumeArgs),
    /// Throw out (or with --spoiled, log spoilage of) a consumable, oldest lot first
    Toss(TossArgs),
    /// Perishable lots expiring soon (or already expired; computed expiry), freezer included
    Expiring(ExpiringArgs),
    /// Kind presets: what a kind of item tracks, its defaults and alerts
    #[command(subcommand)]
    Preset(PresetCmd),
    /// Shelf-life types: how long perishables keep in the pantry, fridge and freezer
    #[command(subcommand, name = "shelf-life")]
    ShelfLife(ShelfLifeCmd),
    /// Set (or with `none`, clear) how many of an item to keep on hand
    Par(ParArgs),
    /// Shopping list: below par, expiring par items, and learned reorders
    Shopping(ShoppingArgs),
    /// Lists (recipes, kits, packing lists, wishlists) checked against a property
    #[command(subcommand)]
    List(ListCmd),
    /// Cohorts: named, reusable matchers (e.g. "nuts" = walnuts, pecans, label nuts)
    #[command(subcommand)]
    Cohort(CohortCmd),
    /// Run the matching ladder over a profile without a list
    Match(MatchArgs),
    /// Reports: insurance inventory, warranties
    #[command(subcommand)]
    Report(ReportCmd),
    /// Import items from Homebox CSV, wherebox CSV/JSON/JSONL or a spreadsheet
    Import(ImportArgs),
    /// Export items (Homebox CSV by default; also csv, json, jsonl)
    Export(ExportArgs),
    /// Property profiles (one database per property)
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// Database maintenance
    #[command(subcommand)]
    Db(DbCmd),
    /// Configuration
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Encrypted audit trail of commands that read or modify data
    #[command(subcommand)]
    Audit(AuditCmd),
    /// Which keys can decrypt the database; add or remove them
    #[command(subcommand)]
    Key(KeyCmd),
    /// Check encryption, keys, file permissions and the audit log
    Doctor,
    /// Generate shell completions
    Completions(CompletionsArgs),
    /// Generate man page(s)
    Man(ManArgs),
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Item fields shared by `add` and `edit`. Money is in dollars (`1299.99`,
/// `$1,299`); dates are YYYY-MM-DD (or `today`).
#[derive(Debug, Args, Clone, Default)]
pub struct ItemFlags {
    /// Quantity (whole number)
    #[arg(long)]
    pub qty: Option<String>,
    /// Purchase price
    #[arg(long)]
    pub price: Option<String>,
    /// Purchase date
    #[arg(long)]
    pub bought: Option<String>,
    /// Where it was bought
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long)]
    pub serial: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Manufacturer
    #[arg(long)]
    pub maker: Option<String>,
    /// Owner entity (a person, trust or LLC)
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long, alias = "desc")]
    pub description: Option<String>,
    /// Notes (up to 1000 characters)
    #[arg(long)]
    pub notes: Option<String>,
    /// Insured
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    pub insured: Option<String>,
    /// Lifetime warranty
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    pub lifetime_warranty: Option<String>,
    /// Warranty end date
    #[arg(long)]
    pub warranty_expires: Option<String>,
    #[arg(long)]
    pub warranty_details: Option<String>,
    /// Replacement value
    #[arg(long)]
    pub value: Option<String>,
    /// Date of the replacement value (default: today when --value is given)
    #[arg(long)]
    pub valued_at: Option<String>,
    /// owned, lent, stored, disposed, sold, lost, gifted
    #[arg(long)]
    pub status: Option<String>,
    /// Asset id (000-000; assigned automatically on add)
    #[arg(long)]
    pub asset_id: Option<String>,
    /// Stable import ref (default: the item id)
    #[arg(long)]
    pub import_ref: Option<String>,
    /// The item this one is stored inside (a toolbox, a safe)
    #[arg(long, value_name = "ITEM")]
    pub inside: Option<String>,
    /// Kind preset name (e.g. meat.raw, appliance)
    #[arg(long)]
    pub preset: Option<String>,
    /// Unit of quantities: ea, g, kg, ml, l, lb, oz, pack
    #[arg(long)]
    pub unit: Option<String>,
    /// Keep at least this many on hand
    #[arg(long)]
    pub par: Option<String>,
    #[arg(long)]
    pub reorder_qty: Option<String>,
    /// Preferred vendor
    #[arg(long)]
    pub vendor: Option<String>,
    /// UPC / ASIN
    #[arg(long)]
    pub product_ref: Option<String>,
    /// Shelf-life type (see `wherebox shelf-life ls`), or `none` for never perishable
    /// (default: classified by name)
    #[arg(long, value_name = "TYPE")]
    pub shelf_life: Option<String>,
    /// Custom field NAME=VALUE (repeatable; an empty VALUE removes it)
    #[arg(long = "field", value_name = "NAME=VALUE")]
    pub fields: Vec<String>,
    /// Where it came from: SOURCE or SOURCE:REF (e.g. amazon-email:113-4412)
    #[arg(long = "source", value_name = "SOURCE[:REF]")]
    pub sources: Vec<String>,
}

/// Dates of a lot (`add` with any of these creates a first lot). There is
/// no expiry date: it is computed from the item's shelf-life type.
#[derive(Debug, Args, Clone, Default)]
pub struct LotFlags {
    /// Printed best-by date (perishables only; overrides the computed expiry; empty clears)
    #[arg(long)]
    pub best_by: Option<String>,
    /// Date opened (marks the lot open)
    #[arg(long)]
    pub opened: Option<String>,
    /// Date frozen
    #[arg(long)]
    pub frozen: Option<String>,
    /// Date acquired (default: today)
    #[arg(long)]
    pub acquired: Option<String>,
}

impl LotFlags {
    pub fn any(&self) -> bool {
        self.best_by.is_some() || self.opened.is_some() || self.frozen.is_some() || self.acquired.is_some()
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Item name
    pub name: String,
    /// Location path, e.g. "Lake House / Garage" (created when missing)
    #[arg(long, short = 'a')]
    pub at: Option<String>,
    /// Label (repeatable or comma-separated)
    #[arg(long = "label", short = 'l', value_delimiter = ',')]
    pub labels: Vec<String>,
    #[command(flatten)]
    pub item: ItemFlags,
    /// Track the quantity as a lot (implied by any lot date)
    #[arg(long)]
    pub lot: bool,
    /// Lot quantity when it differs from --qty (may be fractional: 0.5 kg)
    #[arg(long, value_name = "QTY")]
    pub lot_qty: Option<String>,
    #[command(flatten)]
    pub dates: LotFlags,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// Item: id, asset id, import ref or unique name
    pub item: String,
    /// New name
    #[arg(long)]
    pub name: Option<String>,
    /// Move to this location (created when missing)
    #[arg(long, short = 'a')]
    pub at: Option<String>,
    /// Add label (repeatable or comma-separated)
    #[arg(long = "label", short = 'l', value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Remove label
    #[arg(long = "unlabel", value_delimiter = ',')]
    pub unlabels: Vec<String>,
    /// Clear a field (repeatable), e.g. --clear serial_number
    #[arg(long = "clear", value_name = "FIELD")]
    pub clear: Vec<String>,
    /// No longer inside another item
    #[arg(long)]
    pub not_inside: bool,
    #[command(flatten)]
    pub item_flags: ItemFlags,
}

#[derive(Debug, Args)]
pub struct MvArgs {
    /// Items to move
    #[arg(required = true)]
    pub items: Vec<String>,
    /// Destination location (created when missing)
    #[arg(long, short = 't')]
    pub to: String,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    pub item: String,
    /// What happened: disposed, sold, gifted, lost
    #[arg(long = "as", value_name = "STATUS", default_value = "disposed")]
    pub status: String,
    /// Buyer or recipient (sold/gifted)
    #[arg(long)]
    pub to: Option<String>,
    /// Sale price
    #[arg(long)]
    pub price: Option<String>,
    /// Date (default: today)
    #[arg(long)]
    pub date: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
    /// Delete the item and everything attached to it for good
    #[arg(long)]
    pub purge: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ItemSort {
    Location,
    Name,
    Value,
    Added,
    Asset,
}

#[derive(Debug, Args, Clone, Default)]
pub struct ItemFilter {
    /// Only items at (or below) this location
    #[arg(long, short = 'a')]
    pub at: Option<String>,
    /// Only items with this label (repeatable: all must match)
    #[arg(long = "label", short = 'l', value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Only items of this owner entity
    #[arg(long)]
    pub owner: Option<String>,
    /// Only these statuses (default: everything not disposed/sold/gifted/lost)
    #[arg(long, value_delimiter = ',')]
    pub status: Vec<String>,
    /// Include gone items (disposed, sold, gifted, lost)
    #[arg(long)]
    pub all: bool,
    /// Only items whose warranty ends before this date
    #[arg(long, value_name = "DATE")]
    pub warranty_before: Option<String>,
    /// Only items inside this item
    #[arg(long, value_name = "ITEM")]
    pub inside: Option<String>,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    #[command(flatten)]
    pub filter: ItemFilter,
    #[arg(long, value_enum, default_value = "location")]
    pub sort: ItemSort,
    /// Reverse the sort order
    #[arg(long, short = 'r')]
    pub reverse: bool,
    #[arg(long, short = 'n')]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct FindArgs {
    /// Text to look for (case-insensitive)
    pub text: String,
    #[command(flatten)]
    pub filter: ItemFilter,
}

#[derive(Debug, Args)]
pub struct ValueArgs {
    pub item: String,
    /// Replacement value, e.g. 1299.99
    pub amount: String,
    /// Valuation date (default: today)
    #[arg(long = "on", alias = "date", value_name = "DATE")]
    pub on: Option<String>,
    /// Who or what set the value (appraiser, eBay comps, ...)
    #[arg(long)]
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Locations and labels
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum LocCmd {
    /// Add a location by path ("Lake House / Garage / Shelf 2"); parents are created as needed
    Add {
        path: String,
        /// property, room, container or shelf (default by depth)
        #[arg(long)]
        kind: Option<String>,
        /// Street address (properties)
        #[arg(long)]
        address: Option<String>,
    },
    /// The location tree with item counts and value
    #[command(alias = "list")]
    Ls {
        /// Only this subtree
        path: Option<String>,
        /// Flat list of paths instead of an indented tree
        #[arg(long)]
        flat: bool,
    },
    /// Move a location under another (or `/` for the top level), optionally renaming it
    Mv {
        path: String,
        /// New parent location
        #[arg(long, short = 't')]
        to: Option<String>,
        #[arg(long)]
        rename: Option<String>,
    },
    /// Change a location's kind, address or notes
    Edit {
        path: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },
    /// Remove an empty location (no items, lots or sub-locations)
    Rm { path: String },
}

#[derive(Debug, Subcommand)]
pub enum LabelCmd {
    /// Create a label
    Add {
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Labels with item counts
    #[command(alias = "list")]
    Ls,
    /// Delete a label (items keep everything else)
    Rm { name: String },
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct AttachArgs {
    pub item: String,
    pub file: PathBuf,
    /// photo, receipt, manual, warranty, other (default: from the file type)
    #[arg(long)]
    pub kind: Option<String>,
    /// The item's main attachment of this kind (e.g. the photo shown first)
    #[arg(long)]
    pub primary: bool,
}

#[derive(Debug, Subcommand)]
pub enum AttachmentCmd {
    /// List attachments (of one item, or all)
    #[command(alias = "list")]
    Ls { item: Option<String> },
    /// Write an attachment (decrypted) to DEST
    Get { id: i64, dest: PathBuf },
    /// Remove an attachment (the file too, unless another item shares it)
    Rm { id: i64 },
}

// ---------------------------------------------------------------------------
// Lots and consumption
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum LotCmd {
    /// Add a lot (a purchase) to an item
    Add {
        item: String,
        /// Quantity in the item's unit (may be fractional)
        qty: String,
        /// Where this lot is (fridge, freezer, pantry shelf; default: the item's location)
        #[arg(long, short = 'a')]
        at: Option<String>,
        #[command(flatten)]
        dates: LotFlags,
    },
    /// Lots (of one item, or all); consumed and discarded lots with --all
    #[command(alias = "list")]
    Ls {
        item: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Change a lot's dates, location, quantity or status
    Edit {
        lot: String,
        #[arg(long)]
        qty: Option<String>,
        #[arg(long, short = 'a')]
        at: Option<String>,
        /// sealed, open, consumed, discarded
        #[arg(long)]
        status: Option<String>,
        #[command(flatten)]
        dates: LotFlags,
    },
    /// Delete a lot and its consumption log
    Rm { lot: String },
    /// Assert what is on hand today: restarts the "presumed used up" clock
    /// of every active lot; with --qty the oldest lots are drawn down so N remains
    Confirm {
        item: String,
        /// How much is on hand now (default: every active lot, as logged)
        #[arg(long)]
        qty: Option<String>,
        /// Only this lot
        #[arg(long)]
        lot: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ConsumeArgs {
    pub item: String,
    /// How much (default: 1)
    pub qty: Option<String>,
    /// Take from this lot instead of the oldest
    #[arg(long)]
    pub lot: Option<String>,
    /// When (default: now)
    #[arg(long = "on", value_name = "DATE")]
    pub on: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Debug, Args)]
pub struct TossArgs {
    pub item: String,
    /// How much (default: the whole oldest lot)
    pub qty: Option<String>,
    #[arg(long)]
    pub lot: Option<String>,
    /// It went bad (reason `spoiled` instead of `discarded`)
    #[arg(long)]
    pub spoiled: bool,
    #[arg(long = "on", value_name = "DATE")]
    pub on: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Debug, Args)]
pub struct ExpiringArgs {
    /// Look-ahead, e.g. 3d, 2w (default: the `expiring_within` setting, 7d)
    #[arg(long, value_name = "DURATION")]
    pub within: Option<String>,
    /// Only lots at (or below) this location
    #[arg(long, short = 'a')]
    pub at: Option<String>,
}

// ---------------------------------------------------------------------------
// Presets, par levels, shopping and reports
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum PresetCmd {
    /// Every preset (built-in and from <config dir>/presets/kinds)
    #[command(alias = "list")]
    Ls,
    /// What a preset tracks, its defaults, alerts and sources
    Show { name: String },
}

#[derive(Debug, Subcommand)]
pub enum ShelfLifeCmd {
    /// Every shelf-life type (built-in and from config) with its figures
    #[command(alias = "list")]
    Ls,
    /// One type: figures, synonyms, source and origin
    Show {
        #[arg(value_name = "TYPE")]
        name: String,
    },
    /// Override a type's figures (or add a new type) in config.toml
    Set {
        #[arg(value_name = "TYPE")]
        name: String,
        /// Days in the pantry (5d, 2w, 6m, 1y; `none` = not recommended)
        #[arg(long, value_name = "DURATION")]
        pantry: Option<String>,
        #[arg(long, value_name = "DURATION")]
        fridge: Option<String>,
        #[arg(long, value_name = "DURATION")]
        freezer: Option<String>,
        /// Once opened
        #[arg(long, value_name = "DURATION")]
        opened: Option<String>,
        /// Another name to classify items by (repeatable)
        #[arg(long = "synonym", value_name = "NAME")]
        synonyms: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        /// fruit, vegetable, dairy, eggs, meat, poultry, seafood, bakery, prepared, opened, other
        #[arg(long)]
        group: Option<String>,
    },
    /// Remove a type's config table (back to the built-in figures)
    Reset {
        #[arg(value_name = "TYPE")]
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct ParArgs {
    pub item: String,
    /// Keep at least this many on hand (`none` or `0` clears it)
    pub level: String,
    /// How many to buy when restocking (default: up to par)
    #[arg(long, value_name = "QTY")]
    pub reorder: Option<String>,
}

/// Every property's database instead of just the one in use.
#[derive(Debug, Args, Clone, Default)]
pub struct ProfileScope {
    /// Every property profile with a database (see `profile ls`)
    #[arg(long)]
    pub all_profiles: bool,
}

#[derive(Debug, Args)]
pub struct ShoppingArgs {
    #[command(flatten)]
    pub scope: ProfileScope,
    /// Par items with lots due within this window (default: the `expiring_within` setting, 7d)
    #[arg(long, value_name = "DURATION")]
    pub within: Option<String>,
    /// Print a Markdown checklist (`- [ ] ...`) instead of a table
    #[arg(long)]
    pub checklist: bool,
    /// Recurring items last bought longer ago than this are dropped, not due
    /// (months, or a span such as 2y)
    #[arg(long, value_name = "MONTHS", default_value = "18")]
    pub since: String,
    /// Also what these lists still need (`list shop` rows; repeatable)
    #[arg(long = "include-list", value_name = "LIST")]
    pub include_lists: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum ReportCmd {
    /// Insurance inventory: every owned durable with values, paperwork, totals and gaps.
    /// Formats: table, csv, json, html, xlsx, pdf (xlsx and pdf need --output)
    Insurance(InsuranceArgs),
    /// Warranties ending soon
    Warranties(WarrantyArgs),
}

#[derive(Debug, Args)]
pub struct InsuranceArgs {
    #[command(flatten)]
    pub scope: ProfileScope,
    /// Only items of this owner entity
    #[arg(long)]
    pub owner: Option<String>,
    /// Only lines worth at least this much (unit value x quantity), e.g. 250
    #[arg(long, value_name = "AMOUNT")]
    pub min_value: Option<String>,
    /// Only items bought on or after this date
    #[arg(long, value_name = "DATE")]
    pub bought_after: Option<String>,
}

#[derive(Debug, Args)]
pub struct WarrantyArgs {
    #[command(flatten)]
    pub scope: ProfileScope,
    /// Window ahead, e.g. 30d, 90d, 6m
    #[arg(long, value_name = "DURATION", default_value = "90d")]
    pub expiring: String,
    /// Also list warranties that already ended
    #[arg(long)]
    pub include_expired: bool,
    #[arg(long)]
    pub owner: Option<String>,
}

// ---------------------------------------------------------------------------
// Import / export
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    /// Homebox import/export CSV (HB.* columns)
    Homebox,
    Csv,
    Tsv,
    Json,
    Jsonl,
    /// xlsx, xlsm, xlsb, xls or ods (detected from the extension)
    Spreadsheet,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Input file (`-` for stdin)
    pub file: PathBuf,
    /// Input format (default: from --format, else the file's extension and header)
    #[arg(long, value_enum)]
    pub input_format: Option<InputFormat>,
    /// Column mapping FIELD=COLUMN for generic CSV/spreadsheets (fields: see `wherebox import --help`)
    #[arg(long = "map", value_name = "FIELD=COLUMN", value_delimiter = ',')]
    pub maps: Vec<String>,
    /// TOML mapping file: [columns] FIELD = "Column", [defaults] FIELD = "value"
    #[arg(long, value_name = "FILE")]
    pub mapping: Option<PathBuf>,
    /// Location for rows without one
    #[arg(long, short = 'a')]
    pub at: Option<String>,
    /// Label added to every imported item
    #[arg(long = "label", short = 'l', value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Extra date format for parsing (strftime)
    #[arg(long, value_name = "FMT")]
    pub input_date_format: Option<String>,
    /// Validate and report what would change without writing
    #[arg(long, short = 'n')]
    pub dry_run: bool,
    /// Skip invalid rows instead of aborting the import
    #[arg(long)]
    pub skip_invalid: bool,
    /// CSV input delimiter (default: config csv_delimiter, or tab for .tsv)
    #[arg(long, value_name = "CHAR")]
    pub input_delimiter: Option<String>,
    /// Spreadsheet: sheet to read (default: the first sheet)
    #[arg(long, value_name = "NAME")]
    pub sheet: Option<String>,
    /// Spreadsheet: 1-based header row (default: the first non-empty row)
    #[arg(long, value_name = "N")]
    pub header_row: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[command(flatten)]
    pub filter: ItemFilter,
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// Known properties and their databases
    #[command(alias = "list")]
    Ls,
    /// Create a property's database with its top-level location
    Init {
        /// Profile name (default: --profile)
        name: Option<String>,
        /// Display name of the property (default: the config title or the name)
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        address: Option<String>,
    },
    /// The profile in use (or NAME): database, item count and value
    Show { name: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Nushell,
    Elvish,
    Powershell,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: Shell,
}

#[derive(Debug, Args)]
pub struct ManArgs {
    /// Write one page per (sub)command into DIR instead of printing the main page
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}
