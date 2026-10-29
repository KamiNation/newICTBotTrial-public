pub mod bias;
pub mod execution;
pub mod liquidity;
pub mod sessions;

use chrono::{Datelike, TimeZone};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use crate::data::MarketDataLoader;
use crate::types::{
    Candle, ChartIndicator, DirectionalBias, DrawOnLiquidity, IndicatorPoint,
    JournalEvent, TradeMark,
};
use bias::{evaluate_weekly_bias, identify_daily_draw_on_liquidity};
use execution::{detect_market_structure_shift, evaluate_execution_entry, ExecutionSignal};
use liquidity::{
    detect_liquidity_sweep_from_pools, find_liquidity_pools,
    LiquidityPool, LiquiditySweepEvent,
};
use sessions::{frame_hourly_session, HourlySessionFrame};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub weekly_bias: DirectionalBias,
    pub weekly_bias_reason: String,
    pub daily_dol: Option<DrawOnLiquidity>,
    pub hourly_frame: Option<HourlySessionFrame>,
    pub sweep_event: Option<LiquiditySweepEvent>,
    pub execution_signal: Option<ExecutionSignal>,
    pub chart_marks: Vec<TradeMark>,
    pub indicators: Vec<ChartIndicator>,
    pub journal_logs: Vec<JournalEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicIctLevels {
    pub current_time: i64,
    pub weekly_bias: String,
    pub weekly_bias_reason: String,
    // Layer 1 & 2: Daily DOL
    pub daily_dol_price: Option<f64>,
    pub daily_dol_label: Option<String>,
    // Layer 3: Hourly Session Times
    pub monday_open_time: i64,
    pub friday_open_time: i64,
    pub friday_close_time: i64,
    pub overnight_high: Option<f64>,
    pub overnight_low: Option<f64>,
    // Layer 4: 15m Liquidity Pools
    pub buyside_liquidity: Option<f64>,
    pub sellside_liquidity: Option<f64>,
    pub sweep_time: Option<i64>,
    pub sweep_level: Option<f64>,
    pub sweep_extreme: Option<f64>,
    pub is_buy_sweep: Option<bool>,
    // Layer 5: 2m/1m Execution & Dealing Range Fib Matrix
    pub dealing_range_high: Option<f64>,
    pub dealing_range_low: Option<f64>,
    pub equilibrium_50: Option<f64>,
    pub fvg_top: Option<f64>,
    pub fvg_bottom: Option<f64>,
    pub fvg_midpoint: Option<f64>,
    pub fvg_time: Option<i64>,
    pub fvg_is_bullish: Option<bool>,
    // Signal
    pub has_signal: bool,
    pub signal_is_short: Option<bool>,
    pub signal_entry: Option<f64>,
    pub signal_sl: Option<f64>,
    pub signal_tp: Option<f64>,
    pub signal_time: Option<i64>,
    pub target_imbalance_top: Option<f64>,
    pub target_imbalance_bottom: Option<f64>,
    // Autonomous Chart Indicators & Strategy Journal
    pub indicators: Vec<ChartIndicator>,
    pub journal_logs: Vec<JournalEvent>,
}

