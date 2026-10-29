//! Regression tests pinned to the ICT 2022 Mentorship Episode 2 trade: Friday 21 Jan 2022, NQ.
//!
//! Ground truth (all from data/NQ_1m_Oct21_Mar22.csv, New York time):
//!   - overnight range (prior 18:00 -> 08:30): high 14,816.5 (05:21), low 14,602.2 (Thu 22:35)
//!   - sell side raided ~10:00-10:15 (low 14,532.2), then buy side swept: first 2m candle through
//!     the pool is 11:20, leg extreme 14,858.5 (11:22)
//!   - 2m MSS: 11:30 displacement closes below the 11:26 short-term low; FVG 14,811.5-14,820.8
//!   - fill 11:34 at the FVG's lower edge; target = top of the untouched 2m bullish imbalance
//!     formed at 10:32 (14,655.5-14,677.0), just below the 50% level 14,695.35
//!   - episode's stated fills: short ~14,798, cover ~14,675 (Thinkorswim feed, so not identical)
//!
//! Needs the NQ CSV (data/ or repo root) and fails loudly if it is missing.

use std::sync::OnceLock;

use chrono::TimeZone;
use chrono_tz::America::New_York;

use super::execution::{detect_market_structure_shift, evaluate_execution_entry};
use super::liquidity::{detect_liquidity_sweep_from_pools, overnight_range_pools, LiquidityPool};
use super::{compute_dynamic_ict_levels, DynamicIctLevels};
use crate::data::MarketDataLoader;
use crate::types::{Candle, DirectionalBias};

struct Fixture {
    all: Vec<Candle>,
    weekly: Vec<Candle>,
    daily: Vec<Candle>,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let path = ["data/NQ_1m_Oct21_Mar22.csv", "NQ_1m_Oct21_Mar22.csv"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("regression tests need NQ_1m_Oct21_Mar22.csv in data/ or the repo root");
        let all = MarketDataLoader::load_from_csv(path).expect("CSV load").candles_1m;
        let weekly = MarketDataLoader::aggregate_weekly(&all);
        let daily = MarketDataLoader::aggregate_daily(&all);
        Fixture { all, weekly, daily }
    })
}

/// Epoch seconds for 2022-01-21 hh:mm New York time.
fn ny(h: u32, m: u32) -> i64 {
    New_York.with_ymd_and_hms(2022, 1, 21, h, m, 0).single().unwrap().timestamp()
}

fn levels_at(h: u32, m: u32) -> DynamicIctLevels {
    let f = fixture();
    compute_dynamic_ict_levels(&f.all, &f.weekly, &f.daily, ny(h, m))
}

fn near(a: f64, b: f64, tol: f64, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: got {a}, expected {b} +/- {tol}");
}

#[test]
fn jan21_pools_come_from_prior_1800_overnight_range() {
    let l = levels_at(9, 29);
    near(l.buyside_liquidity.expect("buy-side pool"), 14816.5, 0.05, "buy-side pool");
    // The Thursday 22:35 low, not the Friday-only 00:00-08:30 low (14,638.8) the old code used.
    near(l.sellside_liquidity.expect("sell-side pool"), 14602.2, 0.05, "sell-side pool");
}

#[test]
fn jan21_no_premature_sweep_or_signal() {
    // The old engine "swept" at 09:34 on a 2-point poke through 14,816.5 with no sell-side raid.
    for (h, m) in [(9, 40), (9, 55), (10, 20), (10, 50), (11, 10)] {
        let l = levels_at(h, m);
        assert!(l.sweep_time.is_none(), "unexpected sweep at {h}:{m:02} -> {:?}", l.sweep_time);
        assert!(!l.has_signal, "unexpected signal at {h}:{m:02}");
    }
}

