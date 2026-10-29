use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tracing::info;

use crate::types::Candle;

/// Ingests 1-minute CME Globex NQ historical data from the CSV file.
/// Parses timestamps, validates prices, and aggregates into multi-timeframe candles.
pub struct MarketDataLoader {
    pub candles_1m: Vec<Candle>,
}

impl MarketDataLoader {
    /// Loads candles from the given CSV path (defaults to data/NQ_1m_Oct21_Mar22.csv).
    pub fn load_from_csv<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let file = File::open(&path).map_err(|e| format!("Failed to open CSV at {:?}: {}", path.as_ref(), e))?;
        let reader = BufReader::new(file);

        let mut candles_1m = Vec::new();
        let mut lines = reader.lines();

        // Read header
        if let Some(header) = lines.next() {
            let _h = header.map_err(|e| format!("Failed to read header: {}", e))?;
        }

        for (line_idx, line_res) in lines.enumerate() {
            let line = line_res.map_err(|e| format!("Error at line {}: {}", line_idx + 2, e))?;
            if line.trim().is_empty() {
                continue;
            }

            // Split by tab or comma
            let parts: Vec<&str> = if line.contains('\t') {
                line.split('\t').collect()
            } else {
                line.split(',').collect()
            };

            if parts.len() < 5 {
                continue;
            }

            // Timestamp format: "2022.01.21 15:22:00" or ISO format
            let dt_str = parts[0].trim();
            let naive_dt = NaiveDateTime::parse_from_str(dt_str, "%Y.%m.%d %H:%M:%S")
                .or_else(|_| NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%d %H:%M:%S"))
                .map_err(|e| format!("Failed to parse date '{}' at line {}: {}", dt_str, line_idx + 2, e))?;

            let utc_dt: DateTime<Utc> = Utc.from_utc_datetime(&naive_dt);
            let time = utc_dt.timestamp();

            let open: f64 = parts[1].trim().parse().map_err(|e| format!("Invalid open: {}", e))?;
            let high: f64 = parts[2].trim().parse().map_err(|e| format!("Invalid high: {}", e))?;
            let low: f64 = parts[3].trim().parse().map_err(|e| format!("Invalid low: {}", e))?;
            let close: f64 = parts[4].trim().parse().map_err(|e| format!("Invalid close: {}", e))?;
            let volume: f64 = if parts.len() > 5 {
                parts[5].trim().parse().unwrap_or(0.0)
            } else {
                0.0
            };

            candles_1m.push(Candle {
                time,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        // Sort chronologically ascending
        candles_1m.sort_by_key(|c| c.time);
        info!("Loaded {} 1-minute historical candles from {:?}", candles_1m.len(), path.as_ref());

        Ok(Self { candles_1m })
    }

    /// Aggregates 1m candles into higher timeframe bars based on fixed interval in seconds (e.g. 120s for 2m, 900s for 15m, 3600s for 1h).
    pub fn aggregate_interval(candles_1m: &[Candle], interval_seconds: i64) -> Vec<Candle> {
        let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
        for c in candles_1m {
            let bucket = (c.time / interval_seconds) * interval_seconds;
            grouped.entry(bucket).or_default().push(c);
        }

        let mut res = Vec::with_capacity(grouped.len());
        for (bucket_time, group) in grouped {
            if group.is_empty() {
                continue;
            }
            let open = group.first().unwrap().open;
            let close = group.last().unwrap().close;
            let mut high = f64::NEG_INFINITY;
            let mut low = f64::INFINITY;
            let mut volume = 0.0;

            for bar in &group {
                if bar.high > high {
                    high = bar.high;
                }
                if bar.low < low {
                    low = bar.low;
                }
                volume += bar.volume;
            }

            res.push(Candle {
                time: bucket_time,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        res
    }

    /// Aggregates daily candles aligned to New York calendar days.
    pub fn aggregate_daily(candles_1m: &[Candle]) -> Vec<Candle> {
        let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
        for c in candles_1m {
            let ny_dt = Utc.timestamp_opt(c.time, 0).single().unwrap().with_timezone(&New_York);
            let day_start = New_York
                .with_ymd_and_hms(ny_dt.year(), ny_dt.month(), ny_dt.day(), 0, 0, 0)
                .single()
                .map(|dt| dt.timestamp())
                .unwrap_or(c.time);
            grouped.entry(day_start).or_default().push(c);
        }

        let mut res = Vec::with_capacity(grouped.len());
        for (day_time, group) in grouped {
            if group.is_empty() {
                continue;
            }
            let open = group.first().unwrap().open;
            let close = group.last().unwrap().close;
            let mut high = f64::NEG_INFINITY;
            let mut low = f64::INFINITY;
            let mut volume = 0.0;

            for bar in &group {
                if bar.high > high {
                    high = bar.high;
                }
                if bar.low < low {
                    low = bar.low;
                }
                volume += bar.volume;
            }

            res.push(Candle {
                time: day_time,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        res
    }

    /// Aggregates weekly candles aligned to New York weeks (starting Monday 00:00 NY).
    pub fn aggregate_weekly(candles_1m: &[Candle]) -> Vec<Candle> {
        let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
        for c in candles_1m {
            let ny_dt = Utc.timestamp_opt(c.time, 0).single().unwrap().with_timezone(&New_York);
            let days_from_monday = ny_dt.weekday().num_days_from_monday() as i64;
            let week_monday = ny_dt.date_naive() - chrono::Duration::days(days_from_monday);
            let week_start = New_York
                .with_ymd_and_hms(week_monday.year(), week_monday.month(), week_monday.day(), 0, 0, 0)
                .single()
                .map(|dt| dt.timestamp())
                .unwrap_or(c.time);
            grouped.entry(week_start).or_default().push(c);
        }

        let mut res = Vec::with_capacity(grouped.len());
        for (week_time, group) in grouped {
            if group.is_empty() {
                continue;
            }
            let open = group.first().unwrap().open;
            let close = group.last().unwrap().close;
            let mut high = f64::NEG_INFINITY;
            let mut low = f64::INFINITY;
            let mut volume = 0.0;

            for bar in &group {
                if bar.high > high {
                    high = bar.high;
                }
                if bar.low < low {
                    low = bar.low;
                }
                volume += bar.volume;
            }

            res.push(Candle {
                time: week_time,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        res
    }

    /// Extracts candles for the Episode 2 session (Friday, January 21, 2022).
    /// Pre-session context (up to cutoff_time) and live replay stream (from cutoff_time onwards).
    pub fn get_session_split(
        &self,
        cutoff_time_unix: i64,
    ) -> (Vec<Candle>, Vec<Candle>) {
        let mut history = Vec::new();
        let mut live = Vec::new();

        for c in &self.candles_1m {
            if c.time <= cutoff_time_unix {
                history.push(c.clone());
            } else {
                live.push(c.clone());
            }
        }

        (history, live)
    }
}
