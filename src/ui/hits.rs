//! What the last frame put where, for the mouse.
//!
//! The views are stateless renderers over `&mut App`, so there is no widget
//! tree to ask "what is under this cell". Instead every view records, while
//! it draws, the regions a click or the wheel can act on: the header pills,
//! the rail's peer tickers, the rows of each list with the index of the
//! first drawn row, the text panes that scroll, the Summary cards and the
//! modal on top, if any. `App::handle_mouse` reads the record of the frame
//! on screen; `ui::draw` starts every frame from an empty one, so a region
//! that was not drawn cannot be hit.

use ratatui::layout::{Position, Rect};

/// A list of rows one cell tall, hit by row index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// The watchlist table (Table view and Split).
    Watchlist,
    /// A news or insider feed with a cursor (News and Insider views).
    Feed,
    /// The Split news strip: no cursor of its own, a click opens the News
    /// view on that row.
    NewsStrip,
    Portfolio,
    /// Rows of the agenda table, including its separators: the row index
    /// maps through `Hits::calendar_rows` to an agenda index.
    Calendar,
    /// The category picker's checklist.
    Categories,
    /// The ticker search results, by result index.
    TickerSearch,
}

/// A text pane the wheel scrolls, by the `App` field it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// The article card pane beside or under a feed (`card_scroll`).
    Card,
    /// The earnings read (`earnings_scroll`).
    Earnings,
}

/// The overlay on top of the view, when one is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    Help,
    Article,
    Categories,
    Settings,
    Prompt,
    TickerSearch,
}

#[derive(Clone, Debug)]
pub struct ListHit {
    /// The rows themselves: below the frame and any header row.
    pub area: Rect,
    /// Index of the row drawn at `area.y`, so a scrolled table maps right.
    pub first: usize,
    pub kind: ListKind,
}

#[derive(Debug, Default)]
pub struct Hits {
    /// The view pills in the header, by view index.
    pub pills: Vec<(Rect, usize)>,
    /// The peer tickers at the right end of the rail, by watchlist index.
    pub peers: Vec<(Rect, usize)>,
    pub lists: Vec<ListHit>,
    pub scrolls: Vec<(Rect, Scroll)>,
    /// The Summary cards, by watchlist index.
    pub cards: Vec<(Rect, usize)>,
    /// Agenda index of each row the Calendar table drew, in table order;
    /// `None` for a separator row.
    pub calendar_rows: Vec<Option<usize>>,
    /// The open overlay's box: a click outside a reading overlay closes it.
    pub modal: Option<(Rect, Modal)>,
}

impl Hits {
    /// Records the rows of a list drawn in `area` (frame and header already
    /// excluded) whose first drawn row has index `first`.
    pub fn list(&mut self, area: Rect, first: usize, kind: ListKind) {
        if !area.is_empty() {
            self.lists.push(ListHit { area, first, kind });
        }
    }

    /// The rows of a framed table: inside the frame, below `header` rows.
    pub fn rows_area(inner: Rect, header: u16) -> Rect {
        Rect {
            y: inner.y.saturating_add(header),
            height: inner.height.saturating_sub(header),
            ..inner
        }
    }

    /// The list under `pos` and the index of the row there. The index may
    /// run past the list's length (a click on the blank rows under a short
    /// list); callers check it against what they show. The last recorded
    /// list wins where two overlap: overlays draw after the view under
    /// them, so a picker's rows cover the feed rows behind it.
    pub fn list_at(&self, pos: Position) -> Option<(ListKind, usize)> {
        self.lists
            .iter()
            .rev()
            .find(|l| l.area.contains(pos))
            .map(|l| (l.kind, l.first + usize::from(pos.y - l.area.y)))
    }

    pub fn scroll_at(&self, pos: Position) -> Option<Scroll> {
        at(&self.scrolls, pos)
    }

    pub fn pill_at(&self, pos: Position) -> Option<usize> {
        at(&self.pills, pos)
    }

    pub fn peer_at(&self, pos: Position) -> Option<usize> {
        at(&self.peers, pos)
    }

    pub fn card_at(&self, pos: Position) -> Option<usize> {
        at(&self.cards, pos)
    }

    /// Whether `pos` is outside the open overlay's box. False with no
    /// overlay recorded: nothing to be outside of.
    pub fn outside_modal(&self, pos: Position) -> bool {
        self.modal.is_some_and(|(area, _)| !area.contains(pos))
    }
}

/// Last recorded first, like `list_at`.
fn at<T: Copy>(items: &[(Rect, T)], pos: Position) -> Option<T> {
    items
        .iter()
        .rev()
        .find(|(area, _)| area.contains(pos))
        .map(|(_, item)| *item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_map_through_the_first_drawn_index_and_past_the_end() {
        let mut hits = Hits::default();
        hits.list(Rect::new(2, 5, 20, 4), 7, ListKind::Feed);
        // Empty areas are not recorded at all.
        hits.list(Rect::new(0, 0, 0, 0), 0, ListKind::Watchlist);
        assert_eq!(hits.lists.len(), 1);
        assert_eq!(hits.list_at(Position::new(2, 5)), Some((ListKind::Feed, 7)));
        assert_eq!(
            hits.list_at(Position::new(21, 8)),
            Some((ListKind::Feed, 10))
        );
        assert_eq!(hits.list_at(Position::new(22, 8)), None);
        assert_eq!(hits.list_at(Position::new(2, 9)), None);
        assert_eq!(
            Hits::rows_area(Rect::new(1, 1, 10, 5), 1),
            Rect::new(1, 2, 10, 4)
        );
    }

    #[test]
    fn the_list_drawn_last_covers_the_one_under_it() {
        let mut hits = Hits::default();
        hits.list(Rect::new(0, 0, 40, 20), 0, ListKind::Feed);
        hits.list(Rect::new(10, 5, 10, 5), 2, ListKind::Categories);
        assert_eq!(
            hits.list_at(Position::new(12, 6)),
            Some((ListKind::Categories, 3))
        );
        assert_eq!(hits.list_at(Position::new(2, 6)), Some((ListKind::Feed, 6)));
    }

    #[test]
    fn a_click_is_outside_only_when_an_overlay_is_recorded() {
        let mut hits = Hits::default();
        assert!(!hits.outside_modal(Position::new(0, 0)));
        hits.modal = Some((Rect::new(10, 10, 20, 5), Modal::Help));
        assert!(hits.outside_modal(Position::new(0, 0)));
        assert!(!hits.outside_modal(Position::new(15, 12)));
    }
}