#[test]
fn jan21_sweep_fvg_and_short_signal_match_the_episode() {
    let l = levels_at(11, 40);

    // Sweep: buy side, first 2m candle through the pool is 11:20; marker/extreme is the leg high.
    assert_eq!(l.is_buy_sweep, Some(true));
    assert_eq!(l.sweep_time, Some(ny(11, 20)));
    near(l.sweep_level.unwrap(), 14816.5, 0.05, "swept level");
    near(l.sweep_extreme.unwrap(), 14858.5, 0.05, "sweep leg extreme");

    // 2m bearish FVG centred on the 11:30 displacement candle.
    assert_eq!(l.fvg_time, Some(ny(11, 30)));
    assert_eq!(l.fvg_is_bullish, Some(false));
    near(l.fvg_bottom.unwrap(), 14811.5, 0.05, "FVG bottom");
    near(l.fvg_top.unwrap(), 14820.8, 0.05, "FVG top");

    // Signal: short at the FVG's lower edge, filled by the 11:34 candle.
    assert!(l.has_signal);
    assert_eq!(l.signal_is_short, Some(true));
    assert_eq!(l.signal_time, Some(ny(11, 34)));
    let (entry, sl, tp) = (l.signal_entry.unwrap(), l.signal_sl.unwrap(), l.signal_tp.unwrap());
    near(entry, 14811.5, 0.05, "entry (FVG lower edge)");
    near(tp, 14677.0, 0.05, "target (top of untouched 10:32 bullish imbalance)");
    assert!(sl > 14858.5 && sl < 14880.0, "stop must sit just above the 14,858.5 leg high, got {sl}");

    // Target is below the day's 50% level as of FVG formation: (14,858.5 + 14,532.2) / 2.
    assert!(tp < 14695.35, "target must be below the 50% level");

    // Against the episode's stated fills (different data feed than Thinkorswim):
    near(tp, 14675.0, 5.0, "cover vs episode's ~14,675");
    // Entry is 13.5 pts above his stated 14,798 short: the 2m FVG edge, not his exact fill.
    near(entry, 14798.0, 15.0, "entry vs episode's ~14,798");
    assert!(entry - tp > 100.0, "episode trade is 100+ handles, got {}", entry - tp);
}

#[test]
fn jan21_no_lookahead_signal_never_changes_once_formed() {
    let f = fixture();
    let (mut first_signal_clock, mut seen): (Option<i64>, Option<(f64, f64, f64, i64)>) = (None, None);
    let mut seen_sweep: Option<(i64, f64)> = None;
    let mut seen_fvg: Option<(i64, f64, f64)> = None;

    let mut t = ny(9, 29);
    while t <= ny(13, 0) {
        let l = compute_dynamic_ict_levels(&f.all, &f.weekly, &f.daily, t);

        if let (Some(st), Some(lv)) = (l.sweep_time, l.sweep_level) {
            assert!(st <= t, "sweep stamped in the future at clock {t}");
            let s = (st, lv);
            assert_eq!(*seen_sweep.get_or_insert(s), s, "sweep changed as bars arrived (clock {t})");
        }
        if let (Some(ft), Some(top), Some(bot)) = (l.fvg_time, l.fvg_top, l.fvg_bottom) {
            assert!(ft <= t, "FVG stamped in the future at clock {t}");
            let s = (ft, top, bot);
            assert_eq!(*seen_fvg.get_or_insert(s), s, "FVG changed as bars arrived (clock {t})");
        }
        if l.has_signal {
            let st = l.signal_time.unwrap();
            assert!(st <= t, "signal stamped {st} is after the clock {t} (back-dated / lookahead)");
            let sig = (l.signal_entry.unwrap(), l.signal_sl.unwrap(), l.signal_tp.unwrap(), st);
            first_signal_clock.get_or_insert(t);
            assert_eq!(*seen.get_or_insert(sig), sig, "signal changed after it first appeared (clock {t})");
        } else {
            assert!(seen.is_none(), "signal disappeared at clock {t}");
        }
        t += 60;
    }

    // The 11:34 candle completes when the 11:35 bar prints: never earlier.
    assert_eq!(first_signal_clock, Some(ny(11, 35)), "signal should first appear when 11:34 completes");
}

