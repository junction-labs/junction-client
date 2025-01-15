// Types and wrappers for Junction FFI.
//
// The interfaces exposed from Rust are all fairly low level and
// require keeping opaque pointers to FFI types around so we can
// represent the Runtime etc. This module wraps them up into a
// much more native JS/TS experience.

import * as ffi from "./load.cjs";

declare module "./load.cjs" {
  interface Runtime { }
  interface Client { }
  interface EndpointHandle { }

  type EndpointResult = [EndpointHandle, EndpointProps];

  type Headers = Array<[string, string]>;

  function newRuntime(): Runtime;

  function newClient(
    rt: Runtime,
    adsServer: string,
    node: string,
    cluster: string,
  ): Promise<Client>;

  function resolveHttp(
    rt: Runtime,
    client: Client,
    method: string,
    url: string,
    headers: Headers,
  ): Promise<EndpointResult>;

  function reportStatus(
    rt: Runtime,
    client: Client,
    endpoint: EndpointHandle,
    status?: number,
    error?: string,
  ): Promise<EndpointResult>;
}

const defaultRuntime: ffi.Runtime = ffi.newRuntime();

/**
 * An error in the Junction client.
 */
export class JunctionError extends Error {
  constructor(message?: string) {
    super(message);
    this.name = "JunctionError";
  }
}

/**
 * Options for configuring a Junction client.
 */
export type ClientOpts = {
  /**
   * The URL of the Junction ADS server to connect to.
   */
  adsServer: string;

  /**
   * The name of the individual process running Junction. Should be unique to
   * this process.
   */
  nodeName?: string;

  /**
   * The cluster of nodes this client is part of.
   */
  clusterName?: string;
};

/**
 * Arguments to resolveHttp.
 */
export type ResolveHttpOpts = {
  /**
   * The HTTP method of the request. Defaults to `GET`.
   */
  method?: string;

  /**
   * The URL of the request, including the full path and any query parameters.
   */
  url: string;

  /**
   * The request headers.
   */
  headers?: HeadersInit;
};

/**
 * Arguments to `reportStatus`. Either `status` or `error` must be set.
 */
export type ReportStatusOpts = {
  /**
   * The status code of a complete HTTP request.
   */
  status?: number;

  /**
   * An error returned in place of an incomplete HTTP request.
   */
  error?: Error;
};

/**
 * A Junction retry policy.
 */
// TODO: this should be generated from junction-api
export interface RetryPolicy {
  attempts?: number;
  backoff?: number;
  codes?: [number];
}

/**
 * Junction timeouts.
 */
// TODO: this should be generated from junction-api
export interface Timeouts {
  request?: number;
  backendRequest?: number;
}

/**
 * A socket address.
 */
// TODO: this should be generated from junction-api
export interface SockAddr {
  address: string;
  port: number;
}

/**
 * A Junction endpoint.
 */
// TODO: this should be generated from junction-api
export interface EndpointProps {
  readonly scheme: string;
  readonly sockAddr: SockAddr;
  readonly hostname: string;
  readonly retry?: RetryPolicy;
  readonly timeouts?: Timeouts;
}

// symbols used as method names to hide the Endpoint inside Endpint.

const _buildEndpoint: unique symbol = Symbol();
const _getHandle: unique symbol = Symbol();

export class Endpoint implements EndpointProps {
  #handle: ffi.EndpointHandle;

  readonly scheme: string;
  readonly sockAddr: SockAddr;
  readonly hostname: string;
  readonly retry?: RetryPolicy;
  readonly timeouts?: Timeouts;

  private constructor(
    handle: ffi.EndpointHandle,
    { scheme, sockAddr, hostname, retry, timeouts }: EndpointProps,
  ) {
    this.#handle = handle;
    this.scheme = scheme;
    this.sockAddr = sockAddr;
    this.hostname = hostname;
    this.retry = retry;
    this.timeouts = timeouts;
  }

  // module-private
  static [_buildEndpoint](
    handle: ffi.EndpointHandle,
    props: EndpointProps,
  ): Endpoint {
    return new Endpoint(handle, props);
  }

  // module-private
  [_getHandle](): ffi.EndpointHandle {
    return this.#handle;
  }
}

/**
 * Convert an endpoint to a URL.
 */
export function endpointURL(e: Endpoint): URL {
  return new URL(`${e.scheme}://${e.sockAddr.address}:${e.sockAddr.port}`);
}

/**
 * A Junction client, used for resolving endpoint data and policy.
 */
export class Client {
  #runtime: ffi.Runtime;
  #client: ffi.Client;

  private constructor(rt: ffi.Runtime, client: ffi.Client) {
    this.#runtime = rt;
    this.#client = client;
  }

  /**
   * Build a new client from the given client opts.
   */
  static async build(opts: ClientOpts): Promise<Client> {
    const client = await ffi.newClient(
      defaultRuntime,
      opts.adsServer,
      opts.nodeName || "nodejs",
      opts.clusterName || "junction-node",
    );
    return new Client(defaultRuntime, client);
  }

  /**
   * Resolve an endpoint for an HTTP request.
   */
  async resolveHttp(options: ResolveHttpOpts): Promise<Endpoint> {
    try {
      const [handle, data] = await ffi.resolveHttp(
        this.#runtime,
        this.#client,
        options.method || "GET",
        options.url,
        toFfiHeaders(options.headers),
      );

      return Endpoint[_buildEndpoint](handle, data);
    } catch (err) {
      if (err instanceof Error) {
        throw new JunctionError(err.message);
      }
      throw err;
    }
  }

  /**
   * Report the status of an HTTP request.
   */
  async reportStatus(
    endpoint: Endpoint,
    opts: ReportStatusOpts,
  ): Promise<Endpoint> {
    try {
      const [handle, data] = await ffi.reportStatus(
        this.#runtime,
        this.#client,
        endpoint[_getHandle](),
        opts.status,
        opts.error?.message,
      );
      return Endpoint[_buildEndpoint](handle, data);
    } catch (err) {
      if (err instanceof Error) {
        throw new JunctionError(err.message);
      }
      throw err;
    }
  }
}

function toFfiHeaders(headers?: HeadersInit): ffi.Headers {
  if (Array.isArray(headers)) {
    return headers;
  }

  return [...new Headers(headers)];
}
