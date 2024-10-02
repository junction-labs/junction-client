from collections import defaultdict
from typing import List
import argparse
import urllib3
import junction.requests


def print_test_header(name):
    print(f"***** Beginning Test of {name} *****")


# we get a little clever here, and use the same query_param we use to tell the
# server to send fail codes to also do a route, so we test that too
def test_retries(args):
    default_backend: junction.config.Attachment = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_backend: junction.config.Attachment = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }

    default_routes: List[junction.config.Route] = [
        {
            "attachment": default_backend,
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

    print_test_header("Retries")
    for code in [501, 502]:
        session.get(f"{args.base_url}/?fail_match={code}&fail_match_set_counter=2")
        counters = defaultdict(int)
        resp = session.get(f"{args.base_url}/?fail_match={code}")
        counters[resp.status_code] += 1
        print(f"Error code '{code}' causes eventual response - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key}: {counters[key]}")
    print("")


def test_path_match(args):
    default_backend: junction.config.Attachment = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_backend: junction.config.Attachment = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }

    default_routes: List[junction.config.Route] = [
        {
            "attachment": default_backend,
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

    print_test_header("Header Match")
    for path in ["/index", "/feature-1/index"]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}{path}")
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"For path '{path}' counters are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key}: {counters[key]}")
    print("")


def test_ring_hash(args):
    default_backend: junction.config.Attachment = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = []
    default_backends: List[junction.config.Backend] = [
        {
            "attachment": default_backend,
            "lb": {
                "type": "RingHash",
                "minRingSize": 1024,
                "hashParams": [{"type": "Header", "name": "USER"}],
            },
        }
    ]
    session = junction.requests.Session(default_routes, default_backends)

    print_test_header("RingHash")
    for headers in [{}, {"USER": "inowland"}]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}", headers=headers)
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"With headers '{headers}' counters are - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key}: {counters[key]}")
    print("")


def test_timeouts(args):
    default_backend: junction.config.Attachment = {
        "name": "jct-http-server",
        "namespace": "default",
    }

    default_routes: List[junction.config.Route] = [
        {
            "attachment": default_backend,
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

    print_test_header("Timeouts")
    for sleep_ms in [0, 100]:
        counters = defaultdict(int)
        try:
            session.get(f"{args.base_url}/?sleep_ms={sleep_ms}")
            counters["success"] += 1
        except urllib3.exceptions.ReadTimeoutError:
            counters["exception"] += 1
        print(f"With server sleep time '{sleep_ms}'ms got response - ")
        myKeys = list(counters.keys())
        myKeys.sort()
        for key in myKeys:
            print(f"  {key}: {counters[key]}")
    print("")


##
## to repro this, first deploy jct_http_server, then deploy ezbake
## should get a wierd failure "failed to resolve: jct-http-server.default.svc.cluster.local: backend not found: jct-http-server-feature-1.default.svc.cluster.local:80"
## which is something to do with the LoadAssignments not getting sent by EZBake.
##
## Interestingly if you restart jct_http_server after EZBake is already running, the repro
## goes away
##
def test_bad_xds(args):
    default_backend: junction.config.Attachment = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_backend: junction.config.Attachment = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "attachment": default_backend,
            "rules": [
                {
                    "backends": [
                        default_backend,
                    ]
                },
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    print_test_header("Bad xDS Match")
    session = junction.requests.Session(default_routes, default_backends)
    resp = session.get(f"{args.base_url}")
    resp.raise_for_status()

    default_routes: List[junction.config.Route] = [
        {
            "attachment": default_backend,
            "rules": [
                {
                    "matches": [{"path": {"value": "/feature-1/index"}}],
                    "backends": [
                        {**feature_backend},
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
    resp = session.get(f"{args.base_url}/feature-1/index")
    resp.raise_for_status()


def test_all(args):
    test_retries(args)
    test_path_match(args)
    test_ring_hash(args)
    test_timeouts(args)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Test Junction")
    parser.add_argument(
        "--base-url",
        default="http://jct-http-server.default.svc.cluster.local",
        help="The base url to send requests to",
    )
    parser.add_argument(
        "--test",
        default="test_all",
        help="specific test to run",
    )

    args = parser.parse_args()
    globals()[args.test](args)
