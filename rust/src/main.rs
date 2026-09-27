//! Local recording proxy and tester for the TypeSafe /v1/systemone API.
use jev_proxy::{proxy, routes};
use axum::{body::Bytes, extract::{DefaultBodyLimit, State}, http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri}, response::Response, Router};
use routes::{handle, App};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

fn load_env(path: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return env };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            env.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    env
}

async fn dispatch(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let target = uri.path_and_query().map_or("/", |p| p.as_str()).to_string();
    let headers: Vec<(String, String)> = headers.iter().filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string()))).collect();
    let reply = tokio::task::spawn_blocking(move || handle(&app, method.as_str(), &target, &headers, &body)).await;
    let reply = match reply {
        Ok(r) => r,
        Err(e) => return Response::builder().status(500).body(format!("{{\"error\":\"{e}\"}}").into()).unwrap(),
    };
    let mut resp = Response::builder().status(StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (k, v) in reply.headers {
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
            resp = resp.header(k, v);
        }
    }
    resp.body(reply.body.into()).unwrap()
}

#[tokio::main]
async fn main() {
    // Real environment variables win over a `.env` in the directory the server starts from.
    let mut env = load_env(".env");
    env.extend(std::env::vars());
    let get = |k: &str, d: &str| env.get(k).cloned().unwrap_or_else(|| d.to_string());
    let config = proxy::Config {
        base_url: get("TYPESAFE_BASE_URL", "https://api.typesafe.ai"),
        api_key: get("TYPESAFE_API_KEY", ""),
        cache_ttl: get("CACHE_TTL_SECONDS", "3600").parse().unwrap_or(3600),
        second_base_url: get("SECOND_BASE_URL", ""),
        second_api_key: get("SECOND_API_KEY", ""),
        second_model: get("SECOND_MODEL", ""),
        second_label: get("SECOND_LABEL", "Second"),
        db_path: PathBuf::from(get("JEV_DATA_DIR", "data")).join("jev.db"),
        max_pending_shadows: 4,
    };

    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--prune-calls") {
        let Some(days) = args.get(i + 1).and_then(|d| d.parse::<i64>().ok()) else {
            eprintln!("usage: jev-proxy --prune-calls <days>");
            std::process::exit(2);
        };
        match routes::run_prune(&config, days) {
            Ok(msg) => println!("{msg}"),
            Err(_) => std::process::exit(1),
        }
        return;
    }

    config.open_db().expect("cannot open the database");
    if config.api_key.is_empty() {
        eprintln!("WARNING: TYPESAFE_API_KEY missing (environment or .env), sending will fail");
    }
    let port: u16 = get("PORT", "8420").parse().unwrap_or(8420);
    let app = Arc::new(App { config: Arc::new(config), static_dir: env.get("JEV_STATIC_DIR").map(PathBuf::from) });
    let router = Router::new().fallback(dispatch).layer(DefaultBodyLimit::max(64 * 1024 * 1024)).with_state(app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("cannot bind the port");
    println!("jev tester running at http://127.0.0.1:{port}");
    axum::serve(listener, router).await.unwrap();
}
