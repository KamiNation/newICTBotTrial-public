# ICT 2022 Mentorship - Episode 2 Bot & Real-Time Replay Engine

A decoupled, high-performance trading engine and market replay architecture built in **Rust** with an interactive **TradingView Charting Library** frontend dashboard.

Designed following the exact multi-timeframe ICT methodology taught in **Episode 2 of the 2022 ICT Mentorship**, engineered to serve as a modular, reusable template across all 42 mentorship episodes.

---

## 🏛 Architecture: Decoupled Systems

The application strictly separates **Market Data Ingestion & Live Replay** from the **Strategy Engine**:

```
┌─────────────────────────────────────────────────────────────┐
│                       SYSTEM 1                              │
│       Data Replay & Real-Time Candle Formation Engine       │
│                                                             │
│  - Ingests authentic CME Globex 1m CSV data                 │
│  - Converts UTC to America/New_York (EST)                   │
│  - Aggregates multi-timeframe candles (1W, 1D, 1h, 15m, 2m) │
│  - Streams bars via WebSocket (/ws/replay) into TradingView │
│  - Live Replay Controls: Play, Pause, Step, Speed (1x-50x)  │
└──────────────────────────────┬──────────────────────────────┘
                               │ (Streams live bar updates)
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                       SYSTEM 2                              │
│               ICT Top-Down Strategy Engine                  │
│                                                             │
│  - Layer 1: Weekly Directional Bias (Seasonality & Tone)    │
│  - Layer 2: Daily Draw on Liquidity (Old Swing Lows/Stops)  │
│  - Layer 3: Hourly Range Framing & Sucker Drop Inducement   │
│  - Layer 4: 15-Minute Stop Hunt & Liquidity Sweep (14,820)  │
│  - Layer 5: 2-Minute BMS/MSS + Bearish FVG Entry (14,798)   │
│  - Real-Time Trade Marking & Execution Plotting             │
└─────────────────────────────────────────────────────────────┘
```

---

## 🚀 Getting Started

### Prerequisites
* [Rust](https://www.rust-lang.org/) (Cargo 1.80+)
* Historical CME Globex NQ 1m CSV data placed in `data/`
* **TradingView Charting Library:**
  Due to TradingView's proprietary licensing terms, the Charting Library assets are not distributed in this repository. To enable the interactive frontend chart:
  1. Obtain access to the official [TradingView Charting Library](https://www.tradingview.com/HTML5-stock-forex-bitcoin-charting-library/).
  2. Copy your licensed `charting_library/` folder directly into the root directory of this project.

### Running the Application

```bash
# Clone the repository
git clone git@github.com:KamiNation/newICTBotTrial.git
cd newICTBotTrial

# Compile and start the engine
cargo run
```

Once started, open **`http://localhost:3000`** in your browser.

---

## 🎮 Replay Toolbar Controls

| Control | Action |
| :--- | :--- |
| **Play / Pause** | Toggle continuous real-time bar-by-bar candlestick formation |
| **Step** | Advance by 1 minute to observe bar dynamics tick-by-tick |
| **Reset** | Reset the session tape back to market open (09:29:00 EST) |
| **Speed (1x – 50x)** | Accelerate or slow down the playback feed |
| **Status Bar** | Displays current New York EST clock and active CME NQ price |

---

## 📂 Project Structure

```text
├── data/                                 # Historical Market Data (.gitignored)
│   └── NQ_1m_Oct21_Mar22.csv             # CME Globex E-mini NASDAQ 100 1m Data
├── episodes/                             # Mentorship Episode Transcripts
│   └── episode_02/
│       ├── 2022_ICT_Mentorship_Episode_2__@studyICT.json
│       └── 2022_ICT_Mentorship_Episode_2__@studyICT.txt
├── public/                               # Frontend Dashboard
│   └── index.html                        # TradingView Chart + Replay Controls
├── charting_library/                     # TradingView Charting Library static assets (User provided, .gitignored)
└── src/                                  # Rust Bot & Engine
    ├── main.rs                           # Axum server, WebSocket broker & UDF endpoints
    ├── types.rs                          # Data models (Candle, Bias, FVG, MSS, ReplayStatus)
    ├── data/                             # System 1: Ingestion & Bar Replay
    │   ├── mod.rs
    │   ├── loader.rs                     # CSV parser, timezone converter, MTF aggregator
    │   └── replay.rs                     # Bar-by-bar real-time replay controller
    └── strategy/                         # System 2: 5-Layer Strategy Pipeline
        ├── mod.rs
        ├── bias.rs                       # Layer 1 & 2: Weekly Bias & Daily DOL
        ├── sessions.rs                   # Layer 3: Hourly Range & Sucker Drop
        ├── liquidity.rs                  # Layer 4: 15m Stop Hunt
        └── execution.rs                  # Layer 5: 2m BMS + FVG Trigger
```
