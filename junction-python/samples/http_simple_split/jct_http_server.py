from http.server import HTTPServer, BaseHTTPRequestHandler
import os
import logging
from random import randint

version = os.environ.get("VERSION", "none")
server_id = randint(1, 100)


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def _set_headers(self):
        self.send_response(200)
        self.send_header("Content-type", "text/plain")
        self.end_headers()

    def do_HEAD(self):
        logging.warning("header request recieved")
        self._set_headers()

    def do_GET(self):
        logging.warning("get request recieved")
        self._set_headers()
        self.wfile.write(str.encode(f"version:{version}, server_id:{server_id}"))


address = ("", 8008)
httpd = HTTPServer(address, SimpleHTTPRequestHandler)
logging.warning(
    f"Starting, version {version}, server_id {server_id}, binding to {address}"
)
httpd.serve_forever()