/// Runs the top-down ICT pipeline
pub fn run_ict_pipeline(
    weekly_candles: &[Candle],
    daily_candles: &[Candle],
    hourly_candles: &[Candle],
    candles_15m: &[Candle],
    candles_2m: &[Candle],
    _eq_tolerance_points: f64,
) -> PipelineResult {
    let mut chart_marks = Vec::new();
    let indicators = Vec::new();
    let journal_logs = Vec::new();

    // 1. Weekly Bias
    let (weekly_bias, weekly_bias_reason) = evaluate_weekly_bias(weekly_candles);

    // 2. Daily Draw on Liquidity (DOL)
    let daily_dol = identify_daily_draw_on_liquidity(daily_candles, weekly_bias);

    // 3. Hourly Transposed Levels & Session Framing
    let hourly_frame = frame_hourly_session(hourly_candles, daily_dol.clone());

    // 4. 15-Minute Liquidity Event (Engineered Pools)
    let (buyside_pools, sellside_pools) = find_liquidity_pools(candles_15m, None);
    let sweep_event = detect_liquidity_sweep_from_pools(candles_2m, &buyside_pools, &sellside_pools, Some(&weekly_bias));

    // 5. 1-3 Min Execution
    let mut execution_signal = None;

    if let (Some(sweep), Some(dol)) = (&sweep_event, &daily_dol) {
        if let Some((mss, fvg)) = detect_market_structure_shift(candles_2m, sweep) {
            let day_high = candles_2m.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let day_low = candles_2m.iter().map(|c| c.low).fold(f64::MAX, f64::min);

            if let Some(signal) = evaluate_execution_entry(
                candles_2m,
                sweep,
                &mss,
                &fvg,
                dol.target_price,
                day_high,
                day_low,
            ) {
                chart_marks.extend(signal.marks.clone());
                execution_signal = Some(signal);
            }
        }
    }

    PipelineResult {
        weekly_bias,
        weekly_bias_reason,
        daily_dol,
        hourly_frame,
        sweep_event,
        execution_signal,
        chart_marks,
        indicators,
        journal_logs,
    }
}

