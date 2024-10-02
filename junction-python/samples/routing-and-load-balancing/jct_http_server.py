from http.server import HTTPServer, BaseHTTPRequestHandler
import os
import logging
from random import randint
from time import sleep

target = os.environ.get("TARGET", "none")
server_id = randint(1, 100)
fail_counters = {}


## some ugliness to avoid pulling in a dependency
def get_query_params(path):
    index = path.find("?")
    if index != -1:
        query_string = path[index + 1 :]
        return dict(item.split("=") for item in query_string.split("&"))
    else:
        return {}


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    def _set_headers(self, code):
        self.send_response(code)
        self.send_header("Content-type", "text/plain")
        self.end_headers()

    def do_HEAD(self):
        logging.warning("header request received")
        self._set_headers(200)

    def do_GET(self):
        logging.warning(f"get request received to {self.path}")
        query_params = get_query_params(self.path)
        code = 200
        if "fail_match" in query_params:
            fail_code = int(query_params["fail_match"])
            if "fail_match_set_counter" in query_params:
                fail_counters[fail_code] = int(query_params["fail_match_set_counter"])
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
        self.wfile.write(str.encode(f"target:{target},server_id:{server_id}"))


address = ("", 8008)
httpd = HTTPServer(address, SimpleHTTPRequestHandler)
logging.warning(
    f"Starting, target {target}, server_id {server_id}, binding to {address}"
)
httpd.serve_forever()
