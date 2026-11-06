use super::{App, Prompt, PromptKind};
use crate::alerts::{Level, Sample, quote_sample};
use crate::domain::Quote;

impl App {
    pub(crate) fn alert_sample(&self, symbol: &str) -> Option<Sample> {
        if self.from_cache.contains_key(symbol) {
            return None;
        }
        quote_sample(&self.data.get(symbol)?.quote, self.now())
    }

    pub(super) fn open_alert_prompt(&mut self) {
        let target = self.focused_symbol();
        let input = self
            .alerts
            .get(&target)
            .map(|r| r.level.label.clone())
            .unwrap_or_default();
        self.prompt = Prompt {
            open: true,
            kind: PromptKind::Alert,
            input,
            target,
            error: None,
        };
    }

    pub(super) fn commit_alert(&mut self, input: &str) -> Result<(), String> {
        let target = self.prompt.target.clone();
        if input.trim().is_empty() {
            self.alerts.remove(&target);
            return Ok(());
        }
        let level = Level::parse(input)?;
        if !self.polled_symbols().contains(&target) {
            return Err("Ticker is no longer watched or held".into());
        }
        let sample = self.alert_sample(&target);
        self.alerts.arm(target, level, sample);
        Ok(())
    }

    pub(super) fn check_alert(&mut self, symbol: &str, quote: &Quote) {
        if self.alerts.get(symbol).is_some()
            && let Some(sample) = quote_sample(quote, self.now())
        {
            self.alerts.observe(symbol, sample, self.now());
        }
    }
}
