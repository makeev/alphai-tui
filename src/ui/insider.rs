//! The Insider view: the selected ticker's SEC Form 4 filings, with the sale
//! label AlphAI puts on every sale (whether the insider chose to sell, sold
//! under a plan or for tax, or the filing does not tell), the year's
//! activity in dollars by label, the Form 4 chart, and a second mode (x)
//! that lists the year by person. Everything but the filing list comes from
//! the chart bundle the feed's first page carries, so it costs no request.

use chrono::{Days, NaiveDate, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::alphai::{Article, InsiderTrades, TradeEvent, fmt_usd, insider_key};
use crate::app::{App, FeedKind};
use crate::insider_decision::{
    self as label, InsiderDecision, Kind, Person, ROLE_WIDTH, Rollup, display_name, money,
    role_short,
};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::hits::{Button, Hits, ListKind, Scroll};
use crate::ui::insider_chart::{self, Highlight};
use crate::ui::keycap::{self, Pills, push_keycap, push_pill};
use crate::ui::news::{feed_bottom_hint, is_fresh, render_gate, title_cell, title_width};
use crate::ui::{Hint, View, ViewId, bigtext, ellipsize};

pub struct InsiderView;

use crate::insider_decision::YEAR_DAYS;
/// The tiles replace the two header lines from this size of the view up;
/// below it they would starve the list.
const TILES_MIN_WIDTH: u16 = 120;
const TILES_MIN_HEIGHT: u16 = 44;
const TILE_HEIGHT: u16 = 7;
/// List widths at which the stake and the role columns join the filings.
const STAKE_MIN_WIDTH: u16 = 88;
const ROLE_MIN_WIDTH: u16 = 70;
/// Width of the people table's share bar at its longest.
const SHARE_BAR: usize = 20;
/// Rows of the selected person's pane when it sits under the table.
const PERSON_PANE_HEIGHT: u16 = 8;
/// The header's share bar: never shorter, so the sums beside it fit first,
/// and never longer than it reads as a bar.
const BAR_MIN: usize = 16;
const BAR_MAX: usize = 32;
/// Columns an owner's name keeps however the Why column wants to grow.
const NAME_MIN: u16 = 12;

impl View for InsiderView {
    fn id(&self) -> ViewId {
        ViewId::Insider
    }

    fn title(&self) -> &'static str {
        "Insider"
    }

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::act(&[Action::Up, Action::Down], "row"),
            Hint::act(&[Action::Left, Action::Right], "ticker"),
            Hint::act(&[Action::PriceAlert], "alert"),
            Hint::act(&[Action::Open], "open"),
            Hint::act(&[Action::Card], "card"),
            Hint::act(&[Action::ScoreUp, Action::ScoreDown], "size"),
            Hint::act(&[Action::CycleScope], "filter"),
            Hint::act(&[Action::InsiderChart], "chart"),
            Hint::act(&[Action::CycleLayout], "people"),
            Hint::act(&[Action::Help], "help"),
        ];
        HINTS
    }

    /// The people mode has no card and moves a person, not a row.
    fn hints_for(&self, app: &App) -> &'static [Hint] {
        const PEOPLE: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::act(&[Action::Up, Action::Down], "person"),
            Hint::act(&[Action::Left, Action::Right], "ticker"),
            Hint::act(&[Action::PriceAlert], "alert"),
            Hint::act(&[Action::Open], "open"),
            Hint::act(&[Action::ScoreUp, Action::ScoreDown], "size"),
            Hint::act(&[Action::CycleScope], "filter"),
            Hint::act(&[Action::InsiderChart], "chart"),
            Hint::act(&[Action::CycleLayout], "filings"),
            Hint::act(&[Action::Help], "help"),
        ];
        if app.insider_people {
            PEOPLE
        } else {
            self.hints()
        }
    }

    fn feed_shown(&self) -> Option<FeedKind> {
        Some(FeedKind::Insider)
    }

    fn navigates_articles(&self) -> bool {
        true
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let symbol = app.selected_symbol().to_string();
        let key = insider_key(&symbol);
        let theme = app.theme;
        let gate = theme.panel_titled(format!(" Insider · {symbol} (SEC Form 4) "));
        if render_gate(f, area, &gate, app, &key) {
            return;
        }
        let bundle = &app.feeds[&key];
        let trades = bundle.insider_trades();
        let today = app.now().date_naive();
        let year_from = today - Days::new(YEAR_DAYS - 1);
        let year = trades.map(|t| Rollup::over(t.chart_events.iter(), year_from, today));
        // Labelled when the year's sales carry labels, or when it has no
        // sales at all on a server known to label them (a ticker with only
        // purchases has nothing to label, and keeps the labelled view).
        let labelled = year
            .as_ref()
            .is_some_and(|r| r.has_decisions || (app.insider_labels_seen && r.sold.count == 0));
        let tiles = labelled && area.width >= TILES_MIN_WIDTH && area.height >= TILES_MIN_HEIGHT;
        let head_h = if tiles { TILE_HEIGHT } else { 2 };

        // The year's people once per frame: the header names them, and the
        // people mode lists those the label filter keeps.
        let everyone = trades
            .map(|t| label::people(t.chart_events.iter(), year_from, today))
            .unwrap_or_default();
        let people_mode = app.insider_people;
        let people: Vec<Person> = if people_mode {
            everyone
                .iter()
                .filter(|p| p.keeps(app.insider_decision))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if people_mode {
            app.insider_person = app.insider_person.min(people.len().saturating_sub(1));
        }
        let fits = area.width >= CARD_COLUMN_MIN_WIDTH;
        app.insider_card_fits = fits;
        // What the list needs below the chart: in the filings, a few rows
        // and the detail pane of a narrow terminal; in the people mode, the
        // table's header and up to five people, plus the person's pane
        // stacked under it on a narrow terminal. The chart gives way first.
        let pane_h = if people_mode && !fits {
            PERSON_PANE_HEIGHT
        } else {
            0
        };
        let list_need = if people_mode {
            3 + people.len().clamp(1, 5) as u16 + pane_h
        } else {
            6 + 5
        };
        let chart_h = match (app.insider_chart.days(), trades) {
            (Some(_), Some(t)) if !t.chart_events.is_empty() => {
                insider_chart::panel_height(area.height.saturating_sub(head_h + list_need))
            }
            _ => 0,
        };
        let detail_h = if fits || people_mode { 0 } else { 6 };
        let [head, chart_area, rest, detail] = Layout::vertical([
            Constraint::Length(head_h),
            Constraint::Length(chart_h),
            Constraint::Min(3),
            Constraint::Length(detail_h),
        ])
        .areas(area);

        match &year {
            Some(r) if tiles => render_tiles(f, head, r, &everyone, &theme),
            Some(r) if labelled => f.render_widget(
                Paragraph::new(label_lines(r, &everyone, head.width, &theme)),
                head,
            ),
            _ => f.render_widget(summary_lines(trades, &theme), head),
        }

        if let (true, Some(t)) = (chart_h > 0, trades) {
            let selected_uid = bundle
                .articles
                .get(app.news_selected)
                .map(|a| a.original.uid.as_str())
                .filter(|u| !u.is_empty());
            let highlight = match (people_mode, people.get(app.insider_person), selected_uid) {
                (true, Some(p), _) => Highlight::Person(&p.name),
                (true, None, _) => Highlight::None,
                (false, _, Some(uid)) => Highlight::Uid(uid),
                (false, _, None) => Highlight::None,
            };
            let key = app.keymap.labels(&[Action::InsiderChart]);
            let buttons = insider_chart::render(
                f,
                chart_area,
                t,
                app.insider_chart,
                highlight,
                labelled,
                today,
                &key,
                &theme,
            );
            app.hits.buttons.extend(buttons);
        }

        if people_mode {
            render_people(f, rest, app, &symbol, &people, labelled, today);
            return;
        }

        let fits_card = fits && app.insider_card;
        let (list_area, card_area) = if fits_card {
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(rest);
            (l, Some(r))
        } else {
            (rest, None)
        };
        let score = app.insider_min_score;
        let (mut block, buttons) = list_panel(
            app,
            list_area,
            &format!(" Insider · {symbol} "),
            labelled.then_some(app.insider_decision),
            (score > 1).then_some(score),
        );
        app.hits.buttons.extend(buttons);
        let bundle = &app.feeds[&key];
        // The chart draws every filing of the year; a raised size filter
        // hides the small ones from the list. Say how many and how to get
        // them back, or the chart's extra marks read as lost rows.
        if let Some(n) = trades
            .map(|t| hidden_by_score(t, &bundle.articles, app.insider_decision))
            .filter(|&n| n > 0 && score > 1)
        {
            // Widest form that leaves the left hint its room, else none:
            // a right title cut from the left would misread the count. The
            // key that shows them is a keycap a click presses too.
            let down = keycap::score_keys(app).0;
            let mut long = vec![Span::styled(
                format!(
                    " {} hidden by score {score}+ · ",
                    plural(n, "smaller filing", "smaller filings")
                ),
                theme.subtle(),
            )];
            let mut long_pills = Pills::new();
            if !down.is_empty() {
                let button = Some(Button::Score(-1));
                keycap::push_cap(&mut long, &mut long_pills, &down, button, &theme);
                long.push(Span::styled(" shows them ", theme.subtle()));
            }
            let short = Line::from(format!(" {n} hidden by score {score}+ ")).style(theme.subtle());
            let room = (list_area.width as usize).saturating_sub(2 + 26);
            let forms = [(Line::from(long), long_pills), (short, Pills::new())];
            if let Some((line, pills)) = forms.into_iter().find(|(l, _)| l.width() <= room) {
                let inner_right = list_area.right().saturating_sub(1);
                let x0 = inner_right.saturating_sub(line.width() as u16);
                let y = list_area.bottom().saturating_sub(1);
                app.hits
                    .buttons
                    .extend(keycap::place(pills, x0, y, inner_right));
                block = block.title_bottom(line.right_aligned());
            }
        }
        let at_edge = app.news_selected + 1 >= bundle.articles.len();
        if let Some(hint) = feed_bottom_hint(
            bundle.gated,
            bundle.page_error.as_deref(),
            bundle.next_cursor.is_some(),
            app.is_loading(&key),
            at_edge,
            &theme,
        ) {
            block = block.title_bottom(hint);
        }

        if bundle.articles.is_empty() {
            let msg = empty_message(app, &symbol, app.is_loading(&key));
            f.render_widget(
                Paragraph::new(Line::from(msg).style(theme.subtle())).block(block),
                list_area,
            );
            return;
        }

        let now = app.now();
        let stakes = trades.map(stake_by_uid).unwrap_or_default();
        let (rows, widths, header) = if labelled {
            let mut cols = Columns::for_width(list_area.width);
            let why_need = |detail: bool| {
                bundle
                    .articles
                    .iter()
                    .map(|a| why_text(a, detail).chars().count() + 2)
                    .max()
                    .unwrap_or(5)
                    .clamp(5, 36) as u16
            };
            // The name keeps at least NAME_MIN columns: the reason's detail
            // goes first, then the Why column itself is cut.
            let room = cols.why_room(list_area.width);
            let mut why_w = why_need(cols.why_detail);
            if cols.why_detail && why_w > room {
                cols.why_detail = false;
                why_w = why_need(false);
            }
            let why_w = why_w.min(room).max(5);
            let widths = cols.widths(why_w);
            let title_w = title_width(&widths, list_area.width, true);
            let rows: Vec<Row> = bundle
                .articles
                .iter()
                .map(|a| {
                    labelled_row(
                        a,
                        &cols,
                        &stakes,
                        now,
                        &theme,
                        app.is_unseen(&key, a),
                        title_w,
                    )
                })
                .collect();
            (rows, widths, cols.header())
        } else {
            let widths = vec![
                Constraint::Length(7), // transaction date, when the filing supplies it
                Constraint::Length(5), // trade side, not AI sentiment
                Constraint::Length(8), // trade value
                Constraint::Min(6),    // reporting owner, or the legacy headline
                Constraint::Length(4), // 10b5-1 plan
            ];
            let title_w = title_width(&widths, list_area.width, true);
            let rows: Vec<Row> = bundle
                .articles
                .iter()
                .map(|a| filing_row(a, now, &theme, app.is_unseen(&key, a), title_w))
                .collect();
            let header = vec!["Date", "Side", "Value", "Insider", "Plan"];
            (rows, widths, header)
        };
        let rows_area = Hits::rows_area(block.inner(list_area), 1);
        let table = Table::new(rows, widths)
            .block(block)
            .header(Row::new(header).style(theme.subtle()))
            .row_highlight_style(theme.selected())
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);
        app.hits
            .list(rows_area, app.news_table_state.offset(), ListKind::Feed);

        // The chart bundle knows more about the selected filing than the
        // feed row does (stake moved, tranches, late flag): join by uid and
        // hand it to the card's structured facts.
        let bundle = &app.feeds[&key];
        let trades = bundle.insider_trades();
        let extra = bundle
            .articles
            .get(app.news_selected)
            .filter(|a| !a.original.uid.is_empty())
            .and_then(|a| event_extra(trades, &a.original.uid));
        let selected = bundle.articles.get(app.news_selected);
        if let Some(card_area) = card_area {
            let mut scroll = app.card_scroll;
            crate::ui::article::render_pane_with(
                f,
                card_area,
                selected,
                &symbol,
                None,
                extra,
                &[("pgup/pgdn", "scroll"), ("v", "hide")],
                &mut scroll,
                &theme,
            );
            app.card_scroll = scroll;
            app.hits.scrolls.push((card_area, Scroll::Card));
        } else if detail.height > 0 {
            crate::ui::article::render_pane_with(
                f,
                detail,
                selected,
                &symbol,
                None,
                extra,
                &[("⏎", "open"), ("v", "card")],
                &mut 0,
                &theme,
            );
        }
    }
}

