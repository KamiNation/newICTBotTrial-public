use chrono::{TimeZone, Timelike};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};
use crate::types::{calculate_atr, Candle, DirectionalBias, RelativeEqualPoints, SwingPoint};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquiditySweepEvent {
    pub sweep_time: i64,
    pub swept_level: f64,
    pub is_buy_side: bool, // true = swept buy stops above equal highs; false = swept sell stops
    pub sweep_extreme: f64, // The highest high (for buy stop sweep) or lowest low (for sell stop sweep)
    pub candle_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityPool {
    pub price: f64,
    pub is_buy_side: bool,
    pub start_time: i64,
    pub description: String,
    pub swept: bool,
    pub sweep_time: Option<i64>,
}

/// Check if a given timestamp falls within the NY Killzone (08:30 to 11:00 AM NY)
pub fn is_in_ny_killzone(timestamp: i64) -> bool {
    if let Some(dt) = New_York.timestamp_opt(timestamp, 0).single() {
        let hour = dt.hour();
        let minute = dt.minute();
        // 8:30 AM to 11:00 AM NY Local Time (Sweet Spot)
        (hour == 8 && minute >= 30) || (hour == 9) || (hour == 10) || (hour == 11 && minute == 0)
    } else {
        false
    }
}

/// Layer 4: 15-Minute Engineered Stop Hunt & Equal Highs/Lows (Episode 2: sents 279-319)
/// Dynamically detects prominent liquidity pools (Equal Highs/Lows and Session Extremes).
/// Asset-agnostic: uses ATR-normalized tolerance to support NQ, VIX75, FX, etc.
pub fn find_liquidity_pools(
    candles_15m: &[Candle],
    session_start_time: Option<i64>,
) -> (Vec<LiquidityPool>, Vec<LiquidityPool>) {
    let n = candles_15m.len();
    if n < 3 {
        return (Vec::new(), Vec::new());
    }

    let atr = calculate_atr(candles_15m, 14);
    let tolerance = 0.25 * atr; // Adaptive tolerance: scales to any asset's volatility

    let cutoff = session_start_time.unwrap_or_else(|| {
        candles_15m.last().map(|c| c.time - 86400).unwrap_or(0)
    });

    let session_candles: Vec<&Candle> = candles_15m
        .iter()
        .filter(|c| c.time >= cutoff)
        .collect();

    let m = session_candles.len();
    if m < 3 {
        return (Vec::new(), Vec::new());
    }

    let mut swing_highs = Vec::new();
    let mut swing_lows = Vec::new();

    for i in 1..m.saturating_sub(1) {
        let prev = session_candles[i - 1];
        let curr = session_candles[i];
        let next = session_candles[i + 1];

        if curr.high >= prev.high && curr.high >= next.high {
            let swept = (i + 1..m).any(|k| session_candles[k].high > curr.high);
            swing_highs.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.high,
                is_high: true,
                swept,
            });
        }
        if curr.low <= prev.low && curr.low <= next.low {
            let swept = (i + 1..m).any(|k| session_candles[k].low < curr.low);
            swing_lows.push(SwingPoint {
                index: i,
                time: curr.time,
                price: curr.low,
                is_high: false,
                swept,
            });
        }
    }

    // Only active, unswept swing points represent resting liquidity
    let active_highs: Vec<SwingPoint> = swing_highs.into_iter().filter(|p| !p.swept).collect();
    let active_lows: Vec<SwingPoint> = swing_lows.into_iter().filter(|p| !p.swept).collect();

    // Cluster swing points within tolerance into Relative Equal Highs/Lows
    let mut buyside_pools = cluster_swing_points(&active_highs, tolerance, true);
    let mut sellside_pools = cluster_swing_points(&active_lows, tolerance, false);

    // Ensure prominent Session High and Session Low are present
    let session_high = session_candles.iter().map(|c| c.high).fold(f64::MIN, f64::max);
    if let Some(c) = session_candles.iter().find(|c| (c.high - session_high).abs() < 1e-4) {
        if !buyside_pools.iter().any(|p| (p.price - session_high).abs() <= tolerance) {
            buyside_pools.push(LiquidityPool {
                price: session_high,
                is_buy_side: true,
                start_time: c.time,
                description: format!("Session High ({:.2}) [Buy Stops]", session_high),
                swept: false,
                sweep_time: None,
            });
        }
    }

    let session_low = session_candles.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    if let Some(c) = session_candles.iter().find(|c| (c.low - session_low).abs() < 1e-4) {
        if !sellside_pools.iter().any(|p| (p.price - session_low).abs() <= tolerance) {
            sellside_pools.push(LiquidityPool {
                price: session_low,
                is_buy_side: false,
                start_time: c.time,
                description: format!("Session Low ({:.2}) [Sell Stops]", session_low),
                swept: false,
                sweep_time: None,
            });
        }
    }

    // For Buyside pools: sort ascending so closest pools above active range come first
    buyside_pools.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
    buyside_pools.truncate(3);

    // For Sellside pools: sort descending so closest pools below active range come first
    sellside_pools.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
    sellside_pools.truncate(3);

    (buyside_pools, sellside_pools)
}

