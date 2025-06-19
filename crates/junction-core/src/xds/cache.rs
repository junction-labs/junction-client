//! This package contains the state of the Junction xDS client that survives
//! between individual ADS connections. The cache is built with incremental xDS
//! in mind, but could potentially be used for a State-of-the-World connection
//! or to generate individual Fetch requests if there's a reason to not use the
//! streaming variants of the protocol.
//!
//! The cache in this package is built around having a single writer and any
//! number of concurrent readers (see [Cache::reader]), with the idea that
//! updates are rare relative to reads and that updates should never block
//! reads.
//!
//! The writer handles resource updates, interperets xDS as Junction types, and
//! manages the relationships between resources. As resources are handled and
//! inserted into the cache, they become immediately available to readers.
//!
//! # Junction Resources
//!
//! This package contains a set of Junction-flavored-xDS resources, that include
//! raw xDS and Junction-specific information about a resource, which is a
//! superset of the `junction-api` types a Resource represents. For example, an
//! xDS `RouteConfig` in this package contains both the `Route` that the xDS
//! represents along with the pre-computed xDS clusters that the Route
//! references.
//!
//! # Reading from a Cache
//!
//! All readers are expected to use a [CacheReader] to read from a cache.
//! Readers are cheap to clone, and should be freely handed out to each
//! individual task that wants to read from a cache.
//!
//! # Writing to a Cache
//!
//! The owner of a Cache is expected to call [Cache::insert], [Cache::subscribe]
//! and [Cache::unsubscribe] in batches, and then periodically call
//! [Cache::collect] to remove unused resources and generate a set of resources
//! to add/remove via xDS.
//!
//! The current state of the cache can also be inspected by with method like
//! [Cache::versions] or [Cache::initial_subscriptions]. These methods should
//! generally be called after a call to [Cache::collect] - if called between a
//! change to cache state and a collection, they may return references to
//! resources that are no longer connected to the rest of the resource graph.
//!
//! ## Subscriptions and Garbage Collection
//!
//! One of the primary jobs of the cache writer is to track relationships
//! between resources and determine what resources should be requested on any
//! individual connection. This is a little more complex than just tracking what
//! resources have been explicitly subscribed to with a call to
//! [Cache::subscribe]. A `Listener` subscription creates a subscription to a
//! `RouteConfiguration`, which generates subscriptions to some number of
//! `Cluster`s, and so on. Unsubscribing is also not straight forward for the
//! same reasons - while the caller may not explicitly care about a `Cluster`
//! any more, it may be a dependency of an existing `RouteConfiguration`, and
//! shouldn't be removed from cache even though there is no longer explicit
//! interest in it.
//!
//! To handle resources properly, a Cache builds a reference graph between xDS
//! Resources and treats this like a garbage collection problem. Any resource
//! that's been explicitly subcribed to is treated as a root of the reference
//! graph. If the resource type accepts wildcard subscriptions, any resource
//! inserted as a wildcard is also treated as a root.
//!
//! Using the root set, garbage collection is done with a very simple
//! mark-and-sweep approach. [Cache::collect] forces a collection, and returns
//! the set of changes to resource subscriptions that happened during the last
//! batch of inserts and during garbage collection.
//!
//! ## Subscription Changes
//!
//! As resources get added and removed from a [Cache], it builds up a list of
//! changes to the subscription graph. When [Cache::collect] is called it
//! returns the set of added and removed subscriptions for each resource type in
//! the graph, which includes the set of resources that may have been removed
//! during garbage collection.
//!
//! Tracking changes means it's easy to generate ADS updates for those resource
//! types - changes for a resource type new requests need to be generated to
//! send updates to the server about what the connection is now interested in.
//!
//! Cluster changes can also create a dependency on client DNS. Changes to DNS
//! hostnames of interest are also tracked and returned.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use dashmap::DashMap;
use enum_map::EnumMap;
use junction_api::Hostname;
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{EdgeRef, Visitable},
    Direction,
};
use tokio::sync::Notify;
use xds_api::pb::envoy::service::discovery::v3 as xds_discovery;
use xds_api::pb::google::protobuf;

use super::{
    resources::{
        ApiListener, Cluster, LoadAssignment, Resource, ResourceError, ResourceName,
        RouteConfiguration,
    },
    DnsUpdates, ResourceType, ResourceVersion, XdsConfig,
};

/// A concurrent map of resources, bundled together with an Arc<Notify> so that
/// callers can wait on changes.
#[derive(Debug)]
struct ResourceMap<T> {
    changed: Notify,
    // TODO: use ahash for keys, see if it matters
    map: DashMap<ResourceName, ResourceData<T>>,
}

type ResourceMapEntryRef<'a, T> = dashmap::mapref::one::Ref<'a, ResourceName, ResourceData<T>>;
type ResourceMapEntryIterRef<'a, T> =
    dashmap::mapref::multiple::RefMulti<'a, ResourceName, ResourceData<T>>;

#[derive(Clone, Debug)]
struct ResourceData<T> {
    version: Option<ResourceVersion>,
    last_error: Option<(ResourceVersion, ResourceError)>,
    data: Option<Arc<T>>,
    raw_msg: Option<protobuf::Any>,
}

impl<T> Default for ResourceData<T> {
    fn default() -> Self {
        Self {
            version: None,
            last_error: None,
            data: None,
            raw_msg: None,
        }
    }
}

// NOTE: manually derived because the Derive macro requires `T: Default`, and
// this impl shouldn't care - an empty map doesn't need a T.
impl<T> Default for ResourceMap<T> {
    fn default() -> Self {
        Self {
            changed: Notify::new(),
            map: Default::default(),
        }
    }
}

