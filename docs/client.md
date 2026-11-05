# Web UI

Open `http://127.0.0.1:4170`. The server includes the interface and MCP.

| Want to… | Open |
| --- | --- |
| Edit or run a configuration | Project → Pipelines |
| Inspect one execution | Pipeline → Run |
| Open a test's output or source | Run → Tests → test |
| Inspect dependencies | Run → Graph |
| Read changing output | Run → Logs |
| Compare captured code | Run → Changes |
| Copy workspace/input/output paths | Run → Paths |
| Compare alternatives | Project → Experiments |
| See all current work | Execution |
| Configure the server | Gear icon → Settings |

Use the theme buttons in the header. Copy buttons appear on hover/focus. Lists load
as you scroll; panels scroll independently. Hidden pages release live subscriptions.

## Pipeline graph

- Each stage occupies one column after its deepest dependency. Independent
  roots share the first column.
- Branches reserve separate vertical lanes. A parent sits between its outermost
  child lanes. A shared child is placed once, under a deepest parent, with
  links from all its dependencies.
- When a long link would compete with a same-row parent, the graph can move the
  shared child toward the earlier ancestor and the parent in the opposite
  direction. Extra row spacing is limited to that part of the graph.
- Layout optimization uses separate bounded passes: avoid cards (including a
  clearance margin), reduce connector crossings, then shorten connections.
  Neighboring cards may swap vertically; shared entry and exit points do not
  count as crossings. All connectors use centered ports and the same cubic curve.
- Outer branches may be balanced by moving their parent while intermediate child links may
  grow. Other links and crossing counts must not increase.
- This is a local search, not a proof of a globally minimal drawing.

## Compatibility client

For a separate UI listener against an existing server:

```sh
preflight-client --server http://127.0.0.1:4170 --bind 127.0.0.1:4172
```

This is a compatibility tool; normal installation needs only the server.

The bell in the bottom bar opens recent notifications; **View all** opens the journal.
Toasts disappear after 5 seconds (errors: 10), but stay in history until its 200-entry
limit is reached. Hover/focus pauses expiry. History and unread state are local to this
browser and server origin; they survive reload and synchronize across tabs. Copy
confirmations stay transient. Closing a toast does not mark its history entry read.

Notification history loads as you scroll in both the compact panel and full history.
It is stored in this browser; clearing browser data removes that history.
Pipeline lists show the most recently completed pipeline first. Finished queue
entries use completion time. Use the console arrow buttons to jump to either end.

Single-child chains align their ports exactly where clearance allows. After all
placement and balancing passes, column compaction removes spare gaps only when
it does not add connector/card or connector/connector intersections.
