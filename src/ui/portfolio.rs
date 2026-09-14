//! Portfolio view: what is held, what it is worth, and whether it is up.
//!
//! The watchlist answers "what is the price". This answers "what is that
//! price doing to me", which is the question the rest of the category is
//! built around and this app had no answer to. One row per holding, a
//! total underneath, and `p` to edit the row in place.
//!
//! Holdings that are not on the watchlist are polled too, so every row can
//! be valued; a row still waiting for its first price says so instead of
//! being summed in as zero.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::domain::{Quote, fmt_price};
use crate::keymap::Action;
use crate::portfolio::{self, Position, fmt_money, fmt_signed};
use crate::ui::chart::move_color;
use crate::ui::{Hint, View, ViewId};

const W_SYMBOL: u16 = 10;
const W_QTY: u16 = 10;
const W_AVG: u16 = 11;
const W_LAST: u16 = 11;
const W_VALUE: u16 = 14;
const W_DAY: u16 = 12;
const W_PNL: u16 = 14;
const W_PCT: u16 = 9;
const W_WEIGHT: u16 = 7;
const GAP: u16 = 1;
const MARKER: u16 = 2;

static HINTS: &[Hint] = &[
    Hint::act(&[Action::Quit], "quit"),
    Hint::fixed("tab/1-9", "view"),
    Hint::act(&[Action::Up, Action::Down], "select"),
    Hint::act(&[Action::Position], "position"),
    Hint::act(&[Action::Refresh], "refresh"),
    Hint::act(&[Action::Help], "help"),
];

pub struct PortfolioView;

impl View for PortfolioView {
    fn id(&self) -> ViewId {
        ViewId::Portfolio
    }

    fn title(&self) -> &'static str {
        "Portfolio"
    }

    fn hints(&self) -> &'static [Hint] {
        HINTS
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_portfolio(f, area, app);
    }
}

/// Which optional columns are shown. Same accounting as the watchlist
/// table: whole columns are dropped, least useful first, so the survivors
/// keep their full width instead of every number being squeezed at once.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Columns {
    qty: bool,
    avg: bool,
    last: bool,
    value: bool,
    day: bool,
    pnl: bool,
    weight: bool,
}

fn width_of(c: Columns) -> u16 {
    W_SYMBOL
        + if c.qty { GAP + W_QTY } else { 0 }
        + if c.avg { GAP + W_AVG } else { 0 }
        + if c.last { GAP + W_LAST } else { 0 }
        + if c.value { GAP + W_VALUE } else { 0 }
        + if c.day { GAP + W_DAY } else { 0 }
        + if c.pnl { GAP + W_PNL } else { 0 }
        + GAP
        + W_PCT
        + if c.weight { GAP + W_WEIGHT } else { 0 }
}

/// The widest set that fits. The symbol and the percentage always stay:
/// between them they are the smallest row that still answers the question
/// the view exists for.
fn columns(avail: u16) -> Columns {
    for (qty, avg, last, value, day, pnl, weight) in [
        (true, true, true, true, true, true, true),
        (true, true, true, true, true, true, false),
        (true, false, true, true, true, true, false),
        (true, false, true, true, false, true, false),
        (false, false, true, true, false, true, false),
        (false, false, true, false, false, true, false),
        (false, false, false, false, false, true, false),
    ] {
        let cols = Columns {
            qty,
            avg,
            last,
            value,
            day,
            pnl,
            weight,
        };
        if width_of(cols) <= avail {
            return cols;
        }
    }
    Columns {
        qty: false,
        avg: false,
        last: false,
        value: false,
        day: false,
        pnl: false,
        weight: false,
    }
}

pub fn render_portfolio(f: &mut Frame, area: Rect, app: &mut App) {
    render_portfolio_at(f, area, app, Utc::now());
}

