use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};
use crate::types::{Candle, DrawOnLiquidity, SwingPoint};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlySessionFrame {
    pub weekly_open_time: i64,
    pub overnight_high: f64,
    pub overnight_low: f64,
    pub transposed_daily_dol: Option<DrawOnLiquidity>,
    pub recent_swing_highs: Vec<SwingPoint>,
    pub recent_swing_lows: Vec<SwingPoint>,
    pub sell_side_induced: bool, // Indicates if price dropped early to induce breakout shorts
    pub buy_side_induced: bool,  // Indicates if price popped early to induce breakout buyers
}

/// Layer 3: Hourly Session & Transposed Levels (Episode 2: sents 233-278)
/// - Frames the weekly range from Monday midnight NY to current session.
/// - Transposes daily levels (old daily highs/lows) onto the hourly chart.
/// - Tracks overnight consolidation (Asian & London session highs/lows).
/// - Identifies the "inducement" / sucker play (taking out one side first).
pub fn frame_hourly_session(
    hourly_candles: &[Candle],
    daily_dol: Option<DrawOnLiquidity>,
) -> Option<HourlySessionFrame> {
    if hourly_candles.is_empty() {
        return None;
    }

    let current_candle = hourly_candles.last()?;
    let current_dt = New_York.timestamp_opt(current_candle.time, 0).single()?;

    // Find the Monday midnight NY for the current week
    let days_from_monday = current_dt.weekday().num_days_from_monday() as i64;
    let monday_date = current_dt.date_naive() - chrono::Duration::days(days_from_monday);
    let monday_midnight = New_York
        .with_ymd_and_hms(monday_date.year(), monday_date.month(), monday_date.day(), 0, 0, 0)
        .single()?
        .timestamp();

    // Map Overnight Session (between 18:00 prev day and 08:00 current day NY)
    // Find hourly candles in overnight session
    let mut overnight_high = f64::MIN;
    let mut overnight_low = f64::MAX;

    for c in hourly_candles.iter() {
        if let Some(dt) = New_York.timestamp_opt(c.time, 0).single() {
            let hour = dt.hour();
            // Overnight window: from 18:00 (prev day) to 08:00 NY
            let is_overnight = hour >= 18 || hour < 8;
            if is_overnight && dt.date_naive() == current_dt.date_naive() || (hour >= 18 && (current_dt.date_naive() - dt.date_naive()).num_days() == 1) {
                if c.high > overnight_high {
                    overnight_high = c.high;
                }
                if c.low < overnight_low {
                    overnight_low = c.low;
                }
            }
        }
    }

    if overnight_high == f64::MIN || overnight_low == f64::MAX {
        // Fallback to recent 8 candles
        let lookback = hourly_candles.len().min(8);
        let recent = &hourly_candles[hourly_candles.len() - lookback..];
        overnight_high = recent.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        overnight_low = recent.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    }

    // Detect hourly swing highs and swing lows (short-term liquidity pools)
    let n = hourly_candles.len();
    let mut recent_highs = Vec::new();
    let mut recent_lows = Vec::new();

    for i in 2..n.saturating_sub(1) {
        let prev = &hourly_candles[i - 1];
        let curr = &hourly_candles[i];
        let next = &hourly_candles[i + 1];

        if curr.high > prev.high && curr.high > next.high {
            recent_highs.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.high,
                is_high: true,
                swept: false,
            });
        }
        if curr.low < prev.low && curr.low < next.low {
            recent_lows.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.low,
                is_high: false,
                swept: false,
            });
        }
    }

    // Check for "Inducement" (Episode 2: price dropping first to take sell-stops / induce shorts before driving to buy-stops)
    let mut sell_side_induced = false;
    let mut buy_side_induced = false;

    // Check the candle around 7:00-8:30 AM NY
    if let Some(recent_c) = hourly_candles.last() {
        if recent_c.low < overnight_low {
            sell_side_induced = true; // Sucker play: Sell stops taken first
        }
        if recent_c.high > overnight_high {
            buy_side_induced = true; // Sucker play: Buy stops taken first
        }
    }

    Some(HourlySessionFrame {
        weekly_open_time: monday_midnight,
        overnight_high,
        overnight_low,
        transposed_daily_dol: daily_dol,
        recent_swing_highs: recent_highs,
        recent_swing_lows: recent_lows,
        sell_side_induced,
        buy_side_induced,
    })
}
