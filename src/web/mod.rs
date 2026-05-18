use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use std::sync::{Arc, Mutex};

use crate::db::{Db, Preference};
use crate::{parse_score_band, ScenarioConfig};

// ── App state ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Arc<Mutex<Db>>,
    pub(crate) scenario_config: ScenarioConfig,
}

// ── Error handling ─────────────────────────────────────────────────────────────

struct AppError(anyhow::Error);

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

// ── Query params ───────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct PreferencesQuery {
    scenario: Option<String>,
    keyword: Option<String>,
    score_band: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

// ── Response types ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct StatsResponse {
    total: usize,
    high: usize,
    mid: usize,
    low: usize,
    half_life_days: f64,
    floor: f64,
}

#[derive(serde::Serialize)]
struct ScenarioInfo {
    name: String,
    entries: usize,
    keywords: Vec<String>,
    description: String,
}

#[derive(serde::Serialize)]
struct KeywordInfo {
    keyword: String,
    count: usize,
}

// ── Handlers ───────────────────────────────────────────────────────────────────

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("app.html"))
}

async fn preferences_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PreferencesQuery>,
) -> Result<Json<Vec<Preference>>, AppError> {
    let db = state.db.lock().unwrap();

    // Build queries for keyword search
    let mut queries: Vec<&str> = Vec::new();
    if let Some(ref s) = params.search {
        queries.push(s.as_str());
    }
    if let Some(ref k) = params.keyword {
        queries.push(k.as_str());
    }

    // Score band
    let ranges: Vec<(f64, f64)> = if let Some(ref band) = params.score_band {
        vec![parse_score_band(band)?]
    } else {
        vec![]
    };

    let mut prefs = db.recall(
        &queries,
        &ranges,
        params.scenario.as_deref(),
        None,
        false, // read-only: never touch from web
    )?;

    // Sort
    match params.sort.as_deref() {
        Some("name") => prefs.sort_by(|a, b| a.item.to_lowercase().cmp(&b.item.to_lowercase())),
        Some("base_score") => {
            prefs.sort_by(|a, b| b.base_score.partial_cmp(&a.base_score).unwrap())
        }
        Some("recency") => prefs.sort_by(|a, b| b.last_seen.cmp(&a.last_seen)),
        _ => {
            prefs.sort_by(|a, b| b.effective_weight.partial_cmp(&a.effective_weight).unwrap())
        }
    }

    if params.order.as_deref() == Some("asc") {
        prefs.reverse();
    }

    Ok(Json(prefs))
}

async fn keywords_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<KeywordInfo>>, AppError> {
    let db = state.db.lock().unwrap();
    let mut kws: Vec<KeywordInfo> = db
        .all_keywords()?
        .into_iter()
        .map(|(keyword, count)| KeywordInfo { keyword, count })
        .collect();
    kws.sort_by(|a, b| b.count.cmp(&a.count).then(a.keyword.cmp(&b.keyword)));
    Ok(Json(kws))
}

async fn scenarios_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ScenarioInfo>>, AppError> {
    let db = state.db.lock().unwrap();

    let config_map: std::collections::HashMap<String, (String, Vec<String>)> = state
        .scenario_config
        .scenarios
        .iter()
        .map(|(k, v)| (k.clone(), (v.description.clone(), v.keywords.clone())))
        .collect();

    let stats = db.all_scenarios(&config_map)?;

    let result: Vec<ScenarioInfo> = stats
        .into_iter()
        .map(|(name, s)| {
            let kws = config_map
                .get(&name)
                .map(|(_, kws)| kws.clone())
                .unwrap_or_default();
            ScenarioInfo {
                name,
                entries: s.entries,
                keywords: kws,
                description: s.description,
            }
        })
        .collect();

    Ok(Json(result))
}

async fn stats_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatsResponse>, AppError> {
    let db = state.db.lock().unwrap();
    let stats = db.stats()?;
    let cfg = db.get_decay_config()?;
    Ok(Json(StatsResponse {
        total: stats.total(),
        high: stats.high,
        mid: stats.mid,
        low: stats.low,
        half_life_days: cfg.half_life_days,
        floor: cfg.floor,
    }))
}

// ── Server startup ─────────────────────────────────────────────────────────────

pub(crate) async fn serve(
    db: Db,
    scenario_config: ScenarioConfig,
    port: u16,
    open_browser: bool,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        db: Arc::new(Mutex::new(db)),
        scenario_config,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/preferences", get(preferences_handler))
        .route("/api/keywords", get(keywords_handler))
        .route("/api/scenarios", get(scenarios_handler))
        .route("/api/stats", get(stats_handler))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    eprintln!("aizo web server starting at http://{addr}");

    if open_browser {
        let url = format!("http://localhost:{port}");
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", &url])
                .spawn();
        }
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
