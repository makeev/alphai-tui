use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, NewsLayout, SettingsRow, SettingsState, settings_rows};
use crate::config::{ALPHAI_KEY_FIELD, Config, KeyField};
use crate::source::extended::{Coverage, ExtendedSource, Provider};
use crate::source::registry::{self, SourceInfo};
use crate::source::{alpaca_keys, extended_provider};
use crate::theme::Panels;
use crate::ui::centered;

const WIDTH: u16 = 76;

/// Label column: fits "Pre/after hours" and a gap.
const LABEL: usize = 17;

/// Lines kept for the description of the row under the cursor.
const DESCRIPTION: usize = 3;

/// Modal overlay drawn on top of whatever view is active.
///
/// Rows come in sections, and the box ends with a description of the row
/// under the cursor: what it does, what each choice means, what it costs.
/// Hints and the description read the choices on screen (the config Save
/// would write), not the running source, so they answer before Save.
pub fn render(f: &mut Frame, app: &App) {
    let draft = app.settings_merged_config();
    let width = WIDTH.min(f.area().width.saturating_sub(2));
    let inner = usize::from(width.saturating_sub(2));
    let room = usize::from(f.area().height.saturating_sub(4));
    // A short terminal sheds the extras one step at a time (see `Fit`), so
    // the rows and the description of the one under the cursor go last.
    let mut lines = build(app, &draft, inner, Fit::Roomy);
    for fit in [Fit::Tight, Fit::Tighter, Fit::Tightest] {
        if lines.len() <= room {
            break;
        }
        lines = build(app, &draft, inner, fit);
    }
    let area = centered(f.area(), width, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    let block = app
        .theme
        .modal()
        .title(app.theme.heading(" Settings "))
        .border_style(Style::new().fg(app.theme.accent));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// How much a short terminal leaves out, least to most.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Fit {
    Roomy,
    /// No blank lines between sections: the headings keep them apart.
    Tight,
    /// No config path, and no blank line above the keys help.
    Tighter,
    /// Nor the keys help, the blank above Save or the rule above the
    /// description, which is only as long as it is. 80x24 fits exactly.
    Tightest,
}

/// Every line of the box, wrapped to `inner` columns by hand so the box is
/// exactly as tall as its content.
fn build(app: &App, draft: &Config, inner: usize, fit: Fit) -> Vec<Line<'static>> {
    let s = &app.settings;
    let theme = &app.theme;
    let mut lines: Vec<Line> = Vec::new();
    if s.first_run {
        lines.push(Line::from(" Welcome to alphai-tui.").bold());
        lines.push(Line::from(
            " Prices work out of the box via Yahoo, no key needed.",
        ));
        lines.push(Line::from(
            " The News and Insider views use the AlphAI API: get a free key at",
        ));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("https://alphai.io", Style::new().fg(theme.accent)),
            Span::raw(" (Account -> API keys) and paste it below."),
        ]));
        lines.push(Line::from(""));
    }

    let info = source_info(s);
    for (i, row) in settings_rows().iter().enumerate() {
        let selected = s.cursor == i;
        let marker = if selected { "▶ " } else { "  " };
        if let Some(title) = row.section() {
            if fit == Fit::Roomy && i > 0 {
                lines.push(Line::from(""));
            }
            lines.push(theme.heading(title));
        }
        if let SettingsRow::Save = row {
            if fit < Fit::Tightest {
                lines.push(Line::from(""));
            }
            let style = if selected {
                theme.selected()
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::raw(marker),
                Span::styled("[ Save and close ]", style),
            ]));
            continue;
        }
        let (label, value, hint) = row_text(s, draft, info, i, *row);
        let value_style = if selected && s.editing {
            Style::new().fg(theme.warn)
        } else if selected {
            theme.selected()
        } else {
            Style::new()
        };
        let hint_style = match hint {
            Hint::Plain(_) => theme.subtle(),
            Hint::Warn(_) => Style::new().fg(theme.warn),
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(format!("{label:<LABEL$}"), Style::new().bold()),
            Span::styled(value, value_style),
            Span::raw(" "),
            Span::styled(hint.text(), hint_style),
        ]));
        // The interval only means something against the watchlist length:
        // spell out the budget it lands on when it overruns the plan.
        if let SettingsRow::PollEvery = row
            && let Some(note) = rate_note(s, draft, app.symbols.len())
        {
            for part in wrap(&note, inner.saturating_sub(4)) {
                lines.push(Line::from(Span::styled(
                    format!("    {part}"),
                    Style::new().fg(theme.warn),
                )));
            }
        }
    }

    if fit < Fit::Tightest {
        lines.push(Line::from(Span::styled("─".repeat(inner), theme.faint())));
    }
    let row = settings_rows()[s.cursor.min(settings_rows().len() - 1)];
    let mut about = wrap(&describe(s, info, row), inner.saturating_sub(2));
    // A fixed height, so the box holds still as the cursor moves.
    if fit < Fit::Tightest {
        about.resize(about.len().max(DESCRIPTION), String::new());
    }
    for part in about {
        lines.push(Line::from(format!(" {part}")));
    }
    if let Some(msg) = &s.message {
        for part in wrap(msg, inner.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(
                format!(" {part}"),
                Style::new().fg(theme.error),
            )));
        }
    }
    if fit < Fit::Tighter {
        lines.push(Line::from(""));
    }
    if fit < Fit::Tightest {
        lines.push(
            Line::from(" ↑↓ move · ←→ change · enter edit / save · esc close")
                .style(theme.subtle()),
        );
    }
    if let Some(p) = app.config_path.as_ref().filter(|_| fit < Fit::Tighter) {
        let path = format!("config: {}", tilde(&p.display().to_string()));
        for part in chunks(&path, inner.saturating_sub(2)) {
            lines.push(Line::from(format!(" {part}")).style(theme.subtle()));
        }
    }
    lines
}

