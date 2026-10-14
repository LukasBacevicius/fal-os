import http.server,sys
out=sys.argv[1]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        b=self.rfile.read(int(self.headers.get('content-length',0)))
        open(out,'ab').write(b+b"\n"); self.send_response(200); self.send_header('Access-Control-Allow-Origin','*'); self.end_headers()
    def do_OPTIONS(self):
        self.send_response(200); self.send_header('Access-Control-Allow-Origin','*'); self.send_header('Access-Control-Allow-Headers','*'); self.end_headers()
    def log_message(self,*a): pass
http.server.HTTPServer(('127.0.0.1',7599),H).serve_forever()