pub(crate) fn render_portfolio_at(f: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
    if app.positions.is_empty() {
        render_empty(f, area, app);
        return;
    }
    let cols = columns(area.width.saturating_sub(2 + MARKER));
    let totals = portfolio::totals(
        app.positions
            .iter()
            .map(|p| (p, app.data.get(&p.symbol).map(|d| &d.quote))),
        now,
    );

    let mut rows: Vec<Row> = app
        .positions
        .iter()
        .map(|position| {
            let quote = app.data.get(&position.symbol).map(|d| &d.quote);
            match quote {
                Some(quote) => row(app, position, quote, cols, totals.value, now),
                None => pending_row(app, position),
            }
        })
        .collect();
    rows.push(totals_row(app, &totals, cols));

    let mut widths = vec![Constraint::Length(W_SYMBOL)];
    let mut header = vec![Cell::from("Symbol")];
    let mut push = |on: bool, width: u16, title: &'static str| {
        if on {
            widths.push(Constraint::Length(width));
            header.push(Cell::from(right(title)));
        }
    };
    push(cols.qty, W_QTY, "Qty");
    push(cols.avg, W_AVG, "Avg");
    push(cols.last, W_LAST, "Last");
    push(cols.value, W_VALUE, "Value");
    // Today's move includes the extended session, using the most recent
    // regular close as the reference during premarket.
    push(cols.day, W_DAY, "Day");
    push(cols.pnl, W_PNL, "P&L");
    push(true, W_PCT, "P&L%");
    push(cols.weight, W_WEIGHT, "Wt%");

    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::new().bold().underlined()))
        .block(
            app.theme
                .panel_titled(" Portfolio ")
                .title_bottom(footnote(app, &totals, area.width)),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    app.portfolio_state.select(Some(app.portfolio_selected));
    f.render_stateful_widget(table, area, &mut app.portfolio_state);
}

fn row(
    app: &App,
    position: &Position,
    quote: &Quote,
    cols: Columns,
    total_value: f64,
    now: DateTime<Utc>,
) -> Row<'static> {
    let price = portfolio::price_at(quote, now);
    let pnl = position.pnl(price);
    let pnl_color = move_color(Some(pnl), &app.theme);
    let mut cells = vec![Cell::from(position.symbol.clone()).bold()];
    if cols.qty {
        cells.push(Cell::from(right(format!("{}", position.qty))));
    }
    if cols.avg {
        cells.push(Cell::from(right(fmt_price(position.avg_price))));
    }
    if cols.last {
        let last = if price != quote.price {
            format!("{}*", fmt_price(price))
        } else {
            fmt_price(price)
        };
        cells.push(
            Cell::from(right(last))
                .style(Style::new().fg(move_color(portfolio::day_change(quote, now), &app.theme))),
        );
    }
    if cols.value {
        cells.push(Cell::from(right(fmt_money(position.value(price)))));
    }
    if cols.day {
        let cell = match position.day_pnl(quote, now) {
            Some(day) => Cell::from(right(fmt_signed(day)))
                .style(Style::new().fg(move_color(Some(day), &app.theme))),
            // No reference close, so there is no day to measure.
            None => Cell::from(right("—")).dim(),
        };
        cells.push(cell);
    }
    if cols.pnl {
        cells.push(Cell::from(right(fmt_signed(pnl))).style(Style::new().fg(pnl_color)));
    }
    let pct = match position.pnl_pct(price) {
        Some(pct) => Cell::from(right(format!("{pct:+.2}%"))).style(Style::new().fg(pnl_color)),
        None => Cell::from(right("—")).dim(),
    };
    cells.push(pct);
    if cols.weight {
        let cell = match portfolio::weight(position.value(price), total_value) {
            Some(w) => Cell::from(right(format!("{w:.1}%"))),
            None => Cell::from(right("—")).dim(),
        };
        cells.push(cell);
    }
    Row::new(cells)
}

/// A holding whose price has not arrived yet (or whose fetch failed).
fn pending_row(app: &App, position: &Position) -> Row<'static> {
    let status = match app.errors.get(&position.symbol) {
        Some(_) => Cell::from("error").style(Style::new().fg(app.theme.error)),
        None => Cell::from("…").dim(),
    };
    Row::new(vec![Cell::from(position.symbol.clone()).bold(), status])
}

