//! Durable, expiring transfer grants bound to a project worker credential generation.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
    worker_transfer,
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Write,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    pub run_id: String,
    pub worker_id: String,
    pub generation: u64,
    pub ttl_seconds: u32,
}
pub async fn prepare(engine: Arc<Engine>, r: Prepare) -> Result<Value> {
    prepare_tree(engine, r, None).await
}
pub(crate) async fn prepare_stage(
    engine: Arc<Engine>,
    r: Prepare,
    workspace: std::path::PathBuf,
) -> Result<Value> {
    prepare_tree(engine, r, Some(workspace)).await
}
async fn prepare_tree(
    engine: Arc<Engine>,
    r: Prepare,
    prepared: Option<std::path::PathBuf>,
) -> Result<Value> {
    let permit = engine
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .context("worker transfer capacity exhausted")?;
    tokio::task::spawn_blocking(move||{
        let _permit=permit;
        ensure!((1..=600).contains(&r.ttl_seconds),invalid("transfer lifetime must be 1..600 seconds"));
        ensure!(r.generation>0 && r.generation<=i64::MAX as u64,invalid("invalid worker generation"));
        let run=engine.db.run(&r.run_id)?;
        ensure!(run.pipeline.is_some(),invalid("worker transfer requires a pipeline source snapshot"));
        let inputs=engine.db.inputs(&r.run_id)?.context("run source snapshot is not ready")?;
        let id=uuid::Uuid::now_v7().to_string();
        let _lease=engine.db.begin_operation(&r.run_id,"worker_transfer_prepare",BTreeSet::from([format!("worker-transfer:{id}")]))?;
        let root=crate::pipeline::contained(&engine.config.data_dir,"worker-transfers")?;
        std::fs::create_dir_all(&root)?;
        let temp=tempfile::tempdir_in(&root)?;
        let source=match &prepared { Some(path)=>path.clone(), None=>crate::pipeline::contained(&run.workspace,"source")? };
        let tree=crate::trees::copy(&source,&temp.path().join("tree"))?;
        if prepared.is_none() { ensure!(crate::hashing::hex(Sha256::digest(serde_json::to_vec(&tree.entries)?))==inputs.tree_sha256,invalid("source snapshot no longer matches recorded inputs")); }
        let digest=tree.sha256()?;
        let path=temp.path().join("bundle");
        let mut file=std::fs::File::create(&path)?;
        worker_transfer::encode(&temp.path().join("tree"),&tree,&mut file)?;
        file.flush()?;file.sync_all()?;
        let bytes=file.metadata()?.len();drop(file);
        // The wire object owns all bytes now; the temporary encoding copy is redundant.
        std::fs::remove_dir_all(temp.path().join("tree"))?;
        let directory=temp.path().file_name().context("transfer directory")?.to_str().context("transfer directory encoding")?.to_owned();
        #[cfg(unix)] {
            std::fs::File::open(temp.path())?.sync_all()?;
            std::fs::File::open(&root)?.sync_all()?;
        }
        let mut c=engine.db.connection()?;
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND generation=?2 AND project_id=?3 AND state='registered')",params![r.worker_id,r.generation,run.project_id],|row|row.get(0))?;
        ensure!(valid,invalid("worker project, generation or registration does not match"));
        let now=crate::workers::now();let expiry=now+i64::from(r.ttl_seconds);
        tx.execute("INSERT INTO worker_transfers VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,r.run_id,r.worker_id,r.generation,directory,digest,bytes,expiry,now])?;
        tx.commit()?;let _ = temp.keep();
        Ok(json!({"transfer_id":id,"run_id":r.run_id,"worker_id":r.worker_id,"generation":r.generation,"tree_sha256":digest,"wire_bytes":bytes,"files_and_directories":tree.entries.len(),"logical_bytes":tree.size(),"expires_at":expiry,"format":"preflight-tree-transfer-v1"}))
    }).await?
}
struct Download {
    reader: tokio_util::io::ReaderStream<tokio::fs::File>,
    engine: Arc<Engine>,
    lease_id: String,
    transfer_id: String,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for Download {
    fn drop(&mut self) {
        if let Err(e) = self.engine.db.release_operation(&self.lease_id) {
            eprintln!("transfer operation release failed: {e}")
        }
    }
}
impl futures_core::Stream for Download {
    type Item = std::io::Result<axum::body::Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let valid = (|| -> Result<bool> {
            let c = self.engine.db.connection()?;
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM worker_transfers t JOIN workers w ON w.id=t.worker_id WHERE t.id=?1 AND t.expires_at>?2 AND w.generation=t.generation AND w.state='registered')",params![self.transfer_id,crate::workers::now()],|r|r.get(0))?)
        })();
        if !matches!(valid, Ok(true)) || self.engine.is_stopping() {
            return Poll::Ready(Some(Err(std::io::Error::other(
                "transfer expired, revoked or unavailable",
            ))));
        }
        Pin::new(&mut self.reader).poll_next(cx)
    }
}
pub async fn download(
    axum::extract::State(engine): axum::extract::State<Arc<Engine>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    use axum::http::{StatusCode, header};
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let credential = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let hash = crate::workers::hash(credential).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let permit = engine
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let row=(||->Result<(String,String,u64)>{
        let c=engine.db.connection()?;
        c.query_row("SELECT t.run_id,t.directory,t.wire_bytes FROM worker_transfers t JOIN workers w ON w.id=t.worker_id WHERE t.id=?1 AND t.expires_at>?2 AND t.generation=w.generation AND w.state='registered' AND w.credential_hash=?3",params![id,crate::workers::now(),hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(||Fault::NotFound.into())
    })().map_err(|_|StatusCode::UNAUTHORIZED)?;
    let path = crate::pipeline::contained(
        &engine.config.data_dir,
        &format!("worker-transfers/{}/bundle", row.1),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let lease = engine
        .db
        .begin_operation(
            &row.0,
            "worker_transfer_read",
            BTreeSet::from([format!("worker-transfer:{id}")]),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let lease_id = lease.id.clone();
    std::mem::forget(lease);
    let body = Download {
        reader: tokio_util::io::ReaderStream::with_capacity(file, 65536),
        engine,
        lease_id,
        transfer_id: id,
        _permit: permit,
    };
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, row.2)
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from_stream(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
