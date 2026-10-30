# Change queues (protocol 1.13)

queue_create requires project_id, name, request_id, mode, definition, changes.
Each change is {id,source:{commit,patches?}}; a patch is {name,diff}.
Use full lowercase Git object IDs. The repository must already be registered locally.
Profile selection is optional {name,revision}; revision is explicit. The server freezes
its content and applied definition at creation. request_id and change IDs use 1..64
ASCII letters/digits/_/-. Maximum: 32 changes, 64 KiB request; each resolved source
supports up to 16 patches/32 KiB patch text.

- independent: each change starts from its own commit plus its own patches.
  max_parallel defaults to 1; raise it to run independent changes concurrently (up to 32).
  An item failure does not cancel other independent items.
- cumulative: all changes share the same base commit. Each step adds its patches to
  the prior patch stack. max_parallel must be 1. Failure/cancellation blocks later items.
  This does not edit the user's checkout or author new code automatically.

Creation returns only {queue_id}. Retry exactly the same request_id and body to recover
that ID; a changed request needs a new key. Queue definitions cannot be edited after
creation. Git object availability is checked at execution; a missing object fails the
item. Mutable working-tree/non-Git snapshots are not queue sources yet.

queue_status({queue_id}) returns state, counts and items {i,id,state,run_id?,error?}.
States: queue active/cancelling/finished/cancelled; item
pending/running/succeeded/failed/cancelled/blocked. finished means all items are terminal,
not that all succeeded. Add details:true only for source commit/patch hashes; no raw diffs
are echoed. Fetch build_report({run_id}) only for the items needing investigation.

queues_list({project_id,limit?,before?}) defaults to 20, at most 100. Continue with
before=next_cursor. queue_cancel({queue_id,actor,reason}) stops pending and active items;
actor is user or agent. Cancellation is asynchronous; history remains. pipeline_cancel
can still cancel one run independently. Queue work resumes after server restart;
interrupted runs keep their terminal recovery result and are not silently rerun.
