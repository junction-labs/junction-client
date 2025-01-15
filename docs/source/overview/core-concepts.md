# Concepts

## Resolution

At its heart Junction replaces DNS, except rather than  go from hostname to an
IP, it goes from hostname and header/query params to a list of IPs, and dynamic
configuration such as retry policies, timeouts, rate limits, and mTLS certs.

To implement this lookup Junction has two layers of indirection which allow for
configuration: Routes and Backends. The lookup flow then looks like the
following:

```
     Request                                              
        ?───► Lookup: Hostname + header/querystring params   
        │                                                 
        ▼                                                 
      Route ──────► Also determines: timeouts, retry policy
        ?───► Lookup: Weighting, Mirroring                   
        │                                                 
        ▼                                                 
 Service Backend ─► Also determines: mTLS policy           
        ?───► Lookup: load balancing algorithm               
        │                                                 
        ▼                                                 
  IP address list                                         
```

For those coming from the Kubernetes Gateway API, we choose the names routes and
backends to be exactly the same concepts as it's routes and BackendRefs. However
because Junction is very much targeted as service to service communication, some
things like ParentRefs are not carried across.

## Routes

A route is the client facing half of Junction, and contains most of the
things you'd traditionally find in a hand-rolled HTTP client - timeouts,
retries, URL rewriting and more. Routes match requests based on their
hostname, method, URL, and headers. 

## Hostnames

Junction's main purpose is service discovery, and just like with DNS, the major
input to a lookup is a hostname. However, unlike DNS which needs to handle the
scale of the Internet, Junction is aimed at problems where the entire set of
names can me kept in memory of a single server (at 1,000 bytes per record,
1,000,000 records is just 1 GiB).  Thus in junction there is no logic about
subdomains. Rather, you set up a Route on any hostname you want, and a Junction
will match it. 

One thing Junction does support is wildcard prefixes "*.mydomain.com", to allow
a single route to pick up dynamically allocated hostnames.

## Services

The Junction API is built around the idea that you're always routing requests to
a Service, which is an abstract representation of a place you might want traffic
to go. A Service can be anything, but to use one in Junction you need a way to
uniquely specify it. That could be anything from a DNS name someone else has
already set up to a Kubernetes Service in a cluster you've connected to
Junction.

## Backends

A Backend is a single port on a Service. Backend configuration gives you
control over the things you'd normally configure in a reverse proxy or a
traditional load balancer.
