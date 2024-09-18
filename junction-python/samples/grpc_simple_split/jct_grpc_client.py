# Copyright 2018 gRPC authors.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
"""gRPC Python helloworld.Greeter client with channel options and call timeout parameters."""

from __future__ import print_function

import time
import argparse
import logging

import grpc
import grpc_csds
from grpc_channelz.v1 import channelz
import helloworld_pb2
import helloworld_pb2_grpc
from concurrent import futures


def run(args):
    with grpc.insecure_channel(
        target=args.server,
        options=[
            ("grpc.lb_policy_name", "pick_first"),
            ("grpc.enable_retries", 0),
            ("grpc.keepalive_timeout_ms", 10000),
        ],
    ) as channel:
        if args.adminPort:
            admin_server = grpc.server(
                futures.ThreadPoolExecutor(max_workers=3),
                options=(("grpc.enable_channelz", 1),),
            )
            (admin_server.add_insecure_port("[::]:" + str(args.adminPort)),)
            channelz.add_channelz_servicer(admin_server)
            grpc_csds.add_csds_servicer(admin_server)
            admin_server.start()
        while True:
            stub = helloworld_pb2_grpc.GreeterStub(channel)
            try:
                response = stub.SayHello(
                    helloworld_pb2.HelloRequest(name="you"), timeout=10
                )
                print("Greeter client received: " + response.message)
            except Exception as e:
                print("oops, something went wrong: " + str(e))

            time.sleep(1)


if __name__ == "__main__":
    logging.basicConfig()

    parser = argparse.ArgumentParser(description="Greet someone")
    parser.add_argument("server", default=None, help="The address of the server.")
    parser.add_argument("--adminPort", default=None, help="the admin port.", type=int)
    args = parser.parse_args()

    run(args)