impl<T> ResourceMap<T> {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn get<'a>(&'a self, name: &ResourceName) -> Option<ResourceMapEntryRef<'a, T>> {
        self.map.get(name).map(|r| r)
    }

    async fn get_await<'a>(&'a self, name: &ResourceName) -> Option<ResourceMapEntryRef<'a, T>> {
        // fast path: try a get and return it if it works out.
        if let Some(entry) = self.map.get(name) {
            return Some(entry);
        }

        // slow path: we're waiting
        //
        // on every run through the loop, we need to, IN ORDER, do the
        // following:
        //
        // - register for notifications
        // - try to get a name in the map, returning it if present
        // - wait for the next notification
        //
        // given this order of events, and the guarantees on Notify, there is no
        // interleaving of events where we can miss a notification.
        //
        // SAFETY: this assumes that the writer half of a notify is always using
        // notify_waiters and not ever using notify_one.
        //
        // for an example that uses notify_one() instead, see Notify
        //
        // https://docs.rs/tokio/latest/tokio/sync/futures/struct.Notified.html#method.enable
        let changed = self.changed.notified();
        tokio::pin!(changed);
        loop {
            // check the map
            if let Some(entry) = self.map.get(name) {
                return Some(entry);
            }

            // wait for a change
            changed.as_mut().await;

            // this uses Pin::set so we're not allocating/deallocating a new
            // wakeup future every time.
            changed.set(self.changed.notified());
        }
    }

    fn iter(&self) -> impl Iterator<Item = ResourceMapEntryIterRef<T>> + '_ {
        self.map.iter()
    }

    fn has_data(&self, k: &ResourceName) -> bool {
        match self.get(k) {
            None => false,
            Some(entry) => entry.data.is_some(),
        }
    }

    fn versions(&self) -> HashMap<ResourceName, ResourceVersion> {
        let mut versions = HashMap::new();
        for entry in self.map.iter() {
            if entry.data.is_none() {
                continue;
            };
            let Some(version) = &entry.version else {
                continue;
            };

            let name = entry.key().clone();
            let version = version.clone();
            versions.insert(name, version);
        }

        versions
    }

    fn remove(&self, name: &ResourceName) -> Option<(ResourceName, ResourceData<T>)> {
        let entry = self.map.remove(name);
        self.changed.notify_waiters();
        entry
    }

    fn remove_all<'a, I>(&self, names: I)
    where
        I: IntoIterator<Item = &'a ResourceName>,
    {
        for name in names {
            self.remove(name);
        }
    }

    fn insert_ok(&self, name: ResourceName, version: ResourceVersion, resource: T) {
        self.map.insert(
            name,
            ResourceData {
                version: Some(version),
                last_error: None,
                data: Some(Arc::new(resource)),
                raw_msg: None,
            },
        );
        self.changed.notify_waiters();
    }

    fn insert_tombstone(&self, name: ResourceName) {
        self.map.insert(
            name,
            ResourceData {
                version: None,
                last_error: None,
                data: None,
                raw_msg: None,
            },
        );
        self.changed.notify_waiters();
    }
}

/// The set of subscription changes for a single resource type.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// The names of any newly added subscriptions. Newly added subscriptions
    /// may or may not be in cache, and must be requested from the xDS server.
    pub(crate) added: BTreeSet<ResourceName>,

    /// The names of any removed subscriptions. Callers should assume that any
    /// removed names are also removed from cache.
    pub(crate) removed: BTreeSet<ResourceName>,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Subscription tracking for xDS. This struct exists with the expectation that
/// it's owned by a single [Cache] and is being used to track the relationships
/// between resources in that cache. No introspection of xDS is done here.
///
/// # Wildcard Mode
#[derive(Debug)]
struct Subscriptions {
    subs: DiGraph<SubscriptionInfo, ()>,
    changes: EnumMap<ResourceType, Changes>,
    wildcard: EnumMap<ResourceType, bool>,
}

impl Default for Subscriptions {
    fn default() -> Self {
        let mut wildcard = EnumMap::default();
        for rtype in ResourceType::all() {
            wildcard[*rtype] = rtype.supports_wildcard()
        }

        Self {
            subs: Default::default(),
            changes: Default::default(),
            wildcard,
        }
    }
}

// TODO: add insertion times and TTLs
#[derive(Debug)]
struct SubscriptionInfo {
    // the type of the resource
    resource_type: ResourceType,

    // the name of the resource
    name: ResourceName,

    // true if there is explicit interest in this resource via subscribe.
    explicit: bool,

    // true if this subscription was added by inserting a resource that didn't
    // have an existing subscription.
    wildcard: bool,
}

