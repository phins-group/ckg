// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{net::SocketAddr, path::PathBuf};

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    indexer::{IndexOptions, Indexer},
    model::{
        IndexRequest, IndexResponse, SearchRequest, SearchResponse, TaskContextRequest,
        TaskContextResponse,
    },
    retrieval::RetrievalEngine,
    storage::Storage,
};

#[derive(Clone)]
struct AppState {
    repo_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct NeighborhoodQuery {
    hops: Option<usize>,
}

pub async fn serve(repo_path: PathBuf, port: u16) -> Result<()> {
    let state = AppState {
        repo_path: repo_path.canonicalize().unwrap_or(repo_path),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/index", post(index))
        .route("/search", post(search))
        .route("/task-context", post(task_context))
        .route("/nodes/:id/neighborhood", get(neighborhood))
        .route("/files/*path", get(file_content))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("ckg server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "ckg" }))
}

async fn index(
    State(state): State<AppState>,
    request: Option<Json<IndexRequest>>,
) -> ApiResult<IndexResponse> {
    let request = request.map(|Json(request)| request);
    let full = request
        .as_ref()
        .and_then(|request| request.full)
        .unwrap_or(false);
    let repo_path =
        if let Some(repo_path) = request.and_then(|request| request.repo_path.map(PathBuf::from)) {
            let requested = repo_path
                .canonicalize()
                .map_err(|error| api_error(error.into()))?;
            if requested != state.repo_path {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "repo_path override is not allowed for this server".to_string(),
                ));
            }
            requested
        } else {
            state.repo_path
        };
    let storage = Storage::open_for_repo(&repo_path).map_err(api_error)?;
    let report = Indexer::new(storage)
        .index_repo_with_options(&repo_path, IndexOptions { full })
        .map_err(api_error)?;
    Ok(Json(IndexResponse {
        repo_id: report.repo_id,
        scanned: report.scanned,
        indexed: report.indexed,
        skipped_unchanged: report.skipped_unchanged,
        deleted: report.deleted,
        db_path: report.db_path.display().to_string(),
    }))
}

async fn search(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> ApiResult<SearchResponse> {
    let storage = Storage::open_for_repo(&state.repo_path).map_err(api_error)?;
    let engine = RetrievalEngine::new(storage);
    let hits = engine
        .search(&request.query, request.limit.unwrap_or(20))
        .map_err(api_error)?;
    Ok(Json(SearchResponse { hits }))
}

async fn task_context(
    State(state): State<AppState>,
    Json(request): Json<TaskContextRequest>,
) -> ApiResult<TaskContextResponse> {
    let storage = Storage::open_for_repo(&state.repo_path).map_err(api_error)?;
    let engine = RetrievalEngine::new(storage);
    let response = engine
        .task_context_for_repo(
            Some(&state.repo_path),
            &request.task,
            request.max_tokens.unwrap_or(12_000),
            request.hops.unwrap_or(2),
            true,
        )
        .map_err(api_error)?;
    Ok(Json(response))
}

async fn neighborhood(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<NeighborhoodQuery>,
) -> impl IntoResponse {
    let storage = match Storage::open_for_repo(&state.repo_path) {
        Ok(storage) => storage,
        Err(err) => return api_error(err).into_response(),
    };
    let engine = RetrievalEngine::new(storage);
    match engine.neighborhood(id, query.hops.unwrap_or(2)) {
        Ok(graph) => Json(graph).into_response(),
        Err(err) => api_error(err).into_response(),
    }
}

async fn file_content(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    let storage = match Storage::open_for_repo(&state.repo_path) {
        Ok(storage) => storage,
        Err(err) => return api_error(err).into_response(),
    };
    let engine = RetrievalEngine::new(storage);
    match engine.file_content(&path) {
        Ok(Some(content)) => Json(content).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "file not found".to_string()).into_response(),
        Err(err) => api_error(err).into_response(),
    }
}

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn api_error(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}