/// The bullish path is the exact mirror of the bearish one: flip every price around a constant on
/// the same day and the engine must produce the mirrored sweep, MSS, FVG and long signal.
#[test]
fn bullish_path_mirrors_the_bearish_path() {
    const C: f64 = 30_000.0;
    let f = fixture();
    let day_start = ny(0, 0);
    let as_of = ny(13, 0);

    let day_1m: Vec<Candle> = f.all.iter().filter(|c| c.time >= day_start && c.time <= as_of).cloned().collect();
    let c2 = MarketDataLoader::aggregate_interval(&day_1m, 120);
    let (hi_pool, lo_pool) = overnight_range_pools(&f.all, day_start, as_of).expect("overnight pools");

    // Bearish run on the real data.
    let sw_b = detect_liquidity_sweep_from_pools(
        &c2, &[hi_pool.clone()], &[lo_pool.clone()], Some(&DirectionalBias::Bearish),
    ).expect("bearish sweep");
    let (mss_b, fvg_b) = detect_market_structure_shift(&c2, &sw_b).expect("bearish MSS");
    let sig_b = evaluate_execution_entry(&c2, &sw_b, &mss_b, &fvg_b, 14367.8).expect("short signal");

    // Mirrored data and pools.
    let m2: Vec<Candle> = c2.iter().map(|c| Candle {
        time: c.time, open: C - c.open, high: C - c.low, low: C - c.high, close: C - c.close, volume: c.volume,
    }).collect();
    let mirror = |p: &LiquidityPool, buy: bool| LiquidityPool {
        price: C - p.price, is_buy_side: buy, start_time: p.start_time,
        description: p.description.clone(), swept: false, sweep_time: None,
    };
    let (m_buy, m_sell) = (mirror(&lo_pool, true), mirror(&hi_pool, false));
    let sw_m = detect_liquidity_sweep_from_pools(
        &m2, &[m_buy], &[m_sell], Some(&DirectionalBias::Bullish),
    ).expect("bullish sweep");
    let (mss_m, fvg_m) = detect_market_structure_shift(&m2, &sw_m).expect("bullish MSS");
    let sig_m = evaluate_execution_entry(&m2, &sw_m, &mss_m, &fvg_m, C - 14367.8).expect("long signal");

    assert!(!sw_m.is_buy_side && sw_b.is_buy_side);
    assert_eq!(sw_m.sweep_time, sw_b.sweep_time);
    assert_eq!(mss_m.displacement_candle_time, mss_b.displacement_candle_time);
    assert!(fvg_m.is_bullish && !fvg_b.is_bullish);
    near(fvg_m.top, C - fvg_b.bottom, 1e-6, "mirrored FVG top");
    near(fvg_m.bottom, C - fvg_b.top, 1e-6, "mirrored FVG bottom");
    assert!(!sig_m.is_short && sig_b.is_short);
    assert_eq!(sig_m.time, sig_b.time);
    near(sig_m.entry_price, C - sig_b.entry_price, 1e-6, "mirrored entry");
    near(sig_m.stop_loss, C - sig_b.stop_loss, 1e-6, "mirrored stop");
    near(sig_m.take_profit, C - sig_b.take_profit, 1e-6, "mirrored target");
}

/// `main.rs` seeds the chart marks by running the batch pipeline over the whole Friday session.
/// It must agree with the as-of replay path on the same trade.
#[test]
fn batch_pipeline_agrees_with_replay() {
    let f = fixture();
    let (start, end, cutoff) = (1642723200_i64, 1642802400_i64, 1642775340_i64); // as in main.rs
    let fri: Vec<Candle> = f.all.iter().filter(|c| c.time >= start && c.time <= end).cloned().collect();
    let w: Vec<Candle> = f.weekly.iter().filter(|c| c.time <= cutoff).cloned().collect();
    let d: Vec<Candle> = f.daily.iter().filter(|c| c.time <= cutoff).cloned().collect();
    let r = super::run_ict_pipeline(
        &w, &d,
        &MarketDataLoader::aggregate_interval(&fri, 3600),
        &MarketDataLoader::aggregate_interval(&fri, 900),
        &MarketDataLoader::aggregate_interval(&fri, 120),
        1.5,
    );
    eprintln!("batch sweep: {:?}", r.sweep_event.as_ref().map(|s| (s.sweep_time, s.swept_level, s.is_buy_side)));
    eprintln!("batch signal: {:?}", r.execution_signal.as_ref().map(|s| (s.time, s.entry_price, s.stop_loss, s.take_profit)));
    let sig = r.execution_signal.expect("batch pipeline should find the Episode 2 short");
    assert!(sig.is_short);
    assert_eq!(sig.time, ny(11, 34));
    near(sig.entry_price, 14811.5, 0.05, "batch entry");
    near(sig.take_profit, 14677.0, 0.05, "batch target");
}
