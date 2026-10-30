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
    // ---- CME Globex session helpers -------------------------------------------------------
    // A session "day" runs 18:00 New York on the PREVIOUS calendar day to 17:00 NY, and the
    // session week runs Sunday 18:00 to Friday 17:00 NY. Adding 6 hours of NY wall-clock time
    // maps every bar onto its session date (Sunday 18:00 -> Monday), and stays correct across
    // DST because 18:00 NY is always the boundary.

    fn session_date(ts: i64) -> chrono::NaiveDate {
        let ny = Utc.timestamp_opt(ts, 0).single().unwrap().with_timezone(&New_York);
        (ny.naive_local() + chrono::Duration::hours(6)).date()
    }

    fn ny_time(date: chrono::NaiveDate, hour: u32) -> Option<i64> {
        New_York
            .with_ymd_and_hms(date.year(), date.month(), date.day(), hour, 0, 0)
            .single()
            .map(|dt| dt.timestamp())
    }

    /// Close time (Friday 17:00 NY) of the session week that opened at `week_start`.
    pub fn week_close_time(week_start: i64) -> i64 {
        let sunday = Utc.timestamp_opt(week_start, 0).single().unwrap().with_timezone(&New_York).date_naive();
        Self::ny_time(sunday + chrono::Duration::days(5), 17).unwrap_or(i64::MAX)
    }

    /// The timestamp the chart expects for a session candle: NY midnight of its session date
    /// (Monday 00:00 for the week that opens Sunday 18:00). Engine-side candles are stamped at
    /// the real session open so as-of filtering sees them from the moment the session starts.
    pub fn session_chart_time(session_start: i64) -> i64 {
        Self::ny_time(Self::session_date(session_start), 0).unwrap_or(session_start)
    }

    fn fold_groups(grouped: BTreeMap<i64, Vec<&Candle>>) -> Vec<Candle> {
        let mut res = Vec::with_capacity(grouped.len());
        for (time, group) in grouped {
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
            res.push(Candle { time, open, high, low, close, volume });
        }
        res
    }

    /// Daily candles on CME session boundaries (18:00 NY -> 17:00 NY), stamped at the session
    /// open. Sunday-evening bars belong to Monday's session, not to a separate Sunday candle.
    pub fn aggregate_daily(candles_1m: &[Candle]) -> Vec<Candle> {
        let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
        for c in candles_1m {
            let date = Self::session_date(c.time);
            let start = Self::ny_time(date - chrono::Duration::days(1), 18).unwrap_or(c.time);
            grouped.entry(start).or_default().push(c);
        }
        Self::fold_groups(grouped)
    }

    /// Weekly candles on CME session boundaries (Sunday 18:00 NY -> Friday 17:00 NY), stamped at
    /// the Sunday open. A week's close is its own Friday close, never the next Sunday evening.
    pub fn aggregate_weekly(candles_1m: &[Candle]) -> Vec<Candle> {
        let mut grouped: BTreeMap<i64, Vec<&Candle>> = BTreeMap::new();
        for c in candles_1m {
            let date = Self::session_date(c.time);
            let monday = date - chrono::Duration::days(date.weekday().num_days_from_monday() as i64);
            let start = Self::ny_time(monday - chrono::Duration::days(1), 18).unwrap_or(c.time);
            grouped.entry(start).or_default().push(c);
        }
        Self::fold_groups(grouped)
    }

    /// Session candles (daily or weekly) as they looked at `as_of`. Candles that have not opened
    /// yet are dropped, and the candle in progress is rebuilt from ONLY the 1m bars that exist at
    /// `as_of`, so no future high, low or close leaks in. Completed candles are unchanged.
    /// Both slices must be sorted by time (they are, by construction).
    pub fn session_candles_as_of(session_candles: &[Candle], candles_1m: &[Candle], as_of: i64) -> Vec<Candle> {
        let visible = session_candles.partition_point(|c| c.time <= as_of);
        let mut out: Vec<Candle> = session_candles[..visible].to_vec();
        if let Some(cur) = out.last_mut() {
            let next_open = session_candles.get(visible).map(|c| c.time).unwrap_or(i64::MAX);
            let lo = candles_1m.partition_point(|b| b.time < cur.time);
            let hi = candles_1m
                .partition_point(|b| b.time <= as_of)
                .min(candles_1m.partition_point(|b| b.time < next_open))
                .max(lo);
            let bars = &candles_1m[lo..hi];
            if let (Some(first), Some(last)) = (bars.first(), bars.last()) {
                cur.open = first.open;
                cur.close = last.close;
                cur.high = bars.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
                cur.low = bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
                cur.volume = bars.iter().map(|b| b.volume).sum();
            }
        }
        out
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
