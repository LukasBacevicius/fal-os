// Minimal MCP client (Streamable HTTP, JSON-RPC) for the fal-os host's canvas tools, used by the
// runtime's own tools (placeholders, imports, presence) on behalf of an agent's session.
import { config } from "./config.ts";

export class HostMcp {
  private session: string | null = null;
  private nextId = 1;
  private init: Promise<void> | null = null;

  constructor(
    private url = `${config.hostUrl}/mcp`,
    private token = config.token,
  ) {}

  private async post(body: unknown): Promise<any> {
    const headers: Record<string, string> = {
      authorization: `Bearer ${this.token}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    };
    if (this.session) headers["mcp-session-id"] = this.session;
    const res = await fetch(this.url, { method: "POST", headers, body: JSON.stringify(body) });
    const sid = res.headers.get("mcp-session-id");
    if (sid) this.session = sid;
    const text = await res.text();
    if (!res.ok) throw new Error(`host MCP ${res.status}: ${text.slice(0, 300)}`);
    if (!text.trim()) return null;
    if (text.trimStart().startsWith("{")) return JSON.parse(text);
    // SSE: take the JSON-RPC response.
    for (const line of text.split("\n")) {
      if (!line.startsWith("data:")) continue;
      try {
        const m = JSON.parse(line.slice(5).trim());
        if (m && m.id !== undefined) return m;
      } catch {}
    }
    return null;
  }

  private ensure(): Promise<void> {
    this.init ??= (async () => {
      const r = await this.post({
        jsonrpc: "2.0",
        id: 0,
        method: "initialize",
        params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "falos-agent-runtime", version: "0.1.0" } },
      });
      if (r?.error) throw new Error(`host MCP initialize: ${JSON.stringify(r.error)}`);
      await this.post({ jsonrpc: "2.0", method: "notifications/initialized" }).catch(() => {});
    })();
    return this.init;
  }

  /** Call a tool; returns its text (throws on tool errors). */
  async callText(name: string, args: Record<string, unknown>): Promise<string> {
    await this.ensure();
    const r = await this.post({ jsonrpc: "2.0", id: this.nextId++, method: "tools/call", params: { name, arguments: args } });
    if (r?.error) throw new Error(`${name}: ${r.error.message ?? JSON.stringify(r.error)}`);
    const text = (r?.result?.content ?? []).map((c: any) => c.text ?? "").join("\n");
    if (r?.result?.isError) throw new Error(`${name}: ${text.slice(0, 500)}`);
    return text;
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