/// Below this width the card column does not fit beside a readable list.
const CARD_COLUMN_MIN_WIDTH: u16 = 90;

/// An Insider list's panel: the heading with the label filter's pills on
/// the left, the switch between the filings and the people on the right.
/// Both say their key and both take a click, so neither mode hides behind
/// the footer. The switch goes when the title has no room for it.
/// Returns the block and the pills to record for the mouse.
fn list_panel(
    app: &App,
    area: Rect,
    heading: &str,
    filter: Option<InsiderDecision>,
    min_score: Option<u8>,
) -> (Block<'static>, Vec<(Rect, Button)>) {
    let theme = app.theme;
    let keys = Keys {
        filter: app.keymap.labels(&[Action::CycleScope]),
        score: keycap::score_keys(app),
    };
    let mode_key = app.keymap.labels(&[Action::CycleLayout]);
    // The switch outranks the score's keys: the stepper stays only while
    // the switch keeps a form beside it, else the score goes back to text.
    let fit = |stepper: bool| {
        let (left, pills) = list_title(heading, filter, &keys, min_score, stepper, &theme, area);
        let room = usize::from(area.width.saturating_sub(2)).saturating_sub(left.width() + 1);
        let right = mode_switch(app.insider_people, &mode_key, room, &theme);
        (left, pills, right)
    };
    let (left, left_pills, right) = match fit(true) {
        fitted @ (_, _, Some(_)) => fitted,
        _ => fit(false),
    };

    let mut hits = keycap::place_left(left_pills, area);
    let mut block = theme.panel().title(left);
    if let Some((line, pills)) = right {
        let inner_right = area.right().saturating_sub(1);
        let x0 = inner_right.saturating_sub(line.width() as u16);
        hits.extend(keycap::place(pills, x0, area.y, inner_right));
        block = block.title(line.right_aligned());
    }
    (block, hits)
}

