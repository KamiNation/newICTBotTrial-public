use crate::data::MarketDataLoader;
use crate::types::{Candle, DirectionalBias, DolType, DrawOnLiquidity, SwingPoint, Timeframe};

/// Weekly session candles that are COMPLETE as of `as_of`. A session week closes Friday 17:00 NY,
/// so the week in progress is never included.
pub fn completed_weeks(weekly: &[Candle], as_of: i64) -> Vec<Candle> {
    weekly
        .iter()
        .filter(|c| MarketDataLoader::week_close_time(c.time) <= as_of)
        .cloned()
        .collect()
}

/// Weekly bias as of `as_of`, from completed weeks only. ICT sets it before the week opens
/// (preferably on the weekend), so the week being traded must not influence it.
pub fn weekly_bias_as_of(weekly_all: &[Candle], as_of: i64) -> (DirectionalBias, String) {
    evaluate_weekly_bias(&completed_weeks(weekly_all, as_of))
}

/// Layer 1: Weekly Bias Determination (Episode 2: sents 154-202)
/// Before the week opens, evaluate the expansion direction of the next weekly candle.
/// Evaluates whether the underlying market tone is expanding lower or higher toward HTF liquidity.
/// Structural proxy for the weekly bias. Expects COMPLETED weekly session candles only (the last
/// element is the most recent finished week); use `weekly_bias_as_of` unless they are already filtered.
pub fn evaluate_weekly_bias(weekly_candles: &[Candle]) -> (DirectionalBias, String) {
    if weekly_candles.len() < 2 {
        return (DirectionalBias::Neutral, "Insufficient weekly data".to_string());
    }

    let n = weekly_candles.len();
    let prev_week = &weekly_candles[n - 1];
    let older_week = &weekly_candles[n - 2];

    // Check market structure on weekly
    let mut reason = Vec::new();

    // 1. Bearish Weekly Expansion criteria:
    // - Previous candle closed below older candle low, or rejected a premium high
    // - Recent trend shows lower highs and lower lows
    let prev_closed_bearish = prev_week.close < prev_week.open;
    let making_lower_lows = prev_week.low < older_week.low;
    let making_lower_highs = prev_week.high < older_week.high;

    if prev_closed_bearish && making_lower_lows && making_lower_highs {
        reason.push("Weekly market structure is bearish with consecutive lower highs and lower lows");
        reason.push("Anticipating weekly expansion lower toward HTF sell-side liquidity / old daily lows");
        return (DirectionalBias::Bearish, reason.join("; "));
    }

    // 2. Bullish Weekly Expansion criteria:
    let prev_closed_bullish = prev_week.close > prev_week.open;
    let making_higher_highs = prev_week.high > older_week.high;
    let making_higher_lows = prev_week.low > older_week.low;

    if prev_closed_bullish && making_higher_highs && making_higher_lows {
        reason.push("Weekly market structure is bullish with consecutive higher highs and higher lows");
        reason.push("Anticipating weekly expansion higher toward HTF buy-side liquidity / old daily highs");
        return (DirectionalBias::Bullish, reason.join("; "));
    }

    // If indecisive candle prior, look at last 5 weeks momentum
    let lookback = n.min(5);
    let start_close = weekly_candles[n - lookback].close;
    let last_close = prev_week.close;

    if last_close < start_close {
        (
            DirectionalBias::Bearish,
            "Multi-week tone is net bearish; expecting weekly candle to expand on the lower end".to_string(),
        )
    } else {
        (
            DirectionalBias::Bullish,
            "Multi-week tone is net bullish; expecting weekly candle to expand on the upper end".to_string(),
        )
    }
}

