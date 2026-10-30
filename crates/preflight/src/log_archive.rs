//! Lossless bounded zstd chunks preserve log event identities and the search index.
use crate::{
    db::Db,
    engine::Engine,
    model::{Fault, LogEvent, invalid},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, sync::Arc};
const TEXT_BYTES: usize = 512 * 1024;
const PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub project_id: String,
    pub run_id: String,
    #[serde(default = "chunks")]
    pub max_chunks: usize,
}
fn chunks() -> usize {
    4
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub run_id: String,
}
fn authorize(c: &rusqlite::Connection, project: &str, run: &str) -> Result<bool> {
    c.query_row(
        "SELECT finished_at IS NOT NULL FROM runs WHERE project_id=?1 AND id=?2",
        params![project, run],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| Fault::NotFound.into())
}
fn decode(payload: &[u8], length: usize, sha: &str) -> Result<Vec<(i64, String)>> {
    ensure!(
        length <= PAYLOAD_BYTES && payload.len() <= PAYLOAD_BYTES,
        "log chunk exceeds decode limit"
    );
    let mut decoder = zstd::stream::read::Decoder::new(payload)?;
    decoder.window_log_max(23)?;
    let mut raw = Vec::new();
    decoder
        .take(PAYLOAD_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(raw.len() == length, "log chunk length mismatch");
    ensure!(
        crate::hashing::hex(Sha256::digest(&raw)) == sha,
        "log chunk digest mismatch"
    );
    let records: Vec<(i64, String)> = serde_json::from_slice(&raw)?;
    ensure!(records.len() <= 256, "log chunk event count exceeds limit");
    ensure!(
        records.windows(2).all(|r| r[0].0 < r[1].0),
        "log chunk event order invalid"
    );
    Ok(records)
}
pub(crate) fn hydrate(c: &rusqlite::Connection, run: &str, events: &mut [LogEvent]) -> Result<()> {
    let mut loaded = BTreeMap::<i64, BTreeMap<i64, String>>::new();
    for event in events {
        let chunk: Option<i64> = c.query_row(
            "SELECT chunk_id FROM log_events WHERE id=?1 AND run_id=?2",
            params![event.id, run],
            |r| r.get(0),
        )?;
        if let Some(id) = chunk {
            if let std::collections::btree_map::Entry::Vacant(entry) = loaded.entry(id) {
                let (raw,sha,codec,payload):(usize,String,String,Vec<u8>)=c.query_row("SELECT raw_bytes,sha256,codec,payload FROM log_chunks WHERE id=?1 AND run_id=?2 AND raw_bytes BETWEEN 0 AND ?3 AND length(payload)<=?3",params![id,run,PAYLOAD_BYTES],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                ensure!(codec == "zstd-json-v1", "unsupported log codec");
                entry.insert(decode(&payload, raw, &sha)?.into_iter().collect());
            }
            event.text = loaded[&id]
                .get(&event.id)
                .context("event missing from log chunk")?
                .clone();
        }
    }
    Ok(())
}
impl Db {
    pub fn logs_archive(&self, r: &Request) -> Result<Value> {
        ensure!(
            (1..=16).contains(&r.max_chunks),
            invalid("max_chunks must be 1..16")
        );
        for _ in 0..r.max_chunks {
            let mut c = self.connection()?;
            let tx = c.transaction()?;
            crate::evidence_expiry::available(
                &tx,
                &r.run_id,
                crate::evidence_expiry::Category::Logs,
            )?;
            ensure!(
                authorize(&tx, &r.project_id, &r.run_id)?,
                invalid("archive only completed runs")
            );
            let mut records = Vec::new();
            let mut bytes = 0;
            {
                let mut statement=tx.prepare("SELECT id,text FROM log_events WHERE run_id=?1 AND chunk_id IS NULL AND length(CAST(text AS BLOB))<=?2 ORDER BY id LIMIT 256")?;
                let candidates = statement.query_map(params![r.run_id, TEXT_BYTES], |v| {
                    Ok((v.get::<_, i64>(0)?, v.get::<_, String>(1)?))
                })?;
                for item in candidates {
                    let item = item?;
                    if bytes + item.1.len() > TEXT_BYTES {
                        break;
                    }
                    bytes += item.1.len();
                    records.push(item);
                }
            }
            if records.is_empty() {
                break;
            }
            let first = records[0].0;
            let last = records.last().unwrap().0;
            let raw = serde_json::to_vec(&records)?;
            ensure!(
                raw.len() <= PAYLOAD_BYTES,
                "log serialization exceeds limit"
            );
            let payload = zstd::stream::encode_all(raw.as_slice(), 3)?;
            let sha = crate::hashing::hex(Sha256::digest(&raw));
            // Verify the exact bytes before replacing their SQLite representation.
            ensure!(
                decode(&payload, raw.len(), &sha)? == records,
                "compressed log roundtrip failed"
            );
            tx.execute("INSERT INTO log_chunks(run_id,first_event,last_event,event_count,raw_bytes,text_bytes,sha256,codec,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,'zstd-json-v1',?8)",params![r.run_id,first,last,records.len(),raw.len(),bytes,sha,payload])?;
            let chunk = tx.last_insert_rowid();
            for (id, _) in &records {
                ensure!(tx.execute("UPDATE log_events SET text='',chunk_id=?2 WHERE id=?1 AND chunk_id IS NULL",params![id,chunk])?==1,"log event changed during archive");
            }
            tx.commit()?;
        }
        self.logs_storage(&Query {
            project_id: r.project_id.clone(),
            run_id: r.run_id.clone(),
        })
    }
    pub fn logs_storage(&self, r: &Query) -> Result<Value> {
        let c = self.connection()?;
        let terminal = authorize(&c, &r.project_id, &r.run_id)?;
        let (remaining,raw,oversized):(u64,u64,u64)=c.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(text AS BLOB))),0),COALESCE(SUM(length(CAST(text AS BLOB))>?2),0) FROM log_events WHERE run_id=?1 AND chunk_id IS NULL",params![r.run_id,TEXT_BYTES],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?)))?;
        let (chunks,archived,text,packed):(u64,u64,u64,u64)=c.query_row("SELECT COUNT(*),COALESCE(SUM(event_count),0),COALESCE(SUM(text_bytes),0),COALESCE(SUM(length(payload)),0) FROM log_chunks WHERE run_id=?1",[&r.run_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?,v.get(3)?)))?;
        Ok(
            json!({"project_id":r.project_id,"run_id":r.run_id,"codec":"zstd-json-v1","expired":crate::evidence_expiry::expired(&c,&r.run_id,crate::evidence_expiry::Category::Logs)?,"terminal":terminal,"chunks":chunks,"archived_events":archived,"unarchived_events":remaining,"oversized_events_retained_raw":oversized,"original_text_bytes":raw+text,"stored_payload_bytes":raw+packed,"complete":remaining==0,"more":remaining>oversized,"accounting":"payload bytes only; SQLite pages and redacted FTS index are excluded; archiving does not imply file shrinkage"}),
        )
    }
}
pub async fn archive(e: Arc<Engine>, r: Request) -> Result<Value> {
    tokio::task::spawn_blocking(move || e.db.logs_archive(&r)).await?
}
pub async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
    tick.tick().await;
    loop {
        tokio::select! {biased;_=stop.cancelled()=>break,_=tick.tick()=>{}}
        let engine = e.clone();
        let work=tokio::task::spawn_blocking(move||->Result<()>{
            let candidate:Option<(String,String)>=engine.db.connection()?.query_row("SELECT r.project_id,r.id FROM runs r WHERE r.finished_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM evidence_expiry x WHERE x.run_id=r.id AND x.category='logs') AND EXISTS(SELECT 1 FROM log_events l WHERE l.run_id=r.id AND l.chunk_id IS NULL AND length(CAST(l.text AS BLOB))<=?1) ORDER BY r.id LIMIT 1",[TEXT_BYTES],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            if let Some((project_id,run_id))=candidate{engine.db.logs_archive(&Request{project_id,run_id,max_chunks:4})?;}
            Ok(())
        }).await;
        match work {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("log archive: {error:#}"),
            Err(error) => eprintln!("log archive task: {error}"),
        }
    }
}
