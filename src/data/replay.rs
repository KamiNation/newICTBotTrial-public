use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::info;

use crate::strategy::bias::{identify_daily_draw_on_liquidity, weekly_bias_as_of};
use crate::types::{Candle, DirectionalBias};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarUpdate {
    pub resolution: String,
    pub time: i64, // Unix timestamp in seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub is_closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayStatus {
    pub is_playing: bool,
    pub speed: f64,
    pub start_point: String,
    pub current_time: i64,
    pub current_time_str: String,
    pub current_price: f64,
    pub progress_pct: f64,
    pub total_live_bars: usize,
    pub current_bar_index: usize,
    pub weekly_bias: String,
    pub weekly_bias_reason: String,
    pub daily_dol: String,
    pub daily_dol_price: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", content = "payload")]
pub enum ReplayCommand {
    Play,
    Pause,
    SetSpeed(f64),
    Step,
    Reset,
    SetStartPoint(String), // "episode2" or "point_x"
}

pub struct ReplayEngine {
    pub all_1m_candles: Vec<Candle>,
    pub weekly_candles_all: Vec<Candle>,
    pub daily_candles_all: Vec<Candle>,
    pub live_start_index: usize,
    pub current_index: usize,
    pub is_playing: bool,
    pub speed: f64,
    pub start_point: String,
    pub update_tx: broadcast::Sender<BarUpdate>,
    pub current_2m_bar: Option<Candle>,
}

impl ReplayEngine {
    pub fn new(
        all_1m_candles: Vec<Candle>,
        weekly_candles_all: Vec<Candle>,
        daily_candles_all: Vec<Candle>,
        initial_start_time_unix: i64,
    ) -> (Arc<RwLock<Self>>, broadcast::Receiver<BarUpdate>) {
        let (update_tx, update_rx) = broadcast::channel(10000);

        let live_start_index = all_1m_candles
            .iter()
            .position(|c| c.time >= initial_start_time_unix)
            .unwrap_or(0);

        let engine = Self {
            all_1m_candles,
            weekly_candles_all,
            daily_candles_all,
            live_start_index,
            current_index: live_start_index,
            is_playing: false,
            speed: 5.0,
            start_point: "episode2".to_string(),
            update_tx,
            current_2m_bar: None,
        };

        (Arc::new(RwLock::new(engine)), update_rx)
    }

    pub fn set_start_point(&mut self, point: &str) {
        self.start_point = point.to_string();
        self.is_playing = false;
        self.current_2m_bar = None;

        let target_time = match point {
            "point_x" => {
                // October 1, 2021 (the start of the dataset marked X)
                self.all_1m_candles.first().map(|c| c.time).unwrap_or(1633046400)
            }
            _ => {
                // Episode 2 morning: Friday, Jan 21, 2022 at 09:29:00 EST (14:29 UTC)
                1642775340
            }
        };

        self.live_start_index = self
            .all_1m_candles
            .iter()
            .position(|c| c.time >= target_time)
            .unwrap_or(0);

        self.current_index = self.live_start_index;
        info!("Switched replay start point to '{}' at timestamp {}", point, target_time);
    }

    /// Advances replay by one step. Updates forming 2m and 15m bars and broadcasts updates.
    pub fn step_forward(&mut self) -> Option<(Candle, Option<BarUpdate>)> {
        if self.current_index >= self.all_1m_candles.len() {
            self.is_playing = false;
            return None;
        }

        let c1m = self.all_1m_candles[self.current_index].clone();
        self.current_index += 1;

        // Form 2-minute bar (align to 120-second boundary)
        let bar_2m_time = (c1m.time / 120) * 120;
        let is_second_minute = (c1m.time % 120) >= 60;

        let (bar_2m, is_closed) = match self.current_2m_bar.take() {
            Some(mut active) if active.time == bar_2m_time => {
                active.high = active.high.max(c1m.high);
                active.low = active.low.min(c1m.low);
                active.close = c1m.close;
                active.volume += c1m.volume;
                let closed = is_second_minute;
                let clone = active.clone();
                if !closed {
                    self.current_2m_bar = Some(active);
                }
                (clone, closed)
            }
            _ => {
                let new_bar = Candle {
                    time: bar_2m_time,
                    open: c1m.open,
                    high: c1m.high,
                    low: c1m.low,
                    close: c1m.close,
                    volume: c1m.volume,
                };
                let closed = false;
                self.current_2m_bar = Some(new_bar.clone());
                (new_bar, closed)
            }
        };

        let update_2m = BarUpdate {
            resolution: "2".to_string(),
            time: bar_2m.time,
            open: bar_2m.open,
            high: bar_2m.high,
            low: bar_2m.low,
            close: bar_2m.close,
            volume: bar_2m.volume,
            is_closed,
        };

        let _ = self.update_tx.send(update_2m.clone());

        Some((c1m, Some(update_2m)))
    }

    pub fn get_status(&self) -> ReplayStatus {
        let current_bar = if self.current_index > 0 && self.current_index <= self.all_1m_candles.len() {
            &self.all_1m_candles[self.current_index - 1]
        } else if !self.all_1m_candles.is_empty() {
            &self.all_1m_candles[0]
        } else {
            return ReplayStatus {
                is_playing: false,
                speed: 1.0,
                start_point: self.start_point.clone(),
                current_time: 0,
                current_time_str: "N/A".to_string(),
                current_price: 0.0,
                progress_pct: 0.0,
                total_live_bars: 0,
                current_bar_index: 0,
                weekly_bias: "Neutral".to_string(),
                weekly_bias_reason: "No data".to_string(),
                daily_dol: "None".to_string(),
                daily_dol_price: 0.0,
            };
        };

        let current_time = current_bar.time;
        let total_live = self.all_1m_candles.len().saturating_sub(self.live_start_index);
        let current_offset = self.current_index.saturating_sub(self.live_start_index);
        let progress_pct = if total_live > 0 {
            (current_offset as f64 / total_live as f64) * 100.0
        } else {
            0.0
        };

        // Format in New York time
        let ny_time_str = chrono_tz::America::New_York
            .timestamp_opt(current_bar.time, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S EST").to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        // Dynamically evaluate Weekly Bias and Daily DOL up to current_time (Zero hardcoding)
        let (bias, reason) = weekly_bias_as_of(&self.weekly_candles_all, current_time);

        let daily_up_to_now: Vec<Candle> = crate::data::MarketDataLoader::session_candles_as_of(
            &self.daily_candles_all,
            &self.all_1m_candles,
            current_time,
        );

        let dol_opt = identify_daily_draw_on_liquidity(&daily_up_to_now, bias);
        let (daily_dol_desc, daily_dol_price) = match dol_opt {
            Some(dol) => (dol.description, dol.target_price),
            None => ("No Active DOL Target".to_string(), 0.0),
        };

        let bias_str = match bias {
            DirectionalBias::Bullish => "BULLISH",
            DirectionalBias::Bearish => "BEARISH",
            DirectionalBias::Neutral => "NEUTRAL",
        };

        ReplayStatus {
            is_playing: self.is_playing,
            speed: self.speed,
            start_point: self.start_point.clone(),
            current_time: current_bar.time,
            current_time_str: ny_time_str,
            current_price: current_bar.close,
            progress_pct,
            total_live_bars: total_live,
            current_bar_index: current_offset,
            weekly_bias: bias_str.to_string(),
            weekly_bias_reason: reason,
            daily_dol: daily_dol_desc,
            daily_dol_price,
        }
    }

    pub fn reset(&mut self) {
        self.current_index = self.live_start_index;
        self.is_playing = false;
        self.current_2m_bar = None;
        info!("Replay reset to start index {}", self.live_start_index);
    }
}
