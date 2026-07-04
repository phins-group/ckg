// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Repository,
    Directory,
    File,
    Symbol,
    Function,
    Method,
    Class,
    Type,
    Test,
    Doc,
    Endpoint,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Directory => "Directory",
            Self::File => "File",
            Self::Symbol => "Symbol",
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Class => "Class",
            Self::Type => "Type",
            Self::Test => "Test",
            Self::Doc => "Doc",
            Self::Endpoint => "Endpoint",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Contains,
    Defines,
    Imports,
    Calls,
    References,
    Tests,
    Documents,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "CONTAINS",
            Self::Defines => "DEFINES",
            Self::Imports => "IMPORTS",
            Self::Calls => "CALLS",
            Self::References => "REFERENCES",
            Self::Tests => "TESTS",
            Self::Documents => "DOCUMENTS",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: i64,
    pub repo_id: i64,
    pub path: String,
    pub abs_path: String,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub hash: String,
    pub size: i64,
    pub modified_at: i64,
    pub is_binary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: i64,
    pub repo_id: i64,
    pub file_id: Option<i64>,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub path: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub summary: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub id: i64,
    pub repo_id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub kind: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: String,
    pub ref_id: i64,
    pub file_id: Option<i64>,
    pub node_id: Option<i64>,
    pub path: Option<String>,
    pub name: Option<String>,
    pub snippet: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subgraph {
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexRequest {
    pub repo_path: Option<String>,
    pub full: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexResponse {
    pub repo_id: i64,
    pub scanned: usize,
    pub indexed: usize,
    pub skipped_unchanged: usize,
    pub deleted: usize,
    pub db_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskContextRequest {
    pub task: String,
    pub max_tokens: Option<usize>,
    pub hops: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskContextResponse {
    pub query: String,
    pub relevant_files: Vec<SearchHit>,
    pub relevant_symbols: Vec<SearchHit>,
    pub subgraph: Subgraph,
    pub suggested_tests: Vec<SearchHit>,
    pub context_pack: String,
}