/// The keys a list title names.
struct Keys {
    filter: String,
    score: (String, String),
}

/// A list title: the heading, the label filter as pills behind its key
/// (only the active one where the full set would crowd the title), the
/// size filter.
fn list_title(
    heading: &str,
    filter: Option<InsiderDecision>,
    keys: &Keys,
    min_score: Option<u8>,
    stepper: bool,
    theme: &Theme,
    area: Rect,
) -> (Line<'static>, Pills) {
    let width = area.width;
    let key = keys.filter.as_str();
    let mut spans = vec![Span::styled(
        heading.to_string(),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
    let mut pills = Pills::new();
    if let Some(filter) = filter {
        let next = Some(Button::InsiderFilter(filter.next()));
        if key.is_empty() {
            spans.push(Span::styled("· ", theme.faint()));
        } else {
            push_keycap(&mut spans, &mut pills, key, next, theme);
        }
        let active = Style::new().fg(theme.accent_text).bg(theme.accent);
        if width >= 70 {
            for f in InsiderDecision::ALL {
                let style = if f == filter { active } else { theme.subtle() };
                let button = Some(Button::InsiderFilter(f));
                push_pill(
                    &mut spans,
                    &mut pills,
                    format!(" {} ", f.label()),
                    style,
                    button,
                );
            }
        } else {
            push_pill(
                &mut spans,
                &mut pills,
                format!(" {} ", filter.label()),
                active,
                next,
            );
        }
        spans.push(Span::raw(" "));
    }
    if let Some(score) = min_score {
        // The stepper where the title has room for it, else the bare bar.
        let (mut with_keys, mut with_pills) = (spans.clone(), pills.clone());
        with_keys.push(Span::styled("· ", theme.faint()));
        let score_keys = (keys.score.0.as_str(), keys.score.1.as_str());
        keycap::push_score_stepper(
            &mut with_keys,
            &mut with_pills,
            score,
            score_keys,
            theme.subtle(),
            theme,
        );
        with_keys.push(Span::raw(" "));
        let line = Line::from(with_keys);
        if stepper && line.width() <= usize::from(width.saturating_sub(2)) {
            return (line, with_pills);
        }
        spans.push(Span::styled(format!("· score {score}+ "), theme.subtle()));
    }
    (Line::from(spans), pills)
}

/// The mode switch: both modes as pills behind the key, the current one
/// lit, or just the key and the mode it leads to where that is all that
/// fits in `room`.
fn mode_switch(
    people: bool,
    key: &str,
    room: usize,
    theme: &Theme,
) -> Option<(Line<'static>, Pills)> {
    let active = Style::new().fg(theme.accent_text).bg(theme.accent);
    let toggle = Some(Button::InsiderPeople(!people));
    let start = |spans: &mut Vec<Span<'static>>, pills: &mut Pills| {
        spans.push(Span::raw(" "));
        if !key.is_empty() {
            push_keycap(spans, pills, key, toggle, theme);
        }
    };
    let (mut spans, mut pills) = (Vec::new(), Pills::new());
    start(&mut spans, &mut pills);
    for (mode, is_people) in [("filings", false), ("people", true)] {
        let style = if is_people == people {
            active
        } else {
            theme.subtle()
        };
        let button = Some(Button::InsiderPeople(is_people));
        push_pill(&mut spans, &mut pills, format!(" {mode} "), style, button);
    }
    spans.push(Span::raw(" "));
    let full = Line::from(spans);
    if full.width() <= room {
        return Some((full, pills));
    }
    let (mut spans, mut pills) = (Vec::new(), Pills::new());
    start(&mut spans, &mut pills);
    let target = if people { "filings" } else { "people" };
    push_pill(
        &mut spans,
        &mut pills,
        format!(" {target} "),
        theme.subtle(),
        toggle,
    );
    let short = Line::from(spans);
    (short.width() <= room).then_some((short, pills))
}

/// Filings the chart draws that the size filter keeps out of the list: the
/// year's events published since the oldest row loaded (the feed runs in
/// publication order, so anything older is just not paged in yet) that are
/// not rows, under the same label filter.
fn hidden_by_score(t: &InsiderTrades, rows: &[Article], filter: InsiderDecision) -> usize {
    let listed: std::collections::HashSet<&str> =
        rows.iter().map(|a| a.original.uid.as_str()).collect();
    let Some(oldest) = rows.iter().filter_map(Article::published).min() else {
        return 0;
    };
    t.chart_events
        .iter()
        .filter(|e| {
            e.news_published
                .as_deref()
                .and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok())
                .is_some_and(|p| p >= oldest)
        })
        // Events without an article or filed as another's copy are never
        // rows, whatever the filter.
        .filter(|e| e.has_article && e.duplicate_of.is_none())
        .filter(|e| {
            e.news_uid
                .as_deref()
                .is_some_and(|uid| !uid.is_empty() && !listed.contains(uid))
        })
        .filter(|e| filter == InsiderDecision::All || filter.keeps(e.decision.as_deref()))
        .count()
}

/// What an empty filing list says, and which key loosens it.
/// What an empty filing list says: which filters emptied it, and the bound
/// keys that loosen them (a remapped key is named as remapped, an unbound
/// one not at all).
fn empty_message(app: &App, symbol: &str, loading: bool) -> String {
    if loading {
        return "loading…".to_string();
    }
    let (filter, score) = (app.insider_decision, app.insider_min_score);
    let what = match filter {
        InsiderDecision::All => "Form 4 activity".to_string(),
        f => format!("{} sales", f.label()),
    };
    let mut keys = Vec::new();
    if filter != InsiderDecision::All
        && let Some(key) = keycap::key_word(app, Action::CycleScope)
    {
        keys.push(format!("{key} changes the filter"));
    }
    if score > 1
        && let Some(key) = keycap::key_word(app, Action::ScoreDown)
    {
        keys.push(format!("{key} lowers the score"));
    }
    let scored = if score > 1 {
        format!(" with score {score}+")
    } else {
        String::new()
    };
    if keys.is_empty() {
        format!("no {what} for {symbol} in the feed")
    } else {
        format!("no {what} for {symbol}{scored} ({})", keys.join(", "))
    }
}

