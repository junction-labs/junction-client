# Installing Client - Node.js

The Junction client is [on NPM](https://www.npmjs.com/package/@junction-labs/client). 
This means all you need to do is:

```bash
npm install @junction-labs/client
```

If you are using Next.js, you will also have to add the following to `next.config.ts`:

```typescript
const nextConfig: NextConfig = {
  serverExternalPackages: ['@junction-labs/client'],
};
```


## [fetch](https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch)

Junction provides a `fetch()` method, that's fully compatible with the Fetch
standard, that uses Junction under the hood to route requests and handle
retries.


```typescript
const junction = require("@junction-labs/client");

var response = await junction.fetch("http://httpbin.default.svc.cluster.local:8008/status/418");
console.log(response.status);
// 418
console.log(await response.text());
//
//    -=[ teapot ]=-
//
//       _...._
//     .'  _ _ `.
//    | ."` ^ `". _,
//    \_;`"---"`|//
//      |       ;/
//      \_     _/
//        `"""`
```

## Direct use

To examine and debug configuration, you can instantiate a Junction client and
use it to directly call `resolveHttp`. 

```typescript
const junction = require("@junction-labs/client");

const client = await junction.Client.build({
  adsServer: "grpc://192.168.194.201:8008",
});

console.log(await client.resolveHttp({"url": "https://httpbin.org"));
// Endpoint {
//   scheme: 'https',
//   sockAddr: { address: '50.19.58.113', port: 443 },
//   hostname: 'httpbin.org',
//   retry: undefined,
//   timeouts: undefined
// }
```

APIs for dumping Route and Backend configuration are not yet available.

For more, [see the full API reference](https://docs.junctionlabs.io/api/node/stable/modules/fetch.html).
