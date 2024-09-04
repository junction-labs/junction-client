// This module is a cache that handles SotW XDS behavior. It's the guts of an
// ADS connection and tracks the current state of XDS resoureces, the references
// between resources, and builds internal client config on the fly. If you need
// to add or modify any XDS behavior at all, it's like you'll end up here.
//
// This cache is built entirely around being single-writer, even thought it's
// multi-reader and safe for concurrent reads. If you'd like to change that,
// it's likely you're going to rebuild the internals of the cache entirely.
//
// # Reference Tracking is XDS Subscription Tracking
//
// XDS SotW only ever issues deletes for LDS and CDS resources, so one of the
// most important jobs a cache has is tracking references to delete RDS/EDS
// objects when they're no longer referenced. Tracking the list of referenced
// names is also exactly what we need to be doing for subscription tracking -
// any client using this cache should be subscribing to all RDS resources
// referenced by existing LDS resources, all CDS resources referenced by LDS and
// RDS resources, and so on.
//
// The current implementation manages these references as a `petgraph` graph
// owned by the cache's single writer, and not shared with any of the readers.
// This means that reference data can be stored relatively cheaply (without
// duplicating XDS protobufs) and modified without any coordination between
// reader and writer threads/tasks. It does however mean that there are two
// places to track state, and the writer is now responsible for keeping them
// in sync.
//
// # Reference Tracking is Garbage Collection
//
// Tracking a graph of objects that reference each other and removing the
// unused ones should sound a lot like garbage collection to you - it is
// exactly garbage collection.
//
// Using a graph internally to track object references means that it's
// relatively easy to run a simple mark-and-sweep over the current state of the
// graph, and having a single writer own the reference graph means that writes
// pay the cost of collecting XDS garbage while readers can keep reading
// uninterrupted. In practice, we expect the number of XDS objects to be
// relatively small, and this cost to be low, but this is the right tradeoff
// even if collection does become more expensive.
//
// # User Input is GC Roots
//
// This cache models our entire interaction with ADS, so we need a notion of
// what to request on behalf of all the clients reading config from it. As of
// now, that generally takes the form of LDS resources - a client makes a
// request to a URL, and the hostname of that URL is now a Listener we'd like to
// subscribe to.
//
// That naturally makes LDS/CDS resources our GC roots - when someone explicitly
// subscribes to a Lister (and maybe sets up default routes with targets)
// Listener names become roots in our GC graph. We follow all references
// downstream to other XDS objects to decide what to drop and keep, and even if
// the ADS server tells us those names are temporarily gone, the cache should
// keep trying to subscribe to them - after all, a user has expressed interest.
//
// All of this assumes that there are no wildcard subscriptions - the huge
// assumption here is that subscriptions are going to come in as explicit DNS
// names, and not as wildcards - it doesn't really mean anything to try to make
// an http request to `*.foo.local`. If this changes, we'll have to re-evaluate
// our model of the world.
//
// # TODO
//
// - Track incoming resource versions and last update. When dumping resources
//   for CSDS, there's the opportunity to show both of those pieces of info to
//   a caller, which should be extremely useful for debugging.
//
// - Use the resource graph to track when resources were requested and whether
//   or not they should be considered missing. The XDS protocol documentation
//   recommends a 15 second timeout. This probably involves also inserting
//   markers/tombstones in the data cache.
//   https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol#knowing-when-a-requested-resource-does-not-exist.
//

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crossbeam_skiplist::SkipMap;
use enum_map::EnumMap;
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{self, Visitable},
    Direction,
};
use xds_api::pb::envoy::{
    config::{
        cluster::v3::{self as xds_cluster},
        endpoint::v3::{self as xds_endpoint},
        listener::v3::{self as xds_listener},
        route::v3::{self as xds_route},
    },
    extensions::filters::network::http_connection_manager::v3 as xds_http,
};
use xds_api::pb::google::protobuf;

// collect garbage like a little tracing gc.
//
// this traversal also asserts that the GC graph is a DAG, and will
// panic if it finds cycles. the ref graph is directed, and there
// should be no cycles by design - there are no self-type references,
// and no references "up" the xds type hierarchy.
//
// all listeners are GC roots. walk the graph once to find them here,
// instead of storing them. storing them involves keeping a secondary
// index from name to graph indices, but indices are unstable.
//
// this is mostly a handful of DFS passes on the graph, but with an
// early exit if we've already marked a node.