/// The filing list's columns at one list width: the stake and the role
/// join as the list widens, the reason detail at the widest.
struct Columns {
    stake: bool,
    role: bool,
    why_detail: bool,
}

impl Columns {
    fn for_width(width: u16) -> Self {
        Self {
            stake: width >= STAKE_MIN_WIDTH,
            role: width >= ROLE_MIN_WIDTH,
            why_detail: width >= STAKE_MIN_WIDTH,
        }
    }

    /// Columns the Why cell may take at `width` and still leave the name
    /// `NAME_MIN` (borders, the cursor marker and the gaps counted).
    fn why_room(&self, width: u16) -> u16 {
        let mut fixed = 7 + 8 + 2 + 2; // date, value, borders, marker
        let mut cols = 3; // date, value, name
        if self.stake {
            fixed += 7;
            cols += 1;
        }
        if self.role {
            fixed += ROLE_WIDTH as u16;
            cols += 1;
        }
        // One gap between each pair of columns, the Why column included.
        width.saturating_sub(fixed + cols + NAME_MIN)
    }

    fn widths(&self, why: u16) -> Vec<Constraint> {
        let mut w = vec![Constraint::Length(7), Constraint::Length(8)];
        if self.stake {
            w.push(Constraint::Length(7));
        }
        w.push(Constraint::Min(8));
        if self.role {
            w.push(Constraint::Length(ROLE_WIDTH as u16));
        }
        w.push(Constraint::Length(why));
        w
    }

    fn header(&self) -> Vec<&'static str> {
        let mut h = vec!["Date", "   Value"];
        if self.stake {
            h.push("  Stake");
        }
        h.push("Insider");
        if self.role {
            h.push("Role");
        }
        h.push("Why");
        h
    }
}

/// The Form 4 fields of a feed row, if it has the structured block.
fn row_kind(a: &Article) -> Kind {
    match &a.insider {
        Some(i) => label::kind(
            i.side.as_deref(),
            i.transaction_code.as_deref(),
            i.decision.as_deref(),
        ),
        None => Kind::Other,
    }
}

/// The Why words of a row as plain text, for measuring the column.
fn why_text(a: &Article, detail: bool) -> String {
    let Some(i) = &a.insider else {
        return String::new();
    };
    let (main, more) = label::why_words(row_kind(a), i.decision_reason.as_deref(), i.is_10b5_1);
    match more.filter(|_| detail) {
        Some(more) => format!("{main} · {more}"),
        None => main,
    }
}

/// The Why cell: the glyph in its meaning color, the words in a text tier.
/// A discretionary sale reads in bold body text; the warn color stays on
/// the glyph, where it reads on every theme (as a word it does not on the
/// light ones).
fn why_spans(a: &Article, detail: bool, theme: &Theme) -> Vec<Span<'static>> {
    let Some(i) = &a.insider else {
        return Vec::new();
    };
    let kind = row_kind(a);
    let (main, more) = label::why_words(kind, i.decision_reason.as_deref(), i.is_10b5_1);
    let (glyph_style, word_style) = match kind {
        Kind::Discretionary => (
            Style::new().fg(theme.warn),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Kind::Unclear => (Style::new().fg(theme.text), Style::new().fg(theme.text)),
        Kind::Buy => (Style::new().fg(theme.pos), Style::new().fg(theme.pos)),
        Kind::Sold => (Style::new().fg(theme.neg), theme.subtle()),
        Kind::Routine | Kind::ToIssuer | Kind::Other => (theme.subtle(), theme.subtle()),
    };
    let mut spans = vec![
        Span::styled(format!("{} ", label::glyph(kind)), glyph_style),
        Span::styled(main, word_style),
    ];
    if let Some(more) = more.filter(|_| detail) {
        spans.push(Span::styled(format!(" · {more}"), theme.subtle()));
    }
    spans
}

/// Stake change by article uid, from the chart bundle.
fn stake_by_uid(t: &InsiderTrades) -> std::collections::HashMap<String, String> {
    t.chart_events
        .iter()
        .filter_map(|e| {
            let uid = e.news_uid.clone().filter(|u| !u.is_empty())?;
            let pct = e.stake_change_pct.as_deref()?.parse::<f64>().ok()?;
            Some((uid, format!("{pct:+.1}%")))
        })
        .collect()
}

/// One Form 4 filing with its sale label: no side column (the Why glyph
/// says buy or sell), the owner's name the way a reader says it, bold when
/// the insider chose to sell.
fn labelled_row(
    a: &Article,
    cols: &Columns,
    stakes: &std::collections::HashMap<String, String>,
    now: chrono::DateTime<Utc>,
    theme: &Theme,
    unseen: bool,
    title_w: usize,
) -> Row<'static> {
    let discretionary = row_kind(a) == Kind::Discretionary;
    let strong = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let mut cells = vec![date_cell(a, now, theme)];
    let value = a
        .insider
        .as_ref()
        .and_then(|t| t.total_value_usd.as_deref())
        .map(|v| format!("{:>8}", fmt_usd(v)))
        .unwrap_or_default();
    cells.push(if discretionary {
        Cell::from(value).style(strong)
    } else {
        Cell::from(value)
    });
    if cols.stake {
        let stake = stakes
            .get(&a.original.uid)
            .map(|s| format!("{s:>7}"))
            .unwrap_or_default();
        cells.push(Cell::from(stake).style(theme.subtle()));
    }
    let name = a
        .insider
        .as_ref()
        .map(|t| t.insider_name.as_str())
        .filter(|name| !name.is_empty())
        .map(display_name)
        .unwrap_or_else(|| a.original.title.clone());
    let name_style = if discretionary { strong } else { Style::new() };
    cells.push(title_cell(name, name_style, unseen, theme, title_w));
    if cols.role {
        let role = a
            .insider
            .as_ref()
            .map(|t| {
                role_short(
                    &t.insider_title,
                    t.is_officer,
                    t.is_director,
                    t.is_ten_percent_owner,
                )
            })
            .unwrap_or_default();
        cells.push(Cell::from(role).style(theme.subtle()));
    }
    cells.push(Cell::from(Line::from(why_spans(a, cols.why_detail, theme))));
    Row::new(cells)
}

fn date_cell(a: &Article, now: chrono::DateTime<Utc>, theme: &Theme) -> Cell<'static> {
    let date = a
        .insider
        .as_ref()
        .and_then(|t| t.transaction_date.as_deref())
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map(|d| d.format("%d %b").to_string())
        .unwrap_or_else(|| "?".to_string());
    if is_fresh(a, now) {
        Cell::from(date).style(Style::new().fg(theme.accent))
    } else {
        Cell::from(date).style(theme.subtle())
    }
}

