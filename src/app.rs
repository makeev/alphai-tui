use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::domain::{Interval, Range, TickerData};
use crate::poller::SourceEvent;
use crate::ui;

pub struct App {
    pub symbols: Vec<String>,
    pub data: HashMap<String, TickerData>,
    pub errors: HashMap<String, String>,
    pub selected: usize,
    pub view_idx: usize,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    pub last_update: Option<DateTime<Local>>,
    pub table_state: TableState,
    rx: UnboundedReceiver<SourceEvent>,
    refresh: Arc<Notify>,
}

impl App {
    pub fn new(
        symbols: Vec<String>,
        source_name: &'static str,
        range: Range,
        interval: Interval,
        rx: UnboundedReceiver<SourceEvent>,
        refresh: Arc<Notify>,
    ) -> Self {
        Self {
            symbols,
            data: HashMap::new(),
            errors: HashMap::new(),
            selected: 0,
            view_idx: 0,
            source_name,
            range,
            interval,
            last_update: None,
            table_state: TableState::default(),
            rx,
            refresh,
        }
    }

    pub fn selected_symbol(&self) -> &str {
        &self.symbols[self.selected]
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                self.apply(ev);
            }
            terminal.draw(|f| ui::draw(f, self))?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if self.handle_key(key) {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn apply(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::Data { symbol, data } => {
                self.errors.remove(&symbol);
                self.data.insert(symbol, data);
            }
            SourceEvent::Error { symbol, error } => {
                self.errors.insert(symbol, error);
            }
        }
        self.last_update = Some(Local::now());
    }

    /// Returns true when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Tab => self.view_idx = (self.view_idx + 1) % ui::VIEWS.len(),
            KeyCode::BackTab => {
                self.view_idx = (self.view_idx + ui::VIEWS.len() - 1) % ui::VIEWS.len()
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < ui::VIEWS.len() {
                    self.view_idx = idx;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1)
            }
            KeyCode::Char('r') => self.refresh.notify_one(),
            _ => {}
        }
        false
    }
}
