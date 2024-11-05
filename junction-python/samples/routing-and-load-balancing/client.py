from collections import defaultdict
from typing import List
import argparse
import junction.requests
import urllib3
from junction.urllib3 import PoolManager as JunctionPoolManger
from urllib3.util import Timeout
import requests


def print_header(name):
    print("***** ")
    print(f"***** {name}")
    print("***** ")


def print_counters(counters):
    keys = list(counters.keys())
    keys.sort()
    for key in keys:
        print(f"  {key} = {counters[key]}")
    return counters


# we get a little clever here, and use the same query_param we use to tell the
# server to send fail codes to also do the route, so we demonstrate that too
def retry_sample(args):
    print_header(
        "Retry Test - 502's have retries configured and will succeed, 501s do not"
    )

    default_target: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_target: junction.config.Target = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "vhost": default_target,
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
                        codes=[502],
                        attempts=2,
                        backoff=0.001,
                    ),
                    "backends": [
                        {**feature_target, "port": 8008},
                    ],
                },
                {
                    "backends": [
                        {**default_target, "port": 8008},
                    ]
                },
            ],
        }
    ]
    session = junction.requests.Session(default_routes)

    results = []
    for code in [501, 502]:
        session.get(f"{args.base_url}/?fail_match={code}&fail_match_set_counter=2")
        counters = defaultdict(int)
        resp = session.get(f"{args.base_url}/?fail_match={code}")
        counters[resp.status_code] += 1
        print(f"For initial error code '{code}' actual response code counts are - ")
        results.append(print_counters(counters))

    if args.validate:
        assert len(results) == 2
        assert results[0][501] == 1
        assert results[1][200] == 1
    print("")


def path_match_sample(args):
    print_header("Header Match - 50% of /feature-1/index sent to a different backend")

    default_target: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    feature_target: junction.config.Target = {
        "name": "jct-http-server-feature-1",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "vhost": default_target,
            "rules": [
                {
                    "matches": [{"path": {"value": "/feature-1/index"}}],
                    "backends": [
                        {
                            **default_target,
                            "weight": 50,
                            "port": 8008,
                        },
                        {
                            **feature_target,
                            "weight": 50,
                            "port": 8008,
                        },
                    ],
                },
                {
                    "backends": [
                        {**default_target, "port": 8008},
                    ]
                },
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    session = junction.requests.Session(default_routes, default_backends)

    results = []
    for path in ["/index", "/feature-1/index"]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}{path}")
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"For path '{path}' response body counts are - ")
        results.append(print_counters(counters))

    if args.validate:
        assert len(results) == 2
        assert len(results[0]) == 3
        for key, value in results[0].items():
            assert value == 66 or value == 67
        assert len(results[1]) == 4
        for key, value in results[1].items():
            # unlike round robin, split is non-deterministic, so need wide bars
            # here
            if "jct-http-server-feature-1" in key:
                assert value > 40  # should be ~100
            else:
                assert value < 50  # should be ~ 33
    print("")


def ring_hash_sample(args):
    print_header(
        "RingHash - header USER is hashed so requests with fixed value go to one backend server"
    )
    default_target: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
        "port": 8008,
    }
    default_backends: List[junction.config.Backend] = [
        {
            "id": default_target,
            "lb": {
                "type": "RingHash",
                "minRingSize": 1024,
                "hashParams": [{"type": "Header", "name": "USER"}],
            },
        }
    ]
    session = junction.requests.Session(default_backends=default_backends)

    results = []
    for headers in [{}, {"USER": "user"}]:
        counters = defaultdict(int)
        for _ in range(200):
            resp = session.get(f"{args.base_url}", headers=headers)
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"With headers '{headers}' response body counts are - ")
        results.append(print_counters(counters))

    if args.validate:
        assert len(results) == 2
        assert (
            len(results[0]) == 3
        )  # slightly racy, but with 200 attempts all 3 should see 1 call
        assert len(results[1]) == 1
    print("")


def timeouts_sample(args):
    print_header(
        "Timeouts - timeout is 50ms, so if the server takes longer, request should throw"
    )

    default_target: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "vhost": default_target,
            "rules": [
                {
                    "backends": [
                        {**default_target, "port": 8008},
                    ],
                    "timeouts": {"backend_request": 0.05},
                }
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    session = junction.requests.Session(default_routes, default_backends)

    results = []
    for sleep_ms in [0, 100]:
        counters = defaultdict(int)
        try:
            session.get(f"{args.base_url}/?sleep_ms={sleep_ms}")
            counters["success"] += 1
        except requests.exceptions.ReadTimeout:
            counters["exception"] += 1
        print(f"With server sleep time '{sleep_ms}'ms call result counts are - ")
        results.append(print_counters(counters))

    ## below is just regression validation
    if args.validate:
        assert len(results) == 2
        assert results[0]["success"] == 1
        assert results[1]["exception"] == 1
    print("")


def urllib3_sample(args):
    print_header(
        "urllib3 - We repeat the timeouts sample, using urllib3 directly instead"
    )

    default_target: junction.config.Target = {
        "name": "jct-http-server",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "vhost": default_target,
            "rules": [
                {
                    "backends": [
                        {**default_target, "port": 8008},
                    ],
                    "timeouts": {"backend_request": 0.05},
                }
            ],
        }
    ]
    default_backends: List[junction.config.Backend] = []
    http = JunctionPoolManger(
        default_backends=default_backends, default_routes=default_routes
    )

    results = []
    for sleep_ms in [0, 100]:
        counters = defaultdict(int)
        try:
            http.urlopen("GET", f"{args.base_url}/?sleep_ms={sleep_ms}")
            counters["success"] += 1
        except urllib3.exceptions.MaxRetryError:
            counters["exception"] += 1
        print(f"With server sleep time '{sleep_ms}'ms call result counts are - ")
        results.append(print_counters(counters))

    # now show an override of timeout on the method call still works
    for sleep_ms in [0, 100]:
        counters = defaultdict(int)
        try:
            http.urlopen(
                "GET",
                f"{args.base_url}/?sleep_ms={sleep_ms}",
                timeout=Timeout(connect=1000, read=1000),
            )
            counters["success"] += 1
        except urllib3.exceptions.MaxRetryError:
            counters["exception"] += 1
        print(
            f"With server sleep time '{sleep_ms}'ms and a 1s override on call, call result counts are - "
        )
        results.append(print_counters(counters))

    ## below is just regression validation
    if args.validate:
        assert len(results) == 4
        assert results[0]["success"] == 1
        assert results[1]["exception"] == 1
        assert results[2]["success"] == 1
        assert results[3]["success"] == 1
    print("")


def all_samples(args):
    retry_sample(args)
    path_match_sample(args)
    ring_hash_sample(args)
    timeouts_sample(args)
    urllib3_sample(args)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Samples for Junction")
    parser.add_argument(
        "--base-url",
        default="http://jct-http-server.default.svc.cluster.local:8008",
        help="The base url to send requests to",
    )
    parser.add_argument(
        "--sample",
        default="all_samples",
        help="specific sample to run",
    )

    parser.add_argument(
        "--validate",
        action=argparse.BooleanOptionalAction,
        help="whether to validate the results for regressions",
    )

    args = parser.parse_args()
    globals()[args.sample](args)
