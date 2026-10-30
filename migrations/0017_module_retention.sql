CREATE TABLE module_pins (
 project_id TEXT NOT NULL,
 sha256 TEXT NOT NULL,
 owner TEXT NOT NULL,
 pipeline_id TEXT,
 pinned INTEGER NOT NULL CHECK(pinned IN (0,1)),
 revision INTEGER NOT NULL CHECK(revision>0),
 actor TEXT NOT NULL,
 reason TEXT NOT NULL,
 PRIMARY KEY(project_id,sha256,owner),
 FOREIGN KEY(project_id,sha256) REFERENCES modules(project_id,sha256)
);
CREATE TABLE module_pin_changes (
 project_id TEXT NOT NULL REFERENCES projects(id),
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 result_json TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(project_id,request_id)
);
CREATE VIEW module_retention_references AS
 SELECT project_id,sha256,'publication' AS kind,sha256 AS owner_id,NULL AS run_id,NULL AS stage_id,NULL AS pipeline_id,'retained' AS state FROM modules
 UNION ALL
 SELECT project_id,sha256,'pin',owner,NULL,NULL,pipeline_id,'pinned' FROM module_pins WHERE pinned=1
 UNION ALL
 SELECT m.project_id,m.sha256,'producer',m.contract_sha256,json_extract(m.artifact_json,'$.run_id'),json_extract(m.artifact_json,'$.stage_id'),NULL,'retained' FROM module_contracts m
 UNION ALL
 SELECT r.project_id,json_extract(i.artifact_json,'$.sha256'),'consumed_input',i.path,i.run_id,i.stage_id,json_extract(s.definition_json,'$.cache.pipeline_id'),'retained' FROM module_inputs i JOIN runs r ON r.id=i.run_id JOIN stage_runs s ON s.run_id=i.run_id AND s.name=i.stage_id
 UNION ALL
 SELECT r.project_id,a.sha256,'run_artifact',a.name,a.run_id,a.stage_id,json_extract(s.definition_json,'$.cache.pipeline_id'),'retained' FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN stage_runs s ON s.run_id=a.run_id AND s.name=a.stage_id
 UNION ALL
 SELECT c.project_id,a.sha256,'cache_entry',c.id||':'||a.name,c.producer_run,c.producer_stage,c.pipeline_id,c.state FROM stage_cache c JOIN artifacts a ON a.run_id=c.producer_run AND a.stage_id=c.producer_stage WHERE c.state='active'
 UNION ALL
 SELECT r.project_id,substr(o.resource,8),'operation',l.id,l.run_id,NULL,NULL,l.state FROM operation_resources o JOIN operation_leases l ON l.id=o.lease_id JOIN runs r ON r.id=l.run_id WHERE o.resource LIKE 'module:%' AND l.state IN ('active','abandoned');
INSERT INTO schema_version VALUES (17);
