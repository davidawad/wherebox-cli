//! Per-invocation context: resolved configuration, output options, database access.

use std::cell::{Cell, RefCell};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::rc::Rc;

use crate::audit;
use crate::cli::GlobalOpts;
use crate::config::{self, Layer, Resolved};
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::keys;
use crate::output::{Body, Format, OutputOpts, Report};
use crate::util::Tz;
use crate::vault::{self, OpenOpts};

pub struct Ctx {
    pub resolved: Resolved,
    pub out: OutputOpts,
    pub tz: Tz,
    pub db_path: PathBuf,
    /// The property profile in use, if any (see [`crate::profiles`]).
    pub profile: Option<crate::profiles::Profile>,
    /// `--format` exactly as given on the command line (import reads it as the input format).
    pub format_flag: Option<Format>,
    /// `--json` was given.
    pub json_flag: bool,
    pub insecure: bool,
    pub encrypt_output: bool,
    pub recipients: Vec<String>,
    /// Set once an encrypted database was unlocked; the command is then audited.
    audit: RefCell<Option<audit::Sink>>,
    changes: Rc<Cell<u64>>,
    rows_out: Cell<u64>,
    output_kind: Cell<&'static str>,
    touched_db: Cell<bool>,
}

/// Translate global flags into the highest-precedence config layer.
pub fn flag_layer(g: &GlobalOpts) -> Layer {
    let s = |k: &str, v: &Option<String>| v.as_ref().map(|v| (k.to_string(), v.clone()));
    [
        g.db.as_ref().map(|p| ("db_path".to_string(), p.to_string_lossy().into_owned())),
        g.format.map(|f| ("format".to_string(), format!("{f:?}").to_lowercase())),
        g.json.then(|| ("format".to_string(), "json".to_string())),
        s("profile", &g.profile),
        s("precision", &g.precision),
        s("date_format", &g.date_format),
        s("timezone", &g.timezone),
        s("color", &g.color),
        s("csv_delimiter", &g.delimiter),
        s("csv_quote", &g.quote),
        g.no_header.then(|| ("csv_header".to_string(), "false".to_string())),
        s("null", &g.null),
        g.quiet.then(|| ("quiet".to_string(), "true".to_string())),
        g.verbose.then(|| ("verbose".to_string(), "true".to_string())),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn first_byte(s: &str, fallback: u8) -> u8 {
    s.bytes().next().unwrap_or(fallback)
}

impl Ctx {
    pub fn new(g: &GlobalOpts) -> Result<Self> {
        let resolved = config::resolve(g.config.as_deref(), flag_layer(g))?;
        let to_file = g.output.as_ref().is_some_and(|p| p.as_os_str() != "-");
        let color = match resolved.get("color") {
            "always" => true,
            "never" => false,
            _ => !to_file && std::io::stdout().is_terminal(),
        };
        let out = OutputOpts {
            format: Format::parse(resolved.get("format"))?,
            output: g.output.clone(),
            precision: resolved.get("precision").parse().unwrap_or(2),
            delimiter: first_byte(resolved.get("csv_delimiter"), b','),
            quote: first_byte(resolved.get("csv_quote"), b'"'),
            header: resolved.flag("csv_header"),
            null: resolved.get("null").to_string(),
            color,
            date_format: resolved.get("date_format").to_string(),
            columns: g.columns.clone(),
        };
        let tz = Tz::parse(resolved.get("timezone"))?;
        // A profile chosen at a higher layer than db_path wins, or at the same
        // layer except for flags: `--profile` beats WHEREBOX_DB,
        // WHEREBOX_PROFILE beats WHEREBOX_DB, `--db` beats `--profile`.
        let (ps, ds) = (resolved.source("profile"), resolved.source("db_path"));
        let profile = Some(resolved.get("profile"))
            .filter(|p| !p.is_empty() && (ps > ds || (ps == ds && ps != config::Source::Flag)))
            .map(|p| crate::profiles::get(&resolved.config_path, p))
            .transpose()?;
        let db_path = profile
            .as_ref()
            .map_or_else(|| PathBuf::from(crate::paths::expand_tilde(resolved.get("db_path"))), |p| p.db_path.clone());
        let setting_path = |k: &str| {
            Some(resolved.get(k)).filter(|p| !p.is_empty()).map(|p| PathBuf::from(crate::paths::expand_tilde(p)))
        };
        crate::keyfile::set_override(setting_path("key_file"));
        crate::sshkey::set_override(setting_path("ssh_key"));
        Ok(Self {
            resolved,
            out,
            tz,
            db_path,
            profile,
            format_flag: g.format,
            json_flag: g.json,
            insecure: g.insecure_plaintext,
            encrypt_output: g.encrypt_output || !g.recipients.is_empty(),
            recipients: g.recipients.clone(),
            audit: RefCell::new(None),
            changes: Rc::new(Cell::new(0)),
            rows_out: Cell::new(0),
            output_kind: Cell::new("stdout"),
            touched_db: Cell::new(false),
        })
    }

    pub fn key_source(&self) -> Result<keys::Source> {
        keys::Source::parse(self.resolved.get("key_source"))
    }

    pub fn open_opts(&self) -> Result<OpenOpts> {
        Ok(OpenOpts { source: self.key_source()?, insecure: self.insecure, config: self.resolved.config_path.clone() })
    }

    /// Record how the database at `db_path` opens in the config file's
    /// `[encryption]` section (best effort: a read-only config only warns).
    pub fn record_keys(&self, header: &crate::crypto::Header) {
        match crate::enc_config::sync(&self.resolved.config_path, &self.db_path, header) {
            Ok(true) => self.verbose(&format!("recorded keys in {}", self.resolved.config_path.display())),
            Ok(false) => {}
            Err(e) => self.warn(&format!("warning: could not record keys in the config file: {}", e.message)),
        }
    }

    /// Open (unlocking or creating) the database and apply pending migrations.
    pub fn db(&self) -> Result<Db> {
        let db = self.db_raw()?;
        crate::migrations::migrate(&db)?;
        Ok(db)
    }

    /// Open the database without applying migrations.
    pub fn db_raw(&self) -> Result<Db> {
        self.verbose(&format!("database: {}", self.db_path.display()));
        let db = vault::open(&self.db_path, &self.open_opts()?, &|m| self.warn(m), &mut crate::prompt::Terminal)?
            .with_change_counter(self.changes.clone());
        if let Some(s) = db.sealed() {
            self.record_keys(&s.header);
            self.verbose(&format!("unlocked with key from {}", s.key_source));
            self.set_audit(s.header.db_id, s.keys.audit.clone());
        }
        self.touched_db.set(true);
        Ok(db)
    }

    /// Note that inventory data was read (through another profile's context),
    /// so plaintext output files get the usual warning.
    pub fn mark_touched(&self) {
        self.touched_db.set(true);
    }

    /// Enable auditing of this command for the database with `db_id`.
    pub fn set_audit(&self, db_id: [u8; crate::crypto::ID_LEN], key: crate::crypto::Key) {
        *self.audit.borrow_mut() = Some(audit::Sink { db_path: self.db_path.clone(), db_id, key });
    }

    pub fn audit_sink(&self) -> Option<audit::Sink> {
        self.audit.borrow().clone()
    }

    /// Append the audit record for this invocation (if it touched an encrypted database).
    pub fn finish_audit(&self, command: &str, exit_code: i32) {
        let Some(sink) = self.audit_sink() else { return };
        let rec = audit::Record {
            ts: crate::util::now_iso(),
            user: audit::current_user(),
            command: command.to_string(),
            ok: exit_code == 0,
            exit_code,
            rows_changed: self.changes.get(),
            rows_out: self.rows_out.get(),
            output: self.output_kind.get().to_string(),
        };
        if let Err(e) = sink.append(&rec) {
            eprintln!("wherebox: warning: could not write audit log: {e}");
        }
    }

    /// Security warning on stderr (suppressed by --quiet).
    pub fn warn(&self, msg: &str) {
        if !self.quiet() {
            eprintln!("wherebox: {msg}");
        }
    }

    pub fn quiet(&self) -> bool {
        self.resolved.flag("quiet")
    }

    /// Informational message on stderr (suppressed by --quiet).
    pub fn info(&self, msg: &str) {
        if !self.quiet() {
            eprintln!("{msg}");
        }
    }

    pub fn verbose(&self, msg: &str) {
        if self.resolved.flag("verbose") {
            eprintln!("wherebox: {msg}");
        }
    }

    pub fn emit(&self, r: &Report) -> Result<()> {
        self.emit_with(r, &self.out)
    }

    /// Render with `o` and write to `--output` (0600, warning when plaintext)
    /// or stdout, age-encrypting with `--encrypt-output`.
    pub fn emit_with(&self, r: &Report, o: &OutputOpts) -> Result<()> {
        let n = match &r.body {
            Body::List { rows, .. } => rows.len() as u64,
            Body::Object(_) => 1,
        };
        self.rows_out.set(self.rows_out.get() + n);
        let text = crate::output::render(r, o)?;
        self.write_output(text.as_bytes(), o)
    }

    /// Write already rendered output (text or a binary document such as a
    /// workbook) to `--output` or stdout, as [`Self::emit_with`] does.
    pub fn write_output(&self, data: &[u8], o: &OutputOpts) -> Result<()> {
        let file = o.output.as_ref().filter(|p| p.as_os_str() != "-");
        let bytes = if self.encrypt_output {
            self.output_kind.set(if file.is_some() { "encrypted-file" } else { "encrypted-stdout" });
            crate::seal_output::encrypt(data, &self.recipients)?
        } else {
            if file.is_some() {
                self.output_kind.set("file");
            }
            data.to_vec()
        };
        match file {
            Some(p) => {
                if !self.encrypt_output && self.touched_db.get() {
                    self.warn(&format!(
                        "warning: writing plaintext inventory data to {}; use --encrypt-output to encrypt it",
                        p.display()
                    ));
                }
                crate::crypto::create_private(p)
                    .and_then(|mut f| f.write_all(&bytes))
                    .map_err(|e| AppError::io(format!("writing {}: {e}", p.display())))
            }
            None => crate::output::write_stdout(&bytes),
        }
    }

    /// Emit the result of a mutating command. In table mode the stderr status
    /// line is enough for humans, so nothing is printed to stdout.
    pub fn emit_mutation(&self, r: &Report) -> Result<()> {
        if self.out.format == Format::Table && self.out.output.is_none() {
            Ok(())
        } else {
            self.emit(r)
        }
    }

    /// Where attachment files live: `attachments/` next to the database.
    pub fn attachments_dir(&self) -> PathBuf {
        self.db_path.parent().map_or_else(|| PathBuf::from("attachments"), |p| p.join("attachments"))
    }

    /// Default look-ahead of `expiring`.
    pub fn expiring_within(&self) -> &str {
        self.resolved.get("expiring_within")
    }

    /// Extra date format accepted when parsing input dates.
    pub fn input_date_formats(&self) -> Vec<String> {
        vec![self.out.date_format.clone()]
    }
}
