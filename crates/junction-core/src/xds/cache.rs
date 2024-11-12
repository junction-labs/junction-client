// This module is a cache that handles SotW XDS behavior. It's the guts of an
// ADS connection and tracks the current state of XDS resoureces and the
// references between resources. If you need to add or modify any XDS behavior
// at all, it's like you'll end up here.
//
// This cache is built entirely around being single-writer, even though it's
// multi-reader and safe for concurrent reads. If you'd like to change that,
// it's likely you're going to rebuild the internals of the cache entirely.
//
// # Resource Tracking is XDS Subscription Tracking
//
// XDS SotW only ever issues deletes for LDS and CDS resources, so one of the
// most important jobs a cache has is tracking references between objects so
// that it's possible to delete RDS/EDS objects once they're no longer
// referenced. Tracking the list of referenced names is also exactly what we
// need to be doing for subscription tracking - any client using this cache
// should be subscribing to all RDS resources referenced by existing LDS
// resources, all CDS resources referenced by LDS and RDS resources, and so on.
//
// The current implementation manages these references between resources as a
// `petgraph` graph. That graph is owned by the cache's single writer, and not
// shared with any of the readers. This means that reference data can be stored
// relatively cheaply (without duplicating XDS protobufs) and modified without
// any coordination between reader and writer threads/tasks. It does however
// mean that there are two places to track state, and the writer is now
// responsible for keeping them in sync.
//
// # Resource Reference Tracking is Garbage Collection
//
// Tracking a graph of objects that reference each other and removing the
// unused ones should sound a lot like garbage collection to you - it is
// exactly garbage collection.
//
// Using a graph internally to track object references means that it's
// relatively easy to run a simple mark-and-sweep over the current state of the
// graph. Here all writes pay the cost of collecting XDS garbage so that readers
// can keep reading uninterrupted. In practice, we expect the number of XDS
// objects to be relatively small, and this cost to be low, but this is the
// right tradeoff even if collection does become more expensive - slow
// collections on writes puts backpressure on updates from the network, rather
// than forcing readers to slow down.
//
// # User Input is GC Roots
//
// xDS resources fall into two categories - resources that were requested
// directly by an upstream cache subscriber, and resources that were referenced
// by another resource already in cache.
//
// That first category is naturally our GC roots. It tends to be LDS/CDS
// resources that are GC roots, but nothing in the cache should dictate that. If
// an upstream caller wants to subscribe to a ClusterLoadAssignment, that may be
// pinned as well.
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

use crossbeam_skiplist::SkipMap;
use enum_map::EnumMap;
use junction_api::VirtualHost;
use junction_api::{http::Route, BackendId};
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{self, Visitable},
    Direction,
};
use prost::Name;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use xds_api::pb::envoy::config::{
    cluster::v3::{self as xds_cluster},
    endpoint::v3::{self as xds_endpoint},
    listener::v3::{self as xds_listener},
    route::v3::{self as xds_route},
};
use xds_api::pb::google::protobuf;

use crate::{BackendLb, ConfigCache, EndpointGroup};

use super::resources::{
    ApiListener, ApiListenerRouteConfig, Cluster, ClusterEndpointData, LoadAssignment,
    ResourceError, ResourceName, ResourceType, ResourceTypeSet, ResourceVec, RouteConfig,
};
use super::{notify, ResourceVersion};

/// A cache entry wraps some versioned xDS and its parsed client representation
/// along with any errors while parsing.
///
/// This is generic over the types in [the resources module][super::resources]
/// and implemented for them here.
#[derive(Debug, Clone)]
struct CacheEntry<T> {
    pub version: ResourceVersion,
    pub last_error: Option<(ResourceVersion, ResourceError)>,
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

/// A newtype wrapper around a map from resource name to cache entry.
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

    fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
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

