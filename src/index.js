// relaywash MCP server entry. Registers all tools and starts the stdio server.

import { McpServer } from './mcp/server.js';
import { Ledger } from './burn/sdk.js';
import { recordCall } from './measure.js';

import { pingTool } from './tools/ping.js';
import { searchTool } from './tools/search.js';
import { editTool } from './tools/edit.js';
import { readTool, noteSearchedSymbol } from './tools/read.js';
import { gitStateTool } from './tools/git-state.js';
import { testRunTool } from './tools/test-run.js';
import { buildTool } from './tools/build.js';
import { ghPrTool } from './tools/gh-pr.js';

const ledger = new Ledger({});

// Tools list, in the order we expose them to Claude Code.
const tools = [pingTool, searchTool, editTool, readTool, gitStateTool, testRunTool, buildTool, ghPrTool];

// Wrap each handler to (a) emit a tool_use event into the burn ledger and (b) optionally
// record a shadow comparison for `--measure` mode.
const wrapped = tools.map((t) => ({
  ...t,
  handler: async (args, ctx) => {
    // Side-effect: keep the Read tool's session-state aware of the latest Search query.
    if (t.name === 'relaywash__Search' && args && (args.symbol || args.content)) {
      noteSearchedSymbol(args.symbol || args.content);
    }
    const result = await t.handler(args, ctx);
    try {
      const replaces = result?._meta?.replaces || [];
      const collapsedCalls = result?._meta?.collapsedCalls || 0;
      ledger.recordToolUse(ctx.sessionId || 'default', {
        tool: t.name,
        replaces,
        collapsedCalls,
      });
      recordCall(t.name, args, result);
    } catch {}
    return result;
  },
}));

const server = new McpServer({ name: 'relaywash', version: '0.1.0', tools: wrapped });
server.run().catch((err) => {
  process.stderr.write(`relaywash: fatal: ${err?.stack || err}\n`);
  process.exit(1);
});
