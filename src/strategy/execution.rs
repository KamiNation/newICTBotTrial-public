use serde::{Deserialize, Serialize};
use crate::types::{
    calculate_atr, Candle, FairValueGap, MarketStructureShift, SwingPoint, Timeframe, TradeMark,
};
use crate::strategy::liquidity::LiquiditySweepEvent;

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

/// Detects Market Structure Shift (BMS / MSS) on 2-minute candles following a liquidity sweep
pub fn detect_market_structure_shift(
    candles_2m: &[Candle],
    sweep: &LiquiditySweepEvent,
) -> Option<(MarketStructureShift, FairValueGap)> {
    let n = candles_2m.len();
    if n < 4 {
        return None;
    }

    // Find the 2m candle corresponding to or immediately following the sweep
    let mut sweep_index = 0;
    for (i, c) in candles_2m.iter().enumerate() {
        if c.time >= sweep.sweep_time {
            sweep_index = i;
            break;
        }
    }

    let atr = calculate_atr(candles_2m, 14);
    let min_gap = 0.08 * atr;

    // If sweep was Buy-Side (Swept equal highs -> Bearish setup):
    // We look for the short-term swing low created during/after the sweep,
    // and an aggressive displacement candle that CLOSES below that swing low.
    if sweep.is_buy_side {
        // Find recent short-term swing low
        let mut intermediate_low = None;
        let lookback_start = sweep_index.saturating_sub(6);
        for i in lookback_start..n.saturating_sub(1) {
            let curr = &candles_2m[i];
            if i > 0 && i < n - 1 {
                let prev = &candles_2m[i - 1];
                let next = &candles_2m[i + 1];
                if curr.low < prev.low && curr.low < next.low {
                    intermediate_low = Some(SwingPoint {
                        index: i,
                        time: curr.time,
                        price: curr.low,
                        is_high: false,
                        swept: false,
                    });
                    break;
                }
            }
        }

        let swing_low = intermediate_low.or_else(|| {
            // Fallback: lowest point between lookback and current
            candles_2m[lookback_start..n.saturating_sub(1)]
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.low.partial_cmp(&b.1.low).unwrap())
                .map(|(idx, c)| SwingPoint {
                    index: lookback_start + idx,
                    time: c.time,
                    price: c.low,
                    is_high: false,
                    swept: false,
                })
        })?;

        // Check for displacement candle closing below this swing low
        for d in (swing_low.index + 1)..n {
            let disp_candle = &candles_2m[d];
            if disp_candle.close < swing_low.price && disp_candle.is_bearish() {
                // Check if a Bearish FVG was formed around this displacement candle
                let mut found_fvg = None;
                if d + 1 < n && d >= 1 {
                    let c1 = &candles_2m[d - 1];
                    let c3 = &candles_2m[d + 1];
                    if c1.low > c3.high && (c1.low - c3.high) >= min_gap {
                        found_fvg = Some((c1.low, c3.high, disp_candle.time));
                    }
                }
                if found_fvg.is_none() && d >= 2 {
                    let c1 = &candles_2m[d - 2];
                    let c3 = &candles_2m[d];
                    if c1.low > c3.high && (c1.low - c3.high) >= min_gap {
                        found_fvg = Some((c1.low, c3.high, candles_2m[d - 1].time));
                    }
                }

                if let Some((top, bottom, candle_time)) = found_fvg {
                    let fvg = FairValueGap {
                        id: d,
                        timeframe: Timeframe::Min2,
                        is_bullish: false,
                        top,
                        bottom,
                        midpoint: (top + bottom) / 2.0,
                        candle_time,
                        mitigated: false,
                        mitigated_time: None,
                    };

                    let mss = MarketStructureShift {
                        time: disp_candle.time,
                        broken_swing: swing_low,
                        displacement_candle_time: disp_candle.time,
                        is_bullish: false,
                        fvg: Some(fvg.clone()),
                    };

                    return Some((mss, fvg));
                }
            }
        }
    } else {
        // Bullish setup (Swept equal lows -> Bullish setup):
        let mut intermediate_high = None;
        let lookback_start = sweep_index.saturating_sub(6);
        for i in lookback_start..n.saturating_sub(1) {
            let curr = &candles_2m[i];
            if i > 0 && i < n - 1 {
                let prev = &candles_2m[i - 1];
                let next = &candles_2m[i + 1];
                if curr.high > prev.high && curr.high > next.high {
                    intermediate_high = Some(SwingPoint {
                        index: i,
                        time: curr.time,
                        price: curr.high,
                        is_high: true,
                        swept: false,
                    });
                    break;
                }
            }
        }

        let swing_high = intermediate_high.or_else(|| {
            candles_2m[lookback_start..n.saturating_sub(1)]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.high.partial_cmp(&b.1.high).unwrap())
                .map(|(idx, c)| SwingPoint {
                    index: lookback_start + idx,
                    time: c.time,
                    price: c.high,
                    is_high: true,
                    swept: false,
                })
        })?;

        for i in (swing_high.index + 1)..n {
            let disp_candle = &candles_2m[i];
            if disp_candle.close > swing_high.price && disp_candle.is_bullish() {
                let mut found_fvg = None;
                if i + 1 < n && i >= 1 {
                    let c1 = &candles_2m[i - 1];
                    let c3 = &candles_2m[i + 1];
                    if c3.low > c1.high && (c3.low - c1.high) >= min_gap {
                        found_fvg = Some((c3.low, c1.high, disp_candle.time));
                    }
                }
                if found_fvg.is_none() && i >= 2 {
                    let c1 = &candles_2m[i - 2];
                    let c3 = &candles_2m[i];
                    if c3.low > c1.high && (c3.low - c1.high) >= min_gap {
                        found_fvg = Some((c3.low, c1.high, candles_2m[i - 1].time));
                    }
                }

                if let Some((top, bottom, candle_time)) = found_fvg {
                    let fvg = FairValueGap {
                        id: i,
                        timeframe: Timeframe::Min2,
                        is_bullish: true,
                        top,
                        bottom,
                        midpoint: (top + bottom) / 2.0,
                        candle_time,
                        mitigated: false,
                        mitigated_time: None,
                    };

                    let mss = MarketStructureShift {
                        time: disp_candle.time,
                        broken_swing: swing_high,
                        displacement_candle_time: disp_candle.time,
                        is_bullish: true,
                        fvg: Some(fvg.clone()),
                    };

                    return Some((mss, fvg));
                }
            }
        }
    }

    None
}

