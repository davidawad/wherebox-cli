//! Command-line definitions of `receipt` (docs/SPEC.md, "Receipts").

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args, Clone, Default)]
pub struct ReceiptImportArgs {
    /// Receipt files: images (png, jpg, heic...) and PDFs go through local OCR, text as is
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// The store (id or name) the receipt is from; overrides detection
    #[arg(long, short = 's')]
    pub store: Option<String>,
    /// Purchase date, when the receipt has none or it was misread
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,
    /// Location for new items (e.g. "Home / Kitchen / Pantry")
    #[arg(long, value_name = "LOC")]
    pub at: Option<String>,
    /// Parse, match and show what would be written; write nothing
    #[arg(long, short = 'n')]
    pub dry_run: bool,
    /// Accept every proposal at or above --min-confidence without asking (skip the rest)
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Lowest confidence --yes accepts
    #[arg(long, value_name = "0..1", default_value = "0.8")]
    pub min_confidence: f64,
    /// Write a review file (JSON) to FILE instead of importing; apply it with `receipt apply`
    #[arg(long, value_name = "FILE")]
    pub review: Option<PathBuf>,
    /// Import even when the same receipt (fingerprint) was imported before
    #[arg(long)]
    pub force: bool,
    /// Keep the original file (image or PDF) in the encrypted database
    #[arg(long)]
    pub keep_image: bool,
    /// Do not store the file's path on the receipt
    #[arg(long)]
    pub no_path: bool,
}

#[derive(Debug, Subcommand)]
pub enum ReceiptCmd {
    /// Read grocery receipts into lots: OCR (tesseract, poppler), parse, match lines to items, review, import
    #[command(long_about = "Read grocery receipts into lots.\n\n\
        Each FILE is one receipt: a photo or scan (png, jpg, tiff, webp, heic if your tesseract reads it), \
        a PDF, or an emailed receipt saved as text. Images go through the local `tesseract` CLI \
        (brew install tesseract / apt install tesseract-ocr); PDFs through poppler's `pdftotext`, \
        or `pdftoppm` + tesseract for scans (brew install poppler / apt install poppler-utils). \
        No network, no cloud OCR.\n\n\
        Lines are parsed with the chain profile detected from the header (Sprouts, ALDI, or generic): \
        PLU/UPC/SKU codes, `2 @ 1.99` quantities, `1.12 lb @ 2.99 /lb` weights, coupons, voids, \
        PRICE YOU PAY lines, fees (bags, deposits) and totals; receipt abbreviations are expanded \
        (ORG -> Organic). Each line is matched to an item: a store product with the same code, a store \
        product's name, a food by head noun, an item by name, else a new item.\n\n\
        Then: --yes accepts proposals at or above --min-confidence; on a terminal each line is \
        confirmed, edited or skipped; otherwise (or with --review FILE) a review file is written for \
        `wherebox receipt apply`. Accepted lines become lots (date, unit price, vendor = the store) \
        and update the store's product price. A receipt is imported once (fingerprint); --force \
        imports it again.")]
    Import(ReceiptImportArgs),
    /// Apply a review file written by `receipt import` (after editing its `action`, `item`, `new_name`, `qty`)
    Apply {
        /// The review file (wherebox/receipt-review/v1)
        file: PathBuf,
        /// Show what would be written; write nothing
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Import even when the same receipt was imported before
        #[arg(long)]
        force: bool,
        /// Location for new items
        #[arg(long, value_name = "LOC")]
        at: Option<String>,
    },
    /// Imported receipts: date, store, total, lots
    #[command(alias = "list")]
    Ls,
    /// One receipt and its lots
    Show {
        /// Receipt id (or a unique part of it, 6+ characters)
        receipt: String,
        /// Print the receipt's OCR text
        #[arg(long)]
        text: bool,
        /// Write the kept original file (see `import --keep-image`) to FILE
        #[arg(long, value_name = "FILE")]
        extract_image: Option<PathBuf>,
    },
}
