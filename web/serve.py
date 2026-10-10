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

    def send_head(self):
        # Byte ranges: Safari refuses to play <video> from servers without them.
        rng = self.headers.get("Range", "")
        path = self.translate_path(self.path)
        if not rng.startswith("bytes=") or not os.path.isfile(path):
            return super().send_head()
        size = os.path.getsize(path)
        start_s, _, end_s = rng[6:].split(",")[0].partition("-")
        if start_s:
            start, end = int(start_s), min(int(end_s) if end_s else size - 1, size - 1)
        else:
            start, end = max(size - int(end_s), 0), size - 1
        if start > end:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.end_headers()
            return None
        f = open(path, "rb")
        f.seek(start)
        self.send_response(206)
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Content-Length", str(end - start + 1))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        self._range_left = end - start + 1
        return f

    def copyfile(self, source, outputfile):
        left = getattr(self, "_range_left", None)
        if left is None:
            return super().copyfile(source, outputfile)
        self._range_left = None
        while left > 0:
            chunk = source.read(min(left, 64 * 1024))
            if not chunk:
                break
            outputfile.write(chunk)
            left -= len(chunk)

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
