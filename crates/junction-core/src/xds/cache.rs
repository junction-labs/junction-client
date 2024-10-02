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

use crossbeam_skiplist::SkipMap;
use enum_map::EnumMap;
use junction_api_types::http::Route;
use junction_api_types::shared::Target;
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{self, Visitable},
    Direction,
};
use prost::Name;
use std::collections::BTreeSet;
use std::sync::Arc;
use xds_api::pb::envoy::config::{
    cluster::v3::{self as xds_cluster},
    endpoint::v3::{self as xds_endpoint},
    listener::v3::{self as xds_listener},
    route::v3::{self as xds_route},
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

use crate::config::BackendLb;

use super::resources::{
    ApiListener, ApiListenerRouteConfig, Cluster, ClusterEndpointData, LoadAssignment,
    ResourceType, ResourceTypeSet, ResourceVec, RouteConfig,
};
use super::ResourceVersion;

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    pub version: ResourceVersion,
    pub last_error: Option<(ResourceVersion, junction_api_types::xds::Error)>,
    pub data: Option<T>,
}

impl<T: CacheEntryData> CacheEntry<T> {}

trait CacheEntryData {
    type Xds;

    fn xds(&self) -> &Self::Xds;
}

macro_rules! impl_cache_entry {
    ($entry_ty:ty, $xds_ty:ty) => {
        impl CacheEntryData for $entry_ty {
            type Xds = $xds_ty;

            fn xds(&self) -> &$xds_ty {
                &self.xds
            }
        }
    };
}

impl_cache_entry!(ApiListener, xds_listener::Listener);
impl_cache_entry!(RouteConfig, xds_route::RouteConfiguration);
impl_cache_entry!(Cluster, xds_cluster::Cluster);
impl_cache_entry!(LoadAssignment, xds_endpoint::ClusterLoadAssignment);

#[derive(Debug)]
struct ResourceMap<T>(SkipMap<String, CacheEntry<T>>);

impl<T> Default for ResourceMap<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T: Send + 'static> ResourceMap<T> {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn get<'a>(&'a self, name: &str) -> Option<ResourceEntry<'a, T>> {
        self.0.get(name).map(ResourceEntry)
    }

    fn iter(&self) -> impl Iterator<Item = ResourceEntry<T>> + '_ {
        self.0.iter().map(ResourceEntry)
    }

    fn names(&self) -> impl Iterator<Item = String> + '_ {
        self.0.iter().map(|e| e.key().clone())
    }

    fn remove(&self, name: &str) {
        self.0.remove(name);
    }

    fn remove_all<I>(&self, names: I)
    where
        I: IntoIterator<Item: AsRef<str>>,
    {
        for name in names {
            self.0.remove(name.as_ref());
        }
    }
}

impl<X, T> ResourceMap<T>
where
    T: CacheEntryData<Xds = X> + Clone + Send + 'static,
    X: PartialEq + prost::Name,
{
    fn insert_ok(&self, name: String, version: ResourceVersion, t: T) {
        self.0.insert(
            name,
            CacheEntry {
                version,
                last_error: None,
                data: Some(t),
            },
        );
    }

    fn insert_error(
        &self,
        name: String,
        version: ResourceVersion,
        error: junction_api_types::xds::Error,
    ) {
        match self.0.get(&name) {
            Some(entry) => {
                let mut updated_entry = entry.value().clone();
                updated_entry.last_error = Some((version, error));
                self.0.insert(name, updated_entry);
            }
            None => {
                self.0.insert(
                    name,
                    CacheEntry {
                        version: ResourceVersion::default(),
                        last_error: Some((version, error)),
                        data: None,
                    },
                );
            }
        }
    }

    // FIXME: should this also compare version?
    fn is_changed(&self, name: &str, t: &X) -> bool {
        let Some(entry) = self.0.get(name) else {
            return true;
        };

        let Some(entry_data) = &entry.value().data else {
            return true;
        };
        entry_data.xds() != t
    }
}

struct ResourceEntry<'a, T>(crossbeam_skiplist::map::Entry<'a, String, CacheEntry<T>>);

impl<'a, T> ResourceEntry<'a, T> {
    fn name(&self) -> &str {
        self.0.key()
    }

    fn version(&self) -> &ResourceVersion {
        &self.0.value().version
    }

    fn last_error(&self) -> Option<&(ResourceVersion, junction_api_types::xds::Error)> {
        self.0.value().last_error.as_ref()
    }

    fn data(&self) -> Option<&T> {
        self.0.value().data.as_ref()
    }
}

