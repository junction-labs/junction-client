// unidici splits its connection options in a fun way:
//
// - the Client has a constructor that takes TLS args.
// - every call to a Dispatcher.dispatch takes a hostname/url etc.
//
// when a new connection gets created, it can see the ORIGIN of the request
// the the client options, but nothing else. the library does some fun defensive
// programming to keep you from shoving arbitrary things into the origin.
//
// https://github.com/nodejs/undici/blob/main/lib/dispatcher/agent.js#L68C16-L68C20

// FIXME: this is starting to work. we have to do timeouts and retries, and
// figure out a TLS test quickly.

import { Dispatcher, Pool, Client as UndiciClient } from "undici";
import { type Client, type SockAddr, endpointURL } from "./core.cjs";

function hrToMillis(hrtime?: [number, number]): number | undefined {
  if (hrtime === undefined) {
    return undefined;
  }

  const [seconds, nanos] = hrtime;
  return seconds * 1000 + nanos / 1_000_000;
}

export interface AgentOpts {
  junction: Client;
  pool: Pool.Options;
}

export type NewConnection = (
  origin: URL,
  poolOptions: Pool.Options,
) => Dispatcher;

function newConnection(origin: URL, pool: Pool.Options): Dispatcher {
  return pool.connections && pool.connections === 1
    ? new UndiciClient(origin, pool)
    : new Pool(origin, pool);
}

export class Agent extends Dispatcher {
  junction: Client;
  #clients: Map<SockAddr, Dispatcher>;
  #poolOptions: Pool.Options;
  #newClient: NewConnection;

  constructor({ junction, pool }: AgentOpts) {
    super();

    this.junction = junction;
    this.#poolOptions = pool;
    this.#clients = new Map();
    this.#newClient = pool.factory || newConnection;
  }

  dispatch(
    originalOptions: Dispatcher.DispatchOptions,
    handler: Dispatcher.DispatchHandler,
  ): boolean {
    // FIXME: figure out how to encode query parameters. this should be a new URL
    // but we have to deal with undefined i guess.
    const url = originalOptions.origin + originalOptions.path;

    // FIXME: undici headers aren't assignable to fetch Headers. figure out how to convert.

    this.junction
      .resolveHttp({
        method: originalOptions.method,
        url,
      })
      .then((endpoint) => {
        const origin = endpointURL(endpoint);
        const headersTimeout =
          originalOptions.headersTimeout ||
          hrToMillis(endpoint.timeouts?.backendRequest);

        const clientOptions: Pool.Options = {
          ...this.#poolOptions,
          connect: {
            host: endpoint.sockAddr.address,
            port: endpoint.sockAddr.port,
            servername: endpoint.hostname,
          },
        };
        const requestOptions = {
          ...originalOptions,
          origin,
          headersTimeout,
        };

        let pool = this.#clients.get(endpoint.sockAddr);
        if (!pool) {
          pool = this.#newClient(requestOptions.origin, clientOptions);
        }

        pool.dispatch(originalOptions, handler);
      })
      .catch((err) => {
        // FIXME: lol
        console.log("Oh no", err);
      });

    return true;
  }

  // the close/destroy stuff is copied pretty uncritically from undici's Agent
  // impl. it inehrits from DispatcherBase, which isn't available to us, so we
  // have to handle what goes on in both of those classes here.
  //
  // DispatcherBase looks like it handles dealing with a bunch of concurrent
  // callbacks. it's unclear how much that matters, so we're going with the dumb
  // simple thing that looks like the kClose and kDestroy private methods in
  // Agent.

  close(): Promise<void> {
    return this.#close().then(() => this.destroy());
  }

  async #close() {
    const cs = [];
    for (const client of this.#clients.values()) {
      cs.push(client.close());
    }
    this.#clients.clear();

    await Promise.all(cs);
  }

  destroy(): Promise<void> {
    return this.#destroy();
  }

  async #destroy() {
    const cs = [];
    for (const client of this.#clients.values()) {
      cs.push(client.destroy());
    }
    this.#clients.clear();

    await Promise.all(cs);
  }
}
