use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::alphai::{InsiderSummary, fmt_usd, insider_key};
use crate::app::App;
use crate::ui::View;
use crate::ui::news::{render_detail, render_gate, score_cell};

pub struct InsiderView;

impl View for InsiderView {
    fn title(&self) -> &'static str {
        "Insider"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let symbol = app.selected_symbol().to_string();
        let key = insider_key(&symbol);
        let block = Block::bordered().title(format!(" Insider · {symbol} (SEC Form 4) "));

        if render_gate(f, area, &block, app, &key) {
            return;
        }
        let bundle = &app.insider[&symbol];

        let [head, list_area, detail] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(6),
        ])
        .areas(area);

        f.render_widget(summary_lines(bundle.summary.as_ref()), head);

        if bundle.articles.is_empty() {
            let msg = format!("no Form 4 activity for {symbol} in the feed");
            f.render_widget(Paragraph::new(Line::from(msg).dim()).block(block), list_area);
            return;
        }

        let now = Utc::now();
        let rows: Vec<Row> = bundle
            .articles
            .iter()
            .map(|a| {
                Row::new(vec![
                    Cell::from(a.age(now)).dim(),
                    score_cell(a.score()),
                    Cell::from(a.original.title.clone()),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Min(20),
        ];
        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        app.news_selected = app.news_selected.min(bundle.articles.len() - 1);
        app.news_table_state.select(Some(app.news_selected));
        f.render_stateful_widget(table, list_area, &mut app.news_table_state);

        render_detail(f, detail, bundle.articles.get(app.news_selected));
    }
}

fn summary_lines(summary: Option<&InsiderSummary>) -> Paragraph<'static> {
    let Some(s) = summary else {
        return Paragraph::new(Line::from(" 30d summary unavailable").dim());
    };
    let buys = s
        .buy_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let sells = s
        .sell_value_usd
        .as_deref()
        .map(|v| format!(" {}", fmt_usd(v)))
        .unwrap_or_default();
    let stats = Line::from(vec![
        Span::raw(format!(" {}d  ", s.days)).dim(),
        Span::raw(format!("{} filings · ", s.total_transactions)),
        Span::styled(
            format!("▲ {} buys{buys}", s.buy_count),
            Style::new().fg(Color::Green),
        ),
        Span::raw(" · ").dim(),
        Span::styled(
            format!("▼ {} sells{sells}", s.sell_count),
            Style::new().fg(Color::Red),
        ),
        Span::raw(format!(" · {}% under 10b5-1 plans", s.pct_10b5_1)).dim(),
    ]);
    let top: Vec<String> = s
        .top_insiders
        .iter()
        .take(3)
        .map(|t| {
            let title = if t.title.is_empty() {
                String::new()
            } else {
                format!(" ({})", t.title)
            };
            let net = t
                .net_value
                .as_deref()
                .map(|v| format!(" {}", fmt_usd(v)))
                .unwrap_or_default();
            format!("{}{title}{net}", t.name)
        })
        .collect();
    let top_line = if top.is_empty() {
        Line::from("")
    } else {
        Line::from(format!(" top: {}", top.join(" · "))).dim()
    };
    Paragraph::new(vec![stats, top_line])
}