use super::resources::{
    api_listener, ApiListener, ApiListenerData, Cluster, ClusterData, LoadAssignment, ResourceType,
    ResourceTypeSet, ResourceVec, RouteConfig,
};

trait CacheEntry<T: prost::Name> {
    fn xds(&self) -> &T;
}

macro_rules! impl_cache_entry {
    ($entry_ty:ty, $xds_ty:ty) => {
        impl CacheEntry<$xds_ty> for $entry_ty {
            fn xds(&self) -> &$xds_ty {
                &self.xds
            }
        }
    };
}

impl_cache_entry!(ApiListener, xds_http::HttpConnectionManager);
impl_cache_entry!(RouteConfig, xds_route::RouteConfiguration);
impl_cache_entry!(Cluster, xds_cluster::Cluster);
impl_cache_entry!(LoadAssignment, xds_endpoint::ClusterLoadAssignment);

type ResourceMap<T> = SkipMap<String, T>;

fn names<T, B: FromIterator<String> + Sized>(m: &ResourceMap<T>) -> B {
    m.iter().map(|e| e.key().clone()).collect()
}

fn remove_all<T: Send + 'static>(m: &ResourceMap<T>, names: &[String]) {
    for name in names {
        m.remove(name);
    }
}

fn is_changed<V, X>(m: &ResourceMap<V>, name: &str, t: &X) -> bool
where
    V: CacheEntry<X>,
    X: PartialEq + prost::Name,
{
    let Some(entry) = m.get(name) else {
        return true;
    };

    let entry_xds = entry.value().xds();
    entry_xds != t
}

/// A read-only handle to a [Cache]. `CacheReader`s are meant to passed around
/// and shared and are cheap to clone.
#[derive(Default, Clone)]
pub(crate) struct CacheReader {
    data: Arc<CacheData>,
}

/// A complete-enough view of config for routing.
///
/// Configuration is complete-enough if a full routing table can be
/// assembled. There is no guarantee that the target services are available
/// yet.
#[allow(unused)]
#[derive(Debug)]
pub(crate) struct ConfigView {
    pub routes: Arc<Vec<crate::config::Route>>,
    pub load_balancers: BTreeMap<String, Arc<crate::config::LoadBalancer>>,
    pub endpoints: BTreeMap<String, Arc<crate::config::EndpointGroup>>,
}

impl CacheReader {
    pub(crate) fn iter_any(&self) -> impl Iterator<Item = (String, protobuf::Any)> + '_ {
        let listener_iter = self.data.listeners.iter().map(|entry| {
            let name = entry.key().clone();
            let api_listener =
                protobuf::Any::from_msg(entry.value().xds()).expect("generated invalid protobuf");

            let listener = xds_listener::Listener {
                api_listener: Some(xds_listener::ApiListener {
                    api_listener: Some(api_listener),
                }),
                ..Default::default()
            };

            let any = protobuf::Any::from_msg(&listener).expect("generated invalid protobuf");
            (name, any)
        });

        macro_rules! any_iter {
            ($field:ident) => {
                self.data.$field.iter().map(|entry| {
                    let name = entry.key().clone();
                    let any = protobuf::Any::from_msg(entry.value().xds())
                        .expect("generated invalid protobuf");
                    (name, any)
                })
            };
        }

