//! Command-line definitions of `store` and `source` (docs/SPEC.md, "Stores
//! and sourcing").

use clap::{Args, Subcommand};

/// A store's fields, shared by `store add` and `store edit`.
#[derive(Debug, Args, Clone, Default)]
pub struct StoreFlags {
    /// Chain (free text, e.g. "Sprouts"); stores of a chain share chain products and layouts
    #[arg(long)]
    pub chain: Option<String>,
    /// Display name (default: the chain)
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub address: Option<String>,
    /// The store's (or chain's) website
    #[arg(long)]
    pub website: Option<String>,
    #[arg(long)]
    pub notes: Option<String>,
    /// Sections from entrance to checkout, e.g. "produce, bulk=1-2, pantry=3-7, dairy, frozen, checkout"
    #[arg(long, value_name = "LAYOUT")]
    pub layout: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum StoreCmd {
    /// Add a store (a physical shop of a chain)
    Add {
        /// Store id, a slug such as `sprouts-north`
        id: String,
        #[command(flatten)]
        f: StoreFlags,
    },
    /// Stores, with product counts and which profiles use them
    #[command(alias = "list")]
    Ls {
        /// Only stores of this chain
        #[arg(long)]
        chain: Option<String>,
    },
    /// A store: fields, layout in effect and its products
    Show {
        /// Store id or name
        store: String,
    },
    /// Change a store's fields
    Edit {
        store: String,
        #[command(flatten)]
        f: StoreFlags,
        /// Remove a field (address, website, notes, layout)
        #[arg(long = "clear", value_name = "FIELD")]
        clear: Vec<String>,
    },
    /// Delete a store (refused while products reference it, unless --force)
    Rm {
        store: String,
        /// Also remove the store's own products
        #[arg(long)]
        force: bool,
    },
    /// Show or set a layout: a store's own, or with --chain a chain's default
    Layout {
        /// Store id or name (with --chain: the chain)
        target: String,
        /// TARGET is a chain: its default layout, used by every store of the chain without its own
        #[arg(long)]
        chain: bool,
        /// The new layout (omit to show the one in effect)
        #[arg(conflicts_with = "clear")]
        layout: Option<String>,
        /// Remove the store's (or chain's) own layout
        #[arg(long)]
        clear: bool,
    },
    /// Set the profile's stores, the default first (written to its [profiles.NAME] config table)
    Use {
        /// Store ids or names (none: clear the list)
        stores: Vec<String>,
    },
}

/// A store product's fields, shared by `source add` and `source edit`.
#[derive(Debug, Args, Clone, Default)]
pub struct SourceFlags {
    /// At this store (id or name)
    #[arg(long, short = 's', conflicts_with = "chain")]
    pub store: Option<String>,
    /// At every store of this chain
    #[arg(long)]
    pub chain: Option<String>,
    /// The store's product name (default: the food)
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub brand: Option<String>,
    /// Package size, e.g. "16 oz", "1 lb bag"
    #[arg(long)]
    pub size: Option<String>,
    /// Shelf price
    #[arg(long)]
    pub price: Option<String>,
    /// When the price was seen (default: today, when --price is given)
    #[arg(long, value_name = "DATE")]
    pub seen: Option<String>,
    /// Product page on the store's website
    #[arg(long)]
    pub url: Option<String>,
    /// Store SKU / item number
    #[arg(long)]
    pub sku: Option<String>,
    /// Produce PLU code
    #[arg(long)]
    pub plu: Option<String>,
    /// UPC / EAN barcode
    #[arg(long)]
    pub upc: Option<String>,
    /// Store section (produce, bulk, dairy, pantry...; any other word is a custom section)
    #[arg(long)]
    pub section: Option<String>,
    /// Aisle (free text: 7, 12B, "bins 3-4")
    #[arg(long)]
    pub aisle: Option<String>,
    /// Where exactly, e.g. "top shelf by the nut butters"
    #[arg(long = "note", alias = "notes")]
    pub note: Option<String>,
    /// The website's category text (e.g. "Pantry > Nut Butters"): guesses the section
    #[arg(long, value_name = "TEXT")]
    pub category: Option<String>,
    /// Link to an item (id, asset id, import ref or unique name)
    #[arg(long)]
    pub item: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SourceCmd {
    /// Where to buy a food: a product at a store (or every store of a chain)
    Add {
        /// The food: a list entry name, a food noun or anything else
        food: String,
        #[command(flatten)]
        f: SourceFlags,
    },
    /// Store products (of a food, at a store or chain)
    #[command(alias = "list")]
    Ls {
        /// Only products linked to this food (as list entries link)
        food: Option<String>,
        /// Only products sold at this store (its own and its chain's)
        #[arg(long, short = 's', conflicts_with = "chain")]
        store: Option<String>,
        /// Only products of this chain
        #[arg(long)]
        chain: Option<String>,
    },
    /// Change a store product
    Edit {
        /// Product id (see `source ls`)
        id: String,
        /// The food it is for
        #[arg(long)]
        food: Option<String>,
        #[command(flatten)]
        f: SourceFlags,
        /// Remove a field (brand, size, price, url, sku, plu, upc, section, aisle, note, item)
        #[arg(long = "clear", value_name = "FIELD")]
        clear: Vec<String>,
    },
    /// Delete store products
    Rm {
        #[arg(required = true)]
        ids: Vec<String>,
    },
}
