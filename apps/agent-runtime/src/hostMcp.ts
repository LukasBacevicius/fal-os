// Minimal MCP client (Streamable HTTP, JSON-RPC) for the fal-os host's canvas tools, used by the
// runtime's own tools (placeholders, imports, presence) on behalf of an agent's session.
import { config } from "./config.ts";

/** The host dropped our MCP session (idle expiry, host restart): reconnect and retry. */
const SESSION_GONE = /\b404\b|session not found|unknown session/i;

export type HostTool = { name: string; inputSchema?: unknown };

export class HostMcp {
  private session: string | null = null;
  private nextId = 1;
  private init: Promise<void> | null = null;

  constructor(
    private url = `${config.hostUrl}/mcp`,
    private token = config.token,
    /** Extra headers: `X-Falos-Chat` (sessions made here join the chat), `X-Falos-Project`. */
    private headers: Record<string, string> = {},
  ) {}

  private async post(body: unknown, session = this.session): Promise<{ status: number; json: any; text: string }> {
    const headers: Record<string, string> = {
      ...this.headers,
      authorization: `Bearer ${this.token}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    };
    if (session) headers["mcp-session-id"] = session;
    const res = await fetch(this.url, { method: "POST", headers, body: JSON.stringify(body) });
    const sid = res.headers.get("mcp-session-id");
    if (sid) this.session = sid;
    const text = await res.text();
    if (!res.ok) return { status: res.status, json: null, text };
    if (!text.trim()) return { status: res.status, json: null, text };
    if (text.trimStart().startsWith("{")) return { status: res.status, json: JSON.parse(text), text };
    // SSE: take the JSON-RPC response.
    for (const line of text.split("\n")) {
      if (!line.startsWith("data:")) continue;
      try {
        const m = JSON.parse(line.slice(5).trim());
        if (m && m.id !== undefined) return { status: res.status, json: m, text };
      } catch {}
    }
    return { status: res.status, json: null, text };
  }

  private ensure(): Promise<void> {
    this.init ??= (async () => {
      this.session = null;
      const r = await this.post(
        {
          jsonrpc: "2.0",
          id: 0,
          method: "initialize",
          params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "falos-agent-runtime", version: "0.1.0" } },
        },
        null,
      );
      if (r.status >= 400) throw new Error(`host MCP ${r.status}: ${r.text.slice(0, 300)}`);
      if (r.json?.error) throw new Error(`host MCP initialize: ${JSON.stringify(r.json.error)}`);
      await this.post({ jsonrpc: "2.0", method: "notifications/initialized" }).catch(() => {});
    })().catch((e) => {
      this.init = null;
      throw e;
    });
    return this.init;
  }

  /** One JSON-RPC request; a dropped MCP session is re-established once. */
  private async request(method: string, params: unknown): Promise<any> {
    for (let attempt = 0; ; attempt++) {
      await this.ensure();
      const r = await this.post({ jsonrpc: "2.0", id: this.nextId++, method, params });
      if (r.status >= 400) {
        if (attempt === 0 && SESSION_GONE.test(`${r.status} ${r.text}`)) {
          this.init = null;
          continue;
        }
        throw new Error(`host MCP ${r.status}: ${r.text.slice(0, 300)}`);
      }
      return r.json;
    }
  }

  /** Is the host's MCP endpoint usable (initialize + tools/list on a fresh session)? */
  async ping(): Promise<boolean> {
    try {
      const r = await this.request("tools/list", {});
      return Array.isArray(r?.result?.tools);
    } catch {
      return false;
    }
  }

  /** Call a tool; returns its text (throws on tool errors). */
  async callText(name: string, args: Record<string, unknown>): Promise<string> {
    return (await this.callContent(name, args)).map((c: any) => c.text ?? "").join("\n");
  }

  /** Call a tool; all its content blocks (text and MCP image blocks). Throws on tool errors. */
  async callContent(name: string, args: Record<string, unknown>): Promise<any[]> {
    const r = await this.request("tools/call", { name, arguments: args });
    if (r?.error) throw new Error(`${name}: ${r.error.message ?? JSON.stringify(r.error)}`);
    const content: any[] = r?.result?.content ?? [];
    if (r?.result?.isError) throw new Error(`${name}: ${content.map((c: any) => c.text ?? "").join("\n").slice(0, 500)}`);
    return content;
  }

  /** The host's MCP tools (name + input schema): what this host build can do. */
  async listTools(): Promise<HostTool[]> {
    const r = await this.request("tools/list", {});
    const tools = r?.result?.tools;
    if (!Array.isArray(tools)) throw new Error(`host MCP tools/list: ${JSON.stringify(r?.error ?? r).slice(0, 200)}`);
    return tools.map((t: any) => ({ name: String(t.name), inputSchema: t.inputSchema }));
  }

  /** Call a tool whose result is JSON (notices after the JSON are dropped). */
  async call(name: string, args: Record<string, unknown>): Promise<any> {
    const text = await this.callText(name, args);
    const end = text.indexOf("\n\nnotices");
    const body = end >= 0 ? text.slice(0, end) : text;
    try {
      return JSON.parse(body);
    } catch {
      return body;
    }
  }
}
