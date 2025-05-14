from collections import defaultdict
from time import sleep
from typing import List, Optional
import subprocess
import tempfile
import argparse
import urllib3
import yaml
import requests
import junction.requests
from junction.urllib3 import PoolManager as JunctionPoolManger
from urllib3.util import Timeout


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


# Junction can run get config two ways, completely statically passed to the
# to a server, or dynamically from the same server sending it kube endpoints
# This class wraps which of the ways we are running allowing us to use the
# same test code in either case.
class SessionFactory:
    def __init__(
        self,
        args,
        routes: Optional[List[junction.config.Route]] = None,
        backends: Optional[List[junction.config.Backend]] = None,
    ):
        self.args = args
        self.delete_resources = []
        self.delete_patches = []

        if self.args.use_gateway_api:
            if routes:
                manifests = [
                    junction.dump_kube_route(route=route, namespace="default")
                    for route in routes
                ]
                self._kubectl_apply(*manifests)
                self.delete_resources = manifests
            if backends:
                manifests = [
                    junction.dump_kube_backend(backend) for backend in backends
                ]
                self._kubectl_patch(*manifests)
                for manifest in manifests:
                    spec = yaml.safe_load(manifest)
                    for key in spec["metadata"]["annotations"].keys():
                        spec["metadata"]["annotations"][key] = None
                    self.delete_patches.append(yaml.dump(spec))

            # unfortunately, kube/ezbake do not propagate changes instantly
            # so we need to wait for them to be ready
            sleep(2)
            self.session = junction.requests.Session()
        else:
            self.session = junction.requests.Session(
                static_routes=routes, static_backends=backends
            )

    def __enter__(self):
        return self.session

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.session.close()
        if len(self.delete_resources) > 0:
            self._kubectl_delete(*self.delete_resources)
            self.delete_resources.clear()
        if len(self.delete_patches) > 0:
            self._kubectl_patch(*self.delete_patches)
            self.delete_patches.clear()

    @staticmethod
    def _kubectl_apply(*manifests):
        for manifest in manifests:
            with tempfile.NamedTemporaryFile(mode="w") as f:
                f.write(manifest)
                f.seek(0)
                subprocess.run(
                    [
                        "kubectl",
                        "apply",
                        "-f",
                        f.name,
                    ]
                )

    @staticmethod
    def _kubectl_patch(*manifests):
        for manifest in manifests:
            with tempfile.NamedTemporaryFile(mode="w") as f:
                f.write(manifest)
                f.seek(0)
                subprocess.run(
                    [
                        "kubectl",
                        "patch",
                        "-f",
                        f.name,
                        "--patch-file",
                        f.name,
                    ]
                )

    @staticmethod
    def _kubectl_delete(*manifests):
        for manifest in manifests:
            with tempfile.NamedTemporaryFile(mode="w") as f:
                f.write(manifest)
                f.seek(0)
                subprocess.run(
                    [
                        "kubectl",
                        "delete",
                        "-f",
                        f.name,
                    ]
                )


# we get a little clever here, and use the same query_param we use to tell the
# server to send fail codes to also do the route, so we demonstrate that too
def retry_test(args):
    print_header(
        "Retry Test - 502's have retries configured and will succeed, 501s do not"
    )

    service: junction.config.Service = {
        "type": "kube",
        "name": "jct-simple-app",
        "namespace": "default",
    }
    service_v2: junction.config.Service = {
        "type": "kube",
        "name": "jct-simple-app-v2",
        "namespace": "default",
    }
    routes: List[junction.config.Route] = [
        {
            "id": "smoke-test-retry-test",
            "hostnames": ["jct-simple-app.default.svc.cluster.local"],
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
                        {**service_v2, "port": 8008},
                    ],
                },
                {
                    "backends": [
                        {**service, "port": 8008},
                    ]
                },
            ],
        }
    ]
    with SessionFactory(args, routes) as session:
        results = []
        for code in [501, 502]:
            session.get(f"{args.base_url}/?fail_match={code}&fail_match_set_counter=2")
            counters = defaultdict(int)
            resp = session.get(f"{args.base_url}/?fail_match={code}")
            counters[resp.status_code] += 1
            print(f"For initial error code '{code}' actual response code counts are - ")
            results.append(print_counters(counters))

        assert len(results) == 2
        assert results[0][501] == 1
        assert results[1][200] == 1
        print("")