/// Layer 2: Daily Draw on Liquidity (DOL) (Episode 2: sents 203-232)
/// Finds swing highs and swing lows on the daily chart.
/// "Market draws to one of two things: STOPS (liquidity) or an IMBALANCE (FVG)."
pub fn identify_daily_draw_on_liquidity(
    daily_candles: &[Candle],
    bias: DirectionalBias,
) -> Option<DrawOnLiquidity> {
    if daily_candles.len() < 5 {
        return None;
    }

    let n = daily_candles.len();
    let current_price = daily_candles.last()?.close;

    // Detect daily swing points (3-bar fractal)
    let mut swing_highs: Vec<SwingPoint> = Vec::new();
    let mut swing_lows: Vec<SwingPoint> = Vec::new();

    // The last candle is the session in progress, so it cannot confirm a swing: i stops at n-3.
    for i in 1..n.saturating_sub(2) {
        let prev = &daily_candles[i - 1];
        let curr = &daily_candles[i];
        let next = &daily_candles[i + 1];

        if curr.high > prev.high && curr.high > next.high {
            swing_highs.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.high,
                is_high: true,
                swept: false,
            });
        }

        if curr.low < prev.low && curr.low < next.low {
            swing_lows.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.low,
                is_high: false,
                swept: false,
            });
        }
    }

    // Check Daily Fair Value Gaps (Imbalances)
    // Bullish FVG: low[i] > high[i-2]
    // Bearish FVG: high[i] < low[i-2]
    match bias {
        DirectionalBias::Bearish => {
            // Looking for Old Daily Low (Sell-Side Liquidity) below current price
            // Find the most prominent unviolated daily swing low below current price
            let mut target_low = None;
            for sw in swing_lows.iter().rev() {
                // Live only until a later candle (including the one in progress) trades through it.
                let unraided = daily_candles[sw.index + 1..].iter().all(|c| c.low > sw.price);
                if unraided && sw.price < current_price {
                    target_low = Some(sw);
                    break;
                }
            }

            if let Some(target) = target_low {
                Some(DrawOnLiquidity {
                    dol_type: DolType::SellSideLiquidity(target.price),
                    target_price: target.price,
                    timeframe: Timeframe::Daily,
                    description: format!("Old Daily Low (Sell-Side Liquidity Pool) at {:.2}", target.price),
                })
            } else {
                // Check for unmitigated daily bullish FVG below price
                for i in (2..n).rev() {
                    let c1 = &daily_candles[i - 2];
                    let c3 = &daily_candles[i];
                    if c3.low > c1.high && c1.high < current_price {
                        return Some(DrawOnLiquidity {
                            dol_type: DolType::ImbalanceFvg(c3.low, c1.high),
                            target_price: c1.high,
                            timeframe: Timeframe::Daily,
                            description: format!("Daily Bullish FVG Imbalance target at [{:.2} - {:.2}]", c1.high, c3.low),
                        });
                    }
                }
                None
            }
        }
        DirectionalBias::Bullish => {
            // Looking for Old Daily High (Buy-Side Liquidity) above current price
            let mut target_high = None;
            for sw in swing_highs.iter().rev() {
                let unraided = daily_candles[sw.index + 1..].iter().all(|c| c.high < sw.price);
                if unraided && sw.price > current_price {
                    target_high = Some(sw);
                    break;
                }
            }

            if let Some(target) = target_high {
                Some(DrawOnLiquidity {
                    dol_type: DolType::BuySideLiquidity(target.price),
                    target_price: target.price,
                    timeframe: Timeframe::Daily,
                    description: format!("Old Daily High (Buy-Side Liquidity Pool) at {:.2}", target.price),
                })
            } else {
                // Check for unmitigated daily bearish FVG above price
                for i in (2..n).rev() {
                    let c1 = &daily_candles[i - 2];
                    let c3 = &daily_candles[i];
                    if c1.low > c3.high && c3.high > current_price {
                        return Some(DrawOnLiquidity {
                            dol_type: DolType::ImbalanceFvg(c1.low, c3.high),
                            target_price: c3.high,
                            timeframe: Timeframe::Daily,
                            description: format!("Daily Bearish FVG Imbalance target at [{:.2} - {:.2}]", c3.high, c1.low),
                        });
                    }
                }
                None
            }
        }
        DirectionalBias::Neutral => None,
    }
}