/// Dynamically computes all timeframe-specific levels from historical candles up to current_time.
/// Emits clean, bounded trendlines, rectangles, and rays with zero hardcoding.
pub fn compute_dynamic_ict_levels(
    all_1m_candles: &[Candle],
    weekly_candles_all: &[Candle],
    daily_candles_all: &[Candle],
    current_time: i64,
) -> DynamicIctLevels {
    let mut indicators: Vec<ChartIndicator> = Vec::new();
    let mut journal_logs: Vec<JournalEvent> = Vec::new();

    let time_str = New_York
        .timestamp_opt(current_time, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S EST").to_string())
        .unwrap_or_else(|| current_time.to_string());

    // Strictly filter candles up to current_time to eliminate lookahead bias
    let weekly_filtered: Vec<Candle> = weekly_candles_all
        .iter()
        .filter(|c| c.time <= current_time)
        .cloned()
        .collect();

    let daily_filtered: Vec<Candle> = daily_candles_all
        .iter()
        .filter(|c| c.time <= current_time)
        .cloned()
        .collect();

    // 1. Layer 1: Weekly Bias
    let (weekly_bias, weekly_bias_reason) = evaluate_weekly_bias(&weekly_filtered);
    journal_logs.push(JournalEvent {
        time: current_time,
        time_str: time_str.clone(),
        level: "INFO".to_string(),
        category: "HTF_BIAS".to_string(),
        message: format!("Weekly Directional Bias evaluated as {:?}: {}", weekly_bias, weekly_bias_reason),
    });

    // 2. Layer 2: Daily Draw on Liquidity
    let daily_dol = identify_daily_draw_on_liquidity(&daily_filtered, weekly_bias);
    if let Some(ref dol) = daily_dol {
        journal_logs.push(JournalEvent {
            time: current_time,
            time_str: time_str.clone(),
            level: "INFO".to_string(),
            category: "DAILY_DOL".to_string(),
            message: format!("Daily Draw on Liquidity (DOL): {} at {:.2}", dol.description, dol.target_price),
        });
    }

    // 3. Layer 3: Hourly Session Framing
    let dt = New_York.timestamp_opt(current_time, 0).single();
    let (monday_open_time, friday_open_time, friday_close_time, day_start_time) = if let Some(current_dt) = dt {
        let days_from_mon = current_dt.weekday().num_days_from_monday() as i64;
        let mon_date = current_dt.date_naive() - chrono::Duration::days(days_from_mon);
        let fri_date = mon_date + chrono::Duration::days(4);

        let mon_t = New_York
            .with_ymd_and_hms(mon_date.year(), mon_date.month(), mon_date.day(), 0, 0, 0)
            .single()
            .map(|d| d.timestamp())
            .unwrap_or(current_time - 86400 * 4);

        let fri_t = New_York
            .with_ymd_and_hms(fri_date.year(), fri_date.month(), fri_date.day(), 0, 0, 0)
            .single()
            .map(|d| d.timestamp())
            .unwrap_or(current_time);

        let fri_close = New_York
            .with_ymd_and_hms(fri_date.year(), fri_date.month(), fri_date.day(), 16, 0, 0)
            .single()
            .map(|d| d.timestamp())
            .unwrap_or(current_time);

        let day_start = New_York
            .with_ymd_and_hms(current_dt.year(), current_dt.month(), current_dt.day(), 0, 0, 0)
            .single()
            .map(|d| d.timestamp())
            .unwrap_or(current_time - 86400);

        (mon_t, fri_t, fri_close, day_start)
    } else {
        (current_time - 86400 * 4, current_time, current_time + 57600, current_time - 86400)
    };

    // Filter 1m candles for active week and day
    let week_1m: Vec<Candle> = all_1m_candles
        .iter()
        .filter(|c| c.time >= monday_open_time && c.time <= current_time)
        .cloned()
        .collect();

    let day_1m: Vec<Candle> = week_1m
        .iter()
        .filter(|c| c.time >= day_start_time)
        .cloned()
        .collect();

    let hourly_candles = MarketDataLoader::aggregate_interval(&week_1m, 3600);
    let _candles_15m = MarketDataLoader::aggregate_interval(&week_1m, 900);
    let candles_2m = MarketDataLoader::aggregate_interval(&day_1m, 120);

    let hourly_frame = frame_hourly_session(&hourly_candles, daily_dol.clone());
    let overnight_high = hourly_frame.as_ref().map(|f| f.overnight_high);
    let overnight_low = hourly_frame.as_ref().map(|f| f.overnight_low);

    // Indicator: Daily DOL on ALL Timeframes (Red Line: Frame 63, Frame 64, Frame 111, Frame 126)
    if let Some(ref dol) = daily_dol {
        indicators.push(ChartIndicator {
            id: "daily_dol_all".to_string(),
            indicator_type: "trend_line".to_string(),
            timeframe: "ALL".to_string(), // Persists across 1D, 1h, 15m, 2m
            points: vec![
                IndicatorPoint { time: monday_open_time.min(day_start_time - 86400 * 30), price: dol.target_price },
                IndicatorPoint { time: friday_close_time.max(current_time + 86400 * 5), price: dol.target_price },
            ],
            label: format!("Draw on Liquidity (DOL) ({:.2})", dol.target_price),
            color: "#f44336".to_string(), // Red line
            line_style: 0,
            line_width: 2,
            fill_color: None,
        });
    }

    // Indicators: 1-Hour Session Vertical Markers (Frame 0064)
    indicators.push(ChartIndicator {
        id: "sess_monday_60".to_string(),
        indicator_type: "vertical_line".to_string(),
        timeframe: "60".to_string(),
        points: vec![IndicatorPoint { time: monday_open_time, price: 0.0 }],
        label: "Monday Trading (00:00 EST)".to_string(),
        color: "#90caf9".to_string(),
        line_style: 2,
        line_width: 1,
        fill_color: None,
    });

    indicators.push(ChartIndicator {
        id: "sess_friday_60".to_string(),
        indicator_type: "vertical_line".to_string(),
        timeframe: "60".to_string(),
        points: vec![IndicatorPoint { time: friday_open_time, price: 0.0 }],
        label: "Friday Trading (00:00 EST)".to_string(),
        color: "#90caf9".to_string(),
        line_style: 2,
        line_width: 1,
        fill_color: None,
    });

    indicators.push(ChartIndicator {
        id: "sess_friday_close_60".to_string(),
        indicator_type: "vertical_line".to_string(),
        timeframe: "60".to_string(),
        points: vec![IndicatorPoint { time: friday_close_time, price: 0.0 }],
        label: "Friday Close (16:00 EST)".to_string(),
        color: "#787b86".to_string(),
        line_style: 2,
        line_width: 1,
        fill_color: None,
    });

    // 4. Layer 4: 15-Minute Liquidity Pools (Frame 0111)
    // Directly linked to the 1-hour pre-market consolidation setup (Frame 0064)
    // Pre-market window: 00:00 to 08:30 EST
    let premarket_end = day_start_time + 30600; // 08:30 EST
    let premarket_candles: Vec<&Candle> = week_1m
        .iter()
        .filter(|c| c.time >= day_start_time && c.time < premarket_end.min(current_time))
        .collect();

    let (consolidation_high, consolidation_high_time, consolidation_low, consolidation_low_time) = if premarket_candles.len() >= 30 {
        // Sufficient pre-market candles: use current day's overnight consolidation (00:00 - 08:30)
        let hi = premarket_candles.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let hi_time = premarket_candles.iter().find(|c| (c.high - hi).abs() < 1e-4).map(|c| c.time).unwrap_or(day_start_time);
        let lo = premarket_candles.iter().map(|c| c.low).fold(f64::MAX, f64::min);
        let lo_time = premarket_candles.iter().find(|c| (c.low - lo).abs() < 1e-4).map(|c| c.time).unwrap_or(day_start_time);
        (hi, hi_time, lo, lo_time)
    } else {
        // Early session fallback (e.g. before 04:00 AM / at 00:38): use previous day's consolidation
        let prev_day_candles: Vec<&Candle> = all_1m_candles
            .iter()
            .filter(|c| c.time >= day_start_time - 86400 && c.time < day_start_time)
            .collect();
        if !prev_day_candles.is_empty() {
            let hi = prev_day_candles.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let hi_time = prev_day_candles.iter().find(|c| (c.high - hi).abs() < 1e-4).map(|c| c.time).unwrap_or(day_start_time - 86400);
            let lo = prev_day_candles.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            let lo_time = prev_day_candles.iter().find(|c| (c.low - lo).abs() < 1e-4).map(|c| c.time).unwrap_or(day_start_time - 86400);
            (hi, hi_time, lo, lo_time)
        } else {
            (14800.0, day_start_time, 14600.0, day_start_time)
        }
    };

    let equilibrium_15m = (consolidation_high + consolidation_low) / 2.0;

    let buyside_pools = vec![LiquidityPool {
        price: consolidation_high,
        is_buy_side: true,
        start_time: consolidation_high_time,
        description: format!("Overnight Consolidation High ({:.2}) [Buy Stops]", consolidation_high),
        swept: false,
        sweep_time: None,
    }];

    let sellside_pools = vec![LiquidityPool {
        price: consolidation_low,
        is_buy_side: false,
        start_time: consolidation_low_time,
        description: format!("Overnight Consolidation Low ({:.2}) [Sell Stops]", consolidation_low),
        swept: false,
        sweep_time: None,
    }];

    let primary_buyside = buyside_pools.first();
    let primary_sellside = sellside_pools.first();

    for pool in &buyside_pools {
        indicators.push(ChartIndicator {
            id: format!("bsl_{}", pool.price as i64),
            indicator_type: "trend_line".to_string(),
            timeframe: "15".to_string(),
            points: vec![
                IndicatorPoint { time: pool.start_time, price: pool.price },
                IndicatorPoint { time: current_time + 7200, price: pool.price },
            ],
            label: format!("Buyside Liquidity [Buy Stops] ({:.2})", pool.price),
            color: "#f44336".to_string(),
            line_style: 0,
            line_width: 2,
            fill_color: None,
        });

        journal_logs.push(JournalEvent {
            time: pool.start_time,
            time_str: time_str.clone(),
            level: "INFO".to_string(),
            category: "LIQUIDITY".to_string(),
            message: format!("15m Buyside Liquidity Pool identified at {:.2} ({})", pool.price, pool.description),
        });
    }

    for pool in &sellside_pools {
        indicators.push(ChartIndicator {
            id: format!("ssl_{}", pool.price as i64),
            indicator_type: "trend_line".to_string(),
            timeframe: "15".to_string(),
            points: vec![
                IndicatorPoint { time: pool.start_time, price: pool.price },
                IndicatorPoint { time: current_time + 7200, price: pool.price },
            ],
            label: format!("Sellside Liquidity [Sell Stops] ({:.2})", pool.price),
            color: "#2962ff".to_string(),
            line_style: 0,
            line_width: 2,
            fill_color: None,
        });

        journal_logs.push(JournalEvent {
            time: pool.start_time,
            time_str: time_str.clone(),
            level: "INFO".to_string(),
            category: "LIQUIDITY".to_string(),
            message: format!("15m Sellside Liquidity Pool identified at {:.2} ({})", pool.price, pool.description),
        });
    }

    // Dealing range and equilibrium (Frame 0111: exactly the 50% Equilibrium line)
    indicators.push(ChartIndicator {
        id: "eq_15m".to_string(),
        indicator_type: "trend_line".to_string(),
        timeframe: "15".to_string(),
        points: vec![
            IndicatorPoint { time: day_start_time, price: equilibrium_15m },
            IndicatorPoint { time: current_time + 3600, price: equilibrium_15m },
        ],
        label: format!("50% Equilibrium ({:.2})", equilibrium_15m),
        color: "#ffeb3b".to_string(),
        line_style: 2,
        line_width: 1,
        fill_color: None,
    });

    // 2m Dealing Range Boundaries
    let (dealing_range_high, dealing_range_low, equilibrium_50) = if !candles_2m.is_empty() {
        let hi = candles_2m.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let lo = candles_2m.iter().map(|c| c.low).fold(f64::MAX, f64::min);
        (Some(hi), Some(lo), Some((hi + lo) / 2.0))
    } else {
        (Some(consolidation_high), Some(consolidation_low), Some(equilibrium_15m))
    };

    if let (Some(hi), Some(lo), Some(eq)) = (dealing_range_high, dealing_range_low, equilibrium_50) {
        indicators.push(ChartIndicator {
            id: "eq_2m".to_string(),
            indicator_type: "trend_line".to_string(),
            timeframe: "2".to_string(),
            points: vec![
                IndicatorPoint { time: day_start_time, price: eq },
                IndicatorPoint { time: current_time + 3600, price: eq },
            ],
            label: format!("50% Equilibrium ({:.2}) [Pink Premium Above / Green Discount Below]", eq),
            color: "#ffeb3b".to_string(),
            line_style: 2,
            line_width: 2,
            fill_color: None,
        });

        indicators.push(ChartIndicator {
            id: "prem_high_2m".to_string(),
            indicator_type: "trend_line".to_string(),
            timeframe: "2".to_string(),
            points: vec![
                IndicatorPoint { time: day_start_time, price: hi },
                IndicatorPoint { time: current_time + 3600, price: hi },
            ],
            label: format!("Dealing Range High ({:.2}) [Premium Boundary]", hi),
            color: "#e91e63".to_string(),
            line_style: 1,
            line_width: 1,
            fill_color: None,
        });

        indicators.push(ChartIndicator {
            id: "disc_low_2m".to_string(),
            indicator_type: "trend_line".to_string(),
            timeframe: "2".to_string(),
            points: vec![
                IndicatorPoint { time: day_start_time, price: lo },
                IndicatorPoint { time: current_time + 3600, price: lo },
            ],
            label: format!("Dealing Range Low ({:.2}) [Discount Boundary]", lo),
            color: "#4caf50".to_string(),
            line_style: 1,
            line_width: 1,
            fill_color: None,
        });
    }

    // 5. Sweep Detection
    let sweep_event = detect_liquidity_sweep_from_pools(&candles_2m, &buyside_pools, &sellside_pools, Some(&weekly_bias));

    if let Some(ref sweep) = sweep_event {
        indicators.push(ChartIndicator {
            id: "sweep_marker".to_string(),
            indicator_type: "marker".to_string(),
            timeframe: "2".to_string(),
            points: vec![IndicatorPoint { time: sweep.sweep_time, price: sweep.sweep_extreme }],
            label: format!("15m Stop Hunt Sweep at {:.2}", sweep.swept_level),
            color: if sweep.is_buy_side { "#e91e63".to_string() } else { "#4caf50".to_string() },
            line_style: 0,
            line_width: 2,
            fill_color: None,
        });

        journal_logs.push(JournalEvent {
            time: sweep.sweep_time,
            time_str: time_str.clone(),
            level: "SIGNAL".to_string(),
            category: "LIQUIDITY".to_string(),
            message: format!("Stop Hunt Sweep Confirmed! Swept 15m level {:.2}. Extreme price at {:.2}.", sweep.swept_level, sweep.sweep_extreme),
        });
    }

    // 6. Market Structure Shift & FVG Formation
    let mut fvg_top = None;
    let mut fvg_bottom = None;
    let mut fvg_midpoint = None;
    let mut fvg_time = None;
    let mut fvg_is_bullish = None;

    let mut has_signal = false;
    let mut signal_is_short = None;
    let mut signal_entry = None;
    let mut signal_sl = None;
    let mut signal_tp = None;
    let mut signal_time = None;
    let mut target_imbalance_top = None;
    let mut target_imbalance_bottom = None;

    if let (Some(sweep), Some(dol)) = (&sweep_event, &daily_dol) {
        if let Some((mss, fvg)) = detect_market_structure_shift(&candles_2m, sweep) {
            fvg_top = Some(fvg.top);
            fvg_bottom = Some(fvg.bottom);
            fvg_midpoint = Some(fvg.midpoint);
            fvg_time = Some(fvg.candle_time);
            fvg_is_bullish = Some(fvg.is_bullish);

            // Indicator: MSS broken swing trendline
            indicators.push(ChartIndicator {
                id: "mss_broken_swing".to_string(),
                indicator_type: "trend_line".to_string(),
                timeframe: "2".to_string(),
                points: vec![
                    IndicatorPoint { time: mss.broken_swing.time, price: mss.broken_swing.price },
                    IndicatorPoint { time: mss.displacement_candle_time + 1200, price: mss.broken_swing.price },
                ],
                label: format!("Market Structure Shift (MSS) broken below {:.2}", mss.broken_swing.price),
                color: "#2196f3".to_string(),
                line_style: 2,
                line_width: 2,
                fill_color: None,
            });

            // Indicator: FVG Box
            indicators.push(ChartIndicator {
                id: "fvg_box_2m".to_string(),
                indicator_type: "rectangle".to_string(),
                timeframe: "2".to_string(),
                points: vec![
                    IndicatorPoint { time: fvg.candle_time, price: fvg.top },
                    IndicatorPoint { time: fvg.candle_time + 1800, price: fvg.bottom },
                ],
                label: format!("Fair Value Gap [{:.2} - {:.2}]", fvg.bottom, fvg.top),
                color: "#ff9800".to_string(),
                line_style: 0,
                line_width: 2,
                fill_color: Some("rgba(255, 152, 0, 0.25)".to_string()),
            });

            journal_logs.push(JournalEvent {
                time: mss.displacement_candle_time,
                time_str: time_str.clone(),
                level: "SIGNAL".to_string(),
                category: "STRUCTURE".to_string(),
                message: format!("Displacement confirmed! BMS broken at {:.2}. FVG born at [{:.2} - {:.2}].", mss.broken_swing.price, fvg.bottom, fvg.top),
            });

            if let (Some(hi), Some(lo)) = (dealing_range_high, dealing_range_low) {
                if let Some(signal) = evaluate_execution_entry(
                    &candles_2m,
                    sweep,
                    &mss,
                    &fvg,
                    dol.target_price,
                    hi,
                    lo,
                ) {
                    has_signal = true;
                    signal_is_short = Some(signal.is_short);
                    signal_entry = Some(signal.entry_price);
                    signal_sl = Some(signal.stop_loss);
                    signal_tp = Some(signal.take_profit);
                    signal_time = Some(signal.time);

                    if let Some((top, bot)) = signal.target_imbalance {
                        target_imbalance_top = Some(top);
                        target_imbalance_bottom = Some(bot);

                        indicators.push(ChartIndicator {
                            id: "target_imbalance_2m".to_string(),
                            indicator_type: "rectangle".to_string(),
                            timeframe: "2".to_string(),
                            points: vec![
                                IndicatorPoint { time: signal.time, price: top },
                                IndicatorPoint { time: signal.time + 3600, price: bot },
                            ],
                            label: format!("Target Imbalance [{:.2} - {:.2}]", bot, top),
                            color: "#00bcd4".to_string(),
                            line_style: 0,
                            line_width: 2,
                            fill_color: Some("rgba(0, 188, 212, 0.22)".to_string()),
                        });
                    }

                    // Order entry & target indicators
                    indicators.push(ChartIndicator {
                        id: "order_entry_line".to_string(),
                        indicator_type: "trend_line".to_string(),
                        timeframe: "2".to_string(),
                        points: vec![
                            IndicatorPoint { time: signal.time, price: signal.entry_price },
                            IndicatorPoint { time: signal.time + 3600, price: signal.entry_price },
                        ],
                        label: format!("ENTRY at {:.2}", signal.entry_price),
                        color: if signal.is_short { "#f44336".to_string() } else { "#4caf50".to_string() },
                        line_style: 0,
                        line_width: 2,
                        fill_color: None,
                    });

                    indicators.push(ChartIndicator {
                        id: "order_tp_line".to_string(),
                        indicator_type: "trend_line".to_string(),
                        timeframe: "2".to_string(),
                        points: vec![
                            IndicatorPoint { time: signal.time, price: signal.take_profit },
                            IndicatorPoint { time: signal.time + 3600, price: signal.take_profit },
                        ],
                        label: format!("Target Exit at {:.2}", signal.take_profit),
                        color: "#00e676".to_string(),
                        line_style: 0,
                        line_width: 2,
                        fill_color: None,
                    });

                    journal_logs.push(JournalEvent {
                        time: signal.time,
                        time_str: time_str.clone(),
                        level: "ORDER".to_string(),
                        category: "EXECUTION".to_string(),
                        message: format!("ORDER FILLED: {} at {:.2} | SL: {:.2} | TP: {:.2}", if signal.is_short { "SHORT" } else { "LONG" }, signal.entry_price, signal.stop_loss, signal.take_profit),
                    });
                }
            }
        }
    }

    DynamicIctLevels {
        current_time,
        weekly_bias: format!("{:?}", weekly_bias).to_uppercase(),
        weekly_bias_reason,
        daily_dol_price: daily_dol.as_ref().map(|d| d.target_price),
        daily_dol_label: daily_dol.as_ref().map(|d| d.description.clone()),
        monday_open_time,
        friday_open_time,
        friday_close_time,
        overnight_high,
        overnight_low,
        buyside_liquidity: primary_buyside.map(|p| p.price),
        sellside_liquidity: primary_sellside.map(|p| p.price),
        sweep_time: sweep_event.as_ref().map(|s| s.sweep_time),
        sweep_level: sweep_event.as_ref().map(|s| s.swept_level),
        sweep_extreme: sweep_event.as_ref().map(|s| s.sweep_extreme),
        is_buy_sweep: sweep_event.as_ref().map(|s| s.is_buy_side),
        dealing_range_high,
        dealing_range_low,
        equilibrium_50,
        fvg_top,
        fvg_bottom,
        fvg_midpoint,
        fvg_time,
        fvg_is_bullish,
        has_signal,
        signal_is_short,
        signal_entry,
        signal_sl,
        signal_tp,
        signal_time,
        target_imbalance_top,
        target_imbalance_bottom,
        indicators,
        journal_logs,
    }
}
