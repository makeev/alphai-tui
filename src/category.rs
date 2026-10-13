//! The news feed's categories and the reader's choice among them.
//!
//! Every article is filed under one of the API's 14 categories
//! (`NewsCategory` in the schema), and `/api/news/` filters on them on the
//! server, like it does on the score: a category left out never takes a
//! slot on the page. The choice is kept as the set of hidden categories,
//! and a request names whichever list is shorter, so "only earnings" and
//! "everything but crypto" both go out as one short parameter.

/// One category: the name on the wire, the name the picker spells out, and
/// the label of the list column.
pub struct Category {
    pub api: &'static str,
    pub name: &'static str,
    pub short: &'static str,
}

const fn cat(api: &'static str, name: &'static str, short: &'static str) -> Category {
    Category { api, name, short }
}

/// Every category in picker order: the company's own news first, then the
/// market around it, then the wider world, and the catch-all last.
pub const ALL: [Category; 14] = [
    cat("earnings", "earnings", "earnings"),
    cat("corporate_actions", "corporate actions", "corporate"),
    cat("mergers_acquisitions", "mergers & acquisitions", "m&a"),
    cat("insider", "insider trades", "insider"),
    cat("ipo", "IPOs", "ipo"),
    cat("market_movers", "market movers", "movers"),
    cat("sector_analysis", "sector analysis", "sector"),
    cat("technology", "technology", "tech"),
    cat("regulation", "regulation", "regulation"),
    cat("macro_economy", "macro economy", "macro"),
    cat("geopolitics", "geopolitics", "geopolitics"),
    cat("commodities", "commodities", "commodities"),
    cat("crypto", "crypto", "crypto"),
    cat("other", "other", "other"),
];

/// Columns the list gives the category: the longest label, so none is cut.
pub const COLUMN_WIDTH: u16 = 11;

/// The index of a category by any of its spellings (the API name, the full
/// name or the column label), ignoring case and surrounding blanks.
pub fn find(name: &str) -> Option<usize> {
    let name = name.trim();
    ALL.iter().position(|c| {
        [c.api, c.name, c.short]
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name))
    })
}

/// The category an article's `enrichment.category` names, if it is one the
/// app knows.
pub fn by_api(api: &str) -> Option<&'static Category> {
    ALL.iter().find(|c| c.api == api)
}

/// Which categories the news feeds show. The default shows all of them and
/// sends no parameter at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Categories {
    /// Bit `i` set = `ALL[i]` hidden.
    hidden: u16,
}

impl Categories {
    /// Only the categories at `indices`. An empty list would show nothing,
    /// which no request can ask for, so it keeps every category instead.
    pub fn only(indices: impl IntoIterator<Item = usize>) -> Self {
        let shown = indices.into_iter().fold(0u16, |m, i| m | bit(i));
        if shown == 0 {
            return Self::default();
        }
        Self {
            hidden: !shown & MASK,
        }
    }

    /// Every category except those at `indices` (at least one stays).
    pub fn without(indices: impl IntoIterator<Item = usize>) -> Self {
        let hidden = indices.into_iter().fold(0u16, |m, i| m | bit(i)) & MASK;
        if hidden == MASK {
            return Self::default();
        }
        Self { hidden }
    }

    pub fn shows(self, i: usize) -> bool {
        self.hidden & bit(i) == 0
    }

    pub fn is_all(self) -> bool {
        self.hidden == 0
    }

    pub fn shown(self) -> usize {
        ALL.len() - self.hidden.count_ones() as usize
    }

    /// Show or hide one category. Hiding the last one shown is refused
    /// (returns false): an empty feed is not a filter anyone means.
    pub fn toggle(&mut self, i: usize) -> bool {
        if self.shows(i) && self.shown() == 1 {
            return false;
        }
        self.hidden ^= bit(i);
        true
    }

