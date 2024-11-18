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
use junction_api::{http::Route, BackendId};
use junction_api::{Hostname, Target, VirtualHost};
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{self, Visitable},
    Direction,
};
use prost::Name;
use std::collections::BTreeSet;
use std::str::FromStr;
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

use crate::{BackendLb, ConfigCache, EndpointGroup};

use super::resources::{
    ApiListener, ApiListenerData, Cluster, LoadAssignment, ResourceError, ResourceName,
    ResourceType, ResourceTypeSet, ResourceVec, RouteConfig,
};
use super::{ResourceVersion, XdsConfig};

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    version: ResourceVersion,
    last_error: Option<(ResourceVersion, ResourceError)>,
    data: Option<T>,
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

    fn insert_error<E: Into<ResourceError>>(
        &self,
        name: String,
        version: ResourceVersion,
        error: E,
    ) {
        match self.0.get(&name) {
            Some(entry) => {
                let mut updated_entry = entry.value().clone();
                updated_entry.last_error = Some((version, error.into()));
                self.0.insert(name, updated_entry);
            }
            None => {
                self.0.insert(
                    name,
                    CacheEntry {
                        version: ResourceVersion::default(),
                        last_error: Some((version, error.into())),
                        data: None,
                    },
                );
            }
        }
    }

    // TDODO: should this also compare version? for Cluster and Listener it'd
    // mean a decent amount of churn replacing an identical resource on every
    // update.
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

    fn last_error(&self) -> Option<&(ResourceVersion, ResourceError)> {
        self.0.value().last_error.as_ref()
    }

    fn data(&self) -> Option<&T> {
        self.0.value().data.as_ref()
    }
}

/// A read-only handle to a [Cache]. `CacheReader`s are meant to passed around
/// and shared and are cheap to clone.
#[derive(Default, Clone)]
pub(super) struct CacheReader {
    data: Arc<CacheData>,
}

