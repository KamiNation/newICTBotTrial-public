use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    Weekly,
    Daily,
    Hourly,
    Min15,
    Min3,
    Min2,
    Min1,
}

impl Timeframe {
    pub fn to_seconds(&self) -> i64 {
        match self {
            Timeframe::Weekly => 7 * 24 * 3600,
            Timeframe::Daily => 24 * 3600,
            Timeframe::Hourly => 3600,
            Timeframe::Min15 => 15 * 60,
            Timeframe::Min3 => 3 * 60,
            Timeframe::Min2 => 2 * 60,
            Timeframe::Min1 => 60,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Timeframe::Weekly => "1W",
            Timeframe::Daily => "1D",
            Timeframe::Hourly => "60",
            Timeframe::Min15 => "15",
            Timeframe::Min3 => "3",
            Timeframe::Min2 => "2",
            Timeframe::Min1 => "1",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub time: i64,      // Unix timestamp in seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Candle {
    pub fn is_bullish(&self) -> bool {
        self.close > self.open
    }

    pub fn is_bearish(&self) -> bool {
        self.close < self.open
    }

    pub fn body_size(&self) -> f64 {
        (self.close - self.open).abs()
    }

    pub fn total_range(&self) -> f64 {
        self.high - self.low
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectionalBias {
    Bullish,
    Bearish,
    Neutral,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DolType {
    BuySideLiquidity(f64),  // Old Swing High (Stops)
    SellSideLiquidity(f64), // Old Swing Low (Stops)
    ImbalanceFvg(f64, f64), // Fair Value Gap (top, bottom)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawOnLiquidity {
    pub dol_type: DolType,
    pub target_price: f64,
    pub timeframe: Timeframe,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwingPoint {
    pub index: usize,
    pub time: i64,
    pub price: f64,
    pub is_high: bool,
    pub swept: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelativeEqualPoints {
    pub points: Vec<SwingPoint>,
    pub price_level: f64,
    pub is_high: bool,
    pub swept: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairValueGap {
    pub id: usize,
    pub timeframe: Timeframe,
    pub is_bullish: bool,
    pub top: f64,
    pub bottom: f64,
    pub midpoint: f64, // Consequent Encroachment (50%)
    pub candle_time: i64,
    pub mitigated: bool,
    pub mitigated_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketStructureShift {
    pub time: i64,
    pub broken_swing: SwingPoint,
    pub displacement_candle_time: i64,
    pub is_bullish: bool,
    pub fvg: Option<FairValueGap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeState {
    AwaitingWeeklyDailyAlignment,
    FramingHourlyRange {
        weekly_open_time: i64,
        overnight_high: f64,
        overnight_low: f64,
        inducement_completed: bool,
    },
    M15LiquiditySwept {
        sweep_time: i64,
        swept_price: f64,
        is_buy_stops_swept: bool,
        sweep_extreme: f64,
    },
    M2DisplacementMss {
        mss: MarketStructureShift,
        fvg: FairValueGap,
        in_premium: bool,
    },
    OrderPlaced {
        entry_price: f64,
        stop_loss: f64,
        take_profit: f64,
        fvg_id: usize,
    },
    InPosition {
        entry_time: i64,
        fill_price: f64,
        stop_loss: f64,
        take_profit: f64,
        is_short: bool,
    },
    Closed {
        entry_time: i64,
        exit_time: i64,
        entry_price: f64,
        exit_price: f64,
        pnl_handles: f64,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeMark {
    pub id: usize,
    pub time: i64,
    pub color: String,
    pub text: String,
    pub label: String,
    pub min_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndicatorPoint {
    pub time: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartIndicator {
    pub id: String,
    pub indicator_type: String, // "trend_line" | "ray" | "rectangle" | "vertical_line" | "marker"
    pub timeframe: String,      // "1W" | "1D" | "60" | "15" | "2" | "1" | "ALL"
    pub points: Vec<IndicatorPoint>,
    pub label: String,
    pub color: String,
    pub line_style: u32,        // 0: solid, 1: dotted, 2: dashed
    pub line_width: u32,
    pub fill_color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub time: i64,
    pub time_str: String,
    pub level: String,      // "INFO" | "WARN" | "SIGNAL" | "ORDER"
    pub category: String,   // "HTF_BIAS" | "DAILY_DOL" | "SESSION" | "LIQUIDITY" | "STRUCTURE" | "EXECUTION"
    pub message: String,
}

/// Computes Average True Range (ATR) across the last `period` candles.
/// Asset-agnostic normalizer: scales dynamically for NQ, VIX75, FX, or Crypto.
pub fn calculate_atr(candles: &[Candle], period: usize) -> f64 {
    if candles.len() < 2 {
        return 1.0;
    }
    let p = period.min(candles.len() - 1);
    let mut tr_sum = 0.0;
    for i in (candles.len() - p)..candles.len() {
        let prev = &candles[i - 1];
        let curr = &candles[i];
        let tr = (curr.high - curr.low)
            .max((curr.high - prev.close).abs())
            .max((curr.low - prev.close).abs());
        tr_sum += tr;
    }
    (tr_sum / p as f64).max(0.0001)
}
