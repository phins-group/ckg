// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

mod indexer;
mod mcp;
mod model;
mod parser;
mod retrieval;
mod scanner;
mod server;
mod storage;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use indexer::{IndexOptions, Indexer};
use retrieval::RetrievalEngine;
use storage::Storage;

#[derive(Debug, Parser)]
#[command(name = "ckg")]
#[command(about = "Local-first Code Knowledge Graph service for AI coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Init {
        repo_path: PathBuf,
    },
    Index {
        repo_path: PathBuf,
        #[arg(long)]
        full: bool,
    },
    Search {
        query: String,
        #[arg(long, default_value = ".")]
        repo_path: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    TaskContext {
        repo_path: PathBuf,
        task: String,
        #[arg(long, default_value_t = 12_000)]
        max_tokens: usize,
        #[arg(long, default_value_t = 2)]
        hops: usize,
        #[arg(long)]
        json: bool,
    },
    Doctor {
        repo_path: PathBuf,
        #[arg(long)]
        maintenance: bool,
        #[arg(long)]
        json: bool,
    },
    Mcp {
        repo_path: PathBuf,
        #[arg(long)]
        compact: bool,
    },
    Serve {
        repo_path: PathBuf,
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { repo_path } => {
            let storage = Storage::open_for_repo(&repo_path)?;
            storage.init_repo(&repo_path)?;
            println!("initialized {}", storage.db_path().display());
        }
        Commands::Index { repo_path, full } => {
            let storage = Storage::open_for_repo(&repo_path)?;
            let indexer = Indexer::new(storage);
            let report = if full {
                indexer.index_repo_with_options(&repo_path, IndexOptions { full })?
            } else {
                indexer.index_repo(&repo_path)?
            };
            println!(
                "indexed repo={} scanned={} indexed={} skipped={} deleted={} db={}",
                report.repo_id,
                report.scanned,
                report.indexed,
                report.skipped_unchanged,
                report.deleted,
                report.db_path.display()
            );
        }
        Commands::Search {
            query,
            repo_path,
            limit,
            json,
        } => {
            let storage = Storage::open_for_repo(&repo_path)?;
            let engine = RetrievalEngine::new(storage);
            let hits = engine.search(&query, limit)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&model::SearchResponse { hits })?
                );
                return Ok(());
            }
            for hit in hits {
                println!(
                    "{:.2}\t{}\t{}\t{}",
                    hit.score,
                    hit.kind,
                    hit.path.unwrap_or_default(),
                    hit.name.unwrap_or_default()
                );
                if let Some(snippet) = hit.snippet {
                    let compact = snippet.replace('\n', " ");
                    println!("  {}", compact.chars().take(180).collect::<String>());
                }
            }
        }
        Commands::TaskContext {
            repo_path,
            task,
            max_tokens,
            hops,
            json,
        } => {
            let storage = Storage::open_for_repo(&repo_path)?;
            let engine = RetrievalEngine::new(storage);
            let context =
                engine.task_context_for_repo(Some(&repo_path), &task, max_tokens, hops, true)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&context)?);
            } else {
                println!("{}", context.context_pack);
            }
        }
        Commands::Doctor {
            repo_path,
            maintenance,
            json,
        } => {
            let storage = Storage::open_for_repo(&repo_path)?;
            let repo_id = storage.init_repo(&repo_path)?;
            let report = storage.doctor_report(repo_id, maintenance)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("db={}", report.db_path);
                println!("quick_check={}", report.quick_check);
                println!("indexed_files={}", report.indexed_files);
                println!("db_bytes={}", report.db_bytes);
                println!("wal_bytes={}", report.wal_bytes.unwrap_or(0));
                println!("shm_bytes={}", report.shm_bytes.unwrap_or(0));
                println!("maintenance_ran={}", report.maintenance_ran);
                println!("optimize_ran={}", report.optimize_ran);
                println!("fts_optimize_ran={}", report.fts_optimize_ran);
                println!("wal_checkpoint_ran={}", report.wal_checkpoint_ran);
            }
        }
        Commands::Mcp { repo_path, compact } => {
            mcp::serve_stdio(repo_path, mcp::McpOptions { compact })?;
        }
        Commands::Serve { repo_path, port } => {
            server::serve(repo_path, port).await?;
        }
    }

    Ok(())
}