impl Subscriptions {
    fn explicit(&self, rtype: ResourceType) -> impl Iterator<Item = &ResourceName> + '_ {
        self.subs
            .node_weights()
            .filter(move |w| w.resource_type == rtype && !w.wildcard)
            .map(|w| &w.name)
    }

    fn subscribe(&mut self, rtype: ResourceType, name: &ResourceName) {
        // explicit subscription means never a wildcard
        let sub = self.find_or_create(rtype, name, false);
        self.subs[sub].explicit = true;
    }

    fn unsubscribe(&mut self, rtype: ResourceType, name: &ResourceName) {
        if let Some(sub) = self.find(rtype, name) {
            self.subs[sub].explicit = false;
        }
    }

    fn remove(&mut self, rtype: ResourceType, name: &ResourceName) {
        if let Some(sub) = self.find(rtype, name) {
            self.reset_refs(sub);
        }
    }

    #[inline]
    fn clear_changes(&mut self, rtype: ResourceType, name: &ResourceName) {
        self.changes[rtype].added.remove(name);
        self.changes[rtype].removed.remove(name);
    }

    /// safety: must be called with a valid NodeIndex
    fn remove_sub(&mut self, sub: NodeIndex) {
        let sub = self.subs.remove_node(sub).unwrap();
        self.changes[sub.resource_type].removed.insert(sub.name);
    }

    fn find_or_subscribe(&mut self, rtype: ResourceType, name: &ResourceName) -> Option<NodeIndex> {
        // if this is a wildcard subscription, any name is subscribed. create it
        // and move on.
        if self.wildcard[rtype] {
            // if this is a new node and is a wildcard, don't track the
            // side-effect of adding the subscription. we've gotten the resource
            // already.
            let sub = self.find_or_create(rtype, name, true);
            return Some(sub);
        }

        self.find(rtype, name)
    }

    fn find(&self, rtype: ResourceType, name: &ResourceName) -> Option<NodeIndex> {
        self.subs.node_indices().find(|idx| {
            let sub = &self.subs[*idx];
            sub.resource_type == rtype && sub.name == *name
        })
    }

    fn find_or_create(
        &mut self,
        rtype: ResourceType,
        name: &ResourceName,
        wildcard: bool,
    ) -> NodeIndex {
        match self.find(rtype, name) {
            Some(idx) => idx,
            None => {
                let idx = self.subs.add_node(SubscriptionInfo {
                    name: name.clone(),
                    resource_type: rtype,
                    explicit: false,
                    wildcard,
                });

                // track that this is a newly created sub
                if !wildcard {
                    self.changes[rtype].added.insert(name.clone());
                }

                idx
            }
        }
    }

    /// Remove all of of a subscription node's outgoing edges. Any references to
    /// this node are left untouched.
    ///
    /// Safety: can be called with NodeIndexes outstanding, should not modify
    /// NodeIndexes and make them unstable.
    fn reset_refs(&mut self, sub: NodeIndex) {
        let out_refs: Vec<_> = self
            .subs
            .edges_directed(sub, Direction::Outgoing)
            .map(|edge_ref| edge_ref.id())
            .collect();

        for out_ref in out_refs {
            self.subs.remove_edge(out_ref);
        }
    }

    /// Add a reference from `from_sub` to a node of with a given resource type
    /// and name. Creates the destination subscription if it doesn't already
    /// exist.
    fn add_ref(&mut self, from_sub: NodeIndex, rtype: ResourceType, name: &ResourceName) {
        // when adding a reference that creates a new subscription, even if the
        // destination type is a wildcard, we want a non-wildcard reference to
        // it so that the cache can switch to explicit mode and keep this
        // reference.
        let to_sub = self.find_or_create(rtype, name, false);
        self.subs.add_edge(from_sub, to_sub, ());
    }

    fn collect(&mut self) {
        use petgraph::visit::{Control, DfsEvent};

        // walk the GC graph, keeping the set of the reachable nodes.
        //
        // lean on petgraph's Control to only visit each node once - because
        // the ref graph must be a DAG, we can skip marking nodes twice and
        // emit Control::Prune every time we see a node we've already seen.
        let mut reachable = self.subs.visit_map();
        petgraph::visit::depth_first_search(&self.subs, self.gc_roots(), |event| -> Control<()> {
            if let DfsEvent::Discover(n, _) = event {
                if reachable.contains(n.index()) {
                    return Control::Prune;
                }
                reachable.insert(n.index());
            };

            Control::Continue
        });

        // remove all unreachalbe nodes from the graph
        //
        // safety: remove_node invalidates the last index in the graph when
        // called. walking the indices backwards means that we're guaranteed
        // to not be invalidating an index in the reachable set that we haven't
        // touched yet.
        for idx in self.subs.node_indices().rev() {
            if reachable.contains(idx.index()) {
                continue;
            }

            self.remove_sub(idx);
        }
    }

    fn is_gc_root(&self, node: NodeIndex) -> bool {
        let sub_data = &self.subs[node];
        sub_data.explicit || sub_data.wildcard
    }

    fn gc_roots(&self) -> Vec<NodeIndex> {
        self.subs
            .node_indices()
            .filter(|idx| self.is_gc_root(*idx))
            .collect()
    }
}

/// A counted set of DNS names.
///
/// Keeps track of the number of times a name is used and builds up
/// a set of tracked updates. Updates are cleared and returned when
/// `collect` is called.
#[derive(Clone, Debug, Default)]
struct DnsNames {
    names: HashMap<Hostname, usize>,
    changes: DnsUpdates,
}

impl DnsNames {
    /// Add a name to the set.
    /// present.
    fn add_name(&mut self, name: Hostname) {
        use std::collections::hash_map::Entry;

        match self.names.entry(name) {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() += 1;
            }
            Entry::Vacant(entry) => {
                let name = entry.key().clone();

                // add one
                entry.insert(1);
                // update changes
                self.changes.remove.remove(&name);
                self.changes.add.insert(name);
            }
        }
    }

    /// Remove a name from the tracked set.
    fn remove_name(&mut self, name: &Hostname) {
        let Some(val) = self.names.get_mut(name) else {
            return;
        };

        if *val == 1 {
            self.names.remove(name);
            self.changes.add.remove(name);
            self.changes.remove.insert(name.clone());
        } else {
            *val -= 1;
        }
    }

    /// Clear and return the set of names that have changed since the last call
    /// to `collect`.
    fn collect(&mut self) -> DnsUpdates {
        std::mem::take(&mut self.changes)
    }
}

