# Compaction-aware attribution

Issue: AgentWorkforce/wash#11. Question: can `PreCompact`/`PostCompact` hooks expose enough
state for relaywash to attribute "tokens that survived compaction" to the specific tools
that produced them?

## Findings

### What `PreCompact` and `PostCompact` payloads contain

Claude Code emits both events to hook scripts via stdin as JSON. The shape (as of harness
2024-11-05–era plugins) is:

```jsonc
// PreCompact stdin
{
  "session_id": "string",
  "transcript_path": "string",         // path to JSONL transcript on disk
  "trigger": "manual" | "auto",
  "custom_instructions": "string|null",
  "hook_event_name": "PreCompact"
}

// PostCompact stdin
{
  "session_id": "string",
  "transcript_path": "string",         // **same path**, now contains the compacted transcript
  "trigger": "manual" | "auto",
  "hook_event_name": "PostCompact"
}
```

Crucially, **neither payload includes the transcript inline** — both reference the
on-disk file at `transcript_path`. The harness mutates this file in place when compaction
runs: `PreCompact` fires before the rewrite, `PostCompact` fires after.

This means:

- A `PreCompact` hook *can* read the transcript before it's been rewritten and snapshot it.
- A `PostCompact` hook can read the post-compaction transcript and diff it against the
  snapshot to compute survival per turn / per tool call.
- Token counts, per-message survival flags, and per-tool-call survival are *not* directly
  exposed by the harness — we have to reconstruct them by parsing the transcript.

### Per-tool-call survival is recoverable, with caveats

Each transcript entry is a JSONL line keyed by `uuid` and has a stable `parentUuid`. Tool
calls and tool results land as their own messages with `type: "tool_use"` /
`type: "tool_result"` content blocks. The compactor either:

1. **Drops** a message entirely (survives = false), or
2. **Summarises** it into a synthetic system message (survives = false for the original,
   but a derived stub remains).

By taking the set of `uuid`s in the pre-compaction transcript and intersecting with the
post-compaction transcript, we get exact per-message survival. From there:

- A `tool_use` block on a surviving message → that tool call's output survived.
- A surviving `tool_result` block whose `tool_use_id` matches a relaywash tool → we can
  attribute the surviving tokens to that tool.

For relaywash specifically, the `_meta` annotation we emit on every result is preserved as
part of the message content, so the annotation reader (AgentWorkforce/burn#219) keeps
working: surviving relaywash calls are still attributed; collapsed-call savings still count.

The caveats:

- **Token counts are estimates.** The harness doesn't write per-message token counts to the
  transcript. We have to estimate via a tokenizer (cl100k or Anthropic's). That's fine for
  relative attribution — survival ratio per tool — but not for billing-grade absolutes.
- **Summarised messages** lose their tool-result framing. We attribute their survival to a
  synthetic "compacted-summary" pseudo-tool rather than back to the original tool.

### Recommendation

**Land the snapshot-and-diff approach.** Both hooks have what we need; the harness's
file-on-disk model is actually convenient because we can do the work asynchronously without
blocking the event loop.

Schema for `relayburn/sdk` to ingest:

```ts
interface CompactionEvent {
  sessionId: string;
  trigger: 'manual' | 'auto';
  preTranscriptUuids: string[];        // every uuid present pre-compaction
  postTranscriptUuids: string[];       // every uuid present post-compaction
  perToolSurvival: Record<string, {
    callsBefore: number;
    callsAfter: number;
    estimatedTokensBefore: number;
    estimatedTokensAfter: number;
  }>;
  syntheticSummaries: number;          // count of compactor-generated summary messages
}
```

`burn` can reduce that to "tool X's outputs are surviving compaction at rate Y" and surface
it in `summary --by-tool`.

### Implementation plan

1. Add a `PreCompact` hook (`scripts/precompact-snapshot-hook.js`) that copies the
   transcript file into `${RELAYBURN_HOME}/compaction/${session}-pre.jsonl`.
2. Add a `PostCompact` hook (`scripts/postcompact-diff-hook.js`) that:
   - Reads the snapshot and the current transcript.
   - Computes the per-tool survival object above.
   - Appends it to `${RELAYBURN_HOME}/sessions/${session}.jsonl` as a `kind: 'compaction'`
     event (so the existing `summary()` reader can ignore or surface it).
3. Burn-side: file a follow-up issue (AgentWorkforce/burn) for the ingester to recognise
   `kind: 'compaction'` events and roll them into per-tool survival stats. *Linked below.*

### Status

- **Feasibility:** confirmed — `PreCompact`/`PostCompact` payloads are sufficient.
- **Blocker:** none. We can implement entirely in relaywash.
- **Follow-up:** when the wash hooks ship, file `AgentWorkforce/burn` issue: "ingest
  `kind: 'compaction'` events from relaywash for per-tool survival reporting".

### Notes

- Do **not** try to "own" compaction by intercepting and replacing it via hooks. The
  signal-to-noise ratio of the harness's own compactor is fine for our purposes; reinventing
  it is out of scope and would couple us to harness internals.
- This investigation does not block any other phase. Implementation can land alongside
  Phase 6 work or as a small follow-up PR.
