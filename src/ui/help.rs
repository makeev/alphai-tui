//! Help overlay (? anywhere): every action with the keys it is actually
//! bound to right now and its snake_case name for `[keybindings]`. Pure
//! render over the live `Keymap`, so a remap shows up here too.

use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::App;
use crate::keymap::{Action, action_name};
use crate::ui::centered;

/// One line of the table. The coverage test walks `ACTIONS` against this
/// table, so a new action cannot ship without a help row.
enum Row {
    Group(&'static str),
    Act(Action, &'static str),
    Fixed(&'static str, &'static str),
}

static ROWS: &[Row] = &[
    Row::Group("Global"),
    Row::Act(Action::Help, "this help"),
    Row::Act(Action::Settings, "open settings"),
    Row::Act(Action::Refresh, "refresh prices and the visible feed"),
    Row::Act(Action::CycleTheme, "next color preset (Save keeps it)"),
    Row::Act(Action::NextView, "next view"),
    Row::Act(Action::PrevView, "previous view"),
    Row::Fixed("1-9", "jump to a view"),
    Row::Act(Action::Quit, "quit"),
    Row::Fixed("esc", "quit, close an overlay"),
    Row::Fixed("ctrl-c", "force quit"),
    Row::Group("Navigate"),
    Row::Act(Action::Up, "move up (ticker or article)"),
    Row::Act(Action::Down, "move down; last article row loads more"),
    Row::Act(Action::Left, "previous ticker (News, Insider)"),
    Row::Act(Action::Right, "next ticker (News, Insider)"),
    Row::Act(Action::PageUp, "scroll the article card up"),
    Row::Act(Action::PageDown, "scroll the article card down"),
    Row::Act(Action::Open, "open the article in the browser"),
    Row::Act(Action::Card, "full-screen article card"),
    Row::Group("News & Insider"),
    Row::Act(Action::CycleScope, "news scope: ticker, market, trending"),
    Row::Act(Action::CycleLayout, "news layout: side or stacked"),
    Row::Act(Action::ScoreUp, "raise the feed score filter"),
    Row::Act(Action::ScoreDown, "lower the feed score filter"),
    Row::Group("Chart"),
    Row::Act(Action::ChartStyle, "candles or line chart"),
    Row::Act(Action::ToggleSma, "toggle the SMA overlays"),
    Row::Act(Action::ToggleRsi, "toggle the RSI panel"),
    Row::Act(Action::NextPreset, "next range and interval preset"),
    Row::Act(Action::PrevPreset, "previous range and interval preset"),
];

pub fn render(f: &mut Frame, app: &mut App) {
    let mut lines: Vec<Line> = Vec::new();
    for row in ROWS {
        match row {
            Row::Group(title) => {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    *title,
                    Style::new().fg(app.theme.accent).bold(),
                )));
            }
            Row::Act(action, what) => lines.push(help_line(
                app.keymap.action_labels(*action),
                what,
                action_name(*action),
            )),
            Row::Fixed(keys, what) => lines.push(help_line(keys.to_string(), what, "")),
        }
    }

    let area = centered(
        f.area(),
        66.min(f.area().width.saturating_sub(4)),
        (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2)),
    );
    f.render_widget(Clear, area);
    let block = app
        .theme
        .panel()
        .title(app.theme.heading(" Keys "))
        .title_bottom(Line::from(" remap: [keybindings] in config.toml · esc close ").dim())
        .border_style(Style::new().fg(app.theme.accent));
    let max_scroll = (lines.len() as u16).saturating_sub(area.height.saturating_sub(2));
    app.help.scroll = app.help.scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(lines).scroll((app.help.scroll, 0)).block(block),
        area,
    );
}

/// " keys        description                          config_name" with the
/// name dimmed; fixed rows have no name (they are not remappable).
fn help_line(keys: String, what: &str, name: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!(" {keys:<10} ")),
        Span::raw(format!("{what:<39}")),
        Span::raw(name).dim(),
    ])
}