        listener_iter
            .chain(any_iter!(route_configs))
            .chain(any_iter!(clusters))
            .chain(any_iter!(load_assignments))
    }

    /// Get the routing table for a hostname.
    ///
    /// Will return `None` if the routing table does not yet exist in cache,
    /// either because it hasn't yet been pulled from the ADS servcer or it
    /// doesn't exist.
    pub(crate) fn get_routes(&self, name: &str) -> Option<Arc<Vec<crate::config::Route>>> {
        let listener = self.data.listeners.get(name)?;

        let routes = match &listener.value().data {
            ApiListenerData::RouteConfig { name } => {
                let rc = self.data.route_configs.get(name.as_str())?;

                rc.value().routes.clone()
            }
            ApiListenerData::Inlined { routes, .. } => routes.clone(),
        };
        Some(routes)
    }

    /// Get the load balancer and endpoint group for a routing target.
    ///
    /// May return no data, only a load balancer, or a load balancer and an
    /// endpoint group if configuration hasn't yet been fetched from the ADS
    /// server or if the resources don't exist.
    pub(crate) fn get_target(
        &self,
        name: &str,
    ) -> (
        Option<Arc<crate::config::LoadBalancer>>,
        Option<Arc<crate::config::EndpointGroup>>,
    ) {
        let Some(cluster) = self.data.clusters.get(name) else {
            return (None, None);
        };

        let lb = Some(cluster.value().load_balancer.clone());
        match &cluster.value().data {
            ClusterData::Inlined { endpoint_group, .. } => (lb, Some(endpoint_group.clone())),
            ClusterData::LoadAssignment { name } => {
                let load_assignment = self.data.load_assignments.get(name.as_str());
                let endpoint_group = load_assignment.map(|e| e.value().endpoint_group.clone());
                (lb, endpoint_group)
            }
        }
    }

    /// Get a [complete-enough view][ConfigView] of configuration for a hostname to handle a
    /// request. Returns `None` if configuration is not available for this name.
    #[allow(unused)]
    pub(crate) fn get(&self, name: &str) -> Option<ConfigView> {
        let listener = self.data.listeners.get(name)?;

        let (routes, cluster_names) = match &listener.value().data {
            ApiListenerData::RouteConfig { name } => {
                let rc = self.data.route_configs.get(name.as_str())?;

                let routes = rc.value().routes.clone();
                let cluster_name = rc.value().clusters.clone();
                (routes, cluster_name)
            }
            ApiListenerData::Inlined { routes, clusters } => (routes.clone(), clusters.clone()),
        };

        let mut load_balancers = BTreeMap::new();
        let mut endpoints = BTreeMap::new();

        for cluster_name in cluster_names {
            let Some(cluster) = self.data.clusters.get(cluster_name.as_str()) else {
                continue;
            };

            load_balancers.insert(
                cluster_name.as_string(),
                cluster.value().load_balancer.clone(),
            );

            match &cluster.value().data {
                ClusterData::Inlined {
                    name,
                    endpoint_group,
                } => {
                    endpoints.insert(name.clone(), endpoint_group.clone());
                }
                ClusterData::LoadAssignment { name } => {
                    if let Some(cla) = self.data.load_assignments.get(name.as_str()) {
                        endpoints.insert(name.as_string(), cla.value().endpoint_group.clone());
                    }
                }
            };
        }

        Some(ConfigView {
            routes,
            load_balancers,
            endpoints,
        })
    }
}

/// Shared XDS and client configuration for a SotW XDS client.
///
/// A [Cache] is built on the fly by a single writer, with any number of
/// [CacheReader]s providing read-only access. Readers do not necessarily get a
/// consistent snapshot of configuration (XDS doesn't define what that might
/// even mean!) - see [CacheReader] for a more thorough explanation.
///
/// A `Cache` handles tracking the current state of a XDS resources, validating
/// and converting them to internal configuration, and tracking any references
/// to other resource types.
#[derive(Default, Debug)]
pub(super) struct Cache {
    refs: DiGraph<GCData, ()>,
    data: Arc<CacheData>,
}

#[derive(Debug)]
struct GCData {
    name: String,
    resource_type: ResourceType,
    pinned: bool,
}

#[derive(Debug, Default)]
struct CacheData {
    listeners: ResourceMap<ApiListener>,
    route_configs: ResourceMap<RouteConfig>,
    clusters: ResourceMap<Cluster>,
    load_assignments: ResourceMap<LoadAssignment>,
}

// public API
impl Cache {
    pub fn reader(&self) -> CacheReader {
        CacheReader {
            data: self.data.clone(),
        }
    }

    pub fn subscriptions(&self, resource_type: ResourceType) -> Vec<String> {
        let weights = self
            .refs
            .node_weights()
            .filter(|n| n.resource_type == resource_type);
        weights.map(|n| n.name.clone()).collect()
    }

    pub fn insert(&mut self, resources: ResourceVec) -> (ResourceTypeSet, Vec<crate::xds::Error>) {
        let (changed, errs) = match resources {
            ResourceVec::Listener(ls) => self.insert_listeners(ls),
            ResourceVec::RouteConfiguration(rcs) => self.insert_route_configs(rcs),
            ResourceVec::Cluster(cs) => self.insert_clusters(cs),
            ResourceVec::ClusterLoadAssignment(clas) => self.insert_load_assignments(clas),
        };

        if !changed.is_empty() {
            self.collect();
        }

        (changed, errs)
    }