/// The text beside a value: a plain note, or one that needs acting on.
enum Hint {
    Plain(String),
    Warn(String),
}

impl Hint {
    fn text(self) -> String {
        match self {
            Self::Plain(t) | Self::Warn(t) => t,
        }
    }
}

/// Label, value and hint of one row.
fn row_text(
    s: &SettingsState,
    draft: &Config,
    info: &'static SourceInfo,
    i: usize,
    row: SettingsRow,
) -> (&'static str, String, Hint) {
    match row {
        SettingsRow::SourceChoice => (
            "Source",
            format!("‹ {} ›", s.source_choice),
            Hint::Plain(info.hint.to_string()),
        ),
        SettingsRow::ExtendedSource => (
            "Pre/after hours",
            format!("‹ {} ›", s.extended_choice.name()),
            extended_hint(s.extended_choice, draft, info),
        ),
        SettingsRow::PollEvery => (
            "Poll every",
            if s.cursor == i && s.editing {
                format!("{}▏", s.input)
            } else {
                format!("{}s", s.every_input)
            },
            Hint::Plain("seconds, 2 or more".to_string()),
        ),
        SettingsRow::Key(field) => {
            let stored = s
                .key_values
                .get(field.config_name)
                .map_or("", String::as_str);
            (
                field.label,
                field_value(s, i, stored),
                key_hint(field, stored, draft, info),
            )
        }
        SettingsRow::NewsOpen => (
            "Enter opens",
            format!("‹ {} ›", s.news_open_choice),
            Hint::Plain(news_open_hint(s.news_open_choice.as_str())),
        ),
        SettingsRow::NewsLayout => (
            "Layout",
            format!("‹ {} ›", s.news_layout_choice.name()),
            Hint::Plain(news_layout_hint(s.news_layout_choice)),
        ),
        SettingsRow::Borders => (
            "Panels",
            format!("‹ {} ›", s.borders_choice.name()),
            Hint::Plain(
                match s.borders_choice {
                    Panels::Surface => "tinted panels, no frame lines",
                    Panels::Lines(_) => "frame lines around every panel",
                }
                .to_string(),
            ),
        ),
        SettingsRow::ThemeChoice => (
            "Theme",
            format!("‹ {} ›", s.theme_choice),
            Hint::Plain("color preset; } / { cycle it anywhere".to_string()),
        ),
        SettingsRow::Save => unreachable!("drawn as a button"),
    }
}

