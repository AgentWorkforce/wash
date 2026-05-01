// Minimal MCP server over stdio. Implements the subset of the protocol that Claude Code
// exercises: `initialize`, `tools/list`, `tools/call`, plus standard JSON-RPC plumbing.
//
// We don't depend on @modelcontextprotocol/sdk to keep the bundle tiny. The wire format is
// LSP-style: each JSON-RPC message is framed by `Content-Length` / blank line / JSON body.

import { stdin, stdout, stderr } from 'node:process';

const PROTOCOL_VERSION = '2024-11-05';

/**
 * @typedef {{
 *   name: string;
 *   description: string;
 *   inputSchema: object;
 *   handler: (args: any, ctx: { sessionId?: string }) => Promise<any> | any;
 * }} Tool
 */

export class McpServer {
  /**
   * @param {{ name: string; version: string; tools: Tool[] }} opts
   */
  constructor(opts) {
    this.name = opts.name;
    this.version = opts.version;
    /** @type {Map<string, Tool>} */
    this.tools = new Map();
    for (const t of opts.tools) this.tools.set(t.name, t);
    /** Optional hook the harness may set; kept here to thread through to handlers. */
    this.sessionId = process.env.CLAUDE_SESSION_ID || undefined;
  }

  registerTool(tool) {
    this.tools.set(tool.name, tool);
  }

  /** Start reading from stdin. Resolves on EOF. */
  async run() {
    let buffer = Buffer.alloc(0);

    return new Promise((resolve) => {
      stdin.on('data', (chunk) => {
        buffer = Buffer.concat([buffer, chunk]);
        // Try to extract framed messages.
        while (true) {
          const headerEnd = buffer.indexOf('\r\n\r\n');
          if (headerEnd === -1) break;
          const header = buffer.slice(0, headerEnd).toString('utf8');
          const m = /Content-Length:\s*(\d+)/i.exec(header);
          if (!m) {
            // Malformed — drop everything up to header end and continue.
            buffer = buffer.slice(headerEnd + 4);
            continue;
          }
          const len = Number(m[1]);
          const start = headerEnd + 4;
          if (buffer.length < start + len) break;
          const body = buffer.slice(start, start + len).toString('utf8');
          buffer = buffer.slice(start + len);
          this._handleRaw(body).catch((err) => {
            stderr.write(`relaywash: handler error: ${err?.stack || err}\n`);
          });
        }
      });
      stdin.on('end', () => resolve(undefined));
      stdin.on('close', () => resolve(undefined));
    });
  }

  async _handleRaw(body) {
    let msg;
    try {
      msg = JSON.parse(body);
    } catch {
      return;
    }
    if (Array.isArray(msg)) {
      for (const m of msg) await this._handleOne(m);
      return;
    }
    await this._handleOne(msg);
  }

  async _handleOne(msg) {
    if (typeof msg !== 'object' || !msg) return;
    if (msg.method) {
      // Request or notification
      const id = msg.id;
      try {
        const result = await this._dispatch(msg.method, msg.params || {});
        if (id !== undefined && id !== null) {
          this._send({ jsonrpc: '2.0', id, result });
        }
      } catch (err) {
        if (id !== undefined && id !== null) {
          this._send({
            jsonrpc: '2.0',
            id,
            error: { code: -32000, message: err?.message || String(err) },
          });
        }
      }
    }
    // Responses to server-initiated requests are ignored (we don't initiate).
  }

  async _dispatch(method, params) {
    switch (method) {
      case 'initialize':
        return {
          protocolVersion: PROTOCOL_VERSION,
          serverInfo: { name: this.name, version: this.version },
          capabilities: { tools: {} },
        };
      case 'initialized':
      case 'notifications/initialized':
        return undefined;
      case 'tools/list':
        return {
          tools: Array.from(this.tools.values()).map((t) => ({
            name: t.name,
            description: t.description,
            inputSchema: t.inputSchema,
          })),
        };
      case 'tools/call': {
        const tool = this.tools.get(params.name);
        if (!tool) throw new Error(`Unknown tool: ${params.name}`);
        const args = params.arguments || {};
        const ctx = { sessionId: params._sessionId || this.sessionId };
        const result = await tool.handler(args, ctx);
        // MCP `tools/call` shape: { content: [{ type: 'text', text }], isError?, structuredContent? }
        return formatToolResult(result);
      }
      case 'ping':
        return {};
      case 'shutdown':
      case 'exit':
        process.exit(0);
      default:
        throw new Error(`Method not implemented: ${method}`);
    }
  }

  _send(payload) {
    const body = JSON.stringify(payload);
    const buf = Buffer.from(body, 'utf8');
    stdout.write(`Content-Length: ${buf.length}\r\n\r\n`);
    stdout.write(buf);
  }
}

function formatToolResult(result) {
  // Tools return their own structured object. We emit it as a single text block of JSON
  // so models see the data, plus mirror it under structuredContent for hosts that read it.
  if (result && result.__rawText) {
    return { content: [{ type: 'text', text: result.__rawText }] };
  }
  const text = typeof result === 'string' ? result : JSON.stringify(result, null, 2);
  return {
    content: [{ type: 'text', text }],
    structuredContent: typeof result === 'object' ? result : undefined,
  };
}
