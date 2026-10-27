//! Command-line definitions of `list`, `cohort` and `match` (docs/SPEC.md,
//! "Lists and matching").

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::cli::ProfileScope;

/// Matcher fields of a list entry or cohort. Each takes one value per flag
/// and may be repeated.
#[derive(Debug, Args, Clone, Default)]
pub struct MatcherFlags {
    /// Amazon ASIN (tier a: identifier)
    #[arg(long, value_name = "ASIN")]
    pub asin: Vec<String>,
    /// SKU or model number (tier a)
    #[arg(long, value_name = "SKU")]
    pub sku: Vec<String>,
    /// UPC / EAN / GTIN (tier a; check digit and leading zeros normalized)
    #[arg(long, alias = "gtin", value_name = "CODE")]
    pub upc: Vec<String>,
    /// Product ref (tier a)
    #[arg(long, value_name = "REF")]
    pub product_ref: Vec<String>,
    /// Product page URL (tier b; Amazon URLs give their ASIN)
    #[arg(long, value_name = "URL")]
    pub url: Vec<String>,
    /// Items with this label (tier d)
    #[arg(long, value_name = "LABEL")]
    pub label: Vec<String>,
    /// Items of this kind preset (tier d)
    #[arg(long, alias = "kind", value_name = "PRESET")]
    pub preset: Vec<String>,
    /// Only items (and lots) at a location whose path contains this, e.g. "Kitchen / Freezer"
    #[arg(long, short = 'a', alias = "location", value_name = "PATH")]
    pub at: Vec<String>,
    /// Everything a cohort matches
    #[arg(long, value_name = "COHORT")]
    pub cohort: Vec<String>,
    /// Alternative names (fuzzy, tier e)
    #[arg(long, value_name = "NAME", num_args = 1..)]
    pub any: Vec<String>,
    /// Never match items with these names
    #[arg(long, value_name = "NAME", num_args = 1..)]
    pub exclude: Vec<String>,
    /// Whether the entry's own name is a fuzzy matcher (default: when it has no other selector)
    #[arg(long, value_name = "BOOL")]
    pub fuzzy: Option<String>,
    /// Fuzzy name threshold for this entry (0-1)
    #[arg(long, value_name = "T")]
    pub threshold: Option<String>,
}

/// What an entry wants.
#[derive(Debug, Args, Clone, Default)]
pub struct EntryFlags {
    /// Quantity wanted (default 1; fractions such as 1/2 allowed)
    #[arg(long)]
    pub qty: Option<String>,
    /// Unit of the quantity: ea, g, kg, oz, lb, ml, l, tsp, tbsp, cup, or any word
    #[arg(long)]
    pub unit: Option<String>,
    #[arg(long)]
    pub note: Option<String>,
    /// Nice to have: does not count against `ready`
    #[arg(long)]
    pub optional: bool,
}

#[derive(Debug, Subcommand)]
pub enum ListCmd {
    /// Create a list (`--profile` sets its default profile)
    New {
        name: String,
        /// recipe, kit, packing, wishlist, or any other word (recipe-like)
        #[arg(long = "type", short = 't', default_value = "recipe")]
        kind: String,
        #[arg(long)]
        notes: Option<String>,
        /// Only food matches by name or attribute (default: true for recipes)
        #[arg(long, value_name = "BOOL")]
        food: Option<String>,
    },
    /// Every list, with entry counts (wishlists: open / fulfilled)
    Ls,
    /// A list's entries and matchers
    Show { list: String },
    /// Move a list to <config dir>/lists/.trash
    Rm { list: String },
    /// Add an entry
    Add {
        list: String,
        /// Entry name (also its fuzzy name unless it has other selectors)
        entry: String,
        #[command(flatten)]
        want: EntryFlags,
        #[command(flatten)]
        m: MatcherFlags,
    },
    /// Change an entry: given matcher flags replace that field
    Edit {
        list: String,
        /// Entry name or 1-based position
        entry: String,
        /// Rename the entry
        #[arg(long)]
        name: Option<String>,
        /// No longer optional
        #[arg(long, conflicts_with = "optional")]
        required: bool,
        /// Remove a field (qty, unit, note, asin, sku, upc, product_ref, url, any, label, preset, at, cohort, exclude, fuzzy, threshold, pins)
        #[arg(long = "clear", value_name = "FIELD")]
        clear: Vec<String>,
        #[command(flatten)]
        want: EntryFlags,
        #[command(flatten)]
        m: MatcherFlags,
    },
    /// Remove an entry
    Drop { list: String, entry: String },
    /// Do we have everything? Per entry: have, short, missing, ambiguous, unit-mismatch.
    /// Exit 0 when ready, 3 when not (wishlists: always 0)
    Check {
        list: String,
        #[command(flatten)]
        scope: ProfileScope,
        /// Fuzzy threshold (overrides the list's and the setting)
        #[arg(long, value_name = "T")]
        threshold: Option<String>,
    },
    /// What to buy for a list, as `shopping` rows
    Shop {
        list: String,
        #[command(flatten)]
        scope: ProfileScope,
    },
    /// Confirm an item as an entry's match (stored by identifier when it has one)
    Pin {
        list: String,
        entry: String,
        /// Item: id, asset id, import ref or unique name
        item: String,
    },
    /// Remove a pin (the only one, one matching PIN, or --all)
    Unpin {
        list: String,
        entry: String,
        pin: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Every candidate of an entry with each ladder tier's verdict
    Explain {
        list: String,
        entry: String,
        #[arg(long, value_name = "T")]
        threshold: Option<String>,
    },
    /// Add entries from a plain-text recipe or list (creates the list when missing)
    Import {
        list: String,
        /// Text file (`-` for stdin): one `[qty] [unit] name` per line, URLs on their own line
        file: PathBuf,
        /// Type of a newly created list
        #[arg(long = "type", short = 't', default_value = "recipe")]
        kind: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CohortCmd {
    /// Create a cohort: a named, reusable matcher
    New {
        name: String,
        #[arg(long)]
        notes: Option<String>,
        #[command(flatten)]
        m: MatcherFlags,
    },
    /// Every cohort
    Ls,
    /// A cohort's matchers and the cohorts it includes
    Show { cohort: String },
    /// Add matcher values to a cohort
    Add {
        #[arg(value_name = "COHORT")]
        name: String,
        #[command(flatten)]
        m: MatcherFlags,
    },
    /// Move a cohort to <config dir>/cohorts/.trash
    Rm { cohort: String },
}

#[derive(Debug, Args)]
pub struct MatchArgs {
    /// Product name to look for (fuzzy)
    pub query: Option<String>,
    #[command(flatten)]
    pub m: MatcherFlags,
}
