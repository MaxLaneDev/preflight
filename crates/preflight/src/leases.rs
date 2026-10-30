//! Durable local operation protection under the engine's exclusive server lock.
use crate::{db::Db, pipeline::StageDefinition};
use anyhow::Result;
use std::collections::BTreeSet;

pub struct Lease<'a> {
    pub(crate) db: &'a Db,
    pub(crate) id: String,
    pub(crate) release_on_drop: bool,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        // A failed release remains active (fail closed) until exclusive-owner recovery.
        if !self.release_on_drop {
            return;
        }
        if let Err(error) = self.db.release_operation(&self.id) {
            eprintln!("operation lease {} release failed: {error}", self.id);
        }
    }
}
impl Lease<'_> {
    pub(crate) fn abandon(mut self) -> Result<()> {
        self.release_on_drop = false;
        self.db.connection()?.execute(
            "UPDATE operation_leases SET state='abandoned' WHERE id=?1 AND state='active'",
            [&self.id],
        )?;
        Ok(())
    }
}
pub fn stage_resources(stage: &StageDefinition) -> Result<BTreeSet<String>> {
    let mut resources = BTreeSet::new();
    for value in std::iter::once(&stage.command.program)
        .chain(&stage.command.args)
        .chain(stage.inputs.values())
    {
        if let Some(digest) = crate::modules::reference(value)? {
            resources.insert(format!("module:{digest}"));
        }
    }
    Ok(resources)
}