/// The source on screen; an unknown name reads as the keyless default,
/// which is what Save would fall back to.
fn source_info(s: &SettingsState) -> &'static SourceInfo {
    registry::find(&s.source_choice).unwrap_or(&registry::SOURCES[0])
}

/// What the pre/after hours choice comes to under the source on screen.
fn extended_hint(choice: ExtendedSource, draft: &Config, info: &SourceInfo) -> Hint {
    if choice == ExtendedSource::Alpaca && alpaca_keys(draft).is_none() {
        return Hint::Warn("needs the Alpaca keys below".to_string());
    }
    Hint::Plain(match extended_provider(info, draft) {
        Provider::Sip { .. } => "→ Alpaca SIP, all exchanges, 15 min late".to_string(),
        Provider::Yahoo => "→ Yahoo, all exchanges".to_string(),
        Provider::Own => match (info.coverage)() {
            Coverage::Market => format!("→ {}'s own, all exchanges", info.id),
            Coverage::Venue => format!("→ {}'s own, IEX only", info.id),
            Coverage::Nothing => format!("→ none, {} has no pre/after hours", info.id),
        },
    })
}

/// The longer text under the rows, for the row under the cursor.
fn describe(s: &SettingsState, info: &SourceInfo, row: SettingsRow) -> String {
    match row {
        SettingsRow::SourceChoice => format!("{}: {}", info.id, info.about),
        SettingsRow::ExtendedSource => match s.extended_choice {
            ExtendedSource::Auto => {
                "Whole-market prints when the price source sees one exchange or none: \
                 Alpaca SIP when its keys are set, with Yahoo as backup, otherwise Yahoo. \
                 Sources that see every exchange keep their own."
            }
            ExtendedSource::Same => {
                "Only what the price source reports, no extra requests. On IEX sources \
                 (tiingo, alpaca) that is one exchange: before the open a thin stock may \
                 show a single stale trade."
            }
            ExtendedSource::Alpaca => {
                "Alpaca's consolidated tape: every exchange, 15 minutes late, about 2 \
                 requests a minute per ticker on the Alpaca key. No Yahoo backup."
            }
            ExtendedSource::Yahoo => {
                "Yahoo: every exchange, no key, about 1 request a minute per ticker. \
                 Yahoo blocks an IP that asks too often, and pre/after hours stop for \
                 tens of minutes."
            }
        }
        .to_string(),
        SettingsRow::PollEvery => "How often quotes and candles refresh; Save applies it at once. \
             Every poll costs requests per ticker, so a long watchlist on a keyed plan \
             wants a longer interval."
            .to_string(),
        SettingsRow::Key(field) if field.config_name == ALPHAI_KEY_FIELD.config_name => format!(
            "Your AlphAI key: the News, Insider, Earnings and Calendar views. Free at \
             alphai.io/developers. {} in the environment wins over this.",
            field.env_var
        ),
        SettingsRow::Key(field) => {
            let owner = key_owner(field);
            let tape = if owner.is_some_and(|o| o.id == "alpaca") {
                ", and for whole-market pre/after hours under any source"
            } else {
                ""
            };
            format!(
                "For {} as the price source{tape}. {} {} in the environment wins over this.",
                owner.map_or("its source", |o| o.id),
                owner.map_or("", |o| o.signup),
                field.env_var
            )
        }
        SettingsRow::NewsOpen => "Where Enter takes a news article: its page on alphai.io, with \
             the AlphAI analysis, or the site that published it."
            .to_string(),
        SettingsRow::NewsLayout => "How the News view splits: chart over the list, list over the \
             card, or list beside the card. x cycles it anywhere; the view behind this box \
             shows it live."
            .to_string(),
        SettingsRow::Borders => "rounded and plain draw frame lines around panels; none tints \
             the panels instead and draws no lines. The view behind this box shows it live."
            .to_string(),
        SettingsRow::ThemeChoice => "Color preset, previewed live. } and { cycle it anywhere; \
             single colors can be set under [theme] in the config file."
            .to_string(),
        SettingsRow::Save => "Writes everything above, and the watchlist on screen, to the \
             config file and applies it. Esc closes without writing."
            .to_string(),
    }
}