    fn insert_timeout(&self, name: String) {
        match self.0.get(&name) {
            Some(entry) => {
                let mut updated_entry = entry.value().clone();
                updated_entry.last_error =
                    Some((ResourceVersion::default(), ResourceError::TimedOut));
                self.0.insert(name, updated_entry);
            }
            None => {
                self.0.insert(
                    name,
                    CacheEntry {
                        version: ResourceVersion::default(),
                        last_error: Some((ResourceVersion::default(), ResourceError::TimedOut)),
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

/// An entry in a [ResourceMap].
///
/// This is a thin newtype wrapper around the crossbeam_skiplist's entry
/// to make some common things easy.
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
}

impl ConfigCache for CacheReader {
    fn get_route(&self, target: &VirtualHost) -> Option<Arc<Route>> {
        let listener = self.data.listeners.get(&target.name())?;

        match &listener.data()?.route_config {
            ApiListenerRouteConfig::RouteConfig { name } => {
                let route_config = self.data.route_configs.get(name.as_str())?;
                route_config.data().map(|r| r.route.clone())
            }
            ApiListenerRouteConfig::Inlined { route, .. } => Some(route.clone()),
        }
    }

    fn get_backend(
        &self,
        target: &BackendId,
    ) -> (Option<Arc<BackendLb>>, Option<Arc<EndpointGroup>>) {
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Some(value) => value,
                    None => return (None, None),
                }
            };
        }

        let cluster = tri!(self.data.clusters.get(&target.name()));
        let cluster_data = tri!(cluster.data());

        let backend_and_lb = Some(cluster_data.backend_lb.clone());

        match &cluster_data.endpoints {
            ClusterEndpointData::Inlined { endpoint_group, .. } => {
                (backend_and_lb, Some(endpoint_group.clone()))
            }
            ClusterEndpointData::LoadAssignment { name } => {
                let load_assignment = match self.data.load_assignments.get(name.as_str()) {
                    Some(load_assignment) => load_assignment,
                    None => return (backend_and_lb, None),
                };
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
    meta: DiGraph<Meta, ()>,
    data: Arc<CacheData>,
    notify: EnumMap<ResourceType, BTreeMap<String, Vec<notify::Send>>>,
}

/// Metadata tracked for every xDS resource.
#[derive(Debug)]
struct Meta {
    /// The name of the tracked resource.
    name: String,

    /// The type of the tracked resource.
    resource_type: ResourceType,

    /// Callers waiting to be notified when a resource is first created. This
    /// vec may be dropped during GC without notifying all callers. Callers must
    /// distinguish between a successful notification and a drop.
    notify: Vec<notify::Send>,

    /// A timestamp that tracks when data for a resource was first requested. This
    /// is set only the first time data is requested - because of the xDS protocol
    /// there's no way to figure out if subsequent pushes or requests "time out"
    /// or are NACKed.
    requested_at: Option<Instant>,

    /// A resource is `pinnned` if explicitly requested by a caller, and can't be
    /// removed from cache unless it's explicilty deleted.
    pinned: bool,

    /// A resource is `deleted` if it was pinned and an external caller asked
    /// to remove it.
    deleted: bool,
}

impl Meta {
    #[inline]
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

// public API
impl Cache {
    pub fn reader(&self) -> CacheReader {
        CacheReader {
            data: self.data.clone(),
        }
    }

    pub fn subscriptions(&self, resource_type: ResourceType) -> Vec<String> {
        let weights = self
            .meta
            .node_weights()
            .filter(|n| n.resource_type == resource_type);
        weights.map(|n| n.name.clone()).collect()
    }

    /// Insert a batch of xDS into the cache.
    pub fn insert(
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

    /// Unsubscribe from an XDS resource and delete it from cache.
    pub fn delete(&mut self, resource_type: ResourceType, name: &str) -> bool {
        if !self.delete_meta(resource_type, name, true) {
            return false;
        }

        self.notify[resource_type].remove(name);

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

    /// Subscribe to an XDS resource. Returns `true` if this subscription did
    /// not exist before this call.
    pub fn subscribe(
        &mut self,
        resource_type: ResourceType,
        name: String,
        now: Instant,
        notify: notify::Send,
    ) -> bool {
        let (node, created) = self.find_or_create_idx(resource_type, &name);
        self.set_pinned(node);
        self.meta[node].requested_at = Some(now);

        if self.exists(resource_type, &name) {
            notify.notify();
        } else {
            self.meta[node].notify.push(notify);
        }

        created
    }

    fn exists(&self, resource_type: ResourceType, name: &str) -> bool {
        match resource_type {
            ResourceType::Listener => self.data.listeners.contains(name),
            ResourceType::RouteConfiguration => self.data.route_configs.contains(name),
            ResourceType::Cluster => self.data.clusters.contains(name),
            ResourceType::ClusterLoadAssignment => self.data.load_assignments.contains(name),
        }
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
        // lean on petgraph's Control to only visit each node once - because the
        // metadata graph must be a DAG, we can skip marking nodes twice and
        // emit Control::Prune every time we see a node we've already seen.
        let mut reachable = self.meta.visit_map();
        visit::depth_first_search(&self.meta, self.gc_roots(), |event| -> Control<()> {
            if let DfsEvent::Discover(n, _) = event {
                if reachable.contains(n.index()) {
                    return Control::Prune;
                }
                reachable.insert(n.index());
            };

            Control::Continue
        });

        let unreachable_nodes = self
            .meta
            .node_indices()
            .filter(|n| !reachable.contains(n.index()));

        let mut unreachable_names: EnumMap<ResourceType, Vec<String>> = EnumMap::default();
        for n in unreachable_nodes {
            let n = &self.meta[n];
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
        self.meta
            .retain_nodes(|g, n| g[n].pinned || reachable.contains(n.index()));
    }

    /// Handle timed out nodes. Timed out notes get a tombstone inserted into
    /// cache and treated like their request has completed with a NACK. This
    /// doesn't affect the graph of refs - but should drop the "pending" status
    /// from those nodes so they don't notify again and should clear any pending
    /// notifications.
    pub(crate) fn handle_timeouts(&mut self, now: Instant, timeout: Duration) {
        let mut by_type: EnumMap<ResourceType, Vec<String>> = Default::default();

        for node_idx in self.meta.node_indices() {
            let meta = &mut self.meta[node_idx];

            let timed_out = &meta
                .requested_at
                .map_or(false, |t| now.duration_since(t) >= timeout);

            if !timed_out {
                continue;
            }

            // copy the name to insert a tombstone
            by_type[meta.resource_type].push(meta.name.clone());

            // drop all notifications for this ref. it doesn't matter if the
            // tombstone is written yet, the caller is going to see "no data"
            // either way.
            let _ = std::mem::take(&mut meta.notify);
        }

        for (rtype, names) in by_type {
            match rtype {
                ResourceType::Listener => {
                    for name in names {
                        self.data.listeners.insert_timeout(name);
                    }
                }
                ResourceType::RouteConfiguration => {
                    for name in names {
                        self.data.route_configs.insert_timeout(name);
                    }
                }
                ResourceType::Cluster => {
                    for name in names {
                        self.data.clusters.insert_timeout(name);
                    }
                }
                ResourceType::ClusterLoadAssignment => {
                    for name in names {
                        self.data.load_assignments.insert_timeout(name);
                    }
                }
            }
        }
    }

    fn insert_listeners(
        &mut self,
        version: ResourceVersion,
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
                let (listener_idx, _) =
                    self.find_or_create_idx(ResourceType::Listener, &listener_name);
                self.reset_edges(listener_idx);

                match &api_listener.route_config {
                    ApiListenerRouteConfig::RouteConfig { name } => {
                        let (rc_idx, created) = self
                            .find_or_create_idx(ResourceType::RouteConfiguration, name.as_str());
                        self.meta.update_edge(listener_idx, rc_idx, ());

                        if created {
                            changed.insert(ResourceType::RouteConfiguration);
                        }
                    }
                    ApiListenerRouteConfig::Inlined {
                        clusters,
                        default_action,
                        ..
                    } => {
                        let mut clusters_changed = false;

                        // update cluster refs for everything downstream
                        for cluster in clusters {
                            let (cluster_idx, created) =
                                self.find_or_create_idx(ResourceType::Cluster, cluster.as_str());
                            self.meta.update_edge(listener_idx, cluster_idx, ());
                            clusters_changed |= created;
                        }
                        if clusters_changed {
                            changed.insert(ResourceType::Cluster);
                        }

                        // if this Listener looks like it is the default route
                        // for a Cluster, recompute the LB config for that
                        // Cluster.
                        if let Some((cluster, route_action)) = default_action {
                            if let Err(e) =
                                self.rebuild_cluster(&mut changed, cluster, route_action)
                            {
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

                // send notifications and udpate changed set
                self.set_inserted(listener_idx);
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
            self.delete_meta(ResourceType::Listener, &name, false);
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
                let action = self.find_passthrough_action(&cluster.name);
                if let Err(e) =
                    self.insert_cluster(&mut changed, &version, cluster, action.as_ref())
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
            self.delete_meta(ResourceType::Cluster, &name, false);
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
                // if we did, just ignore this.
                let Some(rc_idx) =
                    self.find_idx(ResourceType::RouteConfiguration, &route_config.name)
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
                if let Some((cluster, route_action)) = &route_config.passthrough_action {
                    if let Err(e) = self.rebuild_cluster(&mut changed, cluster, route_action) {
                        errors.push(e);
                    }
                }

                // add an edge for every cluster reference in this RouteConfig
                self.reset_edges(rc_idx);
                for cluster in &route_config.clusters {
                    let (cluster_idx, _) =
                        self.find_or_create_idx(ResourceType::Cluster, cluster.as_str());
                    self.meta.update_edge(rc_idx, cluster_idx, ());
                }

                // actually insert the route config
                self.data
                    .route_configs
                    .insert_ok(route_config_name, version.clone(), route_config);

                // send notifications
                self.set_inserted(rc_idx);
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
                let Some(cla_idx) = self.find_idx(
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
                    let cluster_idx = self
                        .parent_resources(cla_idx)
                        .next()
                        .expect("GC leak: ClusterLoadAssignment must have a parent cluster");
                    let cluster = self
                        .data
                        .clusters
                        .get(&self.meta[cluster_idx].name)
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
                    load_assignment_name.clone(),
                    version.clone(),
                    load_assignment,
                );

                self.set_inserted(cla_idx);
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
        let Some(cluster_idx) = self.find_idx(ResourceType::Cluster, &cluster.name) else {
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
        self.reset_edges(cluster_idx);

        // remove the old CLA edge and replace it with a new one.
        if let ClusterEndpointData::LoadAssignment { name } = &cluster.endpoints {
            changed.insert(ResourceType::ClusterLoadAssignment);
            let (cla_idx, _) =
                self.find_or_create_idx(ResourceType::ClusterLoadAssignment, name.as_str());
            self.meta.update_edge(cluster_idx, cla_idx, ());
        }

        // try to subscribe to the passthrough Listener for this Cluster if it
        // doesn't already exist in the GC graph.
        let passthrough_listener_name = cluster.backend_lb.config.id.passthrough_route_name();
        let (default_listener_idx, created) =
            self.find_or_create_idx(ResourceType::Listener, &passthrough_listener_name);
        if created {
            changed.insert(ResourceType::Listener);
        }
        self.meta.update_edge(cluster_idx, default_listener_idx, ());

        // insert the cluster
        self.data
            .clusters
            .insert_ok(cluster_name.clone(), version.clone(), cluster);

        // send notifications and mark changes
        self.set_inserted(cluster_idx);
        changed.insert(ResourceType::Cluster);

        Ok(())
    }

    /// Return the indices of all GC roots. GC roots are either Listeners or are
    /// explicitly pinned.
    ///
    /// Safety: NodeIndexes are not stable across deletions. This vec is not
    /// safe to store long term or between collections.
    fn gc_roots(&self) -> Vec<NodeIndex> {
        self.meta
            .node_indices()
            .filter(|idx| self.meta[*idx].is_gc_root())
            .collect()
    }

    /// Delete a resource's metadata.
    fn delete_meta(&mut self, resource_type: ResourceType, name: &str, force: bool) -> bool {
        match self.find_idx(resource_type, name) {
            Some(idx) => {
                if force || !self.meta[idx].pinned {
                    self.meta.remove_node(idx);
                    true
                } else {
                    self.meta[idx].deleted = true;
                    false
                }
            }
            None => false,
        }
    }

    /// Find the parents of this resource.
    fn parent_resources(&self, idx: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.meta.neighbors_directed(idx, Direction::Incoming)
    }

    // TODO: it's annoying that this clones the XDS for a route action, but also
    // its impossible to follow doing it inline and keeping the right refs in scope
    // so that the borrow never drops.
    //
    // if we hit clone as a bottleneck, come back and fuck with this.
    fn find_passthrough_action(&self, cluster_name: &str) -> Option<xds_route::RouteAction> {
        // don't even parse the cluster name as a target, assume that the
        // passthrough listener has the same name as the cluster.
        let target = BackendId::from_str(cluster_name).ok()?;
        let listener = self.data.listeners.get(&target.passthrough_route_name())?;

        match &listener.data()?.route_config {
            ApiListenerRouteConfig::RouteConfig { name } => {
                let route = self.data.route_configs.get(name.as_str())?;
                let default_action = &route.data()?.passthrough_action;
                default_action.as_ref().map(|(_, a)| a.clone())
            }
            ApiListenerRouteConfig::Inlined { default_action, .. } => {
                default_action.as_ref().map(|(_, a)| a.clone())
            }
        }
    }

    /// Find or create a metadtata index for the given name or resource type.
    ///
    /// New GC refs are not marked `reachable` by default.
    fn find_or_create_idx(&mut self, resource_type: ResourceType, name: &str) -> (NodeIndex, bool) {
        if let Some(idx) = self.find_idx(resource_type, name) {
            self.meta[idx].deleted = false;
            return (idx, false);
        }

        let idx = self.meta.add_node(Meta {
            name: name.to_string(),
            resource_type,
            notify: Vec::new(),
            requested_at: None,
            pinned: false,
            deleted: false,
        });
        (idx, true)
    }

    /// Find a GC ref with the given resource type or name.
    fn find_idx(&self, resource_type: ResourceType, name: &str) -> Option<NodeIndex> {
        self.meta.node_indices().find(|idx| {
            let meta = &self.meta[*idx];
            meta.resource_type == resource_type && meta.name == name
        })
    }

    /// Track that a resource should be pinned into cache.
    #[inline]
    fn set_pinned(&mut self, idx: NodeIndex) {
        self.meta[idx].pinned = true;
    }

    /// Track that a data for a resource has been inserted into cache.
    fn set_inserted(&mut self, idx: NodeIndex) {
        let meta = &mut self.meta[idx];

        // reset the request timer
        meta.requested_at = None;

        // notify all waiters
        let to_notify = std::mem::take(&mut meta.notify);
        for notify in to_notify {
            notify.notify();
        }
    }

    /// Remove all of a resource's outgoing edges.
    fn reset_edges(&mut self, idx: NodeIndex) {
        let neighbors: Vec<_> = self
            .meta
            .neighbors_directed(idx, Direction::Outgoing)
            .collect();

        for n in neighbors {
            if let Some((edge, _)) = self.meta.find_edge_undirected(idx, n) {
                self.meta.remove_edge(edge);
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
        assert!(errors.is_empty(), "first error = {}", errors[0]);
        changed
    }

    #[track_caller]
    fn assert_subscribe_insert(
        cache: &mut Cache,
        version: ResourceVersion,
        resources: ResourceVec,
    ) {
        let mut notifications = vec![];
        for name in resources.names() {
            let (tx, rx) = notify::new();
            notifications.push(rx);

            cache.subscribe(resources.resource_type(), name, Instant::now(), tx);
        }

        assert_insert(cache.insert(version, resources));

        for n in &mut notifications {
            assert!(
                n.notified(),
                "notifications should have all been sent for inserts",
            )
        }
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
        let (tx, mut rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "potato".to_string(),
            Instant::now(),
            tx,
        );
        let (changed, errors) = cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_listener::Listener {
                name: "potato".to_string(),
                ..Default::default()
            }]),
        );

        assert!(changed.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(rx.pending());

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
            ResourceVec::Cluster(vec![xds_test::cluster!(eds "cluster1.example:8913")]),
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
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
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
    fn test_insert_load_assignment_not_subscribed() {
        let mut cache = Cache::default();

        assert_subscribe_insert(
            &mut cache,
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
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
        assert_eq!(cache.meta.node_count(), 2);

        assert_insert(cache.insert("123".into(), ResourceVec::Listener(Vec::new())));

        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.meta.node_count(),
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
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
            ]),
        );

        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8913", "cluster2.example:8913"],
        );

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![xds_test::cluster!(eds "cluster2.example:8913")]),
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

        let (tx, mut listener_rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "listener.example.svc.cluster.local".to_string(),
            Instant::now(),
            tx,
        );

        let (tx, mut cluster1_rx) = notify::new();
        cache.subscribe(
            ResourceType::Cluster,
            "cluster1.example:8913".to_string(),
            Instant::now(),
            tx,
        );
        let (tx, mut cluster2_rx) = notify::new();
        cache.subscribe(
            ResourceType::Cluster,
            "cluster2.example:8913".to_string(),
            Instant::now(),
            tx,
        );

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
        assert!(listener_rx.notified());

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
            ]),
        ));
        assert!(cluster1_rx.notified());
        assert!(cluster2_rx.notified());

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
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash eds cluster_name)]),
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
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash eds cluster_name)]),
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
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash eds cluster_name)]),
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
            ResourceVec::Cluster(vec![xds_test::cluster!(ring_hash eds cluster_name)]),
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
    fn test_cache_handle_timeout() {
        let mut cache = Cache::default();

        let timeout = Duration::from_secs(5);
        let subscribe_at = Instant::now();
        let timeout_at = subscribe_at + (2 * timeout);

        // subscribe to two listeners and assert that they both have pending notifications

        let (tx, mut exists_rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "listener.example.svc.cluster.local".to_string(),
            subscribe_at,
            tx,
        );
        let (tx, mut dne_rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "does-not-exist.example.svc.cluster.local".to_string(),
            subscribe_at,
            tx,
        );

        assert!(exists_rx.pending());
        assert!(dne_rx.pending());

        // insert one of the two listeners. it should get notified and be in cache.
        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Listener(vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route-config",
            )]),
        ));
        assert!(exists_rx.notified());

        // run a timeout that should catch the pending listener. the signal should
        // no longer be pending, but it should not have had a message sent.
        cache.handle_timeouts(timeout_at, timeout);
        assert!(!dne_rx.notified() && !dne_rx.pending());

        // verify the cache state. there should be one listener with data, and one
        // tracked as having TimedOut.
        assert_eq!(
            cache.data.listeners.names().collect::<Vec<_>>(),
            vec![
                "does-not-exist.example.svc.cluster.local",
                "listener.example.svc.cluster.local",
            ],
        );
        assert!({
            let entry = cache
                .data
                .listeners
                .get("listener.example.svc.cluster.local")
                .unwrap();

            entry.last_error().is_none()
        });
        assert!(matches!(
            {
                let entry = cache
                    .data
                    .listeners
                    .get("does-not-exist.example.svc.cluster.local")
                    .unwrap();

                entry.last_error().cloned()
            },
            Some((_, ResourceError::TimedOut))
        ));
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
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
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
        assert_eq!(cache.meta.node_count(), 8);

        // delete the listener
        assert_insert(cache.insert("123".into(), ResourceVec::Listener(vec![])));

        assert!(cache.data.listeners.is_empty());
        assert!(cache.data.route_configs.is_empty());
        assert!(cache.data.clusters.is_empty());
        assert!(cache.data.load_assignments.is_empty());

        // should have a single ref left for the Listener we subscribed to
        assert_eq!(cache.meta.node_count(), 1);
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
                xds_test::cluster!(eds "cluster1.example:8913"),
                xds_test::cluster!(eds "cluster2.example:8913"),
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
        let (tx, mut rx) = notify::new();
        cache.subscribe(
            ResourceType::Cluster,
            "cluster1.example:8888".to_string(),
            Instant::now(),
            tx,
        );
        assert!(rx.pending());

        assert_insert(cache.insert(
            "123".into(),
            ResourceVec::Cluster(vec![
                xds_test::cluster!(eds "cluster1.example:8888"),
                xds_test::cluster!(eds "cluster2.example:8888"),
            ]),
        ));

        assert!(rx.notified());
        assert!(cache.data.listeners.is_empty());
        assert_eq!(
            cache.data.clusters.names().collect::<Vec<_>>(),
            vec!["cluster1.example:8888"],
        );

        // add a listener that references both cluster1 and cluster2
        let (tx, mut rx) = notify::new();
        cache.subscribe(
            ResourceType::Listener,
            "listener.example.svc.cluster.local".to_string(),
            Instant::now(),
            tx,
        );
        assert!(rx.pending());

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

        assert!(rx.notified());
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
                xds_test::cluster!(eds "cluster1.example:8888"),
                xds_test::cluster!(eds "cluster2.example:8888"),
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