/// Evaluates Layer 5 Execution Signal:
/// 1. Premium / Discount check (Fib 50% Equilibrium).
/// 2. Entry limit into the FVG.
/// 3. Stop loss anchored at the sweep extreme + volatility buffer.
/// 4. Take profit target anchored at opposing imbalance or Daily DOL.
pub fn evaluate_execution_entry(
    candles_2m: &[Candle],
    sweep: &LiquiditySweepEvent,
    mss: &MarketStructureShift,
    fvg: &FairValueGap,
    daily_target_price: f64,
    day_high: f64,
    day_low: f64,
) -> Option<ExecutionSignal> {
    let equilibrium_50 = (day_high + day_low) / 2.0;
    let atr = calculate_atr(candles_2m, 14);
    let sl_buffer = 0.20 * atr;

    // Check Premium / Discount condition (Episode 2: sents 360-390)
    if !fvg.is_bullish {
        // Bearish setup: entry MUST be in Premium (> 50% equilibrium)
        if fvg.bottom < equilibrium_50 {
            // Imbalance is below 50% -> not a high-probability premium short!
            return None;
        }

        let entry_level = fvg.bottom;
        let stop_loss = sweep.sweep_extreme + sl_buffer;

        // Scan earlier candles for an unmitigated bullish FVG in Discount (< 50%)
        let mut target_imbalance = None;
        for i in 2..candles_2m.len() {
            let c1 = &candles_2m[i - 2];
            let c3 = &candles_2m[i];
            if c3.low > c1.high && c3.low < equilibrium_50 {
                target_imbalance = Some((c3.low, c1.high));
                break;
            }
        }

        // Take profit: target opposing imbalance top if available, else daily DOL
        let take_profit = target_imbalance
            .map(|(top, _)| top)
            .unwrap_or(daily_target_price);

        // Check if price retraced UP into the FVG (entry zone)
        let mut entry_candle = None;
        for c in candles_2m.iter() {
            if c.time > fvg.candle_time && c.high >= fvg.bottom && c.low <= fvg.top {
                entry_candle = Some(c);
                break;
            }
        }

        if let Some(ec) = entry_candle {
            let mut marks = Vec::new();
            marks.push(TradeMark {
                id: 1,
                time: sweep.sweep_time,
                color: "#e91e63".to_string(), // Magenta / Pink for sweep
                text: format!("15m Buy-Side Liquidity Swept at {:.2} (Stop Hunt)", sweep.swept_level),
                label: "SWEEP".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 2,
                time: mss.displacement_candle_time,
                color: "#2196f3".to_string(), // Blue for BMS/MSS
                text: format!("Market Structure Shift (MSS) broken below {:.2}", mss.broken_swing.price),
                label: "BMS".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 3,
                time: fvg.candle_time,
                color: "#ff9800".to_string(), // Orange for FVG
                text: format!("Bearish Fair Value Gap [{:.2} - {:.2}] in Premium", fvg.bottom, fvg.top),
                label: "FVG".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 4,
                time: ec.time,
                color: "#f44336".to_string(), // Red for Short Entry
                text: format!("SHORT ENTRY at {:.2} | SL: {:.2} | TP: {:.2}", entry_level, stop_loss, take_profit),
                label: "SELL".to_string(),
                min_size: 28,
            });

            return Some(ExecutionSignal {
                time: ec.time,
                is_short: true,
                entry_price: entry_level,
                stop_loss,
                take_profit,
                fvg: fvg.clone(),
                mss: mss.clone(),
                marks,
                target_imbalance,
            });
        }
    } else {
        // Bullish setup: entry MUST be in Discount (< 50% equilibrium)
        if fvg.top > equilibrium_50 {
            return None;
        }

        let entry_level = fvg.top;
        let stop_loss = sweep.sweep_extreme - sl_buffer;

        // Scan earlier candles for an unmitigated bearish FVG in Premium (> 50%)
        let mut target_imbalance = None;
        for i in 2..candles_2m.len() {
            let c1 = &candles_2m[i - 2];
            let c3 = &candles_2m[i];
            if c1.low > c3.high && c1.low > equilibrium_50 {
                target_imbalance = Some((c1.low, c3.high));
                break;
            }
        }

        let take_profit = target_imbalance
            .map(|(_, bot)| bot)
            .unwrap_or(daily_target_price);

        let mut entry_candle = None;
        for c in candles_2m.iter() {
            if c.time > fvg.candle_time && c.low <= fvg.top && c.high >= fvg.bottom {
                entry_candle = Some(c);
                break;
            }
        }

        if let Some(ec) = entry_candle {
            let mut marks = Vec::new();
            marks.push(TradeMark {
                id: 1,
                time: sweep.sweep_time,
                color: "#4caf50".to_string(),
                text: format!("15m Sell-Side Liquidity Swept at {:.2} (Stop Hunt)", sweep.swept_level),
                label: "SWEEP".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 2,
                time: mss.displacement_candle_time,
                color: "#2196f3".to_string(),
                text: format!("Market Structure Shift (MSS) broken above {:.2}", mss.broken_swing.price),
                label: "BMS".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 3,
                time: fvg.candle_time,
                color: "#ff9800".to_string(),
                text: format!("Bullish Fair Value Gap [{:.2} - {:.2}] in Discount", fvg.bottom, fvg.top),
                label: "FVG".to_string(),
                min_size: 24,
            });

            marks.push(TradeMark {
                id: 4,
                time: ec.time,
                color: "#4caf50".to_string(),
                text: format!("LONG ENTRY at {:.2} | SL: {:.2} | TP: {:.2}", entry_level, stop_loss, take_profit),
                label: "BUY".to_string(),
                min_size: 28,
            });

            return Some(ExecutionSignal {
                time: ec.time,
                is_short: false,
                entry_price: entry_level,
                stop_loss,
                take_profit,
                fvg: fvg.clone(),
                mss: mss.clone(),
                marks,
                target_imbalance,
            });
        }
    }

    None
}