    /// The request parameter, or None when every category is shown.
    ///
    /// The shorter list goes out, and that also keeps what the reader meant
    /// when the server adds a category: a few picked reads as "only these"
    /// and stays that way, a few dropped reads as "not these" and lets the
    /// newcomer in. A tie names the shown ones.
    pub fn param(self) -> Option<(&'static str, String)> {
        if self.is_all() {
            return None;
        }
        let name_shown = self.shown() <= ALL.len() - self.shown();
        let list: Vec<&str> = ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| self.shows(*i) == name_shown)
            .map(|(_, c)| c.api)
            .collect();
        let key = if name_shown {
            "category"
        } else {
            "exclude_categories"
        };
        Some((key, list.join(",")))
    }

    /// Short description for panel titles: the shown categories when there
    /// are one or two, the hidden ones when there are one or two, else a
    /// count. None when every category is shown.
    pub fn label(self) -> Option<String> {
        if self.is_all() {
            return None;
        }
        let names = |shown: bool| -> Vec<&str> {
            ALL.iter()
                .enumerate()
                .filter(|(i, _)| self.shows(*i) == shown)
                .map(|(_, c)| c.short)
                .collect()
        };
        let hidden = ALL.len() - self.shown();
        Some(if self.shown() <= 2 {
            names(true).join(", ")
        } else if hidden <= 2 {
            format!("without {}", names(false).join(", "))
        } else {
            format!("{} of {} categories", self.shown(), ALL.len())
        })
    }
}

const MASK: u16 = (1 << ALL.len()) - 1;

fn bit(i: usize) -> u16 {
    if i < ALL.len() { 1 << i } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(name: &str) -> usize {
        find(name).unwrap()
    }

    #[test]
    fn every_api_category_is_listed_once_and_fits_the_column() {
        // The `NewsCategory` enum of the openapi schema (1.51.0).
        let api = [
            "earnings",
            "mergers_acquisitions",
            "regulation",
            "macro_economy",
            "sector_analysis",
            "market_movers",
            "technology",
            "commodities",
            "crypto",
            "ipo",
            "geopolitics",
            "insider",
            "corporate_actions",
            "other",
        ];
        for name in api {
            assert_eq!(ALL.iter().filter(|c| c.api == name).count(), 1, "{name}");
        }
        assert_eq!(ALL.len(), api.len());
        for c in &ALL {
            assert!(c.short.len() <= COLUMN_WIDTH as usize, "{} is cut", c.short);
            // Spellings must not collide, or `find` would pick the wrong one.
            for n in [c.api, c.name, c.short] {
                assert_eq!(ALL[find(n).unwrap()].api, c.api, "{n}");
            }
        }
    }

    #[test]
    fn find_takes_any_spelling() {
        assert_eq!(ALL[idx("m&a")].api, "mergers_acquisitions");
        assert_eq!(
            ALL[idx(" Mergers & Acquisitions ")].api,
            "mergers_acquisitions"
        );
        assert_eq!(ALL[idx("MACRO_ECONOMY")].api, "macro_economy");
        assert_eq!(find("weather"), None);
    }

    #[test]
    fn the_request_names_the_shorter_list() {
        assert_eq!(Categories::default().param(), None);
        assert_eq!(
            Categories::only([idx("earnings"), idx("m&a")]).param(),
            Some(("category", "earnings,mergers_acquisitions".into()))
        );
        assert_eq!(
            Categories::without([idx("crypto"), idx("other")]).param(),
            Some(("exclude_categories", "crypto,other".into()))
        );
        // Seven of fourteen is a tie: the shown ones go out.
        let half = Categories::only(0..7);
        assert_eq!(half.param().unwrap().0, "category");
        assert_eq!(
            Categories::only(0..8).param().unwrap().0,
            "exclude_categories"
        );
    }

    #[test]
    fn the_last_shown_category_cannot_be_hidden() {
        let mut only = Categories::only([idx("earnings")]);
        assert!(!only.toggle(idx("earnings")));
        assert!(only.shows(idx("earnings")));
        assert!(only.toggle(idx("crypto")));
        assert_eq!(only.shown(), 2);
        // Lists that would show nothing fall back to everything.
        assert!(Categories::only([]).is_all());
        assert!(Categories::without(0..ALL.len()).is_all());
    }

    #[test]
    fn labels_say_what_is_left() {
        assert_eq!(Categories::default().label(), None);
        assert_eq!(
            Categories::only([idx("m&a"), idx("earnings")]).label(),
            Some("earnings, m&a".into())
        );
        assert_eq!(
            Categories::without([idx("crypto")]).label(),
            Some("without crypto".into())
        );
        assert_eq!(
            Categories::only(0..5).label(),
            Some("5 of 14 categories".into())
        );
    }
}