    /// Unsubscribe from an XDS resource and explicitly delete it from cache.
    pub fn delete(&mut self, resource_type: ResourceType, name: &str) -> bool {
        if !self.delete_ref(resource_type, name) {
            return false;
        }

        match resource_type {
            ResourceType::Cluster => {
                self.data.clusters.remove(name);
            }
            ResourceType::ClusterLoadAssignment => {
                self.data.load_assignments.remove(name);
            }
            ResourceType::Listener => {
                self.data.listeners.remove(name);
            }
            ResourceType::RouteConfiguration => {
                self.data.route_configs.remove(name);
            }
        }

        true
    }

    /// Explicitly subscribe to an XDS resource.
    pub fn subscribe(&mut self, resource_type: ResourceType, name: &str) -> bool {
        let (node, created) = self.find_or_create_ref(resource_type, name);
        self.pin_ref(node);
        created
    }
}

macro_rules! try_continue {
    ($e:expr, $errors:expr) => {
        match $e {
            Ok(ok) => ok,
            Err(e) => {
                $errors.push(e);
                continue;
            }
        }
    };
}

/// safety: `petgraph` NodeIndexes are unstable - when removing a node, it may
/// invalidate an index we previously looked up. It is not sound to hold on to
/// an index from before a deletion took place.
///
/// In practice, this means DO NOT save NodeIndexes anywhere. Use them as locals
/// but don't store them in struct fields etc. This makes it impractical to build
/// e.g. a lookup from (ResourceType, Name) -> NodeIndex. If that becomes necessary
/// petgraph is working on a StableGraph where NodeIndexes are never invalidated.
impl Cache {
    fn collect(&mut self) {
        use visit::{Control, DfsEvent};

        // walk the GC graph, keeping the set of the reachable nodes.
        //
        // lean on petgraph's Control to only visit each node once - because
        // the ref graph must be a DAG, we can skip marking nodes twice and
        // emit Control::Prune every time we see a node we've already seen.
        let mut reachable = self.refs.visit_map();
        visit::depth_first_search(&self.refs, self.gc_roots(), |event| -> Control<()> {
            match event {
                DfsEvent::Discover(n, _) => {
                    if reachable.contains(n.index()) {
                        return Control::Prune;
                    }
                    reachable.insert(n.index());
                }
                DfsEvent::BackEdge(_, _) => {
                    panic!("GC graph is not a DAG")
                }
                _ => (),
            };

            Control::Continue
        });

        // find unreachable names and drop their actual data
        let unreachable_nodes = self
            .refs
            .node_indices()
            .filter(|n| !reachable.contains(n.index()));

        let mut unreachable_names: EnumMap<ResourceType, Vec<String>> = EnumMap::default();
        for n in unreachable_nodes {
            let n = &self.refs[n];
            unreachable_names[n.resource_type].push(n.name.to_string());
        }

        for (resource_type, names) in unreachable_names.into_iter() {
            match resource_type {
                ResourceType::Cluster => remove_all(&self.data.clusters, &names),
                ResourceType::ClusterLoadAssignment => {
                    remove_all(&self.data.load_assignments, &names)
                }
                ResourceType::Listener => remove_all(&self.data.listeners, &names),
                ResourceType::RouteConfiguration => remove_all(&self.data.route_configs, &names),
            }
        }

        // safety: no longer holding any NodeIndexes, safe to remove and invalidate
        self.refs.retain_nodes(|_, n| reachable.contains(n.index()));
    }

