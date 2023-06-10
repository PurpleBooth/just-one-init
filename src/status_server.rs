use std::{
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    extract::State,
    http::StatusCode,
    routing::get,
    Extension,
    Json,
    Router,
    Server,
};
use serde_json::json;
use tokio::{
    sync::RwLock,
    task::JoinHandle,
};
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer,
    trace::TraceLayer,
};
use tracing::instrument;
use tracing_futures::Instrument;

use crate::JustOneInitState;

#[instrument]
pub fn spawn(
    server_listen_addr: SocketAddr,
    current_state: Arc<RwLock<JustOneInitState>>,
) -> JoinHandle<()> {
    let app = Router::new()
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CompressionLayer::new()),
        )
        .layer(Extension(current_state.clone()))
        .route(
            "/",
            get(
                move |State(is_leader): State<Arc<RwLock<JustOneInitState>>>| async move {
                    let state = *(is_leader.read().await);
                    match state {
                        JustOneInitState::BecameFollower => (
                            StatusCode::NOT_FOUND,
                            Json(json!({"status": "ok", "state": JustOneInitState::BecameFollower.to_string()})),
                        ),
                        JustOneInitState::BecameLeader => (
                            StatusCode::OK,
                            Json(json!({"status": "ok", "state": JustOneInitState::BecameLeader.to_string()})),
                        ),
                        JustOneInitState::BeganShutdown => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"status": "error", "state": JustOneInitState::BeganShutdown.to_string()})),
                        ),
                    }
                },
            ),
        )
        .with_state(current_state);

    let start_http_server = async move {
        Server::bind(&server_listen_addr)
            .serve(app.into_make_service())
            .await
            .expect("Failed to start server");
    };
    tokio::spawn(start_http_server.instrument(tracing::info_span!("http_server")))
}
