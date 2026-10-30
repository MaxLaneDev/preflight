# Schedules and experiments (protocol 1.37)

For schedules, pin pipeline/profile revisions. Call schedules_preview with at_ms, optional repeat and count before saving calendar rules. repeat is either {every_seconds,overlap} or {cron,timezone,overlap}. Numeric cron has five fields; timezone is an explicit IANA name. Preview returns nominal UTC times; overlap/misfire are decided later.

Create once with schedules_create and an explicit request_id/actor/reason. Keep schedule_id and revision. Change with expected_revision and a new request_id; exact replay is safe. Pause/cancel/delete stop future dispatch, not active runs. schedules_history pages 20 occurrences; pass next_cursor as before_at. Keep each occurrence run_id for build_report. Deleted schedules retain audit/history and can be read by ID.

For experiments, save immutable definition revisions with experiment_save. Pin the pipeline, optional profile and baseline. Start explicit revision with an idempotency_key. Keep run_id; experiment_status/build_report inspect it, experiment_compare compares to the captured or explicit baseline. Test statuses are per run. Use experiment_cancel for its selected run. Run history is scoped by project and experiment_name.
