import {
  Client,
  type Endpoint,
  type ResolveHttpOpts,
  type RetryPolicy,
  endpointURL,
} from "./core.cjs";

import {
  type Dispatcher,
  Client as HTTPClient,
  Pool as HTTPPool,
} from "undici";

import { setTimeout as setTimeoutPromise } from "node:timers/promises";

// interface for creating a new connection. this is the same as undici's factory
// arg in the pool opts. the odds that we plug an alternate impl in are
// questionable but it's here anyway.
type NewConnection = (origin: URL, poolOptions: HTTPPool.Options) => Dispatcher;

function newConnection(origin: URL, pool: HTTPPool.Options): Dispatcher {
  return pool.connections && pool.connections === 1
    ? new HTTPClient(origin, pool)
    : new HTTPPool(origin, pool);
}

// this is both the connection pool key and all of the TLS args that need to get
// passed to every new connection.
type ConnectOpts = {
  host: string;
  port: number;
  servername: string;
};

class ClientPool {
  #opts: HTTPPool.Options;
  #newClient: NewConnection;
  #clients: Map<ConnectOpts, Dispatcher>;

  constructor(
    newConn: NewConnection = newConnection,
    opts: HTTPPool.Options = {},
  ) {
    this.#opts = opts;
    this.#newClient = newConn;
    this.#clients = new Map();
  }

  getConnection(endpoint: Endpoint): Dispatcher {
    const connect: ConnectOpts = {
      servername: endpoint.hostname,
      host: endpoint.sockAddr.address,
      port: endpoint.sockAddr.port,
    };

    let pool = this.#clients.get(connect);
    if (!pool) {
      pool = this.#newClient(endpointURL(endpoint), {
        ...this.#opts,
        connect,
      });
      this.#clients.set(connect, pool);
    }
    return pool;
  }
}

function resolveOptsFor(request: Request): ResolveHttpOpts {
  return {
    method: request.method,
    url: request.url,
    headers: request.headers,
  };
}

// TODO: support retry-after dates. undici doesn't support those either
// right now.
//
// https://github.com/nodejs/undici/blob/54c5c6827c7d3f801bd72926234243a9ab8f7ef5/lib/handler/retry-handler.js#L123-L129
function retryAfter(
  minBackoff: number,
  response?: Response,
): number | undefined {
  const retryAfter = response?.headers.get("retry-after");
  if (!retryAfter) {
    return undefined;
  }

  const afterSecs = Number(retryAfter);
  if (Number.isNaN(afterSecs)) {
    return undefined;
  }

  return Math.max(afterSecs * 1000, minBackoff);
}

interface ErrorCode {
  code: string;
}

function hasErrorCode(obj: unknown): obj is ErrorCode {
  return (
    (obj as ErrorCode)?.code !== undefined &&
    typeof (obj as ErrorCode).code === "string"
  );
}

function isNetworkError(err: unknown): err is TypeError {
  if (!(err instanceof TypeError)) {
    return false;
  }

  return hasErrorCode(err.cause) && err.cause?.code.startsWith("UND");
}

function isTimeoutError(err: unknown): err is DOMException {
  return err instanceof DOMException && err.name === "TimeoutError";
}

class Retry {
  #statusCodes: number[];
  #backoff: number;
  #maxAttempts: number;
  #attempt: number;

  constructor(policy?: RetryPolicy) {
    this.#attempt = 1;
    this.#maxAttempts = policy?.attempts || 1;
    this.#statusCodes = policy?.codes || [];

    if (policy?.backoff) {
      this.#backoff = policy?.backoff;
    } else {
      this.#backoff = 500;
    }
  }

  shouldRetry(response?: Response): boolean {
    if (this.#attempt > this.#maxAttempts) {
      return false;
    }

    if (response?.status && !this.#statusCodes.includes(response?.status)) {
      return false;
    }

    return true;
  }