impl CacheReader {
    pub(super) fn iter_routes(&self) -> impl Iterator<Item = Arc<Route>> + '_ {
        let listener_routes = self.data.listeners.iter().filter_map(|entry| {
            entry
                .data()
                .and_then(|api_listener| match &api_listener.route_config {
                    ApiListenerData::Inlined { route, .. } => Some(route.clone()),
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

    pub(super) fn iter_backends(&self) -> impl Iterator<Item = Arc<BackendLb>> + '_ {
        self.data
            .clusters
            .iter()
            .filter_map(|entry| entry.data().map(|cluster| cluster.backend_lb.clone()))
    }

    pub(super) fn iter_xds(&self) -> impl Iterator<Item = XdsConfig> + '_ {
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
}

impl ConfigCache for CacheReader {
    fn get_route(&self, target: &VirtualHost) -> Option<Arc<Route>> {
        let listener = self.data.listeners.get(&target.name())?;

        match &listener.data()?.route_config {
            ApiListenerData::RouteConfig { name } => {
                let route_config = self.data.route_configs.get(name.as_str())?;
                route_config.data().map(|r| r.route.clone())
            }
            ApiListenerData::Inlined { route, .. } => Some(route.clone()),
        }
    }

    fn get_backend(&self, id: &BackendId) -> Option<Arc<BackendLb>> {
        let cluster = self.data.clusters.get(&id.name())?;
        let cluster_data = cluster.data()?;
        Some(cluster_data.backend_lb.clone())
    }

    fn get_endpoints(&self, backend: &BackendId) -> Option<Arc<EndpointGroup>> {
        // we don't have DNS targets, but we're just trying everything anyway.
        // it's the callers responsibility to stop us from getting here if this
        // is not a DNS backend.
        let load_assignment = self.data.load_assignments.get(&backend.name())?;
        let load_assignment_data = load_assignment.data()?;
        Some(load_assignment_data.endpoint_group.clone())
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

/// GC data tracked for every xDS resource.
///
/// A resource is `pinnned` if explicitly requested by a caller, and will never
/// be removed from cache.
///
/// A resource is `deleted` if it was already pinned and was deleted because an
/// xDS server or garbage collection removed it. Deleted resources will still
/// be used when generating subscriptions.
#[derive(Debug)]
struct GCData {
    name: String,
    resource_type: ResourceType,
    pinned: bool,
    deleted: bool,
    dns_name: Option<(Hostname, u16)>,
}

impl GCData {
    fn is_gc_root(&self) -> bool {
        self.pinned && !self.deleted
    }
}

#[derive(Debug, Default)]
struct CacheData {
    listeners: ResourceMap<ApiListener>,
    route_configs: ResourceMap<RouteConfig>,
    clusters: ResourceMap<Cluster>,
    load_assignments: ResourceMap<LoadAssignment>,
}

impl Cache {
    /// Create a new read-only handle to this cache.
    ///
    /// Read handles are cheap, and intended to be created and shared across
    /// multiple threads and tasks.
    pub(super) fn reader(&self) -> CacheReader {
        CacheReader {
            data: self.data.clone(),
        }
    }

    /// Explicitly subscribe to an XDS resource.
    pub(super) fn subscribe(&mut self, resource_type: ResourceType, name: &str) -> bool {
        let (node, created) = self.find_or_create_ref(resource_type, name);
        self.pin_ref(node);
        created
    }

    /// Return the resource names that should be subscribed to.
    pub(super) fn subscriptions(&self, resource_type: ResourceType) -> Vec<String> {
        let weights = self
            .refs
            .node_weights()
            .filter(|n| n.resource_type == resource_type);
        weights.map(|n| n.name.clone()).collect()
    }

    /// An iterator over all of the DNS names of external DNS clusters in the
    /// cache.
    pub(super) fn dns_names(&self) -> impl Iterator<Item = (Hostname, u16)> + '_ {
        self.refs.node_weights().filter_map(|n| n.dns_name.clone())
    }

    /// Insert a batch of xDS resources into the cache.
    ///
    /// Returns the set of resource types that indicates the types of resources
    /// that were changed or were newly subscribed to. Callers should use this
    /// set to determine whether to ACK or to re-subscribe to resources of that
    /// type.
    pub(super) fn insert(
        &mut self,
        version: crate::xds::ResourceVersion,
        resources: ResourceVec,
    ) -> (ResourceTypeSet, Vec<ResourceError>) {
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
    pub(super) fn delete(&mut self, resource_type: ResourceType, name: &str) -> bool {
        if !self.delete_ref(resource_type, name, true) {
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
            if let DfsEvent::Discover(n, _) = event {
                if reachable.contains(n.index()) {
                    return Control::Prune;
                }
                reachable.insert(n.index());
            };

            Control::Continue
        });

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
                ResourceType::Listener => self.data.listeners.remove_all(&names),
                ResourceType::RouteConfiguration => self.data.route_configs.remove_all(&names),
                ResourceType::Cluster => self.data.clusters.remove_all(&names),
                ResourceType::ClusterLoadAssignment => {
                    self.data.load_assignments.remove_all(&names);
                }
            }
        }

        // safety: no longer holding any NodeIndexes, it's safe to invalidate
        // any outstanding ref by calling retain_nodes
        self.refs
            .retain_nodes(|g, n| g[n].pinned || reachable.contains(n.index()));
    }

    fn insert_listeners(
        &mut self,
        version: crate::xds::ResourceVersion,
        listeners: Vec<xds_listener::Listener>,
    ) -> (ResourceTypeSet, Vec<ResourceError>) {
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
                    ApiListenerData::RouteConfig { name } => {
                        let (rc_node, created) = self
                            .find_or_create_ref(ResourceType::RouteConfiguration, name.as_str());
                        self.refs.update_edge(node, rc_node, ());

                        if created {
                            changed.insert(ResourceType::RouteConfiguration);
                        }
                    }
                    ApiListenerData::Inlined {
                        clusters,
                        lb_action,
                        ..
                    } => {
                        let mut clusters_changed = false;

                        // update cluster refs for everything downstream
                        for cluster in clusters {
                            let (cluster_node, created) =
                                self.find_or_create_ref(ResourceType::Cluster, cluster.as_str());
                            self.refs.update_edge(node, cluster_node, ());
                            clusters_changed |= created;
                        }
                        if clusters_changed {
                            changed.insert(ResourceType::Cluster);
                        }

                        // if this Listener looks like it is the default route
                        // for a Cluster, recompute the LB config for that
                        // Cluster.
                        if let Some(lb_action) = lb_action {
                            if let Err(e) = self.rebuild_cluster(
                                &mut changed,
                                &lb_action.cluster,
                                &lb_action.route_action,
                            ) {
                                self.data.listeners.insert_error(
                                    listener_name,
                                    version.clone(),
                                    e.clone(),
                                );
                                errors.push(e);
                                continue;
                            }
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
            self.delete_ref(ResourceType::Listener, &name, false);
            self.data.listeners.remove(&name);
        }

        (changed, errors)
    }

    fn insert_clusters(
        &mut self,
        version: crate::xds::ResourceVersion,
        clusters: Vec<xds_cluster::Cluster>,
    ) -> (ResourceTypeSet, Vec<ResourceError>) {
        let mut changed = ResourceTypeSet::default();
        let mut errors = Vec::new();
        let mut to_remove: BTreeSet<_> = self.data.clusters.names().collect();

        for cluster in clusters {
            to_remove.remove(&cluster.name);

            if self.data.clusters.is_changed(&cluster.name, &cluster) {
                let lb_action = self.find_lb_action(&cluster.name);
                if let Err(e) =
                    self.insert_cluster(&mut changed, &version, cluster, lb_action.as_deref())
                {
                    errors.push(e);
                }
            }
        }

        // safety: the refs graph should be in sync with the names of clusters,
        // so panic here if we try to remove a name that didn't exist in the ref
        // graph.
        //
        // this guarantee comes from there only being a single cache writer.
        for name in to_remove {
            changed.insert(ResourceType::Cluster);
            self.delete_ref(ResourceType::Cluster, &name, false);
            self.data.clusters.remove(&name);
        }

        (changed, errors)
    }

    fn insert_route_configs(
        &mut self,
        version: crate::xds::ResourceVersion,
        route_configs: Vec<xds_route::RouteConfiguration>,
    ) -> (ResourceTypeSet, Vec<ResourceError>) {
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
                        errors.push(e.into());
                        continue;
                    }
                };

                // if this looks like the default RouteConfiguration for a
                // Cluster, rebuild it.
                if let Some(a) = &route_config.lb_action {
                    if let Err(e) = self.rebuild_cluster(&mut changed, &a.cluster, &a.route_action)
                    {
                        errors.push(e);
                    }
                }

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
    ) -> (ResourceTypeSet, Vec<ResourceError>) {
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
                    continue;
                };

                // use the GC graph to pull a ref to the parent cluster. with
                // the xdstp:// scheme the name of a Cluster and a
                // ClusterLoadAssignment may not be the same, so using the
                // GC graph is necessary.
                //
                // this assumes that a CLA will only ever have a single parent
                // Cluster.
                let target = {
                    let cluster_node = self
                        .parent_refs(cla_node)
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
                        .id
                        .clone()
                };

                let load_assignment_name = load_assignment.cluster_name.clone();
                let load_assignment = LoadAssignment::from_xds(target, load_assignment);

                self.data.load_assignments.insert_ok(
                    load_assignment_name,
                    version.clone(),
                    load_assignment,
                );
                changed.insert(ResourceType::ClusterLoadAssignment);
            }
        }

        (changed, Vec::new())
    }

    /// Try to rebuild a Cluster and its data, using a new RouteAction to fill
    /// in its load balancing policies.
    fn rebuild_cluster(
        &mut self,
        changed: &mut ResourceTypeSet,
        cluster: &ResourceName<Cluster>,
        route_action: &xds_route::RouteAction,
    ) -> Result<(), ResourceError> {
        // clone the cluster's current version and xds so that borrowck doesn't
        // get mad. it gets upset about a partial borrow of cache data while
        // we're trying to mutate the ref graph in build_cluster.
        //
        // this is a relatively cheap clone so whatever.
        let version_and_xds = self.data.clusters.get(cluster.as_str()).and_then(|e| {
            let version = e.version();
            e.data().map(|d| (version.clone(), d.xds().clone()))
        });

        match version_and_xds {
            Some((version, xds)) => self.insert_cluster(changed, &version, xds, Some(route_action)),
            None => Ok(()),
        }
    }

    /// Build and insert a Cluster based on new xDS, update it's GC refs, etc.
    /// This also has the side effect of inserting a subscription for this
    /// Cluster's default Listener into the cluster.
    ///
    /// This is split out from the inner loop of `insert_clusters` so that it
    /// can be called whenever RouteConfigurations or Listeners with default
    /// routing info change.
    fn insert_cluster(
        &mut self,
        changed: &mut ResourceTypeSet,
        version: &crate::xds::ResourceVersion,
        cluster: xds_cluster::Cluster,
        default_action: Option<&xds_route::RouteAction>,
    ) -> Result<(), ResourceError> {
        // try to find this cluster in the ref graph. it's possible it's
        // now from a stale subscription.
        let Some(node) = self.find_ref(ResourceType::Cluster, &cluster.name) else {
            return Ok(());
        };

        let cluster_name = cluster.name.clone();
        let cluster = match Cluster::from_xds(cluster, default_action) {
            Ok(c) => c,
            Err(e) => {
                self.data
                    .clusters
                    .insert_error(cluster_name, version.clone(), e.clone());
                return Err(e);
            }
        };

        // clear the outgoing edges form this node.
        self.reset_ref(node);

        // remove the old CLA edge and replace it with a new one.
        changed.insert(ResourceType::ClusterLoadAssignment);
        let (cla_node, _) =
            self.find_or_create_ref(ResourceType::ClusterLoadAssignment, &cluster_name);
        self.refs.update_edge(node, cla_node, ());

        // try to subscribe to the passthrough Listener for this Cluster if it
        // doesn't already exist in the GC graph.
        let passthrough_listener_name = cluster.backend_lb.config.id.passthrough_route_name();
        let (default_listener_node, created) =
            self.find_or_create_ref(ResourceType::Listener, &passthrough_listener_name);
        if created {
            changed.insert(ResourceType::Listener);
        }
        self.refs.update_edge(node, default_listener_node, ());

        // insert the cluster
        self.data
            .clusters
            .insert_ok(cluster_name, version.clone(), cluster);
        changed.insert(ResourceType::Cluster);

        Ok(())
    }

    /// Return the node indices of all GC roots. GC roots are either Listeners
    /// or are explicitly pinned.
    ///
    /// Safety: NodeIndexes are not stable across deletions. This vec is not
    /// safe to store long term or between collections.
    fn gc_roots(&self) -> Vec<NodeIndex> {
        self.refs
            .node_indices()
            .filter(|idx| self.refs[*idx].is_gc_root())
            .collect()
    }

    /// Delete a ref from the GC map.
    fn delete_ref(&mut self, resource_type: ResourceType, name: &str, force: bool) -> bool {
        match self.find_ref(resource_type, name) {
            Some(node) => {
                if force || !self.refs[node].pinned {
                    self.refs.remove_node(node);
                    true
                } else {
                    self.refs[node].deleted = true;
                    false
                }
            }
            None => false,
        }
    }

    fn parent_refs(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.refs.neighbors_directed(node, Direction::Incoming)
    }

    fn find_lb_action(&self, cluster_name: &str) -> Option<Arc<xds_route::RouteAction>> {
        // don't even parse the cluster name as a target, assume that the
        // passthrough listener has the same name as the cluster.
        let target = BackendId::from_str(cluster_name).ok()?;
        let listener = self.data.listeners.get(&target.passthrough_route_name())?;

        match &listener.data()?.route_config {
            ApiListenerData::RouteConfig { name } => {
                let route = self.data.route_configs.get(name.as_str())?;
                let lb_action = &route.data()?.lb_action;
                lb_action.as_ref().map(|a| a.route_action.clone())
            }
            ApiListenerData::Inlined { lb_action, .. } => {
                lb_action.as_ref().map(|a| a.route_action.clone())
            }
        }
    }

    /// Find or create a GC ref for the given name or resource type.
    ///
    /// New GC refs are not marked `reachable` by default.
    fn find_or_create_ref(&mut self, resource_type: ResourceType, name: &str) -> (NodeIndex, bool) {
        if let Some(node) = self.find_ref(resource_type, name) {
            self.refs[node].deleted = false;
            return (node, false);
        }

        let mut dns_name = None;
        if resource_type == ResourceType::Cluster {
            if let Ok(backend) = BackendId::from_str(name) {
                if let Target::Dns(dns) = backend.target {
                    dns_name = Some((dns.hostname, backend.port))
                }
            }
        }

        let node = self.refs.add_node(GCData {
            name: name.to_string(),
            resource_type,
            pinned: false,
            deleted: false,
            dns_name,
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
    use junction_api::{backend::LbPolicy, Target};

    use super::*;
    use crate::xds::test as xds_test;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn assert_reader_send_sync() {
        assert_send::<CacheReader>();
        assert_sync::<CacheReader>();
    }

    #[track_caller]
    fn assert_insert((changed, errors): (ResourceTypeSet, Vec<ResourceError>)) -> ResourceTypeSet {
        assert!(errors.is_empty(), "{errors:?}");
        changed
    }

    #[track_caller]
    fn assert_subscribe_insert(
        cache: &mut Cache,
        version: ResourceVersion,
        resources: ResourceVec,
    ) {
        for name in resources.names() {
            cache.subscribe(resources.resource_type(), &name);
        }
        assert_insert(cache.insert(version, resources));
    }

    #[test]
    fn test_insert_listener_inline_route_config() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "cluster.example:80")],
                )],
            )]),
        );

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
        cache.subscribe(ResourceType::Listener, "potato");
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

        assert_subscribe_insert(
            &mut cache,
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
        );

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
                    [xds_test::route!(default "cluster1.example:8913")],
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
                    [xds_test::route!(default "cluster1.example:8913")],
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

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!("cluster1.example:8913")]),
        );

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache.data.clusters.get("cluster1.example:8913").is_some());
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_insert_load_assignment() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
            ]),
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster1.example:8913" => {
                    "zone1" => ["1.1.1.1"]
                }
            )]),
        ));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.route_configs.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );
        assert_eq!(
            cache.data.load_assignments.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::ClusterLoadAssignment(vec![xds_test::cla!(
                "cluster2.example:8913" => {
                    "zone2" => ["2.2.2.2"]
                }
            )]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );
        assert_eq!(
            cache.data.load_assignments.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );
    }

    #[test]
    fn test_insert_load_assignment_missing_ref() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
            ]),
        );

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );

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

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "nginx.default.local" => [xds_test::vhost!(
                    "default",
                    ["nginx.default.local"],
                    [xds_test::route!(default "nginx.default.local:80")],
                )],
            )]),
        );

        assert!(cache.data.listeners.get("nginx.default.local").is_some());
        assert_eq!(cache.refs.node_count(), 2);

        assert_insert(cache.insert("123".into(), ResourceVec::Listener(Vec::new())));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.refs.node_count(),
            1,
            "should still be subscribed to the removed Listener",
        );
    }

    #[test]
    fn test_insert_deletes_clusters() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
            ]),
        );

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!("cluster2.example:8913")]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster2.example:8913"],
        );

        assert_insert(cache.insert("123".into(), ResourceVec::Cluster(vec![])));
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_deletes_keep_subscriptions() {
        let mut cache = Cache::default();

        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        cache.subscribe(ResourceType::Cluster, "cluster1.example:8913");
        cache.subscribe(ResourceType::Cluster, "cluster2.example:8913");

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example:8913"),
                        xds_test::route!(default "cluster1.example:8913"),
                    ],
                )],
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
            ]),
        ));

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
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
            vec!["cluster1.example:8913", "cluster2.example:8913"],
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

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [xds_test::route!(default "cluster1.example:8913")],
                )],
            )]),
        );

        assert!(cache
            .data
            .listeners
            .get("listener.example.svc.cluster.local")
            .is_some());
        assert!(cache.data.clusters.is_empty());
    }

    #[test]
    fn test_cache_cluster_finds_passthrough_listener() {
        let mut cache = Cache::default();

        let svc = Target::kube_service("default", "something")
            .unwrap()
            .into_backend(8910);
        let cluster_name = svc.name().leak();
        let passthrough_name = svc.passthrough_route_name().leak();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                passthrough_name => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [xds_test::route!(default ring_hash = "x-user", cluster_name)],
                )],
            )]),
        );
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash cluster_name)]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");

                let cluster_data = cluster.data().expect("cluster should have data");
                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if !params.hash_params.is_empty(),
                )
            },
            "should have non-empty hash params"
        );
    }

    #[test]
    fn test_cache_cluster_finds_passthrough_route() {
        let mut cache = Cache::default();

        let svc = Target::kube_service("default", "something")
            .unwrap()
            .into_backend(8910);
        let cluster_name = svc.name().leak();
        let passthrough_name = svc.passthrough_route_name().leak();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                passthrough_name,
                "example-route-config", // NOTE: doesn't have to be the same as the listener name!
            )]),
        );
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "example-route-config",
                [xds_test::vhost!(
                    "example-vhost",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default ring_hash = "x-user", cluster_name),],
                )]
            )]),
        ));
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash cluster_name)]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");

                let cluster_data = cluster.data().expect("cluster should have data");
                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if !params.hash_params.is_empty(),
                )
            },
            "should have non-empty hash params"
        );
    }

    #[test]
    fn test_cache_listener_rebuilds_cluster() {
        let mut cache = Cache::default();

        let svc = Target::kube_service("default", "something")
            .unwrap()
            .into_backend(8910);
        let cluster_name = svc.passthrough_route_name().leak();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local"=> [xds_test::vhost!(
                    "default",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default cluster_name)],
                )],
            )]),
        );
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash cluster_name)]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");
                let cluster_data = cluster.data().expect("cluster should have data");

                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if params.hash_params.is_empty(),
                )
            },
            "should have empty hash params before Listener insert"
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![
                xds_test::listener!(
                    "listener.example.svc.cluster.local"=> [xds_test::vhost!(
                        "default",
                        ["listener.example.svc.cluster.local"],
                        [xds_test::route!(default cluster_name)],
                    )],
                ),
                xds_test::listener!(
                    cluster_name => [xds_test::vhost!(
                        "default",
                        ["listener.example.svc.cluster.local"],
                        [xds_test::route!(default ring_hash = "x-user", cluster_name)],
                    )],
                ),
            ]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");

                let cluster_data = cluster.data().expect("cluster should have data");
                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if !params.hash_params.is_empty(),
                )
            },
            "should have non-empty hash params after Listener insert"
        );
    }

    #[test]
    fn test_cache_route_rebuilds_cluster() {
        let mut cache = Cache::default();

        let svc = Target::kube_service("default", "something")
            .unwrap()
            .into_backend(8910);
        let cluster_name = svc.passthrough_route_name().leak();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                cluster_name,
                "example-route-config",
            )]),
        );
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "example-route-config",
                [xds_test::vhost!(
                    "example-vhost",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default cluster_name),],
                )]
            )]),
        ));
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash cluster_name)]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");

                let cluster_data = cluster.data().expect("cluster should have data");
                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if params.hash_params.is_empty(),
                )
            },
            "should have empty hash params before the default route has a hash policy"
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "example-route-config",
                [xds_test::vhost!(
                    "example-vhost",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default ring_hash = "x-user", cluster_name),],
                )]
            )]),
        ));

        assert!(
            {
                let cluster = cache
                    .data
                    .clusters
                    .get(cluster_name)
                    .expect("Cache should contain cluster");

                let cluster_data = cluster.data().expect("cluster should have data");
                matches!(
                    &cluster_data.backend_lb.config.lb,
                    LbPolicy::RingHash(params) if !params.hash_params.is_empty(),
                )
            },
            "should have non-empty hash params when the default route is updated with a hash policy",
        );
    }

    #[test]
    fn test_cache_gc_simple() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "rc.example.svc.cluster.local"
            )]),
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example:8913"),
                        xds_test::route!(default "cluster1.example:8913"),
                    ],
                )]
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
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
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );
        assert!(cache.data.load_assignments.is_empty());

        // should have gc refs for everything
        //
        // listener, rc,  2 * cluster + 2 * default listener, 2 * cla
        assert_eq!(cache.refs.node_count(), 8);

        // delete the listener
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache.data.clusters.is_empty());
        assert!(cache.data.load_assignments.is_empty());

        // should have a single ref left for the Listener we subscribed to
        assert_eq!(cache.refs.node_count(), 1);
    }

    #[test]
    fn test_cache_gc_update_rds() {
        let mut cache = Cache::default();

        // swap the routeconfig for a listener, poitn to the same clusters
        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "rc1.example.svc.cluster.local"
            )]),
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::RouteConfiguration(vec![xds_test::route_config!(
                "rc1.example.svc.cluster.local",
                [xds_test::vhost!(
                    "vhost1.example.svc.cluster.local",
                    ["listener.example.svc.cluster.local"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example:8913"),
                        xds_test::route!(default "cluster1.example:8913"),
                    ],
                )]
            )]),
        ));

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8913"),
                xds_test::cluster!("cluster2.example:8913"),
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
            vec!["cluster1.example:8913", "cluster2.example:8913"],
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
                    [xds_test::route!(default "cluster1.example:8913")],
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
            vec!["cluster1.example:8913"],
        );
        assert!(cache.data.load_assignments.is_empty());
    }

    #[test]
    fn test_cache_gc_pinned() {
        let mut cache = Cache::default();

        // pinning should let us insert a cluster, but only that cluster
        cache.subscribe(ResourceType::Cluster, "cluster1.example:8888");
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8888"),
                xds_test::cluster!("cluster2.example:8888"),
            ]),
        ));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8888"],
        );

        // add a listener that references both cluster1 and cluster2
        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local" => [xds_test::vhost!(
                    "default",
                    ["*"],
                    [
                        xds_test::route!(header "x-staging" => "cluster2.example:8888"),
                        xds_test::route!(default "cluster1.example:8888"),
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
            vec!["cluster1.example:8888"],
        );

        // add both clusters
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!("cluster1.example:8888"),
                xds_test::cluster!("cluster2.example:8888"),
            ]),
        ));

        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec!["listener.example.svc.cluster.local"],
        );
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8888", "cluster2.example:8888"],
        );

        // remove the listener, cluster1 should stay pinned
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8888"],
        );
    }
}
