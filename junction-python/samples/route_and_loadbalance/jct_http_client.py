import argparse
from collections import defaultdict
import junction.requests
import requests


def run(args):
    if args.session == "junction":
        default_backend: junction.config.Attachment = {
            "name": "jct-http-server",
            "namespace": "default",
        }
        feature_backend: junction.config.Attachment = {
            "name": "jct-http-server-feature-1",
            "namespace": "default",
        }

        session = junction.requests.Session(
            default_backends=[
                {
                    "attachment": feature_backend,
                    "lb": {
                        "type": "RingHash",
                        "minRingSize": 1024,
                        "hashParams": [{"type": "Header", "name": "USER"}],
                    },
                }
            ],
            default_routes=[
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
                            # an empty set of matches always matches
                            "backends": [
                                default_backend,
                            ]
                        },
                    ],
                }
            ],
        )
    else:
        session = requests.Session()

    for path in ["/index", "/feature-1/index"]:
        for header_opt in [{}, {"USER": "inowland"}]:
            counters = defaultdict(int)
            for _ in range(100):
                resp = session.get(f"{args.base_url}{path}", headers=header_opt)
                resp.raise_for_status()
                counters[resp.text] += 1
            print(f"***** Responses for Path: '{path}', Headers: '{header_opt}' *****")
            myKeys = list(counters.keys())
            myKeys.sort()
            for key in myKeys:
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
        default="junction",
        choices={"junction", "dns"},
        help="the way to set up requests session",
    )
    args = parser.parse_args()
    run(args)
