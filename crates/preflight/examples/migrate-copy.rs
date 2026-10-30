//! Migration-only acceptance helper. Never starts an Engine or background jobs.
use anyhow::{Result, ensure};
use std::path::Path;
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    ensure!(
        args.len() == 2,
        "usage: migrate-copy /absolute/migration-check.sqlite3"
    );
    let path = Path::new(&args[1]);
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .is_some_and(|n| n == "migration-check.sqlite3"),
        "explicit acceptance-copy path required"
    );
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "existing regular database copy required"
    );
    let db = preflight::db::open(path)?;
    let version: usize = db.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
        row.get(0)
    })?;
    ensure!(
        version == preflight::migrations::VERSION,
        "migration version mismatch"
    );
    println!(
        "{}",
        serde_json::json!({"schema":version,"engine_started":false})
    );
    Ok(())
}
