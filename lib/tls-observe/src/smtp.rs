//! The SMTP STARTTLS preamble: 220 greeting, EHLO, STARTTLS command, 220.
//!
//! The conversation is bounded: at most 64 reply lines and 512 bytes per line,
//! and every read and write is bounded by what is left of the observation
//! budget rather than by a fresh timeout. Lines are read one byte at a time so
//! that no plaintext byte is buffered and lost when the same stream becomes a
//! TLS transport afterwards.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::TlsProbeError;

const MAX_LINE_BYTES: usize = 512;
const MAX_REPLY_LINES: usize = 64;

struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn require(&self, expected: u16, step: &str) -> Result<(), TlsProbeError> {
        if self.code == expected {
            return Ok(());
        }
        Err(TlsProbeError::new(format!(
            "{step}: SMTP server replied {} instead of {expected}: {}",
            self.code,
            self.lines.join(" | ")
        )))
    }

    fn mentions_starttls(&self) -> bool {
        self.lines
            .iter()
            .any(|line| line.to_ascii_uppercase().contains("STARTTLS"))
    }
}

struct Conversation<'a> {
    stream: &'a mut TcpStream,
    lines_left: usize,
    deadline: Instant,
}

impl<'a> Conversation<'a> {
    fn new(stream: &'a mut TcpStream, deadline: Instant) -> Self {
        Self {
            stream,
            lines_left: MAX_REPLY_LINES,
            deadline,
        }
    }

    /// What is left of the whole observation budget.
    fn remaining(&self) -> Result<Duration, TlsProbeError> {
        crate::remaining_budget(self.deadline).ok_or_else(crate::timeout_error)
    }

    fn write_command(&mut self, command: &str) -> Result<(), TlsProbeError> {
        let remaining = self.remaining()?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|err| {
                TlsProbeError::new(format!("could not bound the write timeout: {err}"))
            })?;
        self.stream
            .write_all(command.as_bytes())
            .and_then(|()| self.stream.flush())
            .map_err(|err| {
                crate::io_budget_error(
                    &format!("could not write the SMTP command {command:?}"),
                    err,
                )
            })
    }

    fn read_reply(&mut self, step: &str) -> Result<Reply, TlsProbeError> {
        let first = self.read_line(step)?;
        let code = reply_code(&first).ok_or_else(|| {
            TlsProbeError::new(format!(
                "{step}: SMTP reply {first:?} does not start with a status code"
            ))
        })?;
        let mut lines = vec![first];
        while lines.last().is_some_and(|line| is_continuation(line)) {
            lines.push(self.read_line(step)?);
        }
        let last = lines.last().cloned().unwrap_or_default();
        let last_code = reply_code(&last).ok_or_else(|| {
            TlsProbeError::new(format!(
                "{step}: SMTP continuation {last:?} does not start with a status code"
            ))
        })?;
        if last_code != code {
            return Err(TlsProbeError::new(format!(
                "{step}: SMTP multiline reply changed status code from {code} to {last_code}"
            )));
        }
        Ok(Reply { code, lines })
    }

    fn read_line(&mut self, step: &str) -> Result<String, TlsProbeError> {
        if self.lines_left == 0 {
            return Err(TlsProbeError::new(format!(
                "{step}: SMTP reply exceeds {MAX_REPLY_LINES} lines"
            )));
        }
        self.lines_left -= 1;

        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let remaining = self.remaining()?;
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(|err| {
                    TlsProbeError::new(format!("could not bound the read timeout: {err}"))
                })?;
            match self.stream.read(&mut byte) {
                Ok(0) => {
                    return Err(TlsProbeError::new(format!(
                        "{step}: SMTP connection closed in the middle of a reply"
                    )));
                }
                Ok(_) => {
                    bytes.push(byte[0]);
                    if bytes.len() > MAX_LINE_BYTES {
                        return Err(TlsProbeError::new(format!(
                            "{step}: SMTP reply line exceeds {MAX_LINE_BYTES} bytes"
                        )));
                    }
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(err) => {
                    return Err(crate::io_budget_error(
                        &format!("{step}: could not read the SMTP reply"),
                        err,
                    ));
                }
            }
        }
        while matches!(bytes.last(), Some(b'\r' | b'\n')) {
            bytes.pop();
        }
        String::from_utf8(bytes)
            .map_err(|_| TlsProbeError::new(format!("{step}: SMTP reply is not valid UTF-8")))
    }
}

/// Run the SMTP STARTTLS preamble on a freshly connected stream.
///
/// On success the same stream is ready for the TLS handshake.
pub(crate) fn start_tls(
    stream: &mut TcpStream,
    server_name: &str,
    deadline: Instant,
) -> Result<(), TlsProbeError> {
    let mut conversation = Conversation::new(stream, deadline);

    let greeting = conversation.read_reply("greeting")?;
    greeting.require(220, "greeting")?;

    conversation.write_command(&format!("EHLO {server_name}\r\n"))?;
    let ehlo = conversation.read_reply("EHLO")?;
    ehlo.require(250, "EHLO")?;
    if !ehlo.mentions_starttls() {
        return Err(TlsProbeError::new(
            "the SMTP server's EHLO reply does not offer STARTTLS",
        ));
    }

    conversation.write_command("STARTTLS\r\n")?;
    let ready = conversation.read_reply("STARTTLS")?;
    ready.require(220, "STARTTLS")?;

    Ok(())
}

fn reply_code(line: &str) -> Option<u16> {
    let digits = line.get(..3)?;
    let mut code = 0u16;
    for digit in digits.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        code = code * 10 + u16::from(digit - b'0');
    }
    Some(code)
}

fn is_continuation(line: &str) -> bool {
    line.as_bytes().get(3) == Some(&b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_parsed_from_the_first_three_digits() {
        assert_eq!(reply_code("220 mail.ato.test ESMTP"), Some(220));
        assert_eq!(reply_code("250-STARTTLS"), Some(250));
        assert_eq!(reply_code("2"), None);
        assert_eq!(reply_code("22x OK"), None);
        assert_eq!(reply_code(""), None);
    }

    #[test]
    fn only_a_hyphen_marks_a_continuation() {
        assert!(is_continuation("250-mail.ato.test"));
        assert!(!is_continuation("250 OK"));
        assert!(!is_continuation("250"));
    }
}