/// A persistent cache of Junction xDS state. See the module documentation for
/// more information on what's in a cache and how to use one.
#[derive(Debug, Default)]
pub(super) struct Cache {
    subs: Subscriptions,
    data: Arc<CacheData>,
    dns: DnsNames,
}

#[derive(Debug, Default)]
struct CacheData {
    listeners: ResourceMap<ApiListener>,
    route_configs: ResourceMap<RouteConfiguration>,
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

    /// Set wildcard mode for a resource type. Will have no effect if the
    /// resource doesn't support wildcard mode.
    #[cfg(test)]
    pub(crate) fn set_wildcard(&mut self, rtype: ResourceType, wildcard: bool) {
        if !rtype.supports_wildcard() {
            return;
        }
        self.subs.wildcard[rtype] = wildcard;
    }

    /// Check whether a resource type is in wildcard mode.
    pub(crate) fn is_wildcard(&self, rtype: ResourceType) -> bool {
        self.subs.wildcard[rtype]
    }

    /// Subscribe to a resource by name.
    pub(crate) fn subscribe(&mut self, rtype: ResourceType, name: &ResourceName) {
        self.subs.subscribe(rtype, name);
    }

    /// Unsubscribe from a resource by name.
    pub(crate) fn unsubscribe(&mut self, rtype: ResourceType, name: &ResourceName) {
        self.subs.unsubscribe(rtype, name);
    }

    /// Return the current list of subscriptions for this resource type.
    #[cfg(test)]
    pub(crate) fn subscriptions(&self, rtype: ResourceType) -> Vec<ResourceName> {
        self.subs.explicit(rtype).map(|s| s.clone()).collect()
    }

    pub(crate) fn dns_names(&self) -> impl Iterator<Item = Hostname> + '_ {
        self.data
            .clusters
            .iter()
            .filter_map(|e| e.data.as_ref().and_then(|c| c.dns_name().cloned()))
    }

    /// Return the list of resources the cache has a registered subscription for
    /// but contains no data for.
    pub(crate) fn initial_subscriptions(&self, rtype: ResourceType) -> Vec<ResourceName> {
        macro_rules! missing_from {
            ($m:expr) => {
                self.subs
                    .explicit(rtype)
                    .filter(|k| !$m.has_data(k))
                    .map(|s| s.clone())
                    .collect()
            };
        }

        match rtype {
            ResourceType::Cluster => missing_from!(self.data.clusters),
            ResourceType::ClusterLoadAssignment => missing_from!(self.data.load_assignments),
            ResourceType::Listener => missing_from!(self.data.listeners),
            ResourceType::RouteConfiguration => missing_from!(self.data.route_configs),
        }
    }

    /// Get the versions of all resources currently in cache.
    pub(crate) fn versions(&self, rtype: ResourceType) -> HashMap<ResourceName, ResourceVersion> {
        match rtype {
            ResourceType::Cluster => self.data.clusters.versions(),
            ResourceType::ClusterLoadAssignment => self.data.load_assignments.versions(),
            ResourceType::Listener => self.data.listeners.versions(),
            ResourceType::RouteConfiguration => self.data.route_configs.versions(),
        }
    }

    /// Garbage collect the cache.
    ///
    /// Returns the set of subscription changes and dns updates that have
    /// happened since the last call to `collect`.
    ///
    /// See the module docs for more on how to use a cache and when its
    /// appropriate to call `collect`.
    pub(crate) fn collect(&mut self) -> (EnumMap<ResourceType, Changes>, DnsUpdates) {
        // first, garbage collect and accumulate all of the pending changes.
        self.subs.collect();
        let changes = std::mem::take(&mut self.subs.changes);

        // remove actual resource data in reverse make-before-break order
        // based on the change set.
        macro_rules! remove_all {
            ($field:ident, $rtype:expr) => {
                self.data.$field.remove_all(&changes[$rtype].removed)
            };
        }
        remove_all!(route_configs, ResourceType::RouteConfiguration);
        remove_all!(listeners, ResourceType::Listener);
        remove_all!(load_assignments, ResourceType::ClusterLoadAssignment);

        // when removing clusters based on changes, we also have to remove
        // any DNS names for removed clusters.
        for cluster_name in &changes[ResourceType::Cluster].removed {
            // NOTE: this can be an if-let chain once we upgrade to the 2024 edition.
            if let Some((_, entry)) = self.data.clusters.remove(cluster_name) {
                if let Some(cluster) = entry.data {
                    for dns_name in cluster.dns_names() {
                        self.dns.remove_name(dns_name);
                    }
                }
            }
        }
        let dns = self.dns.collect();

        (changes, dns)
    }

    /// Insert new resources into cache.
    ///
    /// Returns an error for each resource that could not be inserted.
    /// Successfully parsed resources are visible to readers as soon as they're
    /// inserted.
    pub(crate) fn insert(
        &mut self,
        rtype: ResourceType,
        resources: Vec<xds_discovery::Resource>,
    ) -> Vec<ResourceError> {
        macro_rules! dispatch {
            ($($variant:pat => $field:ident),* $(,)*) => {
                match rtype {
                    $(
                        $variant => insert_resources(
                            &mut self.subs,
                            &mut self.dns,
                            &self.data.$field,
                            rtype,
                            resources,
                        ),
                    )*
                }
            }
        }

        dispatch!(
            ResourceType::Cluster => clusters,
            ResourceType::ClusterLoadAssignment => load_assignments,
            ResourceType::Listener => listeners,
            ResourceType::RouteConfiguration => route_configs,
        )
    }

    /// Remove a list of resources from cache by name.
    ///
    /// Removing a resource immediately removes data from the cache, but doesn't
    /// change the cache's subscription interest - a caller has told us the resource
    /// data no longer exists, not that the cache shouldn't care about it anymore.
    ///
    /// Removing a resource does modify the subscription graph - when removing a
    /// resource, we need to invalidate any references to other resource types
    /// that it may have subscribed us to. Note that newly-orphaned resources
    /// may not be fully removed from cache until the next call to
    /// [Cache::collect].
    pub(crate) fn remove(&mut self, rtype: ResourceType, names: &[ResourceName]) {
        macro_rules! tombstone_all {
            ($data:ident, $rtype:expr, $names:expr) => {{
                for name in $names {
                    self.data.$data.insert_tombstone(name.clone());
                    self.subs.remove(rtype, name);
                }
            }};
        }

        match rtype {
            ResourceType::Listener => tombstone_all!(listeners, rtype, names),
            ResourceType::RouteConfiguration => tombstone_all!(route_configs, rtype, names),
            ResourceType::Cluster => tombstone_all!(clusters, rtype, names),
            ResourceType::ClusterLoadAssignment => tombstone_all!(load_assignments, rtype, names),
        }
    }
}

