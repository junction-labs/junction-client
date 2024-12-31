import * as junction from "@junction-labs/client";
import * as undici from "undici";

const client = await junction.Client.build({
  adsServer: "grpc://192.168.194.201:8008",
});

const agent = new junction.Agent({
  junction: client,
  pool: {},
});

const resp = await undici.request("https://httpbin.org/status/418", {
  dispatcher: agent,
  query: {
    q: "123",
  },
});

console.log(resp.statusCode);
console.log(await resp.body.text());