/// A line from zones in order, each zone a list of forms widest first. As
/// many zones as possible go in first, each in its narrowest form (a zone
/// that does not fit is skipped, not the rest of the line cut short, the
/// quote rail's rule); then, left to right, each zone takes the widest form
/// the leftover room allows. So a sum is never dropped for a longer label.
fn fit_line(width: u16, zones: Vec<Vec<Vec<Span<'static>>>>) -> Line<'static> {
    let w =
        |form: &[Span<'static>]| -> usize { form.iter().map(|s| s.content.chars().count()).sum() };
    let width = width as usize;
    let mut chosen: Vec<Option<usize>> = vec![None; zones.len()];
    let mut used = 0;
    for (i, forms) in zones.iter().enumerate() {
        let Some(narrow) = forms.len().checked_sub(1) else {
            continue;
        };
        let need = w(&forms[narrow]);
        if used + need <= width {
            chosen[i] = Some(narrow);
            used += need;
        }
    }
    for (i, forms) in zones.iter().enumerate() {
        let Some(k) = chosen[i] else {
            continue;
        };
        for j in 0..k {
            let extra = w(&forms[j]) - w(&forms[k]);
            if used + extra <= width {
                used += extra;
                chosen[i] = Some(j);
                break;
            }
        }
    }
    let mut out = Vec::new();
    for (forms, pick) in zones.into_iter().zip(chosen) {
        if let Some(j) = pick {
            out.extend(forms.into_iter().nth(j).unwrap_or_default());
        }
    }
    Line::from(out)
}

/// The share of sold dollars the insiders chose to sell, as a bar.
fn share_bar(share: f64, len: usize, theme: &Theme) -> Vec<Span<'static>> {
    let filled = ((share * len as f64).round() as usize).min(len);
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(theme.warn)),
        Span::styled("▒".repeat(len - filled), theme.faint()),
    ]
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The two header lines with sale labels: the year's sales in dollars by
/// label with the share bar, then who chose to sell and who sold routinely.
fn label_lines(r: &Rollup, people: &[Person], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let strong = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let sub = theme.subtle();
    let warn = Style::new().fg(theme.warn);
    let s = |t: String, st: Style| Span::styled(t, st);
    let mut zones: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
    if r.sold_count() == 0 && r.buys.count == 0 {
        return vec![
            Line::from(" 12m  no Form 4 activity").style(sub),
            Line::from(""),
        ];
    }
    zones.push(vec![vec![
        s(" 12m  ".into(), sub),
        s(format!("sold {}", money(r.sold_usd())), strong),
    ]]);
    let share = r.discretionary_share();
    let bar_form = |len: usize| -> Vec<Span<'static>> {
        let mut f = vec![Span::styled("  ", theme.subtle())];
        f.extend(share_bar(share, len, theme));
        f
    };
    // The bar starts short so the sums always come first, and grows into
    // whatever the line has left.
    let mut bar_zone = None;
    if r.sold_count() > 0 {
        bar_zone = Some(zones.len());
        zones.push(vec![bar_form(BAR_MIN)]);
        let pct = format!("{:.0}%", share * 100.0);
        let d = &r.discretionary;
        zones.push(if d.count > 0 {
            vec![
                vec![
                    s("  ▼ ".into(), warn),
                    s(
                        format!("{pct} discretionary {} ×{}", money(d.usd), d.count),
                        strong,
                    ),
                ],
                vec![
                    s("  ▼ ".into(), warn),
                    s(format!("{pct} {} ×{}", money(d.usd), d.count), strong),
                ],
            ]
        } else {
            vec![vec![s("  no discretionary sales".into(), sub)]]
        });
        let rt = &r.routine;
        if rt.count > 0 {
            // The reasons are the routine sum's widest form, so the words
            // that explain the glyphs win the room before them.
            let reasons: Vec<String> = r
                .reasons
                .iter()
                .take(2)
                .map(|(reason, n)| format!("{} {n}", label::reason_label(reason)))
                .collect();
            let words = format!(" · ▾ routine {} ×{}", money(rt.usd), rt.count);
            let mut forms = Vec::new();
            if !reasons.is_empty() {
                forms.push(vec![s(format!("{words} ({})", reasons.join(", ")), sub)]);
            }
            forms.push(vec![s(words, sub)]);
            forms.push(vec![s(
                format!(" · ▾ {} ×{}", money(rt.usd), rt.count),
                sub,
            )]);
            zones.push(forms);
        }
        if r.unclear.count > 0 {
            zones.push(vec![vec![s(
                format!(" · ? unclear {} ×{}", money(r.unclear.usd), r.unclear.count),
                Style::new().fg(theme.text),
            )]]);
        }
    }
    zones.push(if r.buys.count > 0 {
        vec![vec![s(
            format!(" · ▲ bought {} ×{}", money(r.buys.usd), r.buys.count),
            Style::new().fg(theme.pos),
        )]]
    } else {
        vec![vec![s(" · no buys".into(), sub)]]
    });
    zones.push(vec![vec![s(
        format!(" · {}", plural(r.insiders, "insider", "insiders")),
        sub,
    )]]);
    let short = fit_line(width, zones.clone());
    let used: usize = short.spans.iter().map(|s| s.content.chars().count()).sum();
    let has_bar = short
        .spans
        .iter()
        .any(|s| s.content.contains('▒') || s.content.contains('█'));
    let first = match bar_zone {
        Some(i) if has_bar && used < width as usize => {
            let grow = (width as usize - used).min(BAR_MAX - BAR_MIN);
            zones[i] = vec![bar_form(BAR_MIN + grow)];
            fit_line(width, zones)
        }
        _ => short,
    };

    // Who: the people who chose to sell, largest first, then the largest
    // routine sellers, as many as the line holds.
    let mut chose: Vec<&Person> = people.iter().filter(|p| p.discretionary > 0.0).collect();
    chose.sort_by(|a, b| b.discretionary.total_cmp(&a.discretionary));
    let mut routine: Vec<&Person> = people
        .iter()
        .filter(|p| p.routine > 0.0 && p.discretionary == 0.0)
        .collect();
    routine.sort_by(|a, b| b.routine.total_cmp(&a.routine));
    let mut zones: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
    if chose.is_empty() {
        zones.push(vec![vec![s(" no discretionary sales in 12m".into(), sub)]]);
    } else {
        zones.push(vec![vec![s(
            format!(" chose to sell ({}): ", chose.len()),
            sub,
        )]]);
        for (i, p) in chose.iter().enumerate() {
            let sep = if i == 0 { "" } else { " · " };
            zones.push(vec![vec![
                s(sep.into(), theme.faint()),
                s(display_name(&p.name), Style::new().fg(theme.text)),
                s(format!(" {} ", p.role), sub),
                s(money(p.discretionary), strong),
            ]]);
        }
    }
    // The routine label rides with its first name, so it never dangles at
    // the end of a line that had no room for anyone after it.
    for (i, p) in routine.iter().enumerate() {
        let lead = if i == 0 { "   routine: " } else { " · " };
        zones.push(vec![vec![
            s(lead.into(), if i == 0 { sub } else { theme.faint() }),
            s(display_name(&p.name), Style::new().fg(theme.text)),
            s(format!(" {}", money(p.routine)), sub),
        ]]);
    }
    // A name that does not fit must not leave its neighbours' separators
    // dangling: stop at the first person who does not fit.
    let mut left = width as usize;
    let mut second = Vec::new();
    for forms in zones {
        let form = &forms[0];
        let w: usize = form.iter().map(|s| s.content.chars().count()).sum();
        if w > left {
            break;
        }
        left -= w;
        second.extend(form.iter().cloned());
    }
    vec![first, Line::from(second)]
}

