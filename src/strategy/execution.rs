use serde::{Deserialize, Serialize};
use crate::types::{
    calculate_atr, Candle, FairValueGap, MarketStructureShift, SwingPoint, Timeframe, TradeMark,
};
use crate::strategy::liquidity::LiquiditySweepEvent;

/// Minimum FVG size as a fraction of ATR (carried over from the original engine).
const FVG_MIN_ATR: f64 = 0.08;
/// Stop buffer beyond the sweep-leg extreme as a fraction of ATR (carried over).
const SL_BUFFER_ATR: f64 = 0.20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSignal {
    pub time: i64,
    pub is_short: bool,
    pub entry_price: f64,
    pub stop_loss: f64,
    pub take_profit: f64,
    pub fvg: FairValueGap,
    pub mss: MarketStructureShift,
    pub marks: Vec<TradeMark>,
    pub target_imbalance: Option<(f64, f64)>, // (top, bottom) of opposing imbalance
}

/// Detects Fair Value Gaps on the execution timeframe (2-minute / 1-minute / 3-minute)
pub fn find_fair_value_gaps(candles: &[Candle], min_gap_points: f64) -> Vec<FairValueGap> {
    let mut fvgs = Vec::new();
    let n = candles.len();
    if n < 3 {
        return fvgs;
    }

    let atr = calculate_atr(candles, 14);
    let gap_threshold = min_gap_points.max(0.08 * atr);

    for i in 2..n {
        let c1 = &candles[i - 2];
        let c2 = &candles[i - 1]; // Displacement candle
        let c3 = &candles[i];

        // 1. Bearish FVG: Candle 1 Low > Candle 3 High
        if c1.low > c3.high && (c1.low - c3.high) >= gap_threshold {
            let top = c1.low;
            let bottom = c3.high;
            let midpoint = (top + bottom) / 2.0;

            fvgs.push(FairValueGap {
                id: i,
                timeframe: Timeframe::Min2,
                is_bullish: false,
                top,
                bottom,
                midpoint,
                candle_time: c2.time,
                mitigated: false,
                mitigated_time: None,
            });
        }

        // 2. Bullish FVG: Candle 3 Low > Candle 1 High
        if c3.low > c1.high && (c3.low - c1.high) >= gap_threshold {
            let top = c3.low;
            let bottom = c1.high;
            let midpoint = (top + bottom) / 2.0;

            fvgs.push(FairValueGap {
                id: i,
                timeframe: Timeframe::Min2,
                is_bullish: true,
                top,
                bottom,
                midpoint,
                candle_time: c2.time,
                mitigated: false,
                mitigated_time: None,
            });
        }
    }

    fvgs
}

/// Detects the Market Structure Shift (BMS/MSS) and its FVG after a liquidity sweep
/// (Episode 2 rules 6 and 7).
///
/// Bearish setup (buy-side sweep): the sweep leg's highest high is tracked; the most recent
/// 3-bar short-term low formed AFTER that high is the swing; the MSS is the first bearish candle
/// that CLOSES below it. The FVG is the 3-candle gap centred on that displacement candle
/// (candle before has a low above the high of the candle after). A new higher high resets the
/// leg. Bullish setups are the exact mirror (implemented by sign-flipping prices).
///
/// Only completed candles are used: the FVG needs the candle after the displacement, so nothing
/// is returned until that candle exists. If the first break leaves no FVG, no setup is returned.
pub fn detect_market_structure_shift(
    candles: &[Candle],
    sweep: &LiquiditySweepEvent,
) -> Option<(MarketStructureShift, FairValueGap)> {
    let n = candles.len();
    let s = candles.iter().position(|c| c.time >= sweep.sweep_time)?;
    let buy = sweep.is_buy_side;
    let sgn = if buy { 1.0 } else { -1.0 };

    // "Bearish-orientation" views of a candle (identity for buy-side sweeps, mirrored otherwise).
    let hi = |c: &Candle| if buy { c.high } else { -c.low };
    let lo = |c: &Candle| if buy { c.low } else { -c.high };
    let cl = |c: &Candle| sgn * c.close;
    let op = |c: &Candle| sgn * c.open;

    let mut ext = hi(&candles[s]);
    let mut ext_idx = s;
    let mut swing: Option<usize> = None;

    for i in s..n {
        let c = &candles[i];
        if hi(c) > ext {
            ext = hi(c);
            ext_idx = i;
            swing = None; // a new extreme starts a new leg
        }
        // 3-bar short-term low at i-1, confirmed by candle i, strictly after the leg extreme.
        if i >= 2 && i - 1 > ext_idx {
            let j = i - 1;
            if lo(&candles[j]) < lo(&candles[j - 1]) && lo(&candles[j]) < lo(c) {
                swing = Some(j);
            }
        }
        let Some(j) = swing else { continue };
        if i <= j || !(cl(c) < lo(&candles[j]) && cl(c) < op(c)) {
            continue;
        }

        // First displacement candle that closes through the swing.
        if i + 1 >= n {
            return None; // FVG third candle not complete yet
        }
        let c1 = &candles[i - 1];
        let c3 = &candles[i + 1];
        let atr = calculate_atr(&candles[..=i + 1], 14);
        let gap = lo(c1) - hi(c3);
        if gap < FVG_MIN_ATR * atr {
            return None;
        }
        let (top, bottom) = if buy { (lo(c1), hi(c3)) } else { (-hi(c3), -lo(c1)) };

        let fvg = FairValueGap {
            id: i,
            timeframe: Timeframe::Min2,
            is_bullish: !buy,
            top,
            bottom,
            midpoint: (top + bottom) / 2.0,
            candle_time: c.time,
            mitigated: false,
            mitigated_time: None,
        };
        let mss = MarketStructureShift {
            time: c.time,
            broken_swing: SwingPoint {
                index: j,
                time: candles[j].time,
                price: sgn * lo(&candles[j]),
                is_high: !buy,
                swept: false,
            },
            displacement_candle_time: c.time,
            is_bullish: !buy,
            fvg: Some(fvg.clone()),
        };
        return Some((mss, fvg));
    }
    None
}

