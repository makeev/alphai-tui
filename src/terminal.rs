//! Optional synchronized frames, using only capabilities advertised by the
//! current terminal. No stdin probes, terminal-name guesses or passthrough.
use std::io::{self, Write};

const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";

/// The Sync terminfo extension advertises the actual sequence. Missing,
/// older, or non-DEC capabilities keep the ordinary renderer, including on
/// platforms without terminfo. Inside tmux this consults tmux's TERM, not
/// the outer terminal's inherited TERM_PROGRAM.
pub fn synchronized_output_supported() -> bool {
    #[cfg(unix)]
    {
        terminfo::Database::from_env()
            .ok()
            .is_some_and(|db| supports_sync(&db))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
fn supports_sync(db: &terminfo::Database) -> bool {
    let Some(terminfo::capability::Value::String(sequence)) = db.raw("Sync") else {
        return false;
    };
    terminfo::expand!(sequence.as_slice(); 1).ok().as_deref() == Some(BEGIN)
        && terminfo::expand!(sequence.as_slice(); 0).ok().as_deref() == Some(END)
}

/// End the synchronized frame on draw errors and unwinding too. Keep the
/// guard outside the draw closure, so a failed render cannot freeze output.
pub fn draw_frame<T>(
    writer: &mut impl Write,
    synchronized: bool,
    draw: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let mut frame = FrameGuard {
        writer,
        active: synchronized,
    };
    if synchronized {
        frame.writer.write_all(BEGIN)?;
        frame.writer.flush()?;
    }
    let result = draw();
    let end = frame.finish();
    result.and_then(|value| end.map(|()| value))
}

struct FrameGuard<'a, W: Write> {
    writer: &'a mut W,
    active: bool,
}

impl<W: Write> FrameGuard<'_, W> {
    fn finish(&mut self) -> io::Result<()> {
        if self.active {
            self.writer.write_all(END)?;
            self.writer.flush()?;
            self.active = false;
        }
        Ok(())
    }
}

impl<W: Write> Drop for FrameGuard<'_, W> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_frames_emit_no_extensions() {
        let mut bytes = Vec::new();
        assert_eq!(draw_frame(&mut bytes, false, || Ok(42)).unwrap(), 42);
        assert!(bytes.is_empty());
    }

    #[test]
    fn synchronized_frames_end_on_success_error_and_panic() {
        for fail in [false, true] {
            let mut bytes = Vec::new();
            let result = draw_frame(&mut bytes, true, || {
                if fail {
                    Err(io::Error::other("draw failed"))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), fail);
            assert_eq!(bytes, [BEGIN, END].concat());
        }
        let mut bytes = Vec::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = draw_frame::<()>(&mut bytes, true, || panic!("draw failed"));
        }));
        assert!(result.is_err());
        assert_eq!(bytes, [BEGIN, END].concat());
    }

    #[cfg(unix)]
    #[test]
    fn only_an_advertised_dec_sync_pair_enables_frames() {
        for (sequence, expected) in [
            (None, false),
            (Some("unrecognized"), false),
            (Some("\x1b[?2026h"), false),
            (Some("\x1b[?2026%?%p1%{1}%-%tl%eh%;"), true),
        ] {
            let mut builder = terminfo::Database::new();
            builder.name("alphai-test");
            if let Some(sequence) = sequence {
                builder.raw("Sync", sequence);
            }
            assert_eq!(supports_sync(&builder.build().unwrap()), expected);
        }
    }
}