/// The three tiles over the view on a tall terminal: the year's
/// discretionary sales, routine sales and purchases, each with its figure
/// in big digits and two lines on who.
fn render_tiles(f: &mut Frame, area: Rect, r: &Rollup, people: &[Person], theme: &Theme) {
    let [a, b, c] = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .spacing(1)
    .areas(area);
    let top = |pick: &dyn Fn(&Person) -> f64| -> Option<String> {
        people
            .iter()
            .filter(|p| pick(p) > 0.0)
            .max_by(|x, y| pick(x).total_cmp(&pick(y)))
            .map(|p| format!("{}  {}", display_name(&p.name), money(pick(p))))
    };

    let d = &r.discretionary;
    tile(
        f,
        a,
        ("▼", Style::new().fg(theme.warn)),
        " DISCRETIONARY SALES · 12m ",
        (d.count > 0).then(|| money(d.usd)),
        Style::new().fg(theme.warn),
        [
            if d.count > 0 {
                format!(
                    "{} by {}",
                    plural(d.count, "sale", "sales"),
                    plural(d.people.len(), "insider", "insiders")
                )
            } else {
                "no discretionary sales".to_string()
            },
            top(&|p| p.discretionary).unwrap_or_default(),
        ],
        None,
        theme,
    );

    let rt = &r.routine;
    let reasons: Vec<String> = r
        .reasons
        .iter()
        .take(2)
        .map(|(reason, n)| format!("{} {n}", label::reason_label(reason)))
        .collect();
    let unclear = (r.unclear.count > 0)
        .then(|| format!(" ? unclear {} ×{} ", money(r.unclear.usd), r.unclear.count));
    tile(
        f,
        b,
        ("▾", theme.subtle()),
        " ROUTINE SALES · 12m ",
        (rt.count > 0).then(|| money(rt.usd)),
        theme.subtle(),
        [
            if rt.count > 0 {
                format!(
                    "{} by {}",
                    plural(rt.count, "sale", "sales"),
                    plural(rt.people.len(), "insider", "insiders")
                )
            } else {
                "no routine sales".to_string()
            },
            reasons.join(" · "),
        ],
        unclear,
        theme,
    );

    let buys = &r.buys;
    tile(
        f,
        c,
        ("▲", Style::new().fg(theme.pos)),
        " PURCHASES · 12m ",
        (buys.count > 0).then(|| money(buys.usd)),
        Style::new().fg(theme.pos),
        [
            if buys.count > 0 {
                format!(
                    "{} by {}",
                    plural(buys.count, "buy", "buys"),
                    plural(buys.people.len(), "insider", "insiders")
                )
            } else {
                "none in 12m".to_string()
            },
            top(&|p| p.bought).unwrap_or_default(),
        ],
        None,
        theme,
    );
}

