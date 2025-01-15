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

## Direct use

At this point in time Node.js is not supported for direct use of the client.

However, Junction is generally intended to be used indirectly, though interfaces
that that match existing HTTP clients. These are covered in the following
sections.

## [fetch](https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch)

Junction fully replicates the `fetch()` method, with it being mapped as a 
method on the client object:

```typescript
junctionLibrary = require("@junction-labs/client");
await junctionLibrary.fetch("http://jct-simple-app.default.svc.cluster.local:8008", { method: "GET" })
```

For more, [see the full API reference](https://docs.junctionlabs.io/api/node/stable/modules/fetch.html).