def path_match_test(args):
    print_header("Header Match - 50% of /feature-1/index sent to a different backend")

    service: junction.config.Service = {
        "type": "kube",
        "name": "jct-simple-app",
        "namespace": "default",
    }
    service_v2: junction.config.Service = {
        "type": "kube",
        "name": "jct-simple-app-v2",
        "namespace": "default",
    }
    routes: List[junction.config.Route] = [
        {
            "id": "smoke-test-path-match",
            "hostnames": ["jct-simple-app.default.svc.cluster.local"],
            "rules": [
                {
                    "matches": [{"path": {"value": "/feature-1/index"}}],
                    "backends": [
                        {
                            **service,
                            "weight": 50,
                            "port": 8008,
                        },
                        {
                            **service_v2,
                            "weight": 50,
                            "port": 8008,
                        },
                    ],
                },
                {
                    "backends": [
                        {**service, "port": 8008},
                    ]
                },
            ],
        }
    ]

    with SessionFactory(args, routes) as session:
        results = []
        for path in ["/index", "/feature-1/index"]:
            counters = defaultdict(int)
            for _ in range(200):
                resp = session.get(f"{args.base_url}{path}")
                resp.raise_for_status()
                counters[resp.text] += 1
            print(f"For path '{path}' response body counts are - ")
            results.append(print_counters(counters))

        assert len(results) == 2
        assert len(results[0]) == 3
        for key, value in results[0].items():
            assert value == 66 or value == 67
        assert len(results[1]) == 4
        for key, value in results[1].items():
            # unlike round robin, split is non-deterministic, so need wide bars
            # here
            if "jct-simple-app-v2" in key:
                assert value > 40  # should be ~100
            else:
                assert value < 50  # should be ~ 33
        print("")


def ring_hash_test(args):
    print_header(
        "RingHash - header USER is hashed so requests with fixed value go to one backend server"
    )
    backends: List[junction.config.Backend] = [
        {
            "id": {
                "type": "kube",
                "name": "jct-simple-app",
                "namespace": "default",
                "port": 8008,
            },
            "lb": {
                "type": "RingHash",
                "minRingSize": 1024,
                "hashParams": [{"type": "Header", "name": "USER"}],
            },
        }
    ]
    with SessionFactory(args, backends=backends) as session:
        results = []
        for headers in [{}, {"USER": "user"}]:
            counters = defaultdict(int)
            for _ in range(200):
                resp = session.get(f"{args.base_url}", headers=headers)
                resp.raise_for_status()
                counters[resp.text] += 1
            print(f"With headers '{headers}' response body counts are - ")
            results.append(print_counters(counters))

        assert len(results) == 2
        assert (
            len(results[0]) == 3
        )  # slightly racy, but with 200 attempts all 3 should see 1 call
        assert len(results[1]) == 1
        print("")


def timeouts_test(args):
    print_header(
        "Timeouts - timeout is 50ms, so if the server takes longer, request should throw"
    )

    # test both backend_request and request timeouts
    configs = {
        "backend_request": {"backend_request": 0.05},
        "request": {"request": 0.05},
    }
    results = []
    for config_name, config in configs.items():
        service: junction.config.Service = {
            "type": "kube",
            "name": "jct-simple-app",
            "namespace": "default",
        }
        routes: List[junction.config.Route] = [
            {
                "id": "smoke-test-timeouts-test",
                "hostnames": ["jct-simple-app.default.svc.cluster.local"],
                "rules": [
                    {
                        "backends": [
                            {**service, "port": 8008},
                        ],
                        "timeouts": config,
                    }
                ],
            }
        ]
        with SessionFactory(args, routes) as session:
            for sleep_ms in [0, 100]:
                counters = defaultdict(int)
                try:
                    session.get(f"{args.base_url}/?sleep_ms={sleep_ms}")
                    counters["success"] += 1
                except requests.exceptions.ReadTimeout:
                    counters["exception"] += 1
                print(
                    f"With {config_name} configured and server sleep time '{sleep_ms}'ms call result counts are - "
                )
                results.append(print_counters(counters))

    assert len(results) == 4
    assert results[0]["success"] == 1
    assert results[1]["exception"] == 1
    assert results[2]["success"] == 1
    assert results[3]["exception"] == 1
    print("")


def urllib3_test(args):
    print_header(
        "urllib3 - We repeat the timeouts sample, using urllib3 directly instead"
    )

    service: junction.config.Service = {
        "type": "kube",
        "name": "jct-simple-app",
        "namespace": "default",
    }
    default_routes: List[junction.config.Route] = [
        {
            "id": "smoke-test-urllib3",
            "hostnames": ["jct-simple-app.default.svc.cluster.local"],
            "rules": [
                {
                    "backends": [
                        {**service, "port": 8008},
                    ],
                    "timeouts": {"backend_request": 0.05},
                }
            ],
        }
    ]
    http = JunctionPoolManger(static_routes=default_routes)

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

    assert len(results) == 4
    assert results[0]["success"] == 1
    assert results[1]["exception"] == 1
    assert results[2]["success"] == 1
    assert results[3]["success"] == 1
    print("")


def all_tests(args):
    retry_test(args)
    path_match_test(args)
    ring_hash_test(args)
    timeouts_test(args)
    urllib3_test(args)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Samples for Junction")
    parser.add_argument(
        "--base-url",
        default="http://jct-simple-app.default.svc.cluster.local:8008",
        help="The base url to send requests to",
    )
    parser.add_argument(
        "--test",
        default="all_tests",
        help="specific test to run",
    )
    parser.add_argument(
        "--use-gateway-api",
        action=argparse.BooleanOptionalAction,
        help="whether to use the Gateway API for dynamic configuration",
    )

    args = parser.parse_args()
    globals()[args.test](args)
