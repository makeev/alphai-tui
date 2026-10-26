//! Clicks and the wheel, resolved against what the last frame drew
//! (`ui::hits`). The mouse never does anything a key cannot, and nothing
//! that costs a request: the wheel moves within the rows a feed has
//! loaded and never pages it (an inertial trackpad flick would queue
//! pages by the handful), and the chart ignores it altogether (a preset
//! switch refetches every ticker).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use super::{App, ArticleOverlay, HelpOverlay};
use crate::ui::hits::{ListKind, Scroll};
use crate::ui::{self, ViewId};

/// Lines a wheel notch moves a text pane; lists move one row.
const TEXT_STEP: u16 = 3;

impl App {
    /// Returns true when the event changed something worth a frame. Mouse
    /// motion arrives as a stream and changes nothing, so it must not
    /// schedule frames.
    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) -> bool {
        if !self.mouse {
            return false;
        }
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp => self.wheel(pos, -1),
            MouseEventKind::ScrollDown => self.wheel(pos, 1),
            MouseEventKind::Down(MouseButton::Left) => self.click(pos),
            _ => false,
        }
    }

    /// M: hand the mouse to the terminal and back. With it captured the
    /// terminal cannot select text, which is the one thing a reader may
    /// want it for; the notice says how to get it back.
    pub(crate) fn toggle_mouse(&mut self) {
        self.mouse = !self.mouse;
        let text = if self.mouse {
            "Mouse on: click and scroll the views (M hands it back to the terminal)"
        } else {
            "Mouse off: the terminal selects text again (M turns it back on)"
        };
        self.notice = Some((text.into(), Instant::now()));
    }

    /// `dir` is -1 for a notch away from the reader (up), 1 towards.
    fn wheel(&mut self, pos: Position, dir: i16) -> bool {
        let text = |scroll: u16| scroll.saturating_add_signed(dir * TEXT_STEP as i16);
        // An overlay takes the wheel wherever the pointer is, the way it
        // takes every key.
        if self.prompt.open {
            return false;
        }
        if self.ticker_search.open {
            let code = if dir < 0 { KeyCode::Up } else { KeyCode::Down };
            self.handle_ticker_search_key(KeyEvent::from(code));
            return true;
        }
        if self.settings.open {
            if self.settings.editing {
                return false;
            }
            let last = super::settings_rows().len() - 1;
            self.settings.cursor = step(self.settings.cursor, dir, last);
            return true;
        }
        if self.article_overlay.open {
            self.article_overlay.scroll = text(self.article_overlay.scroll);
            return true;
        }
        if self.help.open {
            self.help.scroll = text(self.help.scroll);
            return true;
        }
        if self.category_picker.open {
            let last = crate::category::ALL.len() - 1;
            self.category_picker.cursor = step(self.category_picker.cursor, dir, last);
            return true;
        }
        if let Some(kind) = self.hits.scroll_at(pos) {
            match kind {
                Scroll::Card => self.card_scroll = text(self.card_scroll),
                Scroll::Earnings => self.earnings_scroll = text(self.earnings_scroll),
            }
            return true;
        }
        if let Some((kind, _)) = self.hits.list_at(pos) {
            return match kind {
                ListKind::Watchlist => self.wheel_watchlist(dir),
                ListKind::Feed => {
                    // Within the loaded rows only: paging stays on the key.
                    let len = self.visible_articles().map_or(0, <[_]>::len);
                    let next = step(self.news_selected, dir, len.saturating_sub(1));
                    if len == 0 || next == self.news_selected {
                        return false;
                    }
                    self.news_selected = next;
                    self.card_scroll = 0;
                    true
                }
                ListKind::Portfolio => {
                    let before = self.portfolio_selected;
                    self.move_portfolio(dir.into());
                    before != self.portfolio_selected
                }
                ListKind::Calendar => {
                    self.move_calendar(dir.into());
                    true
                }
                ListKind::NewsStrip | ListKind::Categories | ListKind::TickerSearch => false,
            };
        }
        if self.hits.card_at(pos).is_some() {
            return self.wheel_watchlist(dir);
        }
        false
    }

    fn wheel_watchlist(&mut self, dir: i16) -> bool {
        let next = step(self.selected, dir, self.symbols.len() - 1);
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }

    fn click(&mut self, pos: Position) -> bool {
        if self.prompt.open || self.settings.open {
            // Text forms keep what was typed: a click on the terminal to
            // focus it lands here too, and must not throw the line away.
            return false;
        }
        if self.ticker_search.open {
            if let Some((ListKind::TickerSearch, row)) = self.hits.list_at(pos) {
                let matches = self.ticker_matches();
                if let Some(&idx) = matches.get(row) {
                    self.ticker_search.selected = Some(idx);
                    self.handle_ticker_search_key(KeyEvent::from(KeyCode::Enter));
                    return true;
                }
            }
            return false;
        }
        // The reading overlays close on a click outside their box, like
        // Esc; a click inside is a reader steadying the page.
        if self.article_overlay.open {
            if self.hits.outside_modal(pos) {
                self.article_overlay = ArticleOverlay::default();
                return true;
            }
            return false;
        }
        if self.help.open {
            if self.hits.outside_modal(pos) {
                self.help = HelpOverlay::default();
                return true;
            }
            return false;
        }
        if self.category_picker.open {
            if let Some((ListKind::Categories, row)) = self.hits.list_at(pos)
                && row < crate::category::ALL.len()
            {
                self.category_picker.cursor = row;
                self.category_picker.draft.toggle(row);
                return true;
            }
            return false;
        }
        if let Some(idx) = self.hits.pill_at(pos) {
            self.switch_view(idx);
            return true;
        }
        if let Some(idx) = self
            .hits
            .peer_at(pos)
            .or_else(|| self.hits.card_at(pos))
            .filter(|&idx| idx < self.symbols.len())
        {
            if idx != self.selected {
                self.select_symbol(idx);
            }
            return true;
        }
        let Some((kind, row)) = self.hits.list_at(pos) else {
            return false;
        };
        match kind {
            ListKind::Watchlist => {
                if row >= self.symbols.len() {
                    return false;
                }
                if row != self.selected {
                    self.select_symbol(row);
                }
                true
            }
            ListKind::Feed => {
                let len = self.visible_articles().map_or(0, <[_]>::len);
                if row >= len {
                    return false;
                }
                if row == self.news_selected {
                    // The row under the cursor again: open it, like v.
                    self.article_overlay = ArticleOverlay {
                        open: true,
                        ..Default::default()
                    };
                } else {
                    self.news_selected = row;
                    self.card_scroll = 0;
                }
                true
            }
            ListKind::NewsStrip => {
                // The strip exposes no articles of its own (v and Enter are
                // inert there); the bundle it draws is the News view's.
                let key = self.news_cache_key();
                let len = self.feeds.get(&key).map_or(0, |b| b.articles.len());
                if row >= len {
                    return false;
                }
                self.switch_view(ui::view_index(ViewId::News));
                self.news_selected = row;
                true
            }
            ListKind::Portfolio => {
                if row >= self.positions.len() {
                    return false;
                }
                let delta = row as isize - self.portfolio_selected as isize;
                self.move_portfolio(delta);
                true
            }
            ListKind::Calendar => {
                let Some(Some(index)) = self.hits.calendar_rows.get(row).copied() else {
                    return false;
                };
                let rows = ui::calendar::agenda(self, self.now());
                self.choose_calendar_row(&rows, index);
                true
            }
            ListKind::Categories | ListKind::TickerSearch => false,
        }
    }
}

/// `at` moved `dir` steps, held within `0..=last`.
fn step(at: usize, dir: i16, last: usize) -> usize {
    if dir < 0 {
        at.saturating_sub(1)
    } else {
        (at + 1).min(last)
    }
}
