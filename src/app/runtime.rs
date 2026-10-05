//! Input and data wake the UI directly; only visible effects need frames
//! between events. The slow clock still drives ages, TTLs and fallback.
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;

use super::App;
use crate::{motion, terminal, ui};

const CLOCK: Duration = Duration::from_secs(1);

impl App {
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut events = EventStream::new();
        let mut frames = Frames::new(Instant::now());
        let mut source_open = true;
        let synchronized = self.synchronized_output && terminal::synchronized_output_supported();
        loop {
            let now = Instant::now();
            frames.tick(now);
            self.fallback_if_stuck();
            self.ensure_alphai_data();
            if frames.ready(now) {
                terminal::draw_frame(&mut std::io::stdout(), synchronized, || {
                    terminal.draw(|f| ui::draw(f, self)).map(|_| ())
                })?;
                // Slow terminals skip missed frames instead of catching up.
                frames.painted(Instant::now());
            }
            let deadline = frames.deadline(self.animation_interval(Instant::now()));
            tokio::select! {
                event = events.next() => {
                    match event.transpose()? {
                        None => return Ok(()),
                        Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                            if self.handle_key(key) { return Ok(()); }
                            frames.dirty = true;
                        }
                        Some(Event::Resize(_, _) | Event::FocusGained) => frames.dirty = true,
                        _ => {}
                    }
                }
                event = self.rx.recv(), if source_open => {
                    match event {
                        Some(event) => {
                            self.apply(event);
                            frames.dirty = true;
                        }
                        None => source_open = false,
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => frames.dirty = true,
            }
        }
    }

    fn animation_interval(&self, now: Instant) -> Option<Duration> {
        if !self.animations {
            return None;
        }
        let pulse = self
            .price_flash
            .values()
            .any(|(at, _)| now.saturating_duration_since(*at) < motion::PRICE_FLASH);
        if pulse {
            Some(motion::FRAME)
        } else if self.refreshing() && (!self.bare || self.show_rail) {
            Some(motion::SPINNER_FRAME)
        } else {
            None
        }
    }
}

struct Frames {
    dirty: bool,
    last: Instant,
    clock: Instant,
}

impl Frames {
    fn new(now: Instant) -> Self {
        Self {
            dirty: true,
            last: now - motion::FRAME,
            clock: now + CLOCK,
        }
    }

    fn tick(&mut self, now: Instant) {
        if now >= self.clock {
            self.clock = now + CLOCK;
            self.dirty = true;
        }
    }

    fn ready(&self, now: Instant) -> bool {
        self.dirty && now.saturating_duration_since(self.last) >= motion::FRAME
    }

    fn painted(&mut self, now: Instant) {
        self.last = now;
        self.dirty = false;
    }

    fn deadline(&self, animation: Option<Duration>) -> Instant {
        let interval = if self.dirty {
            Some(motion::FRAME)
        } else {
            animation
        };
        interval.map_or(self.clock, |step| self.clock.min(self.last + step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_waits_for_the_clock_and_input_coalesces_at_the_frame_limit() {
        let now = Instant::now();
        let mut frames = Frames::new(now);
        assert!(frames.ready(now));
        frames.painted(now);
        assert_eq!(frames.deadline(None), now + CLOCK);
        frames.dirty = true;
        assert!(!frames.ready(now + Duration::from_millis(10)));
        assert_eq!(frames.deadline(None), now + motion::FRAME);
        assert!(frames.ready(now + motion::FRAME));
        frames.painted(now + motion::FRAME);
        assert_eq!(frames.deadline(None), now + CLOCK);
    }

    #[test]
    fn effects_schedule_frames_and_slow_draws_never_catch_up() {
        let now = Instant::now();
        let mut frames = Frames::new(now);
        frames.painted(now);
        assert_eq!(frames.deadline(Some(motion::FRAME)), now + motion::FRAME);
        frames.painted(now + Duration::from_millis(200));
        assert_eq!(
            frames.deadline(Some(motion::FRAME)),
            now + Duration::from_millis(234)
        );
        assert_eq!(frames.deadline(None), now + CLOCK);
        frames.tick(now + CLOCK);
        assert!(frames.ready(now + CLOCK));
        assert_eq!(frames.clock, now + CLOCK * 2);
    }
}
