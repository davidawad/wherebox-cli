//! User interaction behind a trait, so key setup and unlocking can be driven
//! by a terminal in real use and by scripted input in tests (no TTY needed).

use std::io::{BufRead, IsTerminal, Write};

use zeroize::Zeroizing;

use crate::error::Result;
use crate::keys::key_error;

pub trait Prompter {
    /// Whether questions can be asked (a terminal is attached).
    fn interactive(&self) -> bool;
    /// Show a message (stderr for the terminal).
    fn notice(&mut self, msg: &str);
    /// Ask a yes/no question whose default is yes.
    fn confirm(&mut self, question: &str) -> Result<bool>;
    /// Read a secret without echo.
    fn secret(&mut self, prompt: &str) -> Result<Zeroizing<String>>;
}

/// `y`, `yes` and an empty answer are yes.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

/// stdin/stderr; interactive when stdin is a terminal.
pub struct Terminal;

impl Prompter for Terminal {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn notice(&mut self, msg: &str) {
        eprintln!("{msg}");
    }

    fn confirm(&mut self, question: &str) -> Result<bool> {
        eprint!("{question} [Y/n] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map_err(|e| key_error(format!("reading answer: {e}")))?;
        Ok(is_yes(&line))
    }

    fn secret(&mut self, prompt: &str) -> Result<Zeroizing<String>> {
        rpassword::prompt_password(prompt)
            .map(Zeroizing::new)
            .map_err(|e| key_error(format!("reading passphrase: {e}")))
    }
}

/// Answers read line by line from `input`; messages written to `output`.
pub struct Scripted<R, W> {
    pub input: R,
    pub output: W,
    pub interactive: bool,
}

impl<R: BufRead, W: Write> Scripted<R, W> {
    fn line(&mut self) -> Result<Zeroizing<String>> {
        let mut s = Zeroizing::new(String::new());
        self.input.read_line(&mut s).map_err(|e| key_error(format!("reading answer: {e}")))?;
        Ok(Zeroizing::new(s.trim_end_matches(['\r', '\n']).to_string()))
    }
}

impl<R: BufRead, W: Write> Prompter for Scripted<R, W> {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn notice(&mut self, msg: &str) {
        let _ = writeln!(self.output, "{msg}");
    }

    fn confirm(&mut self, question: &str) -> Result<bool> {
        let _ = write!(self.output, "{question} [Y/n] ");
        Ok(is_yes(&self.line()?))
    }

    fn secret(&mut self, prompt: &str) -> Result<Zeroizing<String>> {
        let _ = write!(self.output, "{prompt}");
        self.line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_answers_and_crlf_input() {
        let mut p = Scripted { input: "n\r\nsecret pass\r\n\n".as_bytes(), output: Vec::new(), interactive: true };
        assert!(!p.confirm("go?").unwrap());
        assert_eq!(&*p.secret("pass: ").unwrap(), "secret pass");
        assert!(p.confirm("again?").unwrap(), "empty answer is yes");
        p.notice("hello");
        assert!(String::from_utf8(p.output).unwrap().ends_with("hello\n"));
    }
}
