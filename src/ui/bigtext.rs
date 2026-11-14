//! Figures three rows tall, drawn with the half blocks the charts already
//! use: the headline numbers of the Insider tiles and the Summary heat
//! map. Only the characters a dollar sum or a percent move needs; anything else makes `lines` give up, and the caller
//! prints the figure as ordinary bold text instead.

/// One glyph: three rows of equal width.
fn glyph(c: char) -> Option<[&'static str; 3]> {
    Some(match c {
        '0' => ["▄▀▀▄", "█  █", "▀▄▄▀"],
        '1' => [" ▄█ ", "  █ ", " ▄█▄"],
        '2' => ["▄▀▀▄", "  ▄▀", "▄█▄▄"],
        '3' => ["▀▀▀▄", " ▀▀▄", "▄▄▄▀"],
        '4' => ["  ▄█", "▄▀ █", "▀▀▀█"],
        '5' => ["█▀▀▀", "▀▀▀▄", "▄▄▄▀"],
        '6' => ["▄▀▀ ", "█▀▀▄", "▀▄▄▀"],
        '7' => ["▀▀▀█", "  █ ", " █  "],
        '8' => ["▄▀▀▄", "▄▀▀▄", "▀▄▄▀"],
        '9' => ["▄▀▀▄", "▀▄▄█", " ▄▄▀"],
        '.' => ["  ", "  ", "▄ "],
        '$' => ["▄█▀▀", "▀█▀▄", "▀█▀ "],
        'B' => ["█▀▀▄", "█▀▀▄", "█▄▄▀"],
        'M' => ["█▄ ▄█", "█ ▀ █", "█   █"],
        'K' => ["█ ▄▀", "██  ", "█ ▀▄"],
        '-' => ["   ", "▀▀▀", "   "],
        '+' => ["   ", "▄█▄", " ▀ "],
        '%' => ["█ ▄▀", " ▄▀ ", "▄▀ █"],
        ' ' => ["  ", "  ", "  "],
        _ => return None,
    })
}

/// The three rows of `text`, glyphs one column apart; None when a
/// character has no glyph.
pub fn lines(text: &str) -> Option<[String; 3]> {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c)?;
        for (row, part) in rows.iter_mut().zip(g) {
            if i > 0 {
                row.push(' ');
            }
            row.push_str(part);
        }
    }
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_line_up_and_unknown_characters_give_up() {
        let rows = lines("$2.62B").unwrap();
        let w = rows[0].chars().count();
        assert!(rows.iter().all(|r| r.chars().count() == w));
        assert_eq!(lines("1").unwrap()[0].chars().count(), 4);
        assert!(lines("$2.6€").is_none());
    }
}
