import argparse
from collections import defaultdict
import junction.requests
import requests


def run(args):
    if args.session == "no-client-config":
        session = junction.requests.Session()
    elif args.session == "client-config":
        session = junction.requests.Session(
            default_routes=[
                {
                    "attachment": { "name": "jct-http-server", "namespace": "default" },
                    "rules": [
                        {
                            "matches": [{"path": {"value": "/feature-1/index"}}],
                            "backends": [
                                {
                                    "name": "jct-http-server", 
                                    "namespace": "default",
                                    "weight": 80
                                },
                                {
                                    "name": "jct-http-server-feature-1", 
                                    "namespace": "default",
                                    "weight": 20
                                }
                            ]
                        },
                        {
                            "backends": [ { "name": "jct-http-server", "namespace": "default" } ]
                        },
                    ],
                }
            ]
        )
    else:
        session = requests.Session()

    for path in ["/index", "/feature-1/index"]:
        counters = defaultdict(int)
        for _ in range(100):
            resp = session.get(f"{args.base_url}{path}")
            resp.raise_for_status()
            counters[resp.text] += 1
        print(f"{path} hit - ")
        for key in counters.keys():
            print(f"  {key}: {counters[key]}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Test Junction")
    parser.add_argument(
        "--base-url",
        default="http://jct-http-server.default.svc.cluster.local",
        help="The base url to send requests to",
    )
    parser.add_argument(
        "--session",
        default="client-config",
        choices={"client-config", "no-client-config", "dns"},
        help="the way to set up requests session",
    )
    args = parser.parse_args()
    run(args)