// NOTE: this is not a method on Cache because borrowck can't be bothered to
// figure out that data and subs are disjoint. it should be
fn insert_resources<T>(
    subs: &mut Subscriptions,
    dns: &mut DnsNames,
    data: &ResourceMap<T>,
    rtype: ResourceType,
    resources: Vec<xds_discovery::Resource>,
) -> Vec<ResourceError>
where
    T: Resource,
{
    let mut errors = Vec::new();

    for raw_resource in resources {
        let Some(any) = raw_resource.resource else {
            continue;
        };
        let name = raw_resource.name.into();
        let version = raw_resource.version.into();
        let Some(sub) = subs.find_or_subscribe(rtype, &name) else {
            continue;
        };

        // parse and validate
        let resource = match T::from_any(&any) {
            Ok(r) => r,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };

        // reset all outgoing edges and replace them with the new reources
        subs.reset_refs(sub);
        for (ref_type, name) in resource.references() {
            subs.add_ref(sub, ref_type, &name);
        }

        // add any new DNS names to the set of tracked names.
        //
        // NOTE: we know this is almost certainly only going to happen for
        // clusters, but it's less annoying to do the serialization here, once,
        // for all of these types than it is to figure out how to split the
        // cluster-specific steps. we could pass a callback in, but that seems
        // like basically the same thing. just accept the Vec::new call and call
        // it a day.
        for dns_name in resource.dns_names() {
            dns.add_name(dns_name.clone())
        }

        // clear any pending changes for this resource - it's no longer pending
        subs.clear_changes(rtype, &name);
        // actually insert the thing
        data.insert_ok(name, version, resource);
    }

    errors
}

/// A read-only handle to a [Cache]. `CacheReader`s are cheap to clone and
/// share.
#[derive(Default, Clone)]
pub(super) struct CacheReader {
    data: Arc<CacheData>,
}

macro_rules! impl_get {
    ($method_name:ident($field_name:ident)=>$ret:ty) => {
        impl CacheReader {
            pub async fn $method_name(&self, name: &ResourceName) -> Option<Arc<$ret>> {
                self.data
                    .$field_name
                    .get_await(name)
                    .await
                    .and_then(|e| e.data.as_ref().map(|d| Arc::clone(&d)))
            }
        }
    };
}

impl_get!(get_listener(listeners)=>ApiListener);
impl_get!(get_route_config(route_configs)=>RouteConfiguration);
impl_get!(get_cluster(clusters)=>Cluster);
impl_get!(get_load_assignment(load_assignments)=>LoadAssignment);

impl CacheReader {
    pub(super) fn iter_xds(&self) -> impl Iterator<Item = XdsConfig> + '_ {
        self.data.listeners.iter().map(|entry| {
            let name = entry.key().to_string();
            let type_url = ResourceType::Listener.type_url().to_string();
            let version = entry.version.clone();
            let xds = entry.raw_msg.clone();
            let last_error = entry.last_error.clone().map(|(v, e)| (v, e.to_string()));

            XdsConfig {
                name,
                type_url,
                version,
                xds,
                last_error,
            }
        })
    }
}

#[cfg(test)]
mod test {
    use pretty_assertions::assert_eq;
    use xds_api::pb::envoy::config::listener::v3 as xds_listener;

    use super::*;
    use crate::xds::test as xds_test;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn test_reader_send_sync() {
        assert_send::<CacheReader>();
        assert_sync::<CacheReader>();
    }

    #[test]
    fn test_cache_send_sync() {
        assert_send::<Cache>();
        assert_sync::<Cache>();
    }

    macro_rules! resource_names {
        ($($arg:expr),* $(,)?) => {
            [$(
                ResourceName::from($arg.to_string()),
            )*].into_iter().collect()
        }
    }

    macro_rules! versions {
        ($(($k:expr, $v:expr)),* $(,)?) => {
            [$(
                (ResourceName::from($k.to_string()), ResourceVersion::from($v.to_string())),
            )*].into_iter().collect()
        }
    }