    fn insert_listeners(
        &mut self,
        listeners: Vec<xds_listener::Listener>,
    ) -> (ResourceTypeSet, Vec<crate::xds::Error>) {
        let mut changed = ResourceTypeSet::default();
        let mut errors = Vec::new();
        let mut to_remove: BTreeSet<_> = names(&self.data.listeners);

        for listener in listeners {
            to_remove.remove(&listener.name);

            // listeners are weird and we have to unzip the API listener before doing anything.
            let api_listener = try_continue!(api_listener(&listener), errors);

            if is_changed(&self.data.listeners, &listener.name, &api_listener) {
                let api_listener: ApiListener =
                    try_continue!((listener.name, api_listener).try_into(), errors);

                // remove the downstream route config ref and replace it with a new one
                let (node, _) = self.find_or_create_ref(ResourceType::Listener, &api_listener.name);
                self.reset_ref(node);

                match &api_listener.data {
                    ApiListenerData::RouteConfig { name } => {
                        let (rc_node, created) = self
                            .find_or_create_ref(ResourceType::RouteConfiguration, name.as_str());
                        self.refs.update_edge(node, rc_node, ());

                        if created {
                            changed.insert(ResourceType::RouteConfiguration);
                        }
                    }
                    ApiListenerData::Inlined { clusters, .. } => {
                        let mut clusters_changed = false;

                        for cluster in clusters {
                            let (cluster_node, created) =
                                self.find_or_create_ref(ResourceType::Cluster, cluster.as_str());
                            self.refs.update_edge(node, cluster_node, ());
                            clusters_changed |= created;
                        }

                        if clusters_changed {
                            changed.insert(ResourceType::Cluster);
                        }
                    }
                }

                // insert data into cache
                self.data
                    .listeners
                    .insert(api_listener.name.clone(), api_listener);
                changed.insert(ResourceType::Listener);
            }
        }

        // safety: the refs graph should be in sync with the names of clusters,
        // so panic here if we try to remove a name that didn't exist in the ref
        // graph.
        //
        // this guarantee comes from there only being a single cache writer.
        for name in to_remove {
            changed.insert(ResourceType::Listener);
            self.delete_ref(ResourceType::Listener, &name);
            self.data.listeners.remove(&name);
        }

        (changed, errors)
    }

    fn insert_clusters(
        &mut self,
        clusters: Vec<xds_cluster::Cluster>,
    ) -> (ResourceTypeSet, Vec<crate::xds::Error>) {
        let mut changed = ResourceTypeSet::default();
        let mut errors = Vec::new();
        let mut to_remove: BTreeSet<_> = names(&self.data.clusters);

        for cluster in clusters {
            to_remove.remove(&cluster.name);

            if is_changed(&self.data.clusters, &cluster.name, &cluster) {
                // try to find this cluster in the ref graph. it's possible it's
                // now from a stale subscription.
                let Some(node) = self.find_ref(ResourceType::Cluster, &cluster.name) else {
                    continue;
                };

                let cluster: Cluster = try_continue!(cluster.try_into(), errors);

                // remove the old CLA edge and replace it with a new one.
                self.reset_ref(node);
                match &cluster.data {
                    ClusterData::Inlined { .. } => (/* do nothing */),
                    ClusterData::LoadAssignment { name } => {
                        changed.insert(ResourceType::ClusterLoadAssignment);
                        let (cla_node, _) = self
                            .find_or_create_ref(ResourceType::ClusterLoadAssignment, name.as_str());
                        self.refs.update_edge(node, cla_node, ());
                    }
                }

                // insert the cluster
                self.data.clusters.insert(cluster.name.clone(), cluster);

                changed.insert(ResourceType::Cluster);
            }
        }

        // safety: the refs graph should be in sync with the names of clusters,
        // so panic here if we try to remove a name that didn't exist in the ref
        // graph.
        //
        // this guarantee comes from there only being a single cache writer.
        for name in to_remove {
            changed.insert(ResourceType::Cluster);
            self.delete_ref(ResourceType::Cluster, &name);
            self.data.clusters.remove(&name);
        }

        (changed, errors)
    }

    fn insert_route_configs(
        &mut self,
        route_configs: Vec<xds_route::RouteConfiguration>,
    ) -> (ResourceTypeSet, Vec<crate::xds::Error>) {
        let mut errors = Vec::new();
        let mut changed = ResourceTypeSet::default();

        for route_config in route_configs {
            if is_changed(&self.data.route_configs, &route_config.name, &route_config) {
                changed.insert(ResourceType::RouteConfiguration);

                // it's possible that we got delivered a RouteConfiguration that
                // we don't have a subscription for (either because of a silly
                // ADS server or a race).
                //
                // if we did, just ignore the node.
                let Some(node) =
                    self.find_ref(ResourceType::RouteConfiguration, &route_config.name)
                else {
                    continue;
                };

                let route_config: RouteConfig = try_continue!(route_config.try_into(), errors);

                // add an edge for every cluster reference in this RouteConfig
                self.reset_ref(node);
                for cluster in &route_config.clusters {
                    let (cluster, _) =
                        self.find_or_create_ref(ResourceType::Cluster, cluster.as_str());
                    self.refs.update_edge(node, cluster, ());
                }

                // actually insert the route config
                self.data
                    .route_configs
                    .insert(route_config.name.clone(), route_config);
            }
        }

        (changed, errors)
    }

