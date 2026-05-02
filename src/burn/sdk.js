// relayburn/sdk stub.
//
// The real `relayburn/sdk` is blocked on AgentWorkforce/burn#218. This local stub provides
// the same surface (ingest, summary, Ledger) so the rest of the relaywash codebase can be
// written against the final API. When the real SDK is published we drop this in favour of
// the npm package without changing any callers.
//
// Storage format: one JSONL file per session under `${ledgerHome}/sessions/<sessionId>.jsonl`.
// Each line is one event: { ts, kind, tool?, tokensIn?, tokensOut?, replaces?, collapsedCalls? }.

import { mkdirSync, appendFileSync, readFileSync, existsSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const DEFAULT_HOME = join(homedir(), '.relayburn');

function home(opts) {
  return opts?.ledgerHome || process.env.RELAYBURN_HOME || DEFAULT_HOME;
}

function sessionFile(ledgerHome, sessionId) {
  return join(ledgerHome, 'sessions', `${sessionId}.jsonl`);
}

export class Ledger {
  constructor(opts = {}) {
    this.ledgerHome = home(opts);
    mkdirSync(join(this.ledgerHome, 'sessions'), { recursive: true });
  }

  /** Append a tool-use event for `sessionId`. */
  recordToolUse(sessionId, event) {
    mkdirSync(join(this.ledgerHome, 'sessions'), { recursive: true });
    const line = JSON.stringify({ ts: Date.now(), kind: 'tool_use', ...event }) + '\n';
    appendFileSync(sessionFile(this.ledgerHome, sessionId), line);
  }

  /** Read raw events for one session. */
  readSession(sessionId) {
    const f = sessionFile(this.ledgerHome, sessionId);
    if (!existsSync(f)) return [];
    return readFileSync(f, 'utf8')
      .split('\n')
      .filter(Boolean)
      .map((l) => JSON.parse(l));
  }
}

/**
 * Stop-hook entry: ingest a session into the ledger. With no transcript the ledger only
 * has whatever the server appended during the session — that's fine; later we'll parse the
 * transcript and reconstruct full attribution here.
 */
export async function ingest({ sessionId, transcriptPath, ledgerHome } = {}) {
  const dir = ledgerHome || home();
  mkdirSync(join(dir, 'sessions'), { recursive: true });
  const marker = sessionFile(dir, sessionId || 'unknown');
  appendFileSync(
    marker,
    JSON.stringify({ ts: Date.now(), kind: 'session_end', transcriptPath: transcriptPath || null }) +
      '\n',
  );
  return { ok: true, sessionId, ledgerHome: dir };
}

/**
 * Read-side: summarize a session.
 * Returns per-tool counts and a `savings` estimate based on `_meta.collapsedCalls`.
 */
export async function summary({ session, ledgerHome } = {}) {
  const dir = ledgerHome || home();
  if (!session) {
    // Aggregate across all sessions if no session id given.
    const sessionsDir = join(dir, 'sessions');
    if (!existsSync(sessionsDir)) return emptySummary();
    const all = readdirSync(sessionsDir)
      .filter((f) => f.endsWith('.jsonl'))
      .flatMap((f) => readJsonl(join(sessionsDir, f)));
    return aggregate(all);
  }
  const events = readJsonl(sessionFile(dir, session));
  return aggregate(events);
}

function readJsonl(path) {
  if (!existsSync(path)) return [];
  return readFileSync(path, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

function emptySummary() {
  return { byTool: {}, totalCalls: 0, collapsedCalls: 0, replacedTools: [] };
}

function aggregate(events) {
  const byTool = {};
  let totalCalls = 0;
  let collapsedCalls = 0;
  const replacedSet = new Set();
  for (const ev of events) {
    if (ev.kind !== 'tool_use') continue;
    const tool = ev.tool || 'unknown';
    if (!byTool[tool]) byTool[tool] = { calls: 0, collapsedCalls: 0 };
    byTool[tool].calls++;
    totalCalls++;
    if (typeof ev.collapsedCalls === 'number') {
      byTool[tool].collapsedCalls += ev.collapsedCalls;
      collapsedCalls += ev.collapsedCalls;
    }
    if (Array.isArray(ev.replaces)) {
      for (const r of ev.replaces) replacedSet.add(r);
    }
  }
  return {
    byTool,
    totalCalls,
    collapsedCalls,
    replacedTools: Array.from(replacedSet).sort(),
  };
}
