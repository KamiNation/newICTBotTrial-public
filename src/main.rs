pub mod data;
pub mod strategy;
pub mod types;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

use data::{MarketDataLoader, ReplayCommand, ReplayEngine, ReplayStatus};
use strategy::{compute_dynamic_ict_levels, run_ict_pipeline, DynamicIctLevels};
use types::{Candle, ChartIndicator, JournalEvent, TradeMark};

#[derive(Clone)]
pub struct AppState {
    pub all_1m_candles: Vec<Candle>,
    pub weekly_candles_all: Vec<Candle>,
    pub daily_candles_all: Vec<Candle>,
    pub marks: Vec<TradeMark>,
    pub replay_engine: Arc<RwLock<ReplayEngine>>,
    pub cutoff_time: i64,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub symbol: String,
    pub resolution: String,
    pub from: i64,
    pub to: i64,
    #[serde(default)]
    pub countback: Option<i64>,
    #[serde(rename = "firstDataRequest", default)]
    pub first_data_request: Option<bool>,
}

#[derive(Serialize)]
pub struct UdfConfig {
    pub supports_search: bool,
    pub supports_group_request: bool,
    pub supported_resolutions: Vec<String>,
    pub supports_marks: bool,
    pub supports_timescale_marks: bool,
    pub supports_time: bool,
}

#[derive(Serialize)]
pub struct UdfSymbol {
    pub name: String,
    pub ticker: String,
    pub description: String,
    pub r#type: String,
    pub session: String,
    pub exchange: String,
    pub listed_exchange: String,
    pub timezone: String,
    pub minmov: i32,
    pub pricescale: i32,
    pub minmove2: i32,
    pub fractional: bool,
    pub format: String,
    pub has_intraday: bool,
    pub intraday_multipliers: Vec<String>,
    pub has_daily: bool,
    pub has_weekly_and_monthly: bool,
    pub supported_resolutions: Vec<String>,
    pub has_no_volume: bool,
}

#[derive(Serialize)]
pub struct UdfHistoryResponse {
    pub s: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<Vec<f64>>,
    #[serde(rename = "nextTime", skip_serializing_if = "Option::is_none")]
    pub next_time: Option<i64>,
}

#[derive(Serialize)]
pub struct UdfMarksResponse {
    pub id: Vec<usize>,
    pub time: Vec<i64>,
    pub color: Vec<String>,
    pub text: Vec<String>,
    pub label: Vec<String>,
    #[serde(rename = "minSize")]
    pub min_size: Vec<u32>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    info!("Initializing Decoupled System: CME NQ Replay & ICT Bot Engine...");

    // 1. Ingest real CME 1m historical CSV data
    let csv_path = if std::path::Path::new("data/NQ_1m_Oct21_Mar22.csv").exists() {
        "data/NQ_1m_Oct21_Mar22.csv"
    } else {
        "NQ_1m_Oct21_Mar22.csv"
    };

    let loader = MarketDataLoader::load_from_csv(csv_path)
        .expect("Failed to load historical CME Globex NQ 1m CSV");

    // Pre-aggregate full dataset MTF context
    let weekly_candles_all = MarketDataLoader::aggregate_weekly(&loader.candles_1m);
    let daily_candles_all = MarketDataLoader::aggregate_daily(&loader.candles_1m);

    info!("Ingested {} 1m bars. Aggregated {} weekly, {} daily bars across full dataset.",
        loader.candles_1m.len(), weekly_candles_all.len(), daily_candles_all.len()
    );

    // Default cutoff: Friday, Jan 21, 2022 at 09:29:00 EST (14:29 UTC) = 1642775340
    let cutoff_time = 1642775340;

    // 2. Initialize Replay Engine with all candles and dynamic MTF context
    let (replay_engine, _) = ReplayEngine::new(
        loader.candles_1m.clone(),
        weekly_candles_all.clone(),
        daily_candles_all.clone(),
        cutoff_time,
    );

    // 3. Pre-run Episode 2 strategy on the Friday Jan 21 session to compute baseline marks
    let friday_start = 1642723200;
    let friday_end = 1642802400;
    let friday_1m: Vec<Candle> = loader
        .candles_1m
        .iter()
        .filter(|c| c.time >= friday_start && c.time <= friday_end)
        .cloned()
        .collect();

