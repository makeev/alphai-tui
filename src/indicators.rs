//! Indicator math over close series. Pure functions, index-aligned with the
//! input: `out[i]` is the indicator value at candle `i`, `None` while the
//! lookback window is still filling.

/// Default indicator parameters; `[chart]` in the config can override them.
/// The slow SMA period also sizes the history warm-up that
/// `domain::fetch_range` requests beyond the visible window, so the
/// overlays have data from the very first visible candle.
pub const SMA_FAST: usize = 20;
pub const SMA_SLOW: usize = 100;
pub const RSI_PERIOD: usize = 14;

/// How the two moving-average overlays are averaged. The periods, the
/// colors and the history warm-up are shared, so this only changes the
/// weighting: `[chart] ma_type` picks the startup value, the `e` key
/// switches it live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaType {
    #[default]
    Sma,
    Ema,
}

impl MaType {
    /// Legend prefix, as in "EMA20".
    pub fn label(self) -> &'static str {
        match self {
            Self::Sma => "SMA",
            Self::Ema => "EMA",
        }
    }
}

/// The chosen average over `values`; both kinds share the `sma` contract.
pub fn ma(kind: MaType, values: &[f64], period: usize) -> Vec<Option<f64>> {
    match kind {
        MaType::Sma => sma(values, period),
        MaType::Ema => ema(values, period),
    }
}

/// Simple moving average. `None` until a full `period` window is available.
pub fn sma(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; values.len()];
    if period == 0 || period > values.len() {
        return out;
    }
    let mut sum: f64 = values[..period].iter().sum();
    out[period - 1] = Some(sum / period as f64);
    for i in period..values.len() {
        sum += values[i] - values[i - period];
        out[i] = Some(sum / period as f64);
    }
    out
}

/// Exponential moving average, seeded with the simple average of the first
/// `period` values so it starts on the same candle (and at the same point)
/// as the SMA of that period. `None` until the seed window is full.
pub fn ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; values.len()];
    if period == 0 || period > values.len() {
        return out;
    }
    let mut prev: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = Some(prev);
    let k = 2.0 / (period as f64 + 1.0);
    for i in period..values.len() {
        prev += k * (values[i] - prev);
        out[i] = Some(prev);
    }
    out
}

/// RSI with Wilder smoothing. `None` for the first `period` entries.
pub fn rsi(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; closes.len()];
    if period == 0 || closes.len() <= period {
        return out;
    }
    let (mut avg_gain, mut avg_loss) = (0.0, 0.0);
    for i in 1..=period {
        let d = closes[i] - closes[i - 1];
        avg_gain += d.max(0.0);
        avg_loss += (-d).max(0.0);
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    out[period] = Some(rsi_from(avg_gain, avg_loss));
    for i in period + 1..closes.len() {
        let d = closes[i] - closes[i - 1];
        avg_gain = (avg_gain * (period - 1) as f64 + d.max(0.0)) / period as f64;
        avg_loss = (avg_loss * (period - 1) as f64 + (-d).max(0.0)) / period as f64;
        out[i] = Some(rsi_from(avg_gain, avg_loss));
    }
    out
}

/// A dead-flat series (finnhub synthesizes those) is neutral, not overbought.
fn rsi_from(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        if avg_gain == 0.0 { 50.0 } else { 100.0 }
    } else {
        100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_basic() {
        assert_eq!(
            sma(&[1.0, 2.0, 3.0, 4.0, 5.0], 3),
            vec![None, None, Some(2.0), Some(3.0), Some(4.0)]
        );
    }

    #[test]
    fn sma_degenerate() {
        assert_eq!(sma(&[1.0, 2.0], 0), vec![None, None]);
        assert_eq!(sma(&[1.0, 2.0], 3), vec![None, None]);
        assert_eq!(sma(&[], 3), vec![]);
    }

    /// Same warm-up as the SMA of that period, and the same first value:
    /// the two overlays start on the very same candle whichever is picked.
    #[test]
    fn ema_seeds_on_the_sma() {
        let closes = [1.0, 2.0, 3.0, 10.0];
        let out = ema(&closes, 2);
        assert_eq!(out[0], None);
        assert_eq!(out[1], sma(&closes, 2)[1]);
    }

    /// Hand-computed: seed 1.5, k = 2/3, then 2.5 and 7.5. The SMA of the
    /// same window ends at 6.5, so this also pins the faster reaction.
    #[test]
    fn ema_weights_recent_values_more() {
        let closes = [1.0, 2.0, 3.0, 10.0];
        let out = ema(&closes, 2);
        for (i, want) in [(1, 1.5), (2, 2.5), (3, 7.5)] {
            let got = out[i].unwrap();
            assert!((got - want).abs() < 1e-9, "out[{i}] = {got}, want {want}");
        }
        assert!(out[3].unwrap() > sma(&closes, 2)[3].unwrap());
    }

    #[test]
    fn ema_flat_series_is_the_constant() {
        let out = ema(&[42.0; 8], 3);
        assert!(out[2..].iter().all(|v| v.unwrap() == 42.0), "{out:?}");
    }

    #[test]
    fn ema_degenerate() {
        assert_eq!(ema(&[1.0, 2.0], 0), vec![None, None]);
        assert_eq!(ema(&[1.0, 2.0], 3), vec![None, None]);
        assert_eq!(ema(&[], 3), vec![]);
    }

    #[test]
    fn rsi_warmup_is_none() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let out = rsi(&closes, 14);
        assert!(out[..14].iter().all(Option::is_none));
        assert!(out[14..].iter().all(Option::is_some));
        assert!(rsi(&closes[..14], 14).iter().all(Option::is_none));
    }

    #[test]
    fn rsi_monotonic_up_is_100() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        assert_eq!(rsi(&closes, 14)[19], Some(100.0));
    }

    #[test]
    fn rsi_monotonic_down_near_0() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 - i as f64).collect();
        assert!(rsi(&closes, 14)[19].unwrap() < 1e-9);
    }

    #[test]
    fn rsi_flat_is_50() {
        let closes = vec![42.0; 20];
        assert_eq!(rsi(&closes, 14)[19], Some(50.0));
    }

    #[test]
    fn rsi_alternating_first_value_50() {
        let closes: Vec<f64> = (0..20)
            .map(|i| if i % 2 == 0 { 100.0 } else { 101.0 })
            .collect();
        let v = rsi(&closes, 14)[14].unwrap();
        assert!((v - 50.0).abs() < 1e-9, "got {v}");
    }

    /// Wilder's classic worked example (the StockCharts RSI dataset).
    #[test]
    fn rsi_wilder_reference() {
        let closes = [
            44.3389, 44.0902, 44.1497, 43.6124, 44.3278, 44.8264, 45.0955,
            45.4245, 45.8433, 46.0826, 45.8931, 46.0328, 45.6140, 46.2820,
            46.2820, 46.0028, 46.0328, 46.4116, 46.2222,
        ];
        let out = rsi(&closes, 14);
        let v14 = out[14].unwrap();
        let v15 = out[15].unwrap();
        assert!((v14 - 70.46).abs() < 0.3, "out[14] = {v14}");
        assert!((v15 - 66.25).abs() < 0.3, "out[15] = {v15}");
    }
}