/// Highest high (buy-side sweep) or lowest low (sell-side sweep) of the sweep leg, from the sweep
/// candle through `end_time` inclusive. Used for the stop and for drawing the sweep marker.
pub fn sweep_leg_extreme(candles: &[Candle], sweep: &LiquiditySweepEvent, end_time: i64) -> f64 {
    let leg = candles
        .iter()
        .filter(|c| c.time >= sweep.sweep_time && c.time <= end_time);
    if sweep.is_buy_side {
        leg.map(|c| c.high).fold(sweep.sweep_extreme, f64::max)
    } else {
        leg.map(|c| c.low).fold(sweep.sweep_extreme, f64::min)
    }
}

/// Evaluates the execution signal (Episode 2 rules 7 to 10). Every input is taken AS OF the
/// completion of the FVG's third candle, so the verdict never changes as later bars arrive:
///
/// - Premium/discount: 50% of the high/low of the supplied candles up to the FVG's completion;
///   shorts only from above it, longs only from below it.
/// - Entry: limit at the FVG's near edge (lower edge for shorts), filled by the first candle
///   AFTER the FVG completes that trades into the gap. The setup is void if the stop level is
///   taken before a fill.
/// - Stop: beyond the sweep-leg extreme (sweep candle through the displacement candle) plus a
///   volatility buffer.
/// - Target: the nearest UNMITIGATED opposing imbalance beyond the 50% level (for shorts, the
///   highest-topped untouched bullish gap below it; its top is the target). Falls back to the
///   daily draw on liquidity if none exists.
pub fn evaluate_execution_entry(
    candles: &[Candle],
    sweep: &LiquiditySweepEvent,
    mss: &MarketStructureShift,
    fvg: &FairValueGap,
    daily_target_price: f64,
) -> Option<ExecutionSignal> {
    let d = candles.iter().position(|c| c.time == fvg.candle_time)?;
    let c3_idx = d + 1;
    if c3_idx >= candles.len() {
        return None;
    }
    let s = candles.iter().position(|c| c.time >= sweep.sweep_time)?;
    let asof = &candles[..=c3_idx];

    let day_high = asof.iter().map(|c| c.high).fold(f64::MIN, f64::max);
    let day_low = asof.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    let equilibrium_50 = (day_high + day_low) / 2.0;
    let atr = calculate_atr(asof, 14);
    let sl_buffer = SL_BUFFER_ATR * atr;
    let min_gap = FVG_MIN_ATR * atr;

    if !fvg.is_bullish {
        // Bearish setup: the FVG must sit in premium (above the 50% level).
        if fvg.bottom < equilibrium_50 {
            return None;
        }
        let leg_high = candles[s..=d].iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let stop_loss = leg_high + sl_buffer;
        let entry_level = fvg.bottom;

        // Target: highest-topped untouched bullish imbalance below the 50% level.
        let mut target_imbalance: Option<(f64, f64)> = None; // (top, bottom)
        for i in 2..=c3_idx {
            let (c1, c3) = (&candles[i - 2], &candles[i]);
            if c3.low > c1.high && c3.low < equilibrium_50 && (c3.low - c1.high) >= min_gap {
                let top = c3.low;
                let untouched = candles[i + 1..=c3_idx].iter().all(|c| c.low > top);
                if untouched && target_imbalance.map_or(true, |(t, _)| top > t) {
                    target_imbalance = Some((top, c1.high));
                }
            }
        }
        let take_profit = target_imbalance
            .map(|(top, _)| top)
            .unwrap_or(daily_target_price);

        // Fill: first candle after the FVG completes that trades up into the gap.
        let mut entry_candle = None;
        for c in &candles[c3_idx + 1..] {
            if c.high > stop_loss {
                return None; // stop level taken before a fill: setup void
            }
            if c.high >= fvg.bottom && c.low <= fvg.top {
                entry_candle = Some(c);
                break;
            }
        }
        let ec = entry_candle?;

        let marks = vec![
            TradeMark {
                id: 1,
                time: sweep.sweep_time,
                color: "#e91e63".to_string(), // Magenta / Pink for sweep
                text: format!("15m Buy-Side Liquidity Swept at {:.2} (Stop Hunt)", sweep.swept_level),
                label: "SWEEP".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 2,
                time: mss.displacement_candle_time,
                color: "#2196f3".to_string(), // Blue for BMS/MSS
                text: format!("Market Structure Shift (MSS) broken below {:.2}", mss.broken_swing.price),
                label: "BMS".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 3,
                time: fvg.candle_time,
                color: "#ff9800".to_string(), // Orange for FVG
                text: format!("Bearish Fair Value Gap [{:.2} - {:.2}] in Premium", fvg.bottom, fvg.top),
                label: "FVG".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 4,
                time: ec.time,
                color: "#f44336".to_string(), // Red for Short Entry
                text: format!("SHORT ENTRY at {:.2} | SL: {:.2} | TP: {:.2}", entry_level, stop_loss, take_profit),
                label: "SELL".to_string(),
                min_size: 28,
            },
        ];

        Some(ExecutionSignal {
            time: ec.time,
            is_short: true,
            entry_price: entry_level,
            stop_loss,
            take_profit,
            fvg: fvg.clone(),
            mss: mss.clone(),
            marks,
            target_imbalance,
        })
    } else {
        // Bullish setup: the FVG must sit in discount (below the 50% level).
        if fvg.top > equilibrium_50 {
            return None;
        }
        let leg_low = candles[s..=d].iter().map(|c| c.low).fold(f64::MAX, f64::min);
        let stop_loss = leg_low - sl_buffer;
        let entry_level = fvg.top;

        // Target: lowest-bottomed untouched bearish imbalance above the 50% level.
        let mut target_imbalance: Option<(f64, f64)> = None; // (top, bottom)
        for i in 2..=c3_idx {
            let (c1, c3) = (&candles[i - 2], &candles[i]);
            if c1.low > c3.high && c3.high > equilibrium_50 && (c1.low - c3.high) >= min_gap {
                let bottom = c3.high;
                let untouched = candles[i + 1..=c3_idx].iter().all(|c| c.high < bottom);
                if untouched && target_imbalance.map_or(true, |(_, b)| bottom < b) {
                    target_imbalance = Some((c1.low, bottom));
                }
            }
        }
        let take_profit = target_imbalance
            .map(|(_, bottom)| bottom)
            .unwrap_or(daily_target_price);

        let mut entry_candle = None;
        for c in &candles[c3_idx + 1..] {
            if c.low < stop_loss {
                return None;
            }
            if c.low <= fvg.top && c.high >= fvg.bottom {
                entry_candle = Some(c);
                break;
            }
        }
        let ec = entry_candle?;

        let marks = vec![
            TradeMark {
                id: 1,
                time: sweep.sweep_time,
                color: "#4caf50".to_string(),
                text: format!("15m Sell-Side Liquidity Swept at {:.2} (Stop Hunt)", sweep.swept_level),
                label: "SWEEP".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 2,
                time: mss.displacement_candle_time,
                color: "#2196f3".to_string(),
                text: format!("Market Structure Shift (MSS) broken above {:.2}", mss.broken_swing.price),
                label: "BMS".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 3,
                time: fvg.candle_time,
                color: "#ff9800".to_string(),
                text: format!("Bullish Fair Value Gap [{:.2} - {:.2}] in Discount", fvg.bottom, fvg.top),
                label: "FVG".to_string(),
                min_size: 24,
            },
            TradeMark {
                id: 4,
                time: ec.time,
                color: "#4caf50".to_string(),
                text: format!("LONG ENTRY at {:.2} | SL: {:.2} | TP: {:.2}", entry_level, stop_loss, take_profit),
                label: "BUY".to_string(),
                min_size: 28,
            },
        ];

        Some(ExecutionSignal {
            time: ec.time,
            is_short: false,
            entry_price: entry_level,
            stop_loss,
            take_profit,
            fvg: fvg.clone(),
            mss: mss.clone(),
            marks,
            target_imbalance,
        })
    }
}