    #[track_caller]
    fn assert_insert(cache: &mut Cache, resources: Vec<xds_discovery::Resource>) {
        let first = resources.first().expect("expected a non-empty vec");
        let rtype = ResourceType::from_type_url(
            &first
                .resource
                .as_ref()
                .expect("expected a Resource")
                .type_url,
        )
        .expect("expected a valid type url");

        assert_eq!(
            cache.insert(rtype, resources),
            vec![],
            "errors is not empty",
        );
    }

    #[test]
    fn test_empty_cache() {
        let mut cache = Cache::default();

        // changes should return nothing
        let (rs, dns) = cache.collect();
        assert!(rs.values().all(|v| v.is_empty()));
        assert!(dns.is_noop());

        // there should be no initial versions
        assert!(ResourceType::all()
            .iter()
            .all(|&rtype| cache.versions(rtype).is_empty()));

        // there should be no subscriptions
        assert!(cache.subscriptions(ResourceType::Listener).is_empty());
    }

    #[test]
    fn test_insert_listener_lds_explicit() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, false);

        // insert with no errors, and no effects
        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        );
        let (resources, dns) = cache.collect();
        assert_eq!(resources, EnumMap::default());
        assert!(dns.is_noop());

        // subscribe and clear the resulting subs for the listener. should be
        // able to insert with no errors and generate the subscription to the
        // cluster
        cache.subscribe(
            ResourceType::Listener,
            &ResourceName::from("listener.example.svc.cluster.local"),
        );
        let _ = cache.collect();

        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        );
        let (resources, dns) = cache.collect();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    added: resource_names!["example-route"],
                    removed: BTreeSet::new(),
                 },
                _ => Changes::default(),
            }
        );

        // listener subscriptions should be the listener subscribed to versions
        // and should report the version of the listener we have.
        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let names: Vec<_> = resource_names!["listener.example.svc.cluster.local"];
            names
        });
        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![("listener.example.svc.cluster.local", "v123")]
        );
    }

    #[test]
    fn test_insert_listener_lds_wildcard() {
        let mut cache = Cache::default();

        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        );

        // check that we've added an explicit subscription to the new cluster
        // and that there are no DNS updates.
        let (resources, dns) = cache.collect();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    added: resource_names!["example-route"],
                    removed: BTreeSet::new(),
                 },
                _ => Changes::default(),
            }
        );

        // the listener subscription list should be empty - this listener is a
        // wildcard resource. the current versions for listener/cluster should
        // be just the listener in cache
        assert!(cache.subscriptions(ResourceType::Listener).is_empty());
        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![("listener.example.svc.cluster.local", "v123")]
        );
    }

    #[test]
    fn test_insert_listener_lds_inline_rds() {
        let mut cache = Cache::default();

        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local:80",
                "example-route" => [xds_test::vhost!(
                    "a-virtual-host",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "cluster.example:8008")],
                )],
            )],
        );

        // check that we've added an explicit subscription to the new cluster
        // and that there are no DNS updates.
        let (resources, dns) = cache.collect();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: resource_names!["cluster.example:8008"],
                    removed: BTreeSet::new(),
                 },
                _ => Changes::default(),
            }
        );
    }

    #[test]
    fn test_insert_listener_invalid() {
        let mut cache = Cache::default();
        cache.subscribe(ResourceType::Listener, &ResourceName::from("potato"));
        // clear subscription changes
        let _ = cache.collect();

        // the invalid insert should return an error
        let invalid_listener = xds_test::xds_resource(
            "potato".to_string(),
            "v123".to_string(),
            xds_listener::Listener::default(),
        );
        let errors = cache.insert(ResourceType::Listener, vec![invalid_listener]);
        assert_eq!(errors.len(), 1);

        // should not have changed the cache
        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let names: Vec<_> = resource_names!["potato"];
            names
        });
        assert!(cache.versions(ResourceType::Listener).is_empty());
        let (resources, dns) = cache.collect();
        assert_eq!(resources, Default::default());
        assert!(dns.is_noop());
    }

    #[test]
    fn test_insert_cluster_cds_wildcard() {
        let mut cache = Cache::default();

        assert_insert(
            &mut cache,
            vec![
                xds_test::cluster!(logical_dns => "cluster.example", 7890),
                xds_test::cluster!(eds => "whatever.default.svc.cluster.local:4433"),
            ],
        );

        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::ClusterLoadAssignment => Changes {
                    added: resource_names!["whatever.default.svc.cluster.local:4433"],
                    ..Default::default()
                 },
                _ => Changes::default(),
            }
        );
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::from_iter([Hostname::from_static("cluster.example")]),
                ..Default::default()
            },
        );

        // subscriptions should be empty but current versions should match
        assert!(cache.subscriptions(ResourceType::Cluster).is_empty());
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![
                ("cluster.example:7890", "v123"),
                ("whatever.default.svc.cluster.local:4433", "v123"),
            ]
        );
    }

    #[test]
    fn insert_cluster_cds_no_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, false);

        // subscribe only to the kube backend, clear changes
        cache.subscribe(
            ResourceType::Cluster,
            &ResourceName::from("whatever.default.svc.cluster.local:8008"),
        );
        let _ = cache.collect();

        // insert both clusters at the same version
        assert_insert(
            &mut cache,
            vec![
                xds_test::cluster!(eds => "whatever.default.svc.cluster.local:8008"),
                xds_test::cluster!(logical_dns => "example.com", 80),
            ],
        );

        // only the subscribed cluster should have had an effect
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::ClusterLoadAssignment => Changes {
                    added: resource_names!("whatever.default.svc.cluster.local:8008"),
                    ..Default::default()
                 },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());
        let expected: Vec<_> = resource_names!["whatever.default.svc.cluster.local:8008"];
        assert_eq!(cache.subscriptions(ResourceType::Cluster), expected);
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![("whatever.default.svc.cluster.local:8008", "v123")],
        );
    }

    #[test]
    fn test_insert_route_config() {
        let route_config = xds_test::route_config!(
            "example-route",
            vec![xds_test::vhost!(
                "a-vhost",
                ["listener.example.svc.cluster.local"],
                [xds_test::route!(default "cluster.example:8008")]
            )]
        );

        let mut cache = Cache::default();

        // inserting with no subscription is empty
        assert_insert(&mut cache, vec![route_config.clone()]);
        let (resources, dns) = cache.collect();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert!(cache.data.route_configs.is_empty());

        // insert listener, should now be able to insert the route config
        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route"
            )],
        );

        // should now have a new reference to a cluster
        assert_insert(&mut cache, vec![route_config]);
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: resource_names!["cluster.example:8008"],
                    ..Default::default()
                },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());

        assert_eq!(cache.subscriptions(ResourceType::RouteConfiguration), {
            let names: Vec<_> = resource_names!["example-route"];
            names
        });
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("example-route", "v123")],
        );
    }

    #[test]
    fn test_route_config_add_remove_add() {
        let route_config = xds_test::route_config!(
            "example-route",
            vec![xds_test::vhost!(
                "a-vhost",
                ["listener.example.svc.cluster.local"],
                [xds_test::route!(default "cluster.example:8008")]
            )]
        );

        let mut cache = Cache::default();
        cache.subscribe(
            ResourceType::RouteConfiguration,
            &ResourceName::from("example-route"),
        );

        // add the route
        assert_insert(&mut cache, vec![route_config.clone()]);
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: resource_names!["cluster.example:8008"],
                    ..Default::default()
                },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());
        assert_eq!(cache.subscriptions(ResourceType::RouteConfiguration), {
            let names: Vec<_> = resource_names!["example-route"];
            names
        });
        assert_eq!(cache.subscriptions(ResourceType::Cluster), {
            let names: Vec<_> = resource_names!["cluster.example:8008"];
            names
        });
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("example-route", "v123")],
        );

        // remove the route
        cache.remove(
            ResourceType::RouteConfiguration,
            &[ResourceName::from("example-route")],
        );
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    removed: resource_names!["cluster.example:8008"],
                    ..Default::default()
                },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());
        assert_eq!(cache.subscriptions(ResourceType::RouteConfiguration), {
            let names: Vec<_> = resource_names!["example-route"];
            names
        });
        assert!(cache.subscriptions(ResourceType::Cluster).is_empty());

        // remove the cluster
        //
        // NOTE: it doesn't seeem to matter if this cache.collect() is here
        cache.remove(
            ResourceType::Cluster,
            &[ResourceName::from("cluster.example:8008")],
        );
        let _ = cache.collect();

        // add the route config again
        assert_insert(&mut cache, vec![route_config]);
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: resource_names!["cluster.example:8008"],
                    ..Default::default()
                },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());
        assert_eq!(cache.subscriptions(ResourceType::RouteConfiguration), {
            let names: Vec<_> = resource_names!["example-route"];
            names
        });
        assert_eq!(cache.subscriptions(ResourceType::Cluster), {
            let names: Vec<_> = resource_names!["cluster.example:8008"];
            names
        });
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("example-route", "v123")],
        );
    }

    #[test]
    fn test_insert_load_assignment() {
        let mut cache = Cache::default();

        // try to insert before being referenced
        assert_insert(
            &mut cache,
            vec![xds_test::cla!(
                "whatever.default.svc.cluster.local:7890" => {
                    "zone1" => ["1.1.1.1"]
                }
            )],
        );
        let (resources, dns) = cache.collect();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert!(cache.data.load_assignments.is_empty());

        // insert a cluster
        assert_insert(
            &mut cache,
            vec![xds_test::cluster!(eds => "whatever.default.svc.cluster.local:7890")],
        );
        let _ = cache.collect();

        // try again
        assert_insert(
            &mut cache,
            vec![xds_test::cla!(
                "whatever.default.svc.cluster.local:7890" => {
                    "zone1" => ["1.1.1.1"]
                }
            )],
        );

        // should be inserted, but won't cause changes
        let (resources, dns) = cache.collect();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert_eq!(
            cache.versions(ResourceType::ClusterLoadAssignment),
            versions![("whatever.default.svc.cluster.local:7890", "v123")]
        );
    }

    #[test]
    fn test_remove_listener_no_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, false);
        cache.set_wildcard(ResourceType::Listener, false);

        // subscribe to two listeners
        cache.subscribe(
            ResourceType::Listener,
            &ResourceName::from("listener.example.svc.cluster.local"),
        );
        cache.subscribe(
            ResourceType::Listener,
            &ResourceName::from("listener.local"),
        );
        let _ = cache.collect();

        // insert a listener -> route -> cluster -> dns chain of configuration
        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        );
        assert_insert(
            &mut cache,
            vec![xds_test::route_config!(
                "example-route",
                vec![xds_test::vhost!(
                    "a-vhost",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "cluster.example:8008")]
                )]
            )],
        );
        assert_insert(
            &mut cache,
            vec![xds_test::cluster!(logical_dns => "cluster.example", 8008)],
        );

        // check that the first set of resources makes sense
        let _ = dbg!(cache.collect());
        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![("listener.example.svc.cluster.local", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("example-route", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![("cluster.example:8008", "v123")],
        );
        assert!(cache
            .versions(ResourceType::ClusterLoadAssignment)
            .is_empty());
        assert_eq!(
            Vec::<ResourceName>::new(),
            cache.subscriptions(ResourceType::ClusterLoadAssignment),
        );

        // add the second listener/route-config pointing to the same cluster
        // and remove the first one
        cache.remove(
            ResourceType::Listener,
            &[ResourceName::from("listener.example.svc.cluster.local")],
        );
        assert_insert(
            &mut cache,
            vec![xds_test::listener!(
                "listener.local",
                "better-example-route",
            )],
        );
        assert_insert(
            &mut cache,
            vec![xds_test::route_config!(
                "better-example-route",
                vec![xds_test::vhost!(
                    "a-vhost",
                    ["listener.local"],
                    [xds_test::route!(default "cluster.example:8008")]
                )]
            )],
        );

        // should add a remove for the RouteConfig. the cache is still subscribed
        // to the removed Listener, so it shouldn't appear here.
        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    removed: resource_names!["example-route"],
                    ..Default::default()
                },
                _ => Changes::default()
            }
        );
        assert!(dns.is_noop());

        // cache should only contain the data versions for what's there, but
        // should have subscriptions to the original two listeners and the
        // cluster lb config listener
        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let names: Vec<_> =
                resource_names!["listener.example.svc.cluster.local", "listener.local",];
            names
        },);

        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![("listener.local", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("better-example-route", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![("cluster.example:8008", "v123")],
        );

        // removing the other listener should drop all data from cache,
        // but keep both subscriptions.
        cache.remove(
            ResourceType::Listener,
            &[ResourceName::from("listener.local")],
        );
        let (resources, dns) = cache.collect();
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::new(),
                remove: [Hostname::from_static("cluster.example")]
                    .into_iter()
                    .collect(),
                sync: false,
            }
        );

        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: resource_names!("better-example-route"),
                    ..Default::default()
                },
                ResourceType::Cluster => Changes {
                    removed: resource_names!("cluster.example:8008"),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes::default(),
            }
        );

        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let names: Vec<_> =
                resource_names!["listener.example.svc.cluster.local", "listener.local"];
            names
        });
        for &rtype in ResourceType::all() {
            assert_eq!(cache.versions(rtype), HashMap::new());
        }
    }

    #[test]
    fn test_remove_listener_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, true);
        cache.set_wildcard(ResourceType::Listener, true);

        // subscribe to one listener
        cache.subscribe(
            ResourceType::Listener,
            &ResourceName::from("listener.example.svc.cluster.local"),
        );
        let _ = cache.collect();

        // insert two listener->route pairs pointing at the same cluster
        assert_insert(
            &mut cache,
            vec![
                xds_test::listener!("listener.example.svc.cluster.local", "example-route"),
                xds_test::listener!("listener.local", "better-example-route"),
            ],
        );
        assert_insert(
            &mut cache,
            vec![
                xds_test::route_config!(
                    "example-route",
                    vec![xds_test::vhost!(
                        "a-vhost",
                        ["listener.example.svc.cluster.local"],
                        [xds_test::route!(default "cluster.example:8008")]
                    )]
                ),
                xds_test::route_config!(
                    "better-example-route",
                    vec![xds_test::vhost!(
                        "a-vhost",
                        ["listener.local"],
                        [xds_test::route!(default "cluster.example:8008")]
                    )]
                ),
            ],
        );
        assert_insert(
            &mut cache,
            vec![xds_test::cluster!(logical_dns => "cluster.example", 8008)],
        );

        // check that the first set of resources makes sense
        let _ = cache.collect();
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![("cluster.example:8008", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![
                ("listener.local", "v123"),
                ("listener.example.svc.cluster.local", "v123"),
            ],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("example-route", "v123"), ("better-example-route", "v123")],
        );
        assert!(cache
            .versions(ResourceType::ClusterLoadAssignment)
            .is_empty());

        // remove the explicitly subscribed listener
        cache.remove(
            ResourceType::Listener,
            &[ResourceName::from("listener.example.svc.cluster.local")],
        );

        let (resources, dns) = cache.collect();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    removed: resource_names!["example-route"],
                    ..Default::default()
                },
                _ => Changes::default()
            }
        );
        assert!(dns.is_noop());

        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let name: Vec<_> = resource_names!["listener.example.svc.cluster.local",];
            name
        });
        assert_eq!(
            cache.versions(ResourceType::Listener),
            versions![("listener.local", "v123"),],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            versions![("better-example-route", "v123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            versions![("cluster.example:8008", "v123")],
        );

        // removing the wildcard listener should drop the rest of the data
        cache.remove(
            ResourceType::Listener,
            &[ResourceName::from("listener.local")],
        );
        let (resources, dns) = cache.collect();
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::new(),
                remove: [Hostname::from_static("cluster.example")]
                    .into_iter()
                    .collect(),
                sync: false,
            }
        );
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: resource_names!("better-example-route"),
                    ..Default::default()
                },
                ResourceType::Cluster => Changes {
                    removed: resource_names!("cluster.example:8008"),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes::default(),
            }
        );

        assert_eq!(cache.subscriptions(ResourceType::Listener), {
            let names: Vec<_> = resource_names!["listener.example.svc.cluster.local"];
            names
        },);
        assert_eq!(cache.versions(ResourceType::Listener), versions![]);
    }
}
