from http.server import HTTPServer, BaseHTTPRequestHandler
import os
import logging
from random import randint
from time import sleep

version = os.environ.get("VERSION", "none")
server_id = randint(1, 100)

fail_counters = {}


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def _set_headers(self, code):
        self.send_response(code)
        self.send_header("Content-type", "text/plain")
        self.end_headers()

    def do_HEAD(self):
        logging.warning("header request recieved")
        self._set_headers(200)

    def do_GET(self):
        logging.warning(f"get request recieved to {self.path}")
        code = 200
        index = self.path.find("?")
        if index != -1:
            # some ugliness to avoid pulling in a dependency
            query_string = self.path[index + 1 :]
            query_params = dict(item.split("=") for item in query_string.split("&"))
            logging.warning(f"query params found {query_params}")
            if "fail_match" in query_params:
                fail_code = int(query_params["fail_match"])
                if "fail_match_set_counter" in query_params:
                    fail_counters[fail_code] = int(
                        query_params["fail_match_set_counter"]
                    )
                elif fail_code in fail_counters:
                    fail_counters[fail_code] = fail_counters[fail_code] - 1
                    if fail_counters[fail_code] != 0:
                        code = fail_code
                else:
                    logging.warning("got fail_match request but counter was never set")
            elif "sleep_ms" in query_params:
                sleep(float(query_params["sleep_ms"]) / 1000)

        logging.warning(f"returning code {code}")
        self._set_headers(code)
        self.wfile.write(str.encode(f"version:{version}, server_id:{server_id}"))


address = ("", 8008)
httpd = HTTPServer(address, SimpleHTTPRequestHandler)
logging.warning(
    f"Starting, version {version}, server_id {server_id}, binding to {address}"
)
httpd.serve_forever()