    fn insert_load_assignments(
        &mut self,
        load_assignments: Vec<xds_endpoint::ClusterLoadAssignment>,
    ) -> (ResourceTypeSet, Vec<crate::xds::Error>) {
        let mut errors = Vec::new();
        let mut changed = ResourceTypeSet::default();

        for load_assignment in load_assignments {
            if is_changed(
                &self.data.load_assignments,
                &load_assignment.cluster_name,
                &load_assignment,
            ) {
                let Some(cla_node) = self.find_ref(
                    ResourceType::ClusterLoadAssignment,
                    &load_assignment.cluster_name,
                ) else {
                    tracing::trace!(
                        cluster_name = load_assignment.cluster_name,
                        "ClusterLoadAssignment missing ref, skipping"
                    );
                    continue;
                };

                // use the GC graph to pull a ref to the parent cluster. with
                // the xdstp:// scheme the name of a Cluster and a
                // ClusterLoadAssignment may not be the same, so using the
                // GC graph is necessary.
                let cluster_discovery_type = {
                    let cluster_node = self
                        .refs
                        .neighbors_directed(cla_node, Direction::Incoming)
                        .next()
                        .expect("GC leak: ClusterLoadAssignment must have a parent cluster");
                    let cluster = self
                        .data
                        .clusters
                        .get(&self.refs[cluster_node].name)
                        .expect("GC leak: parent Cluster was removed from cache");
                    cluster.value().discovery_type
                };

                let load_assignment: LoadAssignment =
                    try_continue!((cluster_discovery_type, load_assignment).try_into(), errors);
                self.data
                    .load_assignments
                    .insert(load_assignment.name.clone(), load_assignment);

                changed.insert(ResourceType::ClusterLoadAssignment);
            }
        }

        (changed, errors)
    }

    /// Return the node indices of all GC roots. GC roots are either Listeners
    /// or are explicitly pinned.
    ///
    /// Safety: NodeIndexes are not stable across deletions. This vec is not
    /// safe to store long term or between collections.
    fn gc_roots(&self) -> Vec<NodeIndex> {
        self.refs
            .node_indices()
            .filter(|idx| {
                let nw = &self.refs[*idx];
                nw.pinned || nw.resource_type == ResourceType::Listener
            })
            .collect()
    }