/// One tile: a titled panel, its figure in big digits (or `none`), two
/// lines of text, and an optional bottom title.
#[allow(clippy::too_many_arguments)]
fn tile(
    f: &mut Frame,
    area: Rect,
    glyph: (&str, Style),
    heading: &str,
    figure: Option<String>,
    figure_style: Style,
    lines: [String; 2],
    bottom: Option<String>,
    theme: &Theme,
) {
    let mut block: Block = theme.panel().title(Line::from(vec![
        Span::raw(" "),
        Span::styled(glyph.0.to_string(), glyph.1),
        Span::styled(
            heading.to_string(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(bottom) = bottom {
        block = block.title_bottom(Line::from(bottom).style(Style::new().fg(theme.text)));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let room = inner.width.saturating_sub(2) as usize;
    let mut text: Vec<Line> = Vec::new();
    match figure {
        Some(fig) => match bigtext::lines(&fig).filter(|rows| rows[0].chars().count() <= room) {
            Some(rows) => {
                for row in rows {
                    text.push(Line::from(Span::styled(format!(" {row}"), figure_style)));
                }
            }
            None => {
                text.push(Line::from(""));
                text.push(Line::from(Span::styled(
                    format!(" {fig}"),
                    figure_style.add_modifier(Modifier::BOLD),
                )));
                text.push(Line::from(""));
            }
        },
        None => {
            text.push(Line::from(""));
            text.push(Line::from(Span::styled(" none", theme.subtle())));
            text.push(Line::from(""));
        }
    }
    text.push(
        Line::from(format!(" {}", ellipsize(&lines[0], room))).style(Style::new().fg(theme.text)),
    );
    text.push(Line::from(format!(" {}", ellipsize(&lines[1], room))).style(theme.subtle()));
    f.render_widget(Paragraph::new(text), inner);
}

/// The people mode: the year by reporting owner, and the selected owner's
/// events beside (or under, on a narrow terminal) the table.
fn render_people(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    symbol: &str,
    people: &[Person],
    labelled: bool,
    today: NaiveDate,
) {
    let theme = app.theme;
    let (table_area, pane_area) = if area.width >= CARD_COLUMN_MIN_WIDTH {
        let [l, r] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .areas(area);
        (l, r)
    } else {
        // The pane shrinks before the table drops under three people.
        let pane = PERSON_PANE_HEIGHT.min(area.height.saturating_sub(6)).max(3);
        let [l, r] = Layout::vertical([Constraint::Min(3), Constraint::Length(pane)]).areas(area);
        (l, r)
    };
    let (block, buttons) = list_panel(
        app,
        table_area,
        &format!(" Insiders · {symbol} · 12m "),
        labelled.then_some(app.insider_decision),
        None,
    );
    app.hits.buttons.extend(buttons);
    if people.is_empty() {
        let msg = match app.insider_decision {
            // People come from the chart bundle: without it there are none
            // to list, which is not the same as nobody trading.
            _ if app.insider_trades().is_none() => {
                let hint = keycap::key_word(app, Action::Refresh)
                    .map(|k| format!(" ({k} reloads)"))
                    .unwrap_or_default();
                format!("Form 4 chart data unavailable for this ticker{hint}")
            }
            InsiderDecision::All => format!("no Form 4 activity for {symbol} in 12m"),
            f => {
                let hint = keycap::key_word(app, Action::CycleScope)
                    .map(|k| format!(" ({k} changes the filter)"))
                    .unwrap_or_default();
                format!("no {} sales for {symbol} in 12m{hint}", f.label())
            }
        };
        f.render_widget(
            Paragraph::new(Line::from(msg).style(theme.subtle())).block(block),
            area,
        );
        return;
    }

    // Columns from the widest down: the bar goes first, then purchases
    // (only shown when anyone bought), then the routine column.
    let w = table_area.width;
    let bought = people.iter().any(|p| p.bought > 0.0);
    let show_bar = w >= 100;
    let show_bought = bought && w >= 85;
    let show_routine = w >= 70;
    let mut widths = vec![Constraint::Min(12), Constraint::Length(ROLE_WIDTH as u16)];
    let mut header = vec![
        "Insider".to_string(),
        "Role".to_string(),
        format!("{:>13}", "Discretionary"),
    ];
    widths.push(Constraint::Length(13));
    if show_routine {
        widths.push(Constraint::Length(10));
        header.push(format!("{:>10}", "Routine"));
    }
    if show_bought {
        widths.push(Constraint::Length(10));
        header.push(format!("{:>10}", "Bought"));
    }
    if show_bar {
        widths.push(Constraint::Length(SHARE_BAR as u16));
        header.push("Share of $ sold".to_string());
    }
    widths.push(Constraint::Length(6));
    header.push("Last".to_string());

    let max_sold = people
        .iter()
        .map(|p| p.discretionary + p.routine + p.unclear + p.sold)
        .fold(0.0_f64, f64::max);
    let strong = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let dash = |s: &Theme| Cell::from(format!("{:>10}", "·")).style(s.faint());
    let rows: Vec<Row> = people
        .iter()
        .map(|p| {
            let mut cells = vec![
                Cell::from(display_name(&p.name)).style(if p.discretionary > 0.0 {
                    strong
                } else {
                    Style::new()
                }),
                Cell::from(p.role.clone()).style(theme.subtle()),
            ];
            cells.push(if p.discretionary > 0.0 {
                Cell::from(format!("{:>13}", money(p.discretionary))).style(strong)
            } else {
                Cell::from(format!("{:>13}", "·")).style(theme.faint())
            });
            if show_routine {
                cells.push(if p.routine > 0.0 {
                    Cell::from(format!("{:>10}", money(p.routine))).style(theme.subtle())
                } else {
                    dash(&theme)
                });
            }
            if show_bought {
                cells.push(if p.bought > 0.0 {
                    Cell::from(format!("{:>10}", money(p.bought))).style(Style::new().fg(theme.pos))
                } else {
                    dash(&theme)
                });
            }
            if show_bar {
                let sold = p.discretionary + p.routine + p.unclear + p.sold;
                let spans = if sold > 0.0 && max_sold > 0.0 {
                    let len = ((SHARE_BAR as f64 * (sold / max_sold).sqrt()).round() as usize)
                        .clamp(1, SHARE_BAR);
                    share_bar(p.discretionary / sold, len, &theme)
                } else {
                    Vec::new()
                };
                cells.push(Cell::from(Line::from(spans)));
            }
            cells.push(Cell::from(p.last.format("%d %b").to_string()).style(theme.subtle()));
            Row::new(cells)
        })
        .collect();
    let rows_area = Hits::rows_area(block.inner(table_area), 1);
    let table = Table::new(rows, widths)
        .block(block)
        .header(Row::new(header).style(theme.subtle()))
        .row_highlight_style(theme.selected())
        .highlight_symbol("▶ ");
    app.insider_people_state.select(Some(app.insider_person));
    f.render_stateful_widget(table, table_area, &mut app.insider_people_state);
    app.hits.list(
        rows_area,
        app.insider_people_state.offset(),
        ListKind::People,
    );

    if let Some(p) = people.get(app.insider_person) {
        let events: Vec<TradeEvent> = app
            .insider_trades()
            .map(|t| {
                let from = today - Days::new(YEAR_DAYS - 1);
                let mut evs: Vec<TradeEvent> = t
                    .chart_events
                    .iter()
                    .filter(|e| e.insider_name == p.name && e.duplicate_of.is_none())
                    .filter(|e| {
                        e.transaction_date
                            .as_deref()
                            .and_then(label::day)
                            .is_some_and(|d| d >= from && d <= today)
                    })
                    .cloned()
                    .collect();
                evs.sort_by(|a, b| b.transaction_date.cmp(&a.transaction_date));
                evs
            })
            .unwrap_or_default();
        render_person(f, pane_area, p, &events, &theme);
    }
}

/// The selected owner: their name as filed, the figure of what they did in
/// the year, and their events.
fn render_person(f: &mut Frame, area: Rect, p: &Person, events: &[TradeEvent], theme: &Theme) {
    let block = theme.panel().title(Line::from(vec![
        Span::styled(
            format!(" {} ", p.name),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if p.role.is_empty() {
                String::new()
            } else {
                format!("· {} ", p.role)
            },
            theme.subtle(),
        ),
    ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (figure, style, caption) = if p.discretionary > 0.0 {
        (
            p.discretionary,
            Style::new().fg(theme.warn),
            "chose to sell",
        )
    } else if p.routine > 0.0 {
        (p.routine, theme.subtle(), "sold on plans or for tax")
    } else if p.bought > 0.0 {
        (p.bought, Style::new().fg(theme.pos), "bought")
    } else {
        (p.sold + p.unclear, theme.subtle(), "sold")
    };
    let fig = money(figure);
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for e in events {
        let kind = label::kind(
            e.side.as_deref(),
            e.transaction_code.as_deref(),
            e.decision.as_deref(),
        );
        let (main, _) = label::why_words(kind, e.decision_reason.as_deref(), e.is_10b5_1);
        let words = match (kind, e.decision_reason.as_deref()) {
            (Kind::Discretionary, Some(r)) => label::reason_label(r).into_owned(),
            _ => main,
        };
        *reasons.entry(words).or_default() += 1;
    }
    let mut reasons: Vec<(String, usize)> = reasons.into_iter().collect();
    reasons.sort_by_key(|r| std::cmp::Reverse(r.1));
    let filings = plural(events.len(), "filing", "filings");
    let why: Vec<String> = reasons
        .iter()
        .take(2)
        .map(|(w, n)| {
            if reasons.len() == 1 {
                w.clone()
            } else {
                format!("{w} {n}")
            }
        })
        .collect();

    let mut text: Vec<Line> = Vec::new();
    let room = inner.width as usize;
    let side = [caption.to_string(), filings.clone(), why.join(" · ")];
    match bigtext::lines(&fig).filter(|rows| rows[0].chars().count() + 4 + 14 <= room) {
        Some(rows) if inner.height >= 8 => {
            for (row, words) in rows.iter().zip(side.iter()) {
                text.push(Line::from(vec![
                    Span::styled(format!(" {row}"), style),
                    Span::styled(format!("   {words}"), theme.subtle()),
                ]));
            }
        }
        _ => {
            text.push(Line::from(vec![
                Span::styled(format!(" {fig}"), style.add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("  {caption} · {filings} · {}", why.join(" · ")),
                    theme.subtle(),
                ),
            ]));
        }
    }
    text.push(Line::from(""));
    text.push(Line::from("    Date     Value    @ price   Stake  Why").style(theme.subtle()));
    let room_rows = (inner.height as usize).saturating_sub(text.len() + 1);
    for e in events.iter().take(room_rows) {
        let date = e
            .transaction_date
            .as_deref()
            .and_then(label::day)
            .map(|d| d.format("%d %b").to_string())
            .unwrap_or_default();
        let value = e
            .total_value_usd
            .as_deref()
            .map(fmt_usd)
            .unwrap_or_default();
        let price = e
            .avg_price_usd
            .as_deref()
            .and_then(|p| p.parse::<f64>().ok())
            .map(|p| format!("${p:.2}"))
            .unwrap_or_default();
        let stake = e
            .stake_change_pct
            .as_deref()
            .and_then(|p| p.parse::<f64>().ok())
            .map(|p| format!("{p:+.1}%"))
            .unwrap_or_default();
        let kind = label::kind(
            e.side.as_deref(),
            e.transaction_code.as_deref(),
            e.decision.as_deref(),
        );
        // The glyph already says discretionary: the reason says more.
        let (main, detail) = label::why_words(kind, e.decision_reason.as_deref(), e.is_10b5_1);
        let main = match (kind, detail) {
            (Kind::Discretionary, Some(reason)) => reason,
            _ => main,
        };
        let glyph_style = match kind {
            Kind::Discretionary => Style::new().fg(theme.warn),
            Kind::Buy => Style::new().fg(theme.pos),
            Kind::Unclear => Style::new().fg(theme.text),
            _ => theme.subtle(),
        };
        text.push(Line::from(vec![
            Span::styled(format!(" {date:>7}"), theme.subtle()),
            Span::raw(format!(" {value:>9}")),
            Span::raw(format!(" {price:>10}")),
            Span::styled(format!(" {stake:>7}"), theme.subtle()),
            Span::styled(format!("  {}", label::glyph(kind)), glyph_style),
            Span::styled(format!(" {main}"), theme.subtle()),
        ]));
    }
    if events.len() > room_rows {
        text.push(
            Line::from(format!("  +{} more", events.len() - room_rows)).style(theme.subtle()),
        );
    }
    f.render_widget(Paragraph::new(text), inner);
}

/// Detail-pane extras for one filing, joined from the chart bundle by the
/// article uid: the stake share it moved, the tranche count of the folded
/// group, and the late-filing flag. None when nothing extra is known.
pub(crate) fn event_extra(trades: Option<&InsiderTrades>, uid: &str) -> Option<String> {
    if uid.is_empty() {
        return None;
    }
    let e = trades?
        .chart_events
        .iter()
        .find(|e| e.news_uid.as_deref() == Some(uid))?;
    let mut parts = Vec::new();
    if let Some(pct) = e
        .stake_change_pct
        .as_deref()
        .and_then(|p| p.parse::<f64>().ok())
    {
        parts.push(format!("stake {pct:+.1}%"));
    }
    if let Some(n) = e.tranche_count.filter(|&n| n > 1) {
        parts.push(format!("{n} tranches"));
    }
    if e.late_filing {
        parts.push("late filing".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// One Form 4 filing as a ledger row, for a server without sale labels.
/// Only the side carries direction color; the owner's name and the trade
/// value stay neutral.
fn filing_row(
    a: &Article,
    now: chrono::DateTime<Utc>,
    theme: &Theme,
    unseen: bool,
    title_w: usize,
) -> Row<'static> {
    let side = filing_side(a);
    let side_style = match side {
        "BUY" => Style::new().fg(theme.pos),
        "SELL" => Style::new().fg(theme.neg),
        _ => Style::new(),
    };
    let plan = match &a.insider {
        Some(t) if t.is_10b5_1 => Cell::from("plan").style(theme.subtle()),
        _ => Cell::from(" "),
    };
    let value = a
        .insider
        .as_ref()
        .and_then(|t| t.total_value_usd.as_deref())
        .map(|v| Cell::from(format!("{:>8}", fmt_usd(v))))
        .unwrap_or_else(|| Cell::from(" "));
    let name = a
        .insider
        .as_ref()
        .map(|t| t.insider_name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&a.original.title);
    Row::new(vec![
        date_cell(a, now, theme),
        Cell::from(side).style(side_style),
        value,
        title_cell(name.to_string(), Style::new(), unseen, theme, title_w),
        plan,
    ])
}

/// Trade side of a filing row. The API's structured `insider.side` is
/// authoritative when present: "buy"/"sell" color the side, "other" (grants,
/// code D sales back to the issuer, ...) deliberately stays neutral rather
/// than falling back to keyword guessing. Legacy rows without the block keep
/// the deterministic title template. AI sentiment never invents a trade.
fn filing_side(a: &Article) -> &'static str {
    match a.insider.as_ref().and_then(|t| t.side.as_deref()) {
        Some("buy") => "BUY",
        Some("sell") => "SELL",
        Some(_) => "OTHER",
        None => match side_from_title(&a.original.title).as_deref() {
            Some("positive") => "BUY",
            Some("negative") => "SELL",
            _ => "?",
        },
    }
}

/// Trade side from the deterministic Form 4 row template, for legacy rows
/// without the structured block. Tied to the current wording; if the
/// templates change it degrades to an uncolored row, nothing worse.
fn side_from_title(title: &str) -> Option<String> {
    let t = title.to_lowercase();
    if t.contains("sold") || t.contains("sale") {
        Some("negative".to_string())
    } else if t.contains("bought")
        || t.contains("purchase")
        || t.contains("buy")
        || t.contains("acquired")
    {
        Some("positive".to_string())
    } else {
        None
    }
}

/// The header of a server without sale labels: the server's 12-month
/// rollup and its top reporters.
fn summary_lines(trades: Option<&InsiderTrades>, theme: &Theme) -> Paragraph<'static> {
    let Some(w) = trades
        .and_then(|t| t.summary.as_ref())
        .and_then(|s| s.last_12m.as_ref())
    else {
        return Paragraph::new(Line::from(" Form 4 rollup unavailable").style(theme.subtle()));
    };
    let buys = w
        .buy_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let sells = w
        .sell_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let stats = Line::from(vec![
        Span::raw(" 12m  ").style(theme.subtle()),
        Span::raw(format!("{} events · ", w.buy_count + w.sell_count)),
        Span::styled(
            format!("▲ {} buys{buys}", w.buy_count),
            Style::new().fg(theme.pos),
        ),
        Span::styled(" · ", theme.faint()),
        Span::styled(
            format!("▼ {} sells{sells}", w.sell_count),
            Style::new().fg(theme.neg),
        ),
        Span::raw(format!(
            " · {}% under 10b5-1 plans · {} insiders",
            w.pct_10b5_1, w.unique_insiders
        ))
        .style(theme.subtle()),
    ]);
    let top: Vec<String> = trades
        .and_then(|t| t.summary.as_ref())
        .map(|s| s.top_insiders.as_slice())
        .unwrap_or_default()
        .iter()
        .take(3)
        .map(|t| {
            let title = if t.title.is_empty() {
                String::new()
            } else {
                format!(" ({})", t.title)
            };
            let net = t
                .net_value_usd
                .as_deref()
                .map(|v| format!(" {}", fmt_usd(v)))
                .unwrap_or_default();
            let count = if t.event_count > 0 {
                format!(" ×{}", t.event_count)
            } else {
                String::new()
            };
            format!("{}{title}{net}{count}", t.name)
        })
        .collect();
    let top_line = if top.is_empty() {
        Line::from("")
    } else {
        Line::from(format!(" top: {}", top.join(" · "))).style(theme.subtle())
    };
    Paragraph::new(vec![stats, top_line])
}