/// The source a credential belongs to.
fn key_owner(field: &KeyField) -> Option<&'static SourceInfo> {
    registry::SOURCES.iter().find(|s| {
        s.key_fields
            .iter()
            .any(|f| f.config_name == field.config_name)
    })
}

/// Beside a credential: an env override wins; otherwise what the choices
/// on screen use it for, or where to get one while it is empty.
fn key_hint(field: &KeyField, stored: &str, draft: &Config, info: &SourceInfo) -> Hint {
    let env = env_hint(field.env_var);
    if !env.is_empty() {
        return Hint::Plain(env);
    }
    if field.config_name == ALPHAI_KEY_FIELD.config_name {
        return Hint::Plain(if stored.trim().is_empty() {
            "get free on alphai.io/developers".into()
        } else {
            "news, insider, earnings".into()
        });
    }
    let owner = key_owner(field);
    let mut uses = Vec::new();
    if owner.is_some_and(|o| o.id == info.id) {
        uses.push("prices");
    }
    if owner.is_some_and(|o| o.id == "alpaca")
        && matches!(extended_provider(info, draft), Provider::Sip { .. })
    {
        uses.push("pre/after hours");
    }
    match (uses.is_empty(), stored.trim().is_empty()) {
        (false, true) => Hint::Warn(format!("needed for {}", uses.join(" and "))),
        (false, false) => Hint::Plain(format!("used for {}", uses.join(" and "))),
        (true, _) => Hint::Plain(String::new()),
    }
}

fn news_layout_hint(layout: NewsLayout) -> String {
    match layout {
        NewsLayout::Chart => "chart over the list; x cycles it",
        NewsLayout::Side => "list beside the card; x cycles it",
        NewsLayout::Stacked => "list over the card; x cycles it",
    }
    .to_string()
}

/// The request budget the settings on screen would land on: the source
/// picker and the interval buffer rather than the running poller, so the
/// warning appears before Save applies the change.
fn rate_note(s: &SettingsState, draft: &Config, symbols: usize) -> Option<String> {
    let info = registry::find(&s.source_choice)?;
    let every = s.every_input.trim().parse::<u64>().ok()?;
    let sip = matches!(extended_provider(info, draft), Provider::Sip { .. });
    registry::rate_warning(info, symbols, every.max(2), sip)
}

/// While editing show the raw buffer with a cursor mark; otherwise mask.
fn field_value(s: &SettingsState, row: usize, stored: &str) -> String {
    if s.cursor == row && s.editing {
        format!("{}▏", s.input)
    } else {
        mask(stored)
    }
}

pub fn mask(key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return "(not set)".into();
    }
    if key.chars().count() <= 10 {
        return "••••••".into();
    }
    let start: String = key.chars().take(6).collect();
    let end: String = key.chars().skip(key.chars().count() - 4).collect();
    format!("{start}…{end}")
}

fn news_open_hint(choice: &str) -> String {
    match choice {
        "original" => "enter opens the source site".into(),
        _ => "enter opens the article page on alphai.io".into(),
    }
}

fn env_hint(var: &str) -> String {
    if std::env::var(var).is_ok_and(|v| !v.trim().is_empty()) {
        format!("(env {var} overrides this)")
    } else {
        String::new()
    }
}

/// Greedy word wrap to `width` columns.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// A path has no spaces to wrap at: cut it every `width` columns.
fn chunks(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(width.max(10))
        .map(|c| c.iter().collect())
        .collect()
}

fn tilde(path: &str) -> String {
    match dirs::home_dir() {
        Some(h) => path.replacen(&h.display().to_string(), "~", 1),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{chunks, mask, wrap};

    #[test]
    fn masks_keys() {
        assert_eq!(mask(""), "(not set)");
        assert_eq!(mask("short"), "••••••");
        assert_eq!(mask("ak_live_abcdefgh1234"), "ak_liv…1234");
    }

    #[test]
    fn wraps_at_words_and_cuts_paths() {
        assert_eq!(
            wrap("one two three four five six", 13),
            vec!["one two three", "four five six"]
        );
        assert_eq!(wrap("", 20), Vec::<String>::new());
        assert_eq!(chunks("abcdefghijkl", 10), vec!["abcdefghij", "kl"]);
    }
}