    let full_friday_2m = MarketDataLoader::aggregate_interval(&friday_1m, 120);
    let full_friday_15m = MarketDataLoader::aggregate_interval(&friday_1m, 900);
    let full_friday_1h = MarketDataLoader::aggregate_interval(&friday_1m, 3600);

    let weekly_up_to_friday: Vec<Candle> = strategy::bias::completed_weeks(&weekly_candles_all, cutoff_time);
    let daily_up_to_friday: Vec<Candle> =
        MarketDataLoader::session_candles_as_of(&daily_candles_all, &loader.candles_1m, cutoff_time);

    let pipeline_result = run_ict_pipeline(
        &weekly_up_to_friday,
        &daily_up_to_friday,
        &full_friday_1h,
        &full_friday_15m,
        &full_friday_2m,
        1.5,
    );

    let shared_state = Arc::new(RwLock::new(AppState {
        all_1m_candles: loader.candles_1m,
        weekly_candles_all,
        daily_candles_all,
        marks: pipeline_result.chart_marks,
        replay_engine: replay_engine.clone(),
        cutoff_time,
    }));

    // 4. Background Replay Loop
    let bg_state = shared_state.clone();
    tokio::spawn(async move {
        loop {
            let (is_playing, speed) = {
                let s = bg_state.read().await;
                let eng = s.replay_engine.read().await;
                (eng.is_playing, eng.speed)
            };

            if is_playing {
                {
                    let s = bg_state.read().await;
                    let mut eng = s.replay_engine.write().await;
                    eng.step_forward();
                }
                let sleep_ms = ((1000.0 / speed.max(0.1)) as u64).clamp(10, 2000);
                tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
            } else {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    });

    // 5. Axum Server & Routes
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/config", get(udf_config_handler))
        .route("/symbols", get(udf_symbols_handler))
        .route("/history", get(udf_history_handler))
        .route("/marks", get(udf_marks_handler))
        .route("/time", get(udf_time_handler))
        .route("/api/replay/status", get(replay_status_handler))
        .route("/api/replay/control", post(replay_control_handler))
        .route("/api/ict/levels", get(ict_levels_handler))
        .route("/api/ict/indicators", get(ict_indicators_handler))
        .route("/api/ict/journal", get(ict_journal_handler))
        .route("/ws/replay", get(ws_replay_handler))
        .nest_service("/charting_library", ServeDir::new("charting_library"))
        .layer(CorsLayer::permissive())
        .with_state(shared_state);

    let addr = "0.0.0.0:3000";
    info!("Replay Dashboard and UDF API ready at http://localhost:3000");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index_handler() -> Html<String> {
    match tokio::fs::read_to_string("public/index.html").await {
        Ok(content) => Html(content),
        Err(_) => Html("<h1>Error: public/index.html not found</h1>".to_string()),
    }
}

async fn udf_config_handler() -> Json<UdfConfig> {
    Json(UdfConfig {
        supports_search: false,
        supports_group_request: false,
        supported_resolutions: vec![
            "1".to_string(),
            "2".to_string(),
            "15".to_string(),
            "60".to_string(),
            "1D".to_string(),
            "1W".to_string(),
        ],
        supports_marks: true,
        supports_timescale_marks: false,
        supports_time: true,
    })
}

async fn udf_symbols_handler(Query(params): Query<std::collections::HashMap<String, String>>) -> Json<UdfSymbol> {
    let symbol = params.get("symbol").cloned().unwrap_or_else(|| "NQ".to_string());
    Json(UdfSymbol {
        name: symbol.clone(),
        ticker: symbol.clone(),
        description: "E-MINI NASDAQ-100 (MAR 2022) CME Globex".to_string(),
        r#type: "futures".to_string(),
        session: "24x7".to_string(),
        exchange: "CME_MINI".to_string(),
        listed_exchange: "CME".to_string(),
        timezone: "America/New_York".to_string(),
        minmov: 25,
        pricescale: 100,
        minmove2: 0,
        fractional: false,
        format: "price".to_string(),
        has_intraday: true,
        intraday_multipliers: vec!["1".to_string(), "2".to_string(), "15".to_string(), "60".to_string()],
        has_daily: true,
        has_weekly_and_monthly: true,
        supported_resolutions: vec![
            "1".to_string(),
            "2".to_string(),
            "15".to_string(),
            "60".to_string(),
            "1D".to_string(),
            "1W".to_string(),
        ],
        has_no_volume: false,
    })
}

async fn udf_history_handler(
    State(state): State<Arc<RwLock<AppState>>>,
    Query(query): Query<HistoryQuery>,
) -> Json<UdfHistoryResponse> {
    let s = state.read().await;

    // Current simulation timestamp: strictly prevents look-ahead
    let current_replay_time = {
        let eng = s.replay_engine.read().await;
        eng.get_status().current_time
    };

    let base_candles = match query.resolution.as_str() {
        "1" => {
            s.all_1m_candles
                .iter()
                .filter(|c| c.time <= current_replay_time)
                .cloned()
                .collect()
        }
        "2" => {
            let filtered: Vec<Candle> = s.all_1m_candles
                .iter()
                .filter(|c| c.time <= current_replay_time)
                .cloned()
                .collect();
            MarketDataLoader::aggregate_interval(&filtered, 120)
        }
        "15" => {
            let filtered: Vec<Candle> = s.all_1m_candles
                .iter()
                .filter(|c| c.time <= current_replay_time)
                .cloned()
                .collect();
            MarketDataLoader::aggregate_interval(&filtered, 900)
        }
        "60" => {
            let filtered: Vec<Candle> = s.all_1m_candles
                .iter()
                .filter(|c| c.time <= current_replay_time)
                .cloned()
                .collect();
            MarketDataLoader::aggregate_interval(&filtered, 3600)
        }
        // Engine candles are stamped at the real session open; the chart gets NY midnight of the
        // session date, exactly the timestamps it received before the session-boundary fix.
        "D" | "1D" => {
            MarketDataLoader::session_candles_as_of(&s.daily_candles_all, &s.all_1m_candles, current_replay_time)
                .into_iter()
                .map(|c| Candle { time: MarketDataLoader::session_chart_time(c.time), ..c })
                .collect()
        }
        "W" | "1W" => {
            MarketDataLoader::session_candles_as_of(&s.weekly_candles_all, &s.all_1m_candles, current_replay_time)
                .into_iter()
                .map(|c| Candle { time: MarketDataLoader::session_chart_time(c.time), ..c })
                .collect()
        }
        _ => {
            let filtered: Vec<Candle> = s.all_1m_candles
                .iter()
                .filter(|c| c.time <= current_replay_time)
                .cloned()
                .collect();
            MarketDataLoader::aggregate_interval(&filtered, 120)
        }
    };

    let filtered: Vec<&Candle> = base_candles
        .iter()
        .filter(|c| c.time >= query.from && c.time <= query.to)
        .collect();

    if filtered.is_empty() {
        let next_time = base_candles
            .iter()
            .filter(|c| c.time < query.from)
            .map(|c| c.time)
            .next_back();

        return Json(UdfHistoryResponse {
            s: "no_data".to_string(),
            t: None,
            o: None,
            h: None,
            l: None,
            c: None,
            v: None,
            next_time,
        });
    }

    let t: Vec<i64> = filtered.iter().map(|c| c.time).collect();
    let o: Vec<f64> = filtered.iter().map(|c| c.open).collect();
    let h: Vec<f64> = filtered.iter().map(|c| c.high).collect();
    let l: Vec<f64> = filtered.iter().map(|c| c.low).collect();
    let c: Vec<f64> = filtered.iter().map(|c| c.close).collect();
    let v: Vec<f64> = filtered.iter().map(|c| c.volume).collect();

    Json(UdfHistoryResponse {
        s: "ok".to_string(),
        t: Some(t),
        o: Some(o),
        h: Some(h),
        l: Some(l),
        c: Some(c),
        v: Some(v),
        next_time: None,
    })
}

async fn udf_marks_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> Json<UdfMarksResponse> {
    let s = state.read().await;
    let current_replay_time = {
        let eng = s.replay_engine.read().await;
        eng.get_status().current_time
    };

    let mut ids = Vec::new();
    let mut times = Vec::new();
    let mut colors = Vec::new();
    let mut texts = Vec::new();
    let mut labels = Vec::new();
    let mut min_sizes = Vec::new();

    // Only return marks that have occurred up to current replay time
    for m in &s.marks {
        if m.time <= current_replay_time {
            ids.push(m.id);
            times.push(m.time);
            colors.push(m.color.clone());
            texts.push(m.text.clone());
            labels.push(m.label.clone());
            min_sizes.push(m.min_size);
        }
    }

    Json(UdfMarksResponse {
        id: ids,
        time: times,
        color: colors,
        text: texts,
        label: labels,
        min_size: min_sizes,
    })
}

async fn udf_time_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> String {
    let state = state.read().await;
    let eng = state.replay_engine.read().await;
    let t = eng.get_status().current_time;
    t.to_string()
}

async fn replay_status_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> Json<ReplayStatus> {
    let s = state.read().await;
    let eng = s.replay_engine.read().await;
    Json(eng.get_status())
}

async fn replay_control_handler(
    State(state): State<Arc<RwLock<AppState>>>,
    Json(cmd): Json<ReplayCommand>,
) -> Json<ReplayStatus> {
    let s = state.read().await;
    let mut eng = s.replay_engine.write().await;

    match cmd {
        ReplayCommand::Play => eng.is_playing = true,
        ReplayCommand::Pause => eng.is_playing = false,
        ReplayCommand::SetSpeed(speed) => eng.speed = speed,
        ReplayCommand::Step => {
            eng.is_playing = false;
            eng.step_forward();
        }
        ReplayCommand::Reset => eng.reset(),
        ReplayCommand::SetStartPoint(point) => eng.set_start_point(&point),
    }

    Json(eng.get_status())
}

async fn ict_levels_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> Json<DynamicIctLevels> {
    let s = state.read().await;
    let current_replay_time = {
        let eng = s.replay_engine.read().await;
        eng.get_status().current_time
    };

    let levels = compute_dynamic_ict_levels(
        &s.all_1m_candles,
        &s.weekly_candles_all,
        &s.daily_candles_all,
        current_replay_time,
    );

    Json(levels)
}

async fn ict_indicators_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> Json<Vec<ChartIndicator>> {
    let s = state.read().await;
    let current_replay_time = {
        let eng = s.replay_engine.read().await;
        eng.get_status().current_time
    };

    let levels = compute_dynamic_ict_levels(
        &s.all_1m_candles,
        &s.weekly_candles_all,
        &s.daily_candles_all,
        current_replay_time,
    );

    Json(levels.indicators)
}

async fn ict_journal_handler(
    State(state): State<Arc<RwLock<AppState>>>,
) -> Json<Vec<JournalEvent>> {
    let s = state.read().await;
    let current_replay_time = {
        let eng = s.replay_engine.read().await;
        eng.get_status().current_time
    };

    let levels = compute_dynamic_ict_levels(
        &s.all_1m_candles,
        &s.weekly_candles_all,
        &s.daily_candles_all,
        current_replay_time,
    );

    Json(levels.journal_logs)
}

async fn ws_replay_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RwLock<AppState>>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
}

async fn handle_ws_socket(mut socket: WebSocket, state: Arc<RwLock<AppState>>) {
    let mut rx = {
        let s = state.read().await;
        let eng = s.replay_engine.read().await;
        eng.update_tx.subscribe()
    };

    // Send initial status
    let initial_status = {
        let s = state.read().await;
        s.replay_engine.read().await.get_status()
    };
    let initial_msg = json!({
        "type": "status",
        "status": initial_status
    });
    let _ = socket
        .send(Message::Text(serde_json::to_string(&initial_msg).unwrap().into()))
        .await;

    let mut status_ticker = tokio::time::interval(tokio::time::Duration::from_millis(400));

    loop {
        tokio::select! {
            update_res = rx.recv() => {
                match update_res {
                    Ok(bar_update) => {
                        let msg = json!({
                            "type": "bar_update",
                            "bar": bar_update
                        });
                        if socket
                            .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = status_ticker.tick() => {
                let status = {
                    let s = state.read().await;
                    s.replay_engine.read().await.get_status()
                };
                let msg = json!({
                    "type": "status",
                    "status": status
                });
                if socket
                    .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            client_msg = socket.recv() => {
                match client_msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