/// Helper function to cluster swing points into clean, non-redundant liquidity pools
fn cluster_swing_points(points: &[SwingPoint], tolerance: f64, is_buy_side: bool) -> Vec<LiquidityPool> {
    if points.is_empty() {
        return Vec::new();
    }

    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

    let mut clusters: Vec<Vec<SwingPoint>> = Vec::new();
    for pt in sorted {
        if let Some(last_cluster) = clusters.last_mut() {
            let cluster_avg = last_cluster.iter().map(|p| p.price).sum::<f64>() / last_cluster.len() as f64;
            if (pt.price - cluster_avg).abs() <= tolerance {
                last_cluster.push(pt);
                continue;
            }
        }
        clusters.push(vec![pt]);
    }

    let mut pools = Vec::new();
    for cl in clusters {
        let count = cl.len();
        let avg_price = cl.iter().map(|p| p.price).sum::<f64>() / count as f64;
        let start_time = cl.iter().map(|p| p.time).min().unwrap_or(0);

        if count >= 2 {
            pools.push(LiquidityPool {
                price: avg_price,
                is_buy_side,
                start_time,
                description: if is_buy_side {
                    format!("Relative Equal Highs ({:.2}) [Buy Stops]", avg_price)
                } else {
                    format!("Relative Equal Lows ({:.2}) [Sell Stops]", avg_price)
                },
                swept: false,
                sweep_time: None,
            });
        }
    }

    // Ensure the prominent session extreme is present
    if is_buy_side {
        if let Some(highest) = points.iter().max_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal)) {
            if !pools.iter().any(|p| (p.price - highest.price).abs() <= tolerance) {
                pools.push(LiquidityPool {
                    price: highest.price,
                    is_buy_side: true,
                    start_time: highest.time,
                    description: format!("Prominent Session High ({:.2}) [Buy Stops]", highest.price),
                    swept: false,
                    sweep_time: None,
                });
            }
        }
        // For Buyside pools: sort ascending so the closest pools above the active dealing range come first
        pools.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
        pools.truncate(3);
    } else {
        if let Some(lowest) = points.iter().min_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal)) {
            if !pools.iter().any(|p| (p.price - lowest.price).abs() <= tolerance) {
                pools.push(LiquidityPool {
                    price: lowest.price,
                    is_buy_side: false,
                    start_time: lowest.time,
                    description: format!("Prominent Session Low ({:.2}) [Sell Stops]", lowest.price),
                    swept: false,
                    sweep_time: None,
                });
            }
        }
        // For Sellside pools: sort descending so the closest pools below the active dealing range come first
        pools.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
        pools.truncate(3);
    }

    pools
}

/// Backward compatibility wrapper for relative equal points
pub fn find_relative_equal_points(
    candles_15m: &[Candle],
    _tolerance_points: f64,
) -> (Vec<RelativeEqualPoints>, Vec<RelativeEqualPoints>) {
    let (buyside, sellside) = find_liquidity_pools(candles_15m, None);
    let eqh = buyside.iter().map(|p| RelativeEqualPoints {
        points: vec![],
        price_level: p.price,
        is_high: true,
        swept: p.swept,
    }).collect();
    let eql = sellside.iter().map(|p| RelativeEqualPoints {
        points: vec![],
        price_level: p.price,
        is_high: false,
        swept: p.swept,
    }).collect();
    (eqh, eql)
}

/// Detects if a candle in the Killzone has swept a key liquidity pool
/// When HTF bias is Bearish, prioritizes Buyside sweeps (Stop Hunt on Buy Stops before expansion lower).
/// When HTF bias is Bullish, prioritizes Sellside sweeps (Stop Hunt on Sell Stops before expansion higher).
pub fn detect_liquidity_sweep_from_pools(
    candles_exec: &[Candle],
    buyside_pools: &[LiquidityPool],
    sellside_pools: &[LiquidityPool],
    bias: Option<&DirectionalBias>,
) -> Option<LiquiditySweepEvent> {
    for (idx, candle) in candles_exec.iter().enumerate() {
        if !is_in_ny_killzone(candle.time) {
            continue;
        }

        // When HTF bias is Bearish (or Neutral/None), look for sweep of Buyside Liquidity
        let check_buyside = bias.map(|b| *b != DirectionalBias::Bullish).unwrap_or(true);
        if check_buyside {
            for pool in buyside_pools {
                if candle.high > pool.price && candle.time > pool.start_time {
                    return Some(LiquiditySweepEvent {
                        sweep_time: candle.time,
                        swept_level: pool.price,
                        is_buy_side: true,
                        sweep_extreme: candle.high,
                        candle_index: idx,
                    });
                }
            }
        }

        // When HTF bias is Bullish (or Neutral/None), look for sweep of Sellside Liquidity
        let check_sellside = bias.map(|b| *b != DirectionalBias::Bearish).unwrap_or(true);
        if check_sellside {
            for pool in sellside_pools {
                if candle.low < pool.price && candle.time > pool.start_time {
                    return Some(LiquiditySweepEvent {
                        sweep_time: candle.time,
                        swept_level: pool.price,
                        is_buy_side: false,
                        sweep_extreme: candle.low,
                        candle_index: idx,
                    });
                }
            }
        }
    }

    None
}

/// Legacy wrapper for detect_liquidity_sweep
pub fn detect_liquidity_sweep(
    candles_15m: &[Candle],
    equal_highs: &[RelativeEqualPoints],
    equal_lows: &[RelativeEqualPoints],
) -> Option<LiquiditySweepEvent> {
    for (idx, candle) in candles_15m.iter().enumerate() {
        if !is_in_ny_killzone(candle.time) {
            continue;
        }

        for eqh in equal_highs {
            if candle.high > eqh.price_level {
                return Some(LiquiditySweepEvent {
                    sweep_time: candle.time,
                    swept_level: eqh.price_level,
                    is_buy_side: true,
                    sweep_extreme: candle.high,
                    candle_index: idx,
                });
            }
        }

        for eql in equal_lows {
            if candle.low < eql.price_level {
                return Some(LiquiditySweepEvent {
                    sweep_time: candle.time,
                    swept_level: eql.price_level,
                    is_buy_side: false,
                    sweep_extreme: candle.low,
                    candle_index: idx,
                });
            }
        }
    }

    None
}
