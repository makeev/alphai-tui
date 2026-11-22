//! CSV records can span lines. Keep the starting line for diagnostics.
use anyhow::{Result, bail, ensure};

pub(super) struct Record {
    pub line: usize,
    pub cells: Vec<String>,
}

pub(super) fn read(text: &str, delimiter: char) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut cells = Vec::new();
    let mut field = String::new();
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    let (mut line, mut start) = (1, 1);
    let mut quoted = false;
    let mut closed = false;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.next_if_eq(&'"').is_some() => field.push('"'),
                '"' => {
                    quoted = false;
                    closed = true;
                }
                _ => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
        } else if c == delimiter {
            cells.push(std::mem::take(&mut field));
            closed = false;
        } else if matches!(c, '\r' | '\n') {
            if c == '\r' {
                chars.next_if_eq(&'\n');
            }
            cells.push(std::mem::take(&mut field));
            records.push(Record {
                line: start,
                cells: std::mem::take(&mut cells),
            });
            line += 1;
            start = line;
            closed = false;
        } else if closed {
            ensure!(
                c == ' ',
                "unexpected text after a quoted field at line {line}"
            );
        } else if c == '"' {
            ensure!(field.is_empty(), "unexpected quote at line {line}");
            quoted = true;
        } else {
            field.push(c);
        }
    }
    if quoted {
        bail!("unclosed quoted field starting in record at line {start}");
    }
    if closed || !field.is_empty() || !cells.is_empty() {
        cells.push(field);
        records.push(Record { line: start, cells });
    }
    Ok(records)
}
