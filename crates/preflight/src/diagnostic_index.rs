//! Durable incremental derived index. Source logs and their evidence identities are unchanged.
use crate::{
    diagnostics::{Diagnostic, FORMAT, GROUP_LIMIT, Group, LINE_LIMIT, Query, Scan, Stream},
    engine::Engine,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::{collections::BTreeMap, sync::Arc};
pub(crate) fn advance(
    e: &Engine,
    id: &str,
    max_bytes: usize,
    max_events: usize,
    terminal: bool,
    root: &str,
    workspace: &str,
) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    crate::evidence_expiry::available(&tx, id, crate::evidence_expiry::Category::Logs)?;
    let previous:Option<(String,i64,i64,usize,usize,String,usize)>=tx.query_row("SELECT format,event_id,byte_offset,events,bytes,state_json,omitted FROM diagnostic_scans WHERE run_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let (mut event, mut offset, old_events, old_bytes, state, omitted) = match previous {
        Some((format, a, b, c, d, state, f)) if format == FORMAT => (a, b, c, d, state, f),
        previous => {
            if previous.is_some() {
                tx.execute("DELETE FROM diagnostics WHERE run_id=?1", [id])?;
            }
            (0, -1, 0, 0, "[]".into(), 0)
        }
    };
    let states: Vec<(String, String, Stream)> = serde_json::from_str(&state)?;
    let mut streams = states
        .into_iter()
        .map(|(stage, stream, state)| ((stage, stream), state))
        .collect::<BTreeMap<_, _>>();
    let mut result = Scan {
        omitted_lines: omitted,
        ..Scan::default()
    };
    let mut drained = true;
    let mut lines = 0usize;
    {
        let mut statement=tx.prepare("SELECT l.id,l.stage_id,l.stream,substr(CAST(f.text AS BLOB),CASE WHEN l.id=?2 THEN MAX(?3,0)+1 ELSE 1 END,?4) FROM log_events l LEFT JOIN log_search f ON f.rowid=l.id WHERE l.run_id=?1 AND (l.id>?2 OR (l.id=?2 AND ?3>=0)) ORDER BY l.id LIMIT ?5")?;
        let mut rows =
            statement.query(params![id, event, offset, max_bytes + 1, max_events + 1])?;
        while let Some(row) = rows.next()? {
            if result.events >= max_events || result.bytes >= max_bytes || lines >= 10000 {
                drained = false;
                break;
            }
            let current: i64 = row.get(0)?;
            let stage: String = row.get(1)?;
            let stream: String = row.get(2)?;
            let key = (stage.clone(), stream);
            let data: Option<Vec<u8>> = row.get(3)?;
            let Some(data) = data else {
                streams.remove(&key);
                result.omitted_lines += 1;
                result.events += 1;
                event = current;
                offset = -1;
                continue;
            };
            if !streams.contains_key(&key) && streams.len() >= 256 {
                result.omitted_lines += 1;
                result.events += 1;
                event = current;
                offset = -1;
                continue;
            }
            let buffer = streams.entry(key).or_default();
            let start_offset = if current == event {
                offset.max(0) as usize
            } else {
                0
            };
            let budget = (max_bytes - result.bytes).min(data.len());
            let mut end = budget;
            while std::str::from_utf8(&data[..end]).is_err() && end > budget.saturating_sub(4) {
                end -= 1;
            }
            let text = std::str::from_utf8(&data[..end])?;
            let mut consumed = 0;
            for part in text.split_inclusive('\n') {
                if lines >= 10000 {
                    break;
                }
                if buffer.tail.is_empty() {
                    buffer.start = current;
                }
                if buffer.ids.len() < 16 && !buffer.ids.contains(&current) {
                    buffer.ids.push(current);
                }
                if buffer.tail.len() + part.len() > LINE_LIMIT {
                    buffer.tail.clear();
                    buffer.overflow = true;
                }
                if !buffer.overflow {
                    buffer.tail.push_str(part);
                }
                consumed += part.len();
                if part.ends_with('\n') {
                    lines += 1;
                    if buffer.overflow {
                        result.omitted_lines += 1;
                    } else {
                        let line = std::mem::take(&mut buffer.tail);
                        let ids = std::mem::take(&mut buffer.ids);
                        result.line(&stage, buffer, &line, &ids, root, workspace);
                    }
                    buffer.tail.clear();
                    buffer.ids.clear();
                    buffer.overflow = false;
                }
            }
            result.bytes += consumed;
            event = current;
            if consumed < data.len() {
                offset = (start_offset + consumed) as i64;
                drained = false;
                break;
            }
            offset = -1;
            result.events += 1;
        }
    }
    if drained && terminal {
        for ((stage, _), buffer) in &mut streams {
            if buffer.overflow {
                result.omitted_lines += 1;
                buffer.overflow = false;
            }
            if !buffer.tail.is_empty() {
                let line = std::mem::take(&mut buffer.tail);
                let ids = std::mem::take(&mut buffer.ids);
                result.line(stage, buffer, &line, &ids, root, workspace);
            }
            if let Some(d) = buffer.pending.take() {
                result.emit(stage, d, root, workspace);
            }
        }
    }
    // Bound private continuation state as well as each input slice.
    let buffered: usize = streams
        .values()
        .map(|s| s.tail.len() + s.pending.as_ref().map_or(0, |d| d.message.len()))
        .sum();
    if buffered > 1024 * 1024 {
        for buffer in streams.values_mut() {
            buffer.tail.clear();
            buffer.pending = None;
            buffer.overflow = true;
        }
        result.omitted_lines += 1;
    }
    for buffer in streams.values_mut() {
        buffer.tail = crate::log_search::redact(&buffer.tail);
    }
    for (key, group) in result.groups {
        let old: Option<(i64, String)> = tx
            .query_row(
                "SELECT id,evidence_json FROM diagnostics WHERE run_id=?1 AND fingerprint=?2",
                params![id, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((row, encoded)) = old {
            let mut evidence: Vec<i64> = serde_json::from_str(&encoded)?;
            for item in group.evidence {
                if evidence.len() < 4 && !evidence.contains(&item) {
                    evidence.push(item);
                }
            }
            tx.execute(
                "UPDATE diagnostics SET occurrences=occurrences+?2,evidence_json=?3 WHERE id=?1",
                params![row, group.count, serde_json::to_string(&evidence)?],
            )?;
        } else {
            let count: usize = tx.query_row(
                "SELECT count(*) FROM diagnostics WHERE run_id=?1",
                [id],
                |r| r.get(0),
            )?;
            if count >= GROUP_LIMIT {
                result.omitted_lines += 1;
                continue;
            }
            let d = group.sample;
            tx.execute("INSERT INTO diagnostics(run_id,fingerprint,stage_id,severity,code,file,line,column_number,message,adapter,occurrences,evidence_json,normalized_message,details_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![id,key,group.stage,d.severity,d.code,d.file,d.line,d.column,d.message,d.adapter,group.count,serde_json::to_string(&group.evidence)?,crate::diagnostics::normalized_message(&d.message),serde_json::to_string(&d.details)?])?;
        }
    }
    for (fingerprint, events) in result.occurrences {
        for event in events {
            tx.execute("INSERT OR IGNORE INTO diagnostic_evidence(diagnostic_id,event_id) SELECT id,?3 FROM diagnostics WHERE run_id=?1 AND fingerprint=?2",params![id,fingerprint,event])?;
        }
    }
    let states = streams
        .into_iter()
        .map(|((stage, stream), state)| (stage, stream, state))
        .collect::<Vec<_>>();
    tx.execute("INSERT INTO diagnostic_scans(run_id,format,event_id,byte_offset,events,bytes,state_json,omitted,drained,updated_at,terminal_flushed) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(run_id) DO UPDATE SET format=excluded.format,event_id=excluded.event_id,byte_offset=excluded.byte_offset,events=excluded.events,bytes=excluded.bytes,state_json=excluded.state_json,omitted=excluded.omitted,drained=excluded.drained,updated_at=excluded.updated_at,terminal_flushed=excluded.terminal_flushed",params![id,FORMAT,event,offset,old_events+result.events,old_bytes+result.bytes,serde_json::to_string(&states)?,result.omitted_lines,drained,crate::db::now(),terminal&&drained])?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn snapshot(e: &Engine, q: &Query, id: &str, terminal: bool) -> Result<Scan> {
    let c = e.db.connection()?;
    crate::evidence_expiry::available(&c, id, crate::evidence_expiry::Category::Logs)?;
    let mut result:Scan=c.query_row("SELECT events,bytes,omitted,event_id,drained AND byte_offset=-1 AND NOT EXISTS(SELECT 1 FROM log_events WHERE run_id=?1 AND id>diagnostic_scans.event_id) FROM diagnostic_scans WHERE run_id=?1",[id],|r|Ok(Scan{events:r.get(0)?,bytes:r.get(1)?,omitted_lines:r.get(2)?,last_event:Some(r.get(3)?),complete:r.get::<_,bool>(4)?&&terminal,drained:r.get(4)?,..Scan::default()}))?;
    result.complete &= result.omitted_lines == 0;
    let terms = q
        .query
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let predicate = if terms.is_empty() {
        "?5=''"
    } else {
        "diagnostics_search MATCH ?5"
    };
    let sql = format!(
        "SELECT d.fingerprint,d.stage_id,d.severity,d.code,d.file,d.line,d.column_number,d.message,d.adapter,d.occurrences,d.evidence_json,d.details_json FROM diagnostics d JOIN diagnostics_search ON diagnostics_search.rowid=d.id WHERE d.run_id=?1 AND (?2 IS NULL OR d.stage_id=?2) AND (?3 IS NULL OR d.severity=?3) AND (?4 IS NULL OR d.file=?4) AND {predicate} ORDER BY d.fingerprint"
    );
    let mut statement = c.prepare(&sql)?;
    let mut rows = statement.query(params![id, q.stage_id, q.severity, q.file, terms])?;
    while let Some(row) = rows.next()? {
        let evidence: Vec<i64> = serde_json::from_str(&row.get::<_, String>(10)?)?;
        result.groups.insert(
            row.get(0)?,
            Group {
                stage: row.get(1)?,
                sample: Diagnostic {
                    severity: row.get(2)?,
                    code: row.get(3)?,
                    file: row.get(4)?,
                    line: row.get(5)?,
                    column: row.get(6)?,
                    message: row.get(7)?,
                    adapter: row.get(8)?,
                    evidence: evidence.clone(),
                    details: serde_json::from_str(&row.get::<_, String>(11)?)?,
                },
                count: row.get(9)?,
                evidence,
            },
        );
    }
    Ok(result)
}
pub(crate) async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tokio::select! {biased;_=stop.cancelled()=>break,_=timer.tick()=>{}}
        let engine = e.clone();
        let outcome=tokio::task::spawn_blocking(move||->Result<()>{
   let ids={let c=engine.db.connection()?;c.prepare("SELECT r.id FROM runs r LEFT JOIN diagnostic_scans s ON s.run_id=r.id WHERE NOT EXISTS(SELECT 1 FROM evidence_expiry x WHERE x.run_id=r.id AND x.category='logs') AND (s.format!=?1 OR EXISTS(SELECT 1 FROM log_events l WHERE l.run_id=r.id AND (l.id>COALESCE(s.event_id,0) OR (l.id=s.event_id AND s.byte_offset>=0))) OR (r.finished_at IS NOT NULL AND s.terminal_flushed=0)) ORDER BY r.finished_at IS NOT NULL,COALESCE(s.updated_at,0),r.id LIMIT 4")?.query_map([FORMAT],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?};
   for id in ids {if engine.is_stopping(){break;}let run=engine.db.run(&id)?;let project=engine.db.project(&run.project_id)?;advance(&engine,&id,262144,1024,run.status.terminal(),&project.root.to_string_lossy(),&run.workspace.to_string_lossy())?;}
   Ok(())
  }).await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("diagnostic indexing: {error}"),
            Err(error) => eprintln!("diagnostic indexing task: {error}"),
        }
    }
}

pub(crate) fn compact(e: &Engine, id: &str, terminal: bool) -> Result<serde_json::Value> {
    let c = e.db.connection()?;
    if crate::evidence_expiry::expired(&c, id, crate::evidence_expiry::Category::Logs)? {
        return Ok(
            serde_json::json!({"indexed":false,"complete":false,"expired":true,"reason":"logs_expired"}),
        );
    }
    let progress:Option<(bool,usize)>=c.query_row("SELECT drained AND terminal_flushed AND byte_offset=-1 AND NOT EXISTS(SELECT 1 FROM log_events WHERE run_id=?1 AND id>diagnostic_scans.event_id),omitted FROM diagnostic_scans WHERE run_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((complete, omitted)) = progress else {
        return Ok(
            serde_json::json!({"indexed":false,"complete":false,"details_tool":"diagnostics_report","resource_uri":format!("preflight://runs/{id}/diagnostics")}),
        );
    };
    let counts = c
        .prepare(
            "SELECT severity,sum(occurrences) FROM diagnostics WHERE run_id=?1 GROUP BY severity",
        )?
        .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let top=c.prepare("SELECT fingerprint,stage_id,severity,code,message,evidence_json FROM diagnostics WHERE run_id=?1 ORDER BY severity,occurrences DESC,id LIMIT 3")?.query_map([id],|r|{let evidence:Vec<i64>=serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or_default();Ok(serde_json::json!({"fingerprint":r.get::<_,String>(0)?,"stage_id":r.get::<_,String>(1)?,"severity":r.get::<_,String>(2)?,"code":r.get::<_,Option<String>>(3)?,"summary":r.get::<_,String>(4)?.chars().take(256).collect::<String>(),"evidence_ids":evidence.into_iter().take(2).map(|id|format!("log:{id}")).collect::<Vec<_>>()}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        serde_json::json!({"indexed":true,"complete":complete&&terminal&&omitted==0,"observed_counts":counts,"top":top,"details_tool":"diagnostics_report","resource_uri":format!("preflight://runs/{id}/diagnostics")}),
    )
}
