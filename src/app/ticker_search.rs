//! A local watchlist search. Browsing results does not select a ticker or
//! start its news/earnings requests; only Enter commits the choice.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::App;

#[derive(Default)]
pub struct TickerSearch {
    pub open: bool,
    pub input: String,
    /// Watchlist index, not result index: an arriving company name must
    /// not move the cursor to a different ticker when results reorder.
    pub selected: Option<usize>,
}

impl TickerSearch {
    pub fn cursor(&self, matches: &[usize]) -> Option<usize> {
        (!matches.is_empty()).then(|| {
            matches
                .iter()
                .position(|i| Some(*i) == self.selected)
                .unwrap_or(0)
        })
    }
}

impl App {
    pub(super) fn open_ticker_search(&mut self) {
        self.ticker_search = TickerSearch {
            open: true,
            selected: Some(self.selected),
            ..Default::default()
        };
    }

    /// Exact symbols, prefixes, symbol substrings, then company names.
    /// Ties keep watchlist order; an empty query shows the original list.
    pub fn ticker_matches(&self) -> Vec<usize> {
        let query = self.ticker_search.input.trim().to_lowercase();
        let mut matches: Vec<(usize, u8)> = self
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(i, symbol)| {
                let symbol_lower = symbol.to_lowercase();
                let rank = if query.is_empty() || symbol_lower == query {
                    0
                } else if symbol_lower.starts_with(&query) {
                    1
                } else if symbol_lower.contains(&query) {
                    2
                } else if self
                    .data
                    .get(symbol)
                    .and_then(|data| data.quote.name.as_deref())
                    .is_some_and(|name| name.to_lowercase().contains(&query))
                {
                    3
                } else {
                    return None;
                };
                Some((i, rank))
            })
            .collect();
        matches.sort_by_key(|&(_, rank)| rank);
        matches.into_iter().map(|(i, _)| i).collect()
    }

    pub(super) fn handle_ticker_search_key(&mut self, key: KeyEvent) -> bool {
        let matches = self.ticker_matches();
        let cursor = self.ticker_search.cursor(&matches);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Esc => self.ticker_search = TickerSearch::default(),
            KeyCode::Enter => {
                if let Some(cursor) = cursor {
                    let idx = matches[cursor];
                    if idx != self.selected {
                        self.select_symbol(idx);
                    }
                    // Portfolio owns a separate cursor. Keep its holding
                    // under the rail, or open the chart for an unheld ticker.
                    if self.view_id() == crate::ui::ViewId::Portfolio {
                        if let Some(position) = self
                            .positions
                            .iter()
                            .position(|p| p.symbol == self.symbols[idx])
                        {
                            self.portfolio_selected = position;
                        } else {
                            self.switch_view(crate::ui::view_index(crate::ui::ViewId::Chart));
                        }
                    }
                    self.ticker_search = TickerSearch::default();
                }
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => {
                if let Some(cursor) = cursor {
                    let next = match key.code {
                        KeyCode::Up => cursor.saturating_sub(1),
                        KeyCode::Down => (cursor + 1).min(matches.len() - 1),
                        KeyCode::PageUp => cursor.saturating_sub(10),
                        KeyCode::PageDown => (cursor + 10).min(matches.len() - 1),
                        KeyCode::Home => 0,
                        _ => matches.len() - 1,
                    };
                    self.ticker_search.selected = Some(matches[next]);
                }
            }
            KeyCode::Backspace => {
                if let Some((end, _)) = self.ticker_search.input.grapheme_indices(true).next_back()
                {
                    self.ticker_search.input.truncate(end);
                }
                self.reset_ticker_search_cursor();
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.ticker_search.input.clear();
                self.reset_ticker_search_cursor();
            }
            KeyCode::Char(c)
                if plain && !c.is_control() && self.ticker_search.input.chars().count() < 64 =>
            {
                self.ticker_search.input.push(c);
                self.reset_ticker_search_cursor();
            }
            _ => {}
        }
        false
    }

    fn reset_ticker_search_cursor(&mut self) {
        self.ticker_search.selected = self.ticker_matches().first().copied();
    }
}