  increment(response?: Response): number {
    // backoff is either what the retry-after header says or `(2^attempts) * policy.backoff`
    const backoff: number =
      retryAfter(this.#backoff, response) || 2 ** this.#attempt * this.#backoff;

    this.#attempt += 1;
    return backoff;
  }
}

// a cancelable sleep.
async function sleep(millis: number, signals: AbortSignal[]) {
  const signal = AbortSignal.any(signals);
  await setTimeoutPromise(millis, undefined, { signal });
}

// a global junction client and connection pool back the fetch implementation.
//
// this isn't ideal - it'd be nice to have an undici.Dispatcher we could pass to
// undici.setGlobalDispatcher() instead, and not rely on globals here. building
// a Dispatcher is a bit hairy - there interfaces are pretty complex and there's
// no way to pass state around - so we went with this approach first. the price
// we're paying is more module-global variables.

const clients = new ClientPool();

async function clientFromEnv(): Promise<Client> {
  if (!process.env.JUNCTION_ADS_SERVER) {
    throw new Error("JUNCTION_ADS_SERVER must be set");
  }
  return await Client.build({
    adsServer: process.env.JUNCTION_ADS_SERVER || "",
    nodeName: process.env.JUNCTION_NODE || `junction-node-${process.pid}`,
    clusterName: process.env.JUNCTION_CLUSTER || "junction-node",
  });
}

let defaultClient: Promise<Client | Error> = clientFromEnv().catch(
  (err) => err,
);

/**
 * Set the default Junction client used by calls to `fetch`. If no client is
 * set, a default client is configured with the `JUNCTION_ADS_SERVER`,
 * `JUNCTION_NODE`, and `JUNCTION_CLUSTER` environment variables.
 */
export function setDefaultClient(junction: Client) {
  defaultClient = Promise.resolve(junction);
}

/**
 * Implements Node [fetch](https://fetch.spec.whatwg.org/#fetch-method)
 * using Junction for service discovery.
 *
 * A `dispatcher` passed to this function or set with `setGlobalDispatcher`
 * will be ignored in favor of the Junction dispatcher. To set the Junction
 * client, use `setDefaultClient`.
 *
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API
 */
export async function fetch(
  input: RequestInfo,
  init?: RequestInit,
): Promise<Response> {
  // immediatly go fetch a junction endpoint
  //
  // NOTE: we can't set the Host header here, it's forbidden by the fetch API.
  // while MDN says it's supposed to be forbidden and throw an exception, Node
  // seems to just silently rewrite it.
  //
  // this may eventually be problematic when dealing with external services that
  // rely on the host header being set to a specific value - if we inject an
  // agent into a `fetch` call and use a dispatcher to change the address it
  // hits, fetch and undici seem to always use the pool's address.
  //
  // for example if `potato` is a hostname that resolves to 127.0.0.1 with a cert
  // with the dns name `localhost`, and we send a fetch by injecting an undici
  // dispatcher like so:
  //
  // ```
  //   var resp = await fetch("https://potato:8000", {
  //        dispatcher: new undici.Pool("https://127.0.0.1:8000", {
  //          connect: {ca, servername: "localhost"}
  //        })
  //    });
  // ```
  //
  // Node will set `host: 127.0.0.1:8000` even if we try to override the host
  // header. Setting the `host` in the Pool's `connect` options doesn't seem
  // to do anything either.
  const request = new Request(input, init);
  const junction = await defaultClient;
  if (junction instanceof Error) {
    throw junction;
  }

  let endpoint = await junction.resolveHttp(resolveOptsFor(request));

  // combine the AbortSignal on the initial request plus an overall request
  // timeout signal. keep them around as an array so that we can combine them
  // with a per-request timeout - we could combine them here, but just do it
  // once for each request instead.
  const signals = [];
  if (init?.signal) {
    signals.push(init.signal);
  }
  if (endpoint.timeouts?.request) {
    signals.push(AbortSignal.timeout(endpoint.timeouts.request));
  }

  const retry = new Retry(endpoint.retry);
  for (;;) {
    // grab a connection (or pool) for the endpoint that was selected. the
    // connection sets the individual request timeout policy.
    const dispatcher = clients.getConnection(endpoint);

    // combine the existing signals with the per-request timeout.
    let signal: AbortSignal;
    if (endpoint.timeouts?.backendRequest) {
      signal = AbortSignal.any(
        signals.concat([AbortSignal.timeout(endpoint.timeouts.backendRequest)]),
      );
    } else {
      signal = AbortSignal.any(signals);
    }

    // try to actually call fetch.
    //
    // the spec mandates that any network error here is a TypeError. if we catch
    // any other kind of error, immediately rethrow it.
    //
    // for whatever reason, RequestInit doesn't typecheck with dispatcher. it
    // still gets passed in fine, so construct the args untyped and pass em in
    // later.
    const requestInit = {
      ...init,
      signal,
      dispatcher,
    };
    let resp: Response | undefined;
    let err: TypeError | undefined;
    try {
      resp = await globalThis.fetch(request, requestInit);
    } catch (e) {
      if (!isNetworkError(e) && !isTimeoutError(e)) {
        throw e;
      }
      err = e;
    }

    // re-throw here if any signal is aborted and there was an error present.
    //
    // we only want to check here instead of checking the error returned from
    // fetch - the signal passed into fetch will have an extra per-request
    // timeout added, and we'll re-throw that only if we're out of retries.
    // the signals checked here are the global timeout and whatever user signal
    // was passed in - if either of those are done, we're done.
    for (const signal of signals) {
      signal.throwIfAborted();
    }

    // always report status
    endpoint = await junction.reportStatus(endpoint, {
      status: resp?.status,
      error: err,
    });

    // if we're done, we're done.
    if (!retry.shouldRetry(resp)) {
      if (resp) {
        return resp;
      }
      throw err;
    }

    // wait to try again, aborting if any signals fire.
    await sleep(retry.increment(resp), signals);
  }
}
