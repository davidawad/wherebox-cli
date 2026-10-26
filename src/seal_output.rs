//! `--encrypt-output`: ASCII-armored age encryption of command output, so
//! exports can be decrypted with the standard `age -d` / `rage -d` tools.

use std::io::Write;

use crate::error::{AppError, Result};

pub const ENV_EXPORT_PASSPHRASE: &str = "WHEREBOX_EXPORT_PASSPHRASE";

fn io(e: impl std::fmt::Display) -> AppError {
    AppError::io(format!("encrypting output: {e}"))
}

/// Encrypt `plaintext` to the age X25519 `recipients`, or, when there are
/// none, to a passphrase (`WHEREBOX_EXPORT_PASSPHRASE` or an interactive prompt).
pub fn encrypt(plaintext: &[u8], recipients: &[String]) -> Result<Vec<u8>> {
    let encryptor = if recipients.is_empty() {
        let pass = match std::env::var(ENV_EXPORT_PASSPHRASE).ok().filter(|p| !p.is_empty()) {
            Some(p) => p,
            None if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                let a = rpassword::prompt_password("Passphrase for encrypted output: ").map_err(io)?;
                let b = rpassword::prompt_password("Repeat passphrase: ").map_err(io)?;
                if a != b {
                    return Err(AppError::usage("passphrases do not match"));
                }
                a
            }
            None => {
                return Err(AppError::usage(format!(
                    "--encrypt-output needs --recipient age1... or {ENV_EXPORT_PASSPHRASE} (no terminal to prompt)"
                )))
            }
        };
        age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(pass))
    } else {
        let rs = recipients
            .iter()
            .map(|r| {
                r.parse::<age::x25519::Recipient>()
                    .map_err(|e| AppError::usage(format!("invalid --recipient '{r}': {e}")))
            })
            .collect::<Result<Vec<_>>>()?;
        age::Encryptor::with_recipients(rs.iter().map(|r| r as &dyn age::Recipient)).map_err(io)?
    };
    let mut out = Vec::new();
    let armor = age::armor::ArmoredWriter::wrap_output(&mut out, age::armor::Format::AsciiArmor).map_err(io)?;
    let mut w = encryptor.wrap_output(armor).map_err(io)?;
    w.write_all(plaintext).map_err(io)?;
    w.finish().and_then(age::armor::ArmoredWriter::finish).map_err(io)?;
    Ok(out)
}
