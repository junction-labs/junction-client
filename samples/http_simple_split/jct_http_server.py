from http.server import HTTPServer, BaseHTTPRequestHandler
import os
import logging

version = os.environ.get('VERSION', "none")

class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def _set_headers(self):
        self.send_response(200)
        self.send_header('Content-type', 'text/plain')
        self.end_headers()

    def do_HEAD(self):
        logging.warning(f"header request recieved")
        self._set_headers()

    def do_GET(self):
        logging.warning(f"get request recieved")
        self._set_headers()
        self.wfile.write(str.encode(version))

address = ('', 8008)
httpd = HTTPServer(address, SimpleHTTPRequestHandler)
logging.warning(f"Starting, version {version}, binding to {address}")
httpd.serve_forever()
