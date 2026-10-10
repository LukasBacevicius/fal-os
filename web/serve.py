"""Static server for web/ that also collects benchmark JSON posted to /bench."""
import http.server, json, os, sys, time

ROOT = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(ROOT, "bench-results.jsonl")


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=ROOT, **kw)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        if self.path != "/bench":
            return self.send_error(404)
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        body["user_agent"] = self.headers.get("User-Agent", "")
        body["at"] = time.strftime("%Y-%m-%dT%H:%M:%S")
        with open(RESULTS, "a") as f:
            f.write(json.dumps(body) + "\n")
        print("BENCH", json.dumps(body), flush=True)
        self.send_response(204)
        self.end_headers()

    def log_message(self, *a):
        pass


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