/// A read-only handle to a [Cache]. `CacheReader`s are meant to passed around
/// and shared and are cheap to clone.
#[derive(Default, Clone)]
pub(crate) struct CacheReader {
    data: Arc<CacheData>,
}

/// A single xDS configuration object, with additional metadata about when it
/// was fetched and processed.
#[derive(Debug, Default, Clone)]
pub struct XdsConfig {
    pub name: String,
    pub type_url: String,
    pub version: ResourceVersion,
    pub xds: Option<protobuf::Any>,
    pub last_error: Option<(ResourceVersion, String)>,
}

impl CacheReader {
    pub(crate) fn iter_routes(&self) -> impl Iterator<Item = Arc<Route>> + '_ {
        let listener_routes = self.data.listeners.iter().filter_map(|entry| {
            entry
                .data()
                .and_then(|api_listener| match &api_listener.route_config {
                    ApiListenerRouteConfig::Inlined { route, .. } => Some(route.clone()),
                    _ => None,
                })
        });

        let route_config_routes = self
            .data
            .route_configs
            .iter()
            .filter_map(|entry| entry.data().map(|route_config| route_config.route.clone()));

        listener_routes.chain(route_config_routes)
    }

    pub(crate) fn iter_backends(&self) -> impl Iterator<Item = Arc<BackendLb>> + '_ {
        self.data
            .clusters
            .iter()
            .filter_map(|entry| entry.data().map(|cluster| cluster.backend_lb.clone()))
    }

    pub(crate) fn iter_xds(&self) -> impl Iterator<Item = XdsConfig> + '_ {
        macro_rules! any_iter {
            ($field:ident, $xds_type:ty) => {
                self.data.$field.iter().map(|entry| {
                    let name = entry.name().to_string();
                    let type_url = <$xds_type>::type_url();
                    let version = entry.version().clone();
                    let xds = entry.data().map(|data| {
                        protobuf::Any::from_msg(data.xds()).expect("generated invalid protobuf")
                    });
                    let last_error = entry.last_error().map(|(v, e)| (v.clone(), e.to_string()));
                    XdsConfig {
                        name,
                        type_url,
                        version,
                        xds,
                        last_error,
                    }
                })
            };
        }

        any_iter!(listeners, xds_listener::Listener)
            .chain(any_iter!(route_configs, xds_route::RouteConfiguration))
            .chain(any_iter!(clusters, xds_cluster::Cluster))
            .chain(any_iter!(
                load_assignments,
                xds_endpoint::ClusterLoadAssignment
            ))
    }

    /// Get the routing table for a hostname.
    ///
    /// Will return `None` if the routing table does not yet exist in cache,
    /// either because it hasn't yet been pulled from the ADS servcer or it
    /// doesn't exist.
    pub(crate) fn get_route(&self, target: &Target) -> Option<Arc<Route>> {
        let listener = self.data.listeners.get(&target.as_listener_xds_name())?;

        match &listener.data()?.route_config {
            ApiListenerRouteConfig::RouteConfig { name } => {
                let route_config = self.data.route_configs.get(name.as_str())?;
                route_config.data().map(|r| r.route.clone())
            }
            ApiListenerRouteConfig::Inlined { route, .. } => Some(route.clone()),
        }
    }

    /// Get the load balancer config, load balancer, and endpoint group for a
    /// routing target.
    ///
    /// May return no data, or load balancer, or a load balancer and an
    /// endpoint group if configuration hasn't yet been fetched from the ADS
    /// server or if the resources don't exist.
    pub(crate) fn get_target(
        &self,
        target: &Target,
    ) -> (
        Option<Arc<BackendLb>>,
        Option<Arc<crate::config::EndpointGroup>>,
    ) {
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Some(value) => value,
                    None => return (None, None),
                }
            };
        }

        let cluster = tri!(self.data.clusters.get(&target.as_cluster_xds_name()));
        let cluster_data = tri!(cluster.data());

        let backend_and_lb = Some(cluster_data.backend_lb.clone());

        match &cluster_data.endpoints {
            ClusterEndpointData::Inlined { endpoint_group, .. } => {
                (backend_and_lb, Some(endpoint_group.clone()))
            }
            ClusterEndpointData::LoadAssignment { name } => {
                let load_assignment = tri!(self.data.load_assignments.get(name.as_str()));
                let endpoint_group = load_assignment.data().map(|d| d.endpoint_group.clone());
                (backend_and_lb, endpoint_group)
            }
        }
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

    pub fn insert(
        &mut self,
        version: crate::xds::ResourceVersion,
        resources: ResourceVec,
    ) -> (ResourceTypeSet, Vec<junction_api_types::xds::Error>) {
        let (changed, errs) = match resources {
            ResourceVec::Listener(ls) => self.insert_listeners(version, ls),
            ResourceVec::RouteConfiguration(rcs) => self.insert_route_configs(version, rcs),
            ResourceVec::Cluster(cs) => self.insert_clusters(version, cs),
            ResourceVec::ClusterLoadAssignment(clas) => self.insert_load_assignments(version, clas),
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
                ResourceType::Cluster => self.data.clusters.remove_all(&names),
                ResourceType::ClusterLoadAssignment => {
                    self.data.load_assignments.remove_all(&names);
                }
                ResourceType::Listener => self.data.listeners.remove_all(&names),
                ResourceType::RouteConfiguration => self.data.route_configs.remove_all(&names),
            }
        }

        // safety: no longer holding any NodeIndexes, safe to remove and invalidate
        self.refs.retain_nodes(|_, n| reachable.contains(n.index()));
    }

    fn insert_listeners(
        &mut self,
        version: crate::xds::ResourceVersion,
        listeners: Vec<xds_listener::Listener>,
    ) -> (ResourceTypeSet, Vec<junction_api_types::xds::Error>) {
        let mut changed = ResourceTypeSet::default();
        let mut errors = Vec::new();
        let mut to_remove: BTreeSet<_> = self.data.listeners.names().collect();

        for listener in listeners {
            to_remove.remove(&listener.name);

            if self.data.listeners.is_changed(&listener.name, &listener) {
                let listener_name = listener.name.clone();
                let api_listener = match ApiListener::from_xds(&listener_name, listener) {
                    Ok(l) => l,
                    Err(e) => {
                        self.data
                            .listeners
                            .insert_error(listener_name, version.clone(), e.clone());
                        errors.push(e);
                        continue;
                    }
                };

                // remove the downstream route config ref and replace it with a new one
                let (node, _) = self.find_or_create_ref(ResourceType::Listener, &listener_name);
                self.reset_ref(node);

                match &api_listener.route_config {
                    ApiListenerRouteConfig::RouteConfig { name } => {
                        let (rc_node, created) = self
                            .find_or_create_ref(ResourceType::RouteConfiguration, name.as_str());
                        self.refs.update_edge(node, rc_node, ());

                        if created {
                            changed.insert(ResourceType::RouteConfiguration);
                        }
                    }
                    ApiListenerRouteConfig::Inlined { clusters, .. } => {
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
                    .insert_ok(listener_name, version.clone(), api_listener);
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
        version: crate::xds::ResourceVersion,
        clusters: Vec<xds_cluster::Cluster>,
    ) -> (ResourceTypeSet, Vec<junction_api_types::xds::Error>) {
        let mut changed = ResourceTypeSet::default();
        let mut errors = Vec::new();
        let mut to_remove: BTreeSet<_> = self.data.clusters.names().collect();

        for cluster in clusters {
            to_remove.remove(&cluster.name);

            if self.data.clusters.is_changed(&cluster.name, &cluster) {
                // try to find this cluster in the ref graph. it's possible it's
                // now from a stale subscription.
                let Some(node) = self.find_ref(ResourceType::Cluster, &cluster.name) else {
                    continue;
                };

                let cluster_name = cluster.name.clone();
                let cluster = match Cluster::from_xds(cluster) {
                    Ok(c) => c,
                    Err(e) => {
                        self.data
                            .clusters
                            .insert_error(cluster_name, version.clone(), e.clone());
                        errors.push(e);
                        continue;
                    }
                };

                // remove the old CLA edge and replace it with a new one.
                self.reset_ref(node);
                match &cluster.endpoints {
                    ClusterEndpointData::Inlined { .. } => (/* do nothing */),
                    ClusterEndpointData::LoadAssignment { name } => {
                        changed.insert(ResourceType::ClusterLoadAssignment);
                        let (cla_node, _) = self
                            .find_or_create_ref(ResourceType::ClusterLoadAssignment, name.as_str());
                        self.refs.update_edge(node, cla_node, ());
                    }
                }

                // insert the cluster
                self.data
                    .clusters
                    .insert_ok(cluster_name, version.clone(), cluster);
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
        version: crate::xds::ResourceVersion,
        route_configs: Vec<xds_route::RouteConfiguration>,
    ) -> (ResourceTypeSet, Vec<junction_api_types::xds::Error>) {
        let mut errors = Vec::new();
        let mut changed = ResourceTypeSet::default();

        for route_config in route_configs {
            if self
                .data
                .route_configs
                .is_changed(&route_config.name, &route_config)
            {
                changed.insert(ResourceType::RouteConfiguration);

                // it's possible that we got delivered a RouteConfiguration that
                // we don't have a subscription for (either because of a silly
                // ADS server or a race).
                //
                // if we did, just ignore the node.
                let Some(node) =
                    self.find_ref(ResourceType::RouteConfiguration, &route_config.name)
                else {
                    tracing::trace!(
                        route_config_name = &route_config.name,
                        "RouteConfiguration is missing a ref, skipping"
                    );
                    continue;
                };

                let route_config_name = route_config.name.clone();
                let route_config = match RouteConfig::from_xds(route_config) {
                    Ok(rc) => rc,
                    Err(e) => {
                        self.data.route_configs.insert_error(
                            route_config_name,
                            version.clone(),
                            e.clone(),
                        );
                        errors.push(e);
                        continue;
                    }
                };

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
                    .insert_ok(route_config_name, version.clone(), route_config);
            }
        }

        (changed, errors)
    }

    fn insert_load_assignments(
        &mut self,
        version: crate::xds::ResourceVersion,
        load_assignments: Vec<xds_endpoint::ClusterLoadAssignment>,
    ) -> (ResourceTypeSet, Vec<junction_api_types::xds::Error>) {
        let mut errors = Vec::new();
        let mut changed = ResourceTypeSet::default();

        for load_assignment in load_assignments {
            if self
                .data
                .load_assignments
                .is_changed(&load_assignment.cluster_name, &load_assignment)
            {
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
                let target = {
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
                    cluster
                        .data()
                        .expect("GC leak: parent Cluster has no data")
                        .backend_lb
                        .config
                        .target
                        .clone()
                };

                let load_assignment_name = load_assignment.cluster_name.clone();
                let load_assignment = match LoadAssignment::from_xds(target, load_assignment) {
                    Ok(l) => l,
                    Err(e) => {
                        self.data.load_assignments.insert_error(
                            load_assignment_name,
                            version.clone(),
                            e.clone(),
                        );
                        errors.push(e);
                        continue;
                    }
                };

                self.data.load_assignments.insert_ok(
                    load_assignment_name,
                    version.clone(),
                    load_assignment,
                );
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
        (changed, errors): (ResourceTypeSet, Vec<junction_api_types::xds::Error>),
    ) -> ResourceTypeSet {
        assert!(errors.is_empty(), "first error = {}", errors[0]);
        changed
    }

    #[test]
    fn test_insert_listener_inline_route_config() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "local/example/cluster")],
                )],
            )]),
        ));

        assert!(cache
            .data
            .listeners
            .get("listener.example.svc.cluster.local")
            .is_some());
        assert!(cache.data.route_configs.is_empty());
    }

    #[test]
    fn test_insert_invalid_listener() {
        let mut cache = Cache::default();

        // insert a listener with no api_listener
        let (changed, errors) = cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_listener::Listener {
                name: "potato".to_string(),
                ..Default::default()
            }]),
        );

        assert!(changed.is_empty());
        assert_eq!(errors.len(), 1);

        let listener_data = cache.data.listeners.get("potato").unwrap();
        assert!(listener_data.data().is_none());
        assert!(listener_data.name() == "potato");
        assert!(*listener_data.version() == "".into());
        assert!(matches!(listener_data.last_error(), Some((v, _)) if *v == "123".into()));
    }

    #[test]
    fn test_insert_listener_rds() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![
                xds_test::listener!(
                    "listener1.example.svc.cluster.local",
                    "rc1.example.svc.cluster.local"
                ),
                xds_test::listener!(
                    "listener2.example.svc.cluster.local",
                    "rc2.example.svc.cluster.local"
                ),
            ]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec![
                "listener1.example.svc.cluster.local",
                "listener2.example.svc.cluster.local"
            ],
        );
        assert!(cache.data.route_configs.is_empty());

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc1.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "example/cluster1/cluster")],
                )]
            )]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec![
                "listener1.example.svc.cluster.local",
                "listener2.example.svc.cluster.local"
            ],
        );
        assert_eq!(
            cache.data.route_configs.names().collect::<Vec<_>>(),
            vec!["rc1.example.svc.cluster.local"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc2.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "example/cluster1/cluster")],
                )]
            )]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec![
                "listener1.example.svc.cluster.local",
                "listener2.example.svc.cluster.local"
            ],
        );
        assert_eq!(
            cache.data.route_configs.names().collect::<Vec<_>>(),
            vec![
                "rc1.example.svc.cluster.local",
                "rc2.example.svc.cluster.local"
            ],
        );
    }

    #[test]
    fn test_insert_cluster_eds() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "example/cluster1/cluster")],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(eds "example/cluster1/cluster")]),
        ));

        assert!(cache
            .data
            .listeners
            .get("listener.example.svc.cluster.local")
            .is_some());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache
            .data
            .clusters
            .get("example/cluster1/cluster")
            .is_some());
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_insert_cla() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "example/cluster1/cluster" => {
                    "zone1" => ["1.1.1.1"]
                }
            )]),
        ));

        assert!(cache
            .data
            .listeners
            .get("listener.example.svc.cluster.local")
            .is_some());
        assert!(cache.data.route_configs.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
        assert_eq!(
            cache.data.load_assignments.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "example/cluster2/cluster" => {
                    "zone2" => ["2.2.2.2"]
                }
            )]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
        assert_eq!(
            cache.data.load_assignments.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
    }

    #[test]
    fn test_insert_cla_missing_ref() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        // add a CLA referencing a cluster that doesn't exist. it should just fall on the floor
        let (changed, errors) = cache.insert(
            "123".into(),
            ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster3.example.svc.cluster.local" => {
                    "zone2" => ["2.2.2.2"]
                }
            )]),
        );

        assert!(changed.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn test_insert_deletes_listeners() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "nginx.default.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.local"],
                    [xds_test::route!(default "default/nginx/cluster")],
                )],
            )]),
        ));

        assert!(cache.data.listeners.get("nginx.default.local").is_some());

        assert_insert(cache.insert("123".into(), ResourceVec::Listener(Vec::new())));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(cache.refs.node_count(), 0);
    }

    #[test]
    fn test_insert_deletes_clusters() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(eds "example/cluster2/cluster")]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster2/cluster"],
        );

        assert_insert(cache.insert("123".into(), ResourceVec::Cluster(vec![])));
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_deletes_keep_subscriptions() {
        let mut cache = Cache::default();

        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        cache.subscribe(ResourceType::Cluster, "example/cluster1/cluster");
        cache.subscribe(ResourceType::Cluster, "example/cluster2/cluster");

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );

        // delete everything
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));
        assert_insert(cache.insert("123".into(), ResourceVec::Cluster(vec![])));

        // subscriptions should still exist, but data should be gone
        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.clusters.is_empty());
        assert_eq!(
            cache.subscriptions(ResourceType::Listener),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.subscriptions(ResourceType::Cluster),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
    }

    #[test]
    fn test_insert_out_of_order() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(
                inline "cluster1.example.svc.cluster.local" => {
                "zone1" => ["1.1.1.1", "2.2.2.2"],
                "zone2" => ["3.3.3.3"]
            })]),
        ));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.clusters.is_empty());

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [xds_test::route!(default "example/cluster1/cluster")],
                )],
            )]),
        ));

        assert!(cache
            .data
            .listeners
            .get("listener.example.svc.cluster.local")
            .is_some());
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_cache_gc_simple() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "rc.example.svc.cluster.local"
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )]
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.route_configs.names().collect::<Vec<_>>(),
            vec!["rc.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
        assert!(cache.data.load_assignments.is_empty());

        // should have gc refs for the CLAs we're expecting.
        //
        // listener, rc,  2 * cluster, 2 * cla
        assert_eq!(cache.refs.node_count(), 6);

        // delete the listener
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));

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
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "rc1.example.svc.cluster.local"
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc1.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )]
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        // we should have listener -> rc1 -> {cluster1, cluster2}
        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.route_configs.names().collect::<Vec<_>>(),
            vec!["rc1.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );
        assert!(cache.data.load_assignments.is_empty());

        // update the targets for rc1.
        //
        // should now have listener -> rc1 -> cluster1
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc1.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "example/cluster1/cluster"),],
                )]
            )]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.route_configs.names().collect::<Vec<_>>(),
            vec!["rc1.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster"],
        );
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_cache_gc_pinned() {
        let mut cache = Cache::default();

        // pinning should let us insert a cluster, but only that cluster
        cache.subscribe(ResourceType::Cluster, "example/cluster1/cluster");
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster"],
        );

        // add a listener that references both cluster1 and cluster2
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "example/cluster2/cluster"),
                        xds_test::route!(default "example/cluster1/cluster"),
                    ],
                )],
            )]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster"],
        );

        // add both clusters
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "example/cluster1/cluster"),
                xds_test::cluster!(eds "example/cluster2/cluster"),
            ]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster", "example/cluster2/cluster"],
        );

        // remove the listener, cluster1 should stay pinned
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["example/cluster1/cluster"],
        );
    }
}
