from collections import defaultdict
from typing import List
import argparse
import urllib3
import junction.requests


def print_header(name):
    print("***** ")
    print(f"***** {name}")
    print("***** ")


# we get a little clever here, and use the same query_param we use to tell the server to send fail
# codes to also do the route, so we demonstrate that too
def retry_sample(args):
    print_header(
        "Retry Test - 502's have retries configured and will succeed, 501s do not"
    )

    default_backend: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_backend: junction.config.Target = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "target": default_backend,
            "rules": [
                {
                    "matches": [
                        {
                            "query_params": [
                                {"type": "regex", "name": "fail_match", "value": ".*"}
                            ]
                        }
                    ],
                    "retry": junction.config.RouteRetry(
                        codes=[502], attempts=2, backoff="1ms"
                    ),
                    "backends": [
                        feature_backend,
                    ],
                },
                {
                    "backends": [
                        default_backend,
                    ]
                },
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    session = junction.requests.Session(default_routes, default_backends)

    for code in [501, 502]:
        session.get(f"{args.base_url}/?fail_match={code}&fail_match_set_counter=2")
        counters = defaultdict(int)
        resp = session.get(f"{args.base_url}/?fail_match={code}")
        counters[resp.status_code] += 1
        print(f"For initial error code '{code}' actual response code counts are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key} = {counters[key]}")
    print("")


def path_match_sample(args):
    print_header(
        "Header Match - 50% of /feature-1/index sent to a different backend target"
    )

    default_backend: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_backend: junction.config.Target = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "target": default_backend,
            "rules": [
                {
                    "matches": [{"path": {"value": "/feature-1/index"}}],
                    "backends": [
                        {
                            **default_backend,
                            "weight": 50,
                        },
                        {
                            **feature_backend,
                            "weight": 50,
                        },
                    ],
                },
                {
                    "backends": [
                        default_backend,
                    ]
                },
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    session = junction.requests.Session(default_routes, default_backends)

    for path in ["/index", "/feature-1/index"]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}{path}")
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"For path '{path}' response body counts are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key} = {counters[key]}")
    print("")


def ring_hash_sample(args):
    print_header(
        "RingHash - header USER is hashed so requests with fixed value go to one backend server"
    )
    default_backend: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = []
    default_backends: List[junction.config.Backend] = [
        {
            "target": default_backend,
            "lb": {
                "type": "RingHash",
                "minRingSize": 1024,
                "hashParams": [{"type": "Header", "name": "USER"}],
            },
        }
    ]
    session = junction.requests.Session(default_routes, default_backends)

    for headers in [{}, {"USER": "user"}]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}", headers=headers)
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"With headers '{headers}' response body counts are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key} = {counters[key]}")
    print("")


def timeouts_sample(args):
    print_header(
        "Timeouts - timeout is 50ms, so if the server takes longer, request should throw"
    )

    default_backend: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "target": default_backend,
            "rules": [
                {
                    "backends": [
                        default_backend,
                    ],
                    "timeouts": {"backend_request": "50ms"},
                }
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    session = junction.requests.Session(default_routes, default_backends)

    for sleep_ms in [0, 100]:
        counters = defaultdict(int)
        try:
            session.get(f"{args.base_url}/?sleep_ms={sleep_ms}")
            counters["success"] += 1
        except urllib3.exceptions.ReadTimeoutError:
            counters["exception"] += 1
        print(f"With server sleep time '{sleep_ms}'ms call result counts are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key} = {counters[key]}")
    print("")


def all_samples(args):
    retry_sample(args)
    path_match_sample(args)
    ring_hash_sample(args)
    timeouts_sample(args)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Samples for Junction")
    parser.add_argument(
        "--base-url",
        default="http://jct-http-server.default.svc.cluster.local",
        help="The base url to send requests to",
    )
    parser.add_argument(
        "--sample",
        default="all_samples",
        help="specific sample to run",
    )

    args = parser.parse_args()
    globals()[args.sample](args)
