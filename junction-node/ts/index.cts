// The CJS entry point for junction-labs/client.

// pull in the ffi addon but don't export it directly. it's got some rough edges
// we hide with this layer
import * as ffi from './load.cjs';

// types for the ffi module, since they're not declared for us.
declare module "./load.cjs" {
  interface Runtime { }
  interface Client { }
  interface EndpointHandle { }

  type EndpointResult = [EndpointHandle, Endpoint];

  type Headers = Array<[string, string]>;

  function newRuntime(): Runtime;
  function newClient(rt: Runtime, adsServer: string, node: string, cluster: string): Promise<Client>;
  function resolveHttp(rt: Runtime, client: Client, method: String, url: String, headers: Headers): Promise<EndpointResult>;
  function reportStatus(rt: Runtime, client: Client, endpoint: EndpointHandle, status?: number, error?: string): Promise<EndpointResult>;
}

const defaultRuntime: ffi.Runtime = ffi.newRuntime();

fetch

export type ClientOpts = {
  adsServer: string,
  nodeName?: string,
  clusterName?: string,
}

export type ResolveHttpOpts = {
  method?: string,
  url: string,
  headers?: HeadersInit,
}

export type ReportStatusOpts = {
  code?: number,
  error?: string,
}

export interface RetryPolicy {
  attempts?: number,
  backoff?: [number, number],
  codes?: [number],
}

export interface Timeouts {
  request?: [number, number],
  backendRequest?: [number, number],
}

export interface Endpoint {
  readonly scheme: string,
  readonly address: string,
  readonly port: number,
  readonly hostname: string,
  readonly retryPolicy?: RetryPolicy,
  readonly timeouts?: Timeouts,
}

interface PrivateEndpoint extends Endpoint {
  [_FFI_ENDPOINT]: ffi.EndpointHandle
}

const _FFI_RT: unique symbol = Symbol();
const _FFI_CLIENT: unique symbol = Symbol();
const _FFI_ENDPOINT: unique symbol = Symbol();

export class Client {
  [_FFI_RT]: ffi.Runtime
  [_FFI_CLIENT]: ffi.Client

  private constructor(rt: ffi.Runtime, client: ffi.Client) {
    this[_FFI_RT] = rt
    this[_FFI_CLIENT] = client
  }

  static async build(opts: ClientOpts): Promise<Client> {
    let client = await ffi.newClient(
      defaultRuntime,
      opts.adsServer,
      opts.nodeName || "nodejs",
      opts.clusterName || "junction-node",
    )
    return new Client(defaultRuntime, client)
  }

  async resolveHttp(options: ResolveHttpOpts): Promise<Endpoint> {
    let [handle, data] = await ffi.resolveHttp(
      this[_FFI_RT],
      this[_FFI_CLIENT],
      options.method || "GET",
      options.url,
      toFfiHeaders(options.headers),
    );

    let endpoint: PrivateEndpoint = {
      [_FFI_ENDPOINT]: handle,
      ...data,
    }
    return endpoint;
  }

  async reportStatus(endpoint: Endpoint, opts: ReportStatusOpts): Promise<Endpoint> {
    if (!hasPrivateEndpoint(endpoint)) {
      throw new TypeError("endpoint is missing Junction core data");
    }

    let [handle, data] = await ffi.reportStatus(
      this[_FFI_RT],
      this[_FFI_CLIENT],
      endpoint[_FFI_ENDPOINT],
      opts.code,
      opts.error,
    );

    let newEndpoint: PrivateEndpoint = {
      [_FFI_ENDPOINT]: handle,
      ...data
    };
    return newEndpoint
  }
}

function toFfiHeaders(headers?: HeadersInit): ffi.Headers {
  if (headers instanceof Array) {
    return headers
  }

  return [...new Headers(headers)]
}

function hasPrivateEndpoint(endpoint: Endpoint): endpoint is PrivateEndpoint {
  return (endpoint as PrivateEndpoint)[_FFI_ENDPOINT] !== undefined
}