fn totals_row(app: &App, totals: &portfolio::Totals, cols: Columns) -> Row<'static> {
    let color = move_color(Some(totals.pnl), &app.theme);
    let mut cells = vec![Cell::from("Total").bold()];
    if cols.qty {
        cells.push(Cell::from(""));
    }
    if cols.avg {
        cells.push(Cell::from(""));
    }
    if cols.last {
        cells.push(Cell::from(""));
    }
    if cols.value {
        cells.push(Cell::from(right(fmt_money(totals.value))).bold());
    }
    if cols.day {
        let cell = match totals.day_pnl {
            Some(day) => Cell::from(right(fmt_signed(day)))
                .style(Style::new().fg(move_color(Some(day), &app.theme)))
                .bold(),
            None => Cell::from(right("—")).dim(),
        };
        cells.push(cell);
    }
    if cols.pnl {
        cells.push(
            Cell::from(right(fmt_signed(totals.pnl)))
                .style(Style::new().fg(color))
                .bold(),
        );
    }
    let pct = match totals.pnl_pct() {
        Some(pct) => Cell::from(right(format!("{pct:+.2}%")))
            .style(Style::new().fg(color))
            .bold(),
        None => Cell::from(right("—")).dim(),
    };
    cells.push(pct);
    if cols.weight {
        cells.push(Cell::from(right("100%")).dim());
    }
    Row::new(cells)
}

/// The bottom border line: the money put in, how many rows could be
/// valued, and a warning when the sums mix currencies. There is no
/// conversion in this app and there is not going to be one, so a total
/// over two currencies has to say what it is rather than look precise.
fn footnote(app: &App, totals: &portfolio::Totals, width: u16) -> Line<'static> {
    let mut parts = vec![format!("cost {}", fmt_money(totals.cost))];
    if app.positions.iter().any(|p| {
        app.data
            .get(&p.symbol)
            .is_some_and(|d| portfolio::price(&d.quote) != d.quote.price)
    }) {
        parts.push("* pre/after-hours".to_string());
    }
    if totals.priced < totals.total {
        parts.push(format!("{}/{} priced", totals.priced, totals.total));
    }
    if mixed_currencies(app) {
        parts.push("mixed currencies".to_string());
    }
    let text = format!(" {} ", parts.join(" · "));
    if text.chars().count() as u16 > width.saturating_sub(4) {
        return Line::from("");
    }
    Line::from(text).dim().right_aligned()
}

fn mixed_currencies(app: &App) -> bool {
    let mut seen: Option<&str> = None;
    for position in &app.positions {
        let Some(currency) = app
            .data
            .get(&position.symbol)
            .and_then(|d| d.quote.currency.as_deref())
        else {
            continue;
        };
        match seen {
            Some(first) if first != currency => return true,
            Some(_) => {}
            None => seen = Some(currency),
        }
    }
    false
}

/// No positions yet. The view is always in the tab cycle, so this is the
/// screen most people see first: it has to say how to fill it, both from
/// the keyboard and from the config.
fn render_empty(f: &mut Frame, area: Rect, app: &App) {
    let key = app.keymap.action_labels(Action::Position);
    let lines = vec![
        Line::from(""),
        Line::from(" Nothing held yet.").bold(),
        Line::from(""),
        Line::from(format!(
            " Press {key} to record what you own of the selected ticker:"
        ))
        .dim(),
        Line::from(" a quantity and the average price you paid, e.g. 12 182.31.").dim(),
        Line::from(""),
        Line::from(" The same thing in the config file:").dim(),
        Line::from("   [[positions]]").dim(),
        Line::from("   symbol = \"AAPL\"").dim(),
        Line::from("   qty = 12").dim(),
        Line::from("   avg_price = 182.31").dim(),
    ];
    f.render_widget(
        Paragraph::new(lines).block(app.theme.panel_titled(" Portfolio ")),
        area,
    );
}

/// Right-aligned cell content (numbers line up under each other).
fn right(text: impl Into<String>) -> Text<'static> {
    Text::from(text.into()).right_aligned()
}
