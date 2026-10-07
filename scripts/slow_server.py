#!/usr/bin/env python3
"""A static file server for tests: waits before every response, so crawls take long enough
to watch `crawl status -f` and to kill a node mid-crawl. Optionally "flaky": the first request
for every path fails with 503, so the crawler's retries can be tested.

    python3 scripts/slow_server.py [directory] [port] [delay_ms] [flaky]
    python3 scripts/slow_server.py test-big 8001 300
    python3 scripts/slow_server.py test-site 8002 0 flaky
"""
import functools, sys, threading, time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

directory = sys.argv[1] if len(sys.argv) > 1 else "test-site"
port = int(sys.argv[2]) if len(sys.argv) > 2 else 8001
delay = (int(sys.argv[3]) if len(sys.argv) > 3 else 300) / 1000
flaky = len(sys.argv) > 4 and sys.argv[4] == "flaky"
seen, lock = set(), threading.Lock()

class SlowHandler(SimpleHTTPRequestHandler):
    # HTTP/1.1 keeps connections open between requests (keep-alive), like real servers.
    # Python's default, HTTP/1.0, closes the connection after every response.
    protocol_version = "HTTP/1.1"

    def send_head(self):
        time.sleep(delay)
        if flaky:
            with lock:
                first = self.path not in seen
                seen.add(self.path)
            if first:
                self.send_error(503, "flaky test server: try again")
                return None
        return super().send_head()
    def log_message(self, *args):
        pass

class Server(ThreadingHTTPServer):
    # The default (5) is too small for many crawler loops connecting at once: macOS
    # refuses the extra connections outright, which looks like broken pages.
    request_queue_size = 256
    daemon_threads = True

mode = ", flaky" if flaky else ""
print(f"serving {directory} on http://localhost:{port}/ with {int(delay*1000)} ms delay{mode}", flush=True)
Server(("", port), functools.partial(SlowHandler, directory=directory)).serve_forever()