    /// Delete a ref from the GC map.
    ///
    /// TODO: should this returned deleted/pinned/notfound?
    fn delete_ref(&mut self, resource_type: ResourceType, name: &str) -> bool {
        match self.find_ref(resource_type, name) {
            Some(node) => {
                if !self.refs[node].pinned {
                    self.refs.remove_node(node);
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// Find or create a GC ref for the given name or resource type.
    ///
    /// New GC refs are not marked `reachable` by default.
    fn find_or_create_ref(&mut self, resource_type: ResourceType, name: &str) -> (NodeIndex, bool) {
        if let Some(node) = self.find_ref(resource_type, name) {
            return (node, false);
        }

        let node = self.refs.add_node(GCData {
            name: name.to_string(),
            resource_type,
            pinned: false,
        });
        (node, true)
    }

    /// Find a GC ref with the given resource type or name.
    fn find_ref(&self, resource_type: ResourceType, name: &str) -> Option<NodeIndex> {
        self.refs.node_indices().find(|n| {
            let n = &self.refs[*n];
            n.resource_type == resource_type && n.name == name
        })
    }

    fn pin_ref(&mut self, node: NodeIndex) {
        self.refs[node].pinned = true;
    }

    /// Remove all of a GC ref's outgoing edges.
    fn reset_ref(&mut self, node: NodeIndex) {
        let neighbors: Vec<_> = self
            .refs
            .neighbors_directed(node, Direction::Outgoing)
            .collect();

        for n in neighbors {
            if let Some((edge, _)) = self.refs.find_edge_undirected(node, n) {
                self.refs.remove_edge(edge);
            };
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::xds::test as xds_test;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn assert_reader_send_sync() {
        assert_send::<CacheReader>();
        assert_sync::<CacheReader>();
    }

    fn assert_insert(
        (changed, errors): (ResourceTypeSet, Vec<crate::xds::Error>),
    ) -> ResourceTypeSet {
        assert!(errors.is_empty());
        changed
    }

    #[test]
    fn test_insert_listener_inline_route_config() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "vhost1.example.local",
                ["listener.example.local"],
                [xds_test::route!(default "cluster.example.local")],
            )],
        )])));

        assert!(cache.data.listeners.get("listener.example.local").is_some());
        assert!(cache.data.route_configs.is_empty());
    }

    #[test]
    fn test_insert_listener_rds() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![
            xds_test::listener!("listener1.example.local", "rc1.example.local"),
            xds_test::listener!("listener2.example.local", "rc2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener1.example.local", "listener2.example.local"],
        );
        assert!(cache.data.route_configs.is_empty());

        assert_insert(cache.insert(ResourceVec::RouteConfiguration(vec![
            xds_test::route_config!(
                "rc1.example.local",
                [xds_test::vhost!(
                    "vhost1.example.local",
                    ["listener.example.local"],
                    [xds_test::route!(default "cluster1.example.local")],
                )]
            ),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener1.example.local", "listener2.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.route_configs),
            vec!["rc1.example.local"],
        );

        assert_insert(cache.insert(ResourceVec::RouteConfiguration(vec![
            xds_test::route_config!(
                "rc2.example.local",
                [xds_test::vhost!(
                    "vhost1.example.local",
                    ["listener.example.local"],
                    [xds_test::route!(default "cluster1.example.local")],
                )]
            ),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener1.example.local", "listener2.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.route_configs),
            vec!["rc1.example.local", "rc2.example.local"],
        );
    }

    #[test]
    fn test_insert_cluster_eds() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["listener.example.local"],
                [xds_test::route!(default "cluster1.example.local")],
            )],
        )])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
        ])));

        assert!(cache.data.listeners.get("listener.example.local").is_some());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache.data.clusters.get("cluster1.example.local").is_some());
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_insert_cla() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [
                    xds_test::route!(header "x-staging" => "cluster2.example.local"),
                    xds_test::route!(default "cluster1.example.local"),
                ],
            )],
        )])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_insert(
            cache.insert(ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster1.example.local" => {
                    "zone1" => ["1.1.1.1"]
                }
            )])),
        );

        assert!(cache.data.listeners.get("listener.example.local").is_some());
        assert!(cache.data.route_configs.is_empty());
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.load_assignments),
            vec!["cluster1.example.local"],
        );

        assert_insert(
            cache.insert(ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster2.example.local" => {
                    "zone2" => ["2.2.2.2"]
                }
            )])),
        );

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.load_assignments),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
    }

    #[test]
    fn test_insert_cla_missing_ref() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [
                    xds_test::route!(header "x-staging" => "cluster2.example.local"),
                    xds_test::route!(default "cluster1.example.local"),
                ],
            )],
        )])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        // add a CLA referencing a cluster that doesn't exist. it should just fall on the floor
        let (changed, errors) =
            cache.insert(ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster3.example.local" => {
                    "zone2" => ["2.2.2.2"]
                }
            )]));

        assert!(changed.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn test_insert_deletes_listeners() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "nginx.default.local" => [xds_test::vhost!(
                "default",
                ["nginx.default.local"],
                [xds_test::route!(default "nginx.default.local")],
            )],
        )])));

        assert!(cache.data.listeners.get("nginx.default.local").is_some());

        assert_insert(cache.insert(ResourceVec::Listener(Vec::new())));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(cache.refs.node_count(), 0);
    }

    #[test]
    fn test_insert_deletes_clusters() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [
                    xds_test::route!(header "x-staging" => "cluster2.example.local"),
                    xds_test::route!(default "cluster1.example.local"),
                ],
            )],
        )])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster2.example.local"],
        );

        assert_insert(cache.insert(ResourceVec::Cluster(vec![])));
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_deletes_keep_subscriptions() {
        let mut cache = Cache::default();

        cache.subscribe(ResourceType::Listener, "listener.example.local");
        cache.subscribe(ResourceType::Cluster, "cluster1.example.local");
        cache.subscribe(ResourceType::Cluster, "cluster2.example.local");

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [
                    xds_test::route!(header "x-staging" => "cluster2.example.local"),
                    xds_test::route!(default "cluster1.example.local"),
                ],
            )],
        )])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );

        // delete everything
        assert_insert(cache.insert(ResourceVec::Listener(vec![])));
        assert_insert(cache.insert(ResourceVec::Cluster(vec![])));

        // subscriptions should still exist, but data should be gone
        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.clusters.is_empty());
        assert_eq!(
            cache.subscriptions(ResourceType::Listener),
            vec!["listener.example.local"],
        );
        assert_eq!(
            cache.subscriptions(ResourceType::Cluster),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
    }

    #[test]
    fn test_insert_out_of_order() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Cluster(vec![xds_test::cluster!(
            inline "cluster1.example.local" => {
            "zone1" => ["1.1.1.1", "2.2.2.2"],
            "zone2" => ["3.3.3.3"]
        })])));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.clusters.is_empty());

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [xds_test::route!(default "cluster1.example.local")],
            )],
        )])));

        assert!(cache.data.listeners.get("listener.example.local").is_some());
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_cache_gc_simple() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local",
            "rc.example.local"
        )])));

        assert_insert(cache.insert(ResourceVec::RouteConfiguration(vec![
            xds_test::route_config!(
                "rc.example.local",
                [xds_test::vhost!(
                    "vhost1.example.local",
                    ["listener.example.local"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example.local"),
                        xds_test::route!(default "cluster1.example.local"),
                    ],
                )]
            ),
        ])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.route_configs),
            vec!["rc.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
        assert!(cache.data.load_assignments.is_empty());

        // should have gc refs for the CLAs we're expecting.
        //
        // listener, rc,  2 * cluster, 2 * cla
        assert_eq!(cache.refs.node_count(), 6);

        // delete the listener
        assert_insert(cache.insert(ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache.data.clusters.is_empty());
        assert!(cache.data.load_assignments.is_empty());
        assert_eq!(cache.refs.node_count(), 0);
    }

    #[test]
    fn test_cache_gc_update_rds() {
        let mut cache = Cache::default();

        // swap the routeconfig for a listener, poitn to the same clusters
        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local",
            "rc1.example.local"
        )])));

        assert_insert(cache.insert(ResourceVec::RouteConfiguration(vec![
            xds_test::route_config!(
                "rc1.example.local",
                [xds_test::vhost!(
                    "vhost1.example.local",
                    ["listener.example.local"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example.local"),
                        xds_test::route!(default "cluster1.example.local"),
                    ],
                )]
            ),
        ])));

        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        // we should have listener -> rc1 -> {cluster1, cluster2}
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.route_configs),
            vec!["rc1.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );
        assert!(cache.data.load_assignments.is_empty());

        // update the targets for rc1.
        //
        // should now have listener -> rc1 -> cluster1
        assert_insert(cache.insert(ResourceVec::RouteConfiguration(vec![
            xds_test::route_config!(
                "rc1.example.local",
                [xds_test::vhost!(
                    "vhost1.example.local",
                    ["listener.example.local"],
                    [xds_test::route!(default "cluster1.example.local"),],
                )]
            ),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.route_configs),
            vec!["rc1.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local"],
        );
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_cache_gc_pinned() {
        let mut cache = Cache::default();

        // pinning should let us insert a cluster, but only that cluster
        cache.subscribe(ResourceType::Cluster, "cluster1.example.local");
        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local"],
        );

        // add a listener that references both cluster1 and cluster2
        assert_insert(cache.insert(ResourceVec::Listener(vec![xds_test::listener!(
            "listener.example.local" => [xds_test::vhost!(
                "default",
                ["*"],
                [
                    xds_test::route!(header "x-staging" => "cluster2.example.local"),
                    xds_test::route!(default "cluster1.example.local"),
                ],
            )],
        )])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local"],
        );

        // add both clusters
        assert_insert(cache.insert(ResourceVec::Cluster(vec![
            xds_test::cluster!(eds "cluster1.example.local"),
            xds_test::cluster!(eds "cluster2.example.local"),
        ])));

        assert_eq!(
            names::<_, Vec<_>>(&cache.data.listeners),
            vec!["listener.example.local"],
        );
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local", "cluster2.example.local"],
        );

        // remove the listener, cluster1 should stay pinned
        assert_insert(cache.insert(ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            names::<_, Vec<_>>(&cache.data.clusters),
            vec!["cluster1.example.local"],
        );
    }
}
