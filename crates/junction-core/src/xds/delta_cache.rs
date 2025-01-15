//! in progress impl notes:
//!
//! - 2025/01/16 - need to finish tests for all the fiddly LB config listener stuff and for the GC.
//!   also the way wildcards get detected at insert is wrong - it's a wildcard if we
//!   have no subscription for it, but inserted it anyway and are willing to keep it.
//!
//! - the way the DNS stuff worked out is nice. it's based on the actual resources
//!   which is good. just keep bubbling it up.
//!
//! - should we move some from_xds tests out of here?
//!
//! - can we use this cache for sotw too? that would be nice. can probably if we can
//!   clear a resource type before inserting LDS/RDS. then have to figure out subs?
//!
//! - can we use this for fetch? seems like it would absolutely work - fetch a
//!   Resource (maybe by name, maybe empty list for wildcard) then insert into the
//!   cache, look for changes, and keep iterating until we have a fixed-point.
//!
//!   behavior would be different than the streaming Client - is that bad? could
//!
//! - the stick-a-oneshot-in-the-cache method of subscribing still kinda seems
//!   nice, now that we're fully on get_await. have to clean up in here enough
//!   that maybe it's easy to go back to? there's less racy nonsense that way,
//!   a call is either subscribed to a resource and gets it on insert/it being
//!   there, or it doesn't.
//!
//!

use std::{
    collections::{BTreeSet, HashMap},
    str::FromStr,
    sync::Arc,
};

use crossbeam_skiplist::SkipMap;
use enum_map::EnumMap;
use junction_api::{backend::BackendId, http::Route, Hostname, Service};
use petgraph::{
    graph::{DiGraph, NodeIndex},
    visit::{EdgeRef, Visitable},
    Direction,
};
use tokio::sync::Notify;
use xds_api::pb::envoy::config::{
    cluster::v3 as xds_cluster, endpoint::v3 as xds_endpoint, listener::v3 as xds_listener,
    route::v3 as xds_route,
};
use xds_api::pb::google::protobuf;

use crate::{endpoints::EndpointGroup, BackendLb};

use super::{
    resources::{
        ApiListener, ApiListenerData, Cluster, LoadAssignment, ResourceError, RouteConfig,
        RouteConfigData,
    },
    ConfigCache, DnsUpdates, ResourceType, ResourceVec, ResourceVersion, XdsConfig,
};

#[derive(Clone, Debug)]
struct CacheEntry<T> {
    version: Option<ResourceVersion>,
    last_error: Option<(ResourceVersion, ResourceError)>,
    data: Option<T>,
}

impl<T> Default for CacheEntry<T> {
    fn default() -> Self {
        Self {
            version: None,
            last_error: None,
            data: None,
        }
    }
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
struct ResourceMap<T> {
    changed: Arc<Notify>,
    map: SkipMap<String, CacheEntry<T>>,
}

// NOTE: manually derived because the Derive macro requires T: Default, and
// this impl doesn't care - an empty map doesn't need a T.
impl<T> Default for ResourceMap<T> {
    fn default() -> Self {
        Self {
            changed: Arc::new(Notify::new()),
            map: Default::default(),
        }
    }
}

impl<T: Send + 'static> ResourceMap<T> {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn get<'a>(&'a self, name: &str) -> Option<ResourceEntry<'a, T>> {
        self.map.get(name).map(ResourceEntry)
    }

    async fn get_await<'a>(&'a self, name: &str) -> Option<ResourceEntry<'a, T>> {
        // fast path: try a get and return it if it works out.
        if let Some(entry) = self.map.get(name).map(ResourceEntry) {
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
            if let Some(entry) = self.map.get(name).map(ResourceEntry) {
                return Some(entry);
            }

            // wait for a change
            changed.as_mut().await;

            // this uses Pin::set so we're not allocating/deallocating a new
            // wakeup future every time.
            changed.set(self.changed.notified());
        }
    }

    fn iter(&self) -> impl Iterator<Item = ResourceEntry<T>> + '_ {
        self.map.iter().map(ResourceEntry)
    }

    fn contains_key(&self, k: &str) -> bool {
        self.map.contains_key(k)
    }

    fn names(&self) -> impl Iterator<Item = String> + '_ {
        self.map.iter().map(|e| e.key().clone())
    }

    fn versions(&self) -> HashMap<String, String> {
        let mut versions = HashMap::new();
        for entry in self.iter() {
            let Some(version) = entry.version() else {
                continue;
            };

            let name = entry.name().to_string();
            let version = version.to_string();
            versions.insert(name, version);
        }

        versions
    }

    fn remove(&self, name: &str) -> Option<ResourceEntry<T>> {
        let entry = self.map.remove(name);
        self.changed.notify_waiters();
        entry.map(ResourceEntry)
    }

    fn remove_all<I>(&self, names: I)
    where
        I: IntoIterator<Item: AsRef<str>>,
    {
        for name in names {
            self.map.remove(name.as_ref());
            self.changed.notify_waiters();
        }
    }
}

impl<X, T> ResourceMap<T>
where
    T: CacheEntryData<Xds = X> + Clone + Send + 'static,
    X: PartialEq + prost::Name,
{
    // fn touch(&self, name: &str) -> bool {
    //     if self.map.contains_key(name) {
    //         return false;
    //     }

    //     self.map.insert(name.to_string(), Default::default());
    //     true
    // }

    fn insert_ok(&self, name: String, version: ResourceVersion, t: T) {
        self.map.insert(
            name,
            CacheEntry {
                version: Some(version),
                last_error: None,
                data: Some(t),
            },
        );
        self.changed.notify_waiters();
    }

    fn insert_error<E: Into<ResourceError>>(
        &self,
        name: String,
        version: ResourceVersion,
        error: E,
    ) {
        match self.map.get(&name) {
            Some(entry) => {
                let mut updated_entry = entry.value().clone();
                updated_entry.last_error = Some((version, error.into()));
                self.map.insert(name, updated_entry);
                self.changed.notify_waiters();
            }
            None => {
                self.map.insert(
                    name,
                    CacheEntry {
                        version: None,
                        last_error: Some((version, error.into())),
                        data: None,
                    },
                );
                self.changed.notify_waiters();
            }
        }
    }

    // TDODO: should this compare version to short-circuit full eq?
    fn is_changed(&self, name: &str, t: &X) -> bool {
        let Some(entry) = self.map.get(name) else {
            return true;
        };

        let Some(entry_data) = &entry.value().data else {
            return true;
        };
        entry_data.xds() != t
    }
}

struct ResourceEntry<'a, T>(crossbeam_skiplist::map::Entry<'a, String, CacheEntry<T>>);

impl<T> ResourceEntry<'_, T> {
    fn name(&self) -> &str {
        self.0.key()
    }

    fn version(&self) -> Option<&ResourceVersion> {
        self.0.value().version.as_ref()
    }

    fn last_error(&self) -> Option<&(ResourceVersion, ResourceError)> {
        self.0.value().last_error.as_ref()
    }

    fn data(&self) -> Option<&T> {
        self.0.value().data.as_ref()
    }
}

/// Subscription tracking for xDS. This struct exists with the expectation that
/// it's owned by a single [Cache] and is being used to track the relationships
/// between resources in that cache. No introspection of xDS is done here.
///
/// After updating subscriptions, the [Cache] that owns this set of
/// subscriptions is expected to call [changes][Self::changes] to clean up the
/// cache and get a list of what has changed.
///
/// # Wildcard Mode
///
/// There
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
    name: String,

    // true if there is explicit interest in this resource via subscribe.
    explicit: bool,

    // true if this subscription was added by inserting a resource that didn't
    // have an existing subscription.
    wildcard: bool,
}

/// The set of subscription changes for a single resource type.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// The names of any newly added subscriptions. Newly added subscriptions
    /// may or may not be in cache, and must be requested from the xDS server.
    pub(crate) added: BTreeSet<String>,

    /// The names of any removed subscriptions. Callers should assume that any
    /// removed names are also removed from cache.
    pub(crate) removed: BTreeSet<String>,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl Subscriptions {
    fn explicit(&self, rtype: ResourceType) -> impl Iterator<Item = &str> + '_ {
        self.subs
            .node_weights()
            .filter(move |w| w.resource_type == rtype && !w.wildcard)
            .map(|w| &w.name[..])
    }

    fn subscribe(&mut self, rtype: ResourceType, name: &str) {
        // explicit subscription means never a wildcard
        let sub = self.find_or_create(rtype, name, false);
        self.subs[sub].explicit = true;
    }

    fn unsubscribe(&mut self, rtype: ResourceType, name: &str) {
        if let Some(sub) = self.find(rtype, name) {
            self.subs[sub].explicit = false;
        }
    }

    fn remove(&mut self, rtype: ResourceType, name: &str) {
        if let Some(sub) = self.find(rtype, name) {
            self.remove_sub(sub);
        }
    }

    #[inline]
    fn clear_changes(&mut self, rtype: ResourceType, name: &str) {
        self.changes[rtype].added.remove(name);
        self.changes[rtype].removed.remove(name);
    }

    /// safety: must be called with a valid NodeIndex
    fn remove_sub(&mut self, sub: NodeIndex) {
        let sub = self.subs.remove_node(sub).unwrap();
        self.changes[sub.resource_type].removed.insert(sub.name);
    }

    fn find_subcribed(&mut self, rtype: ResourceType, name: &str) -> Option<NodeIndex> {
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

    fn find(&self, rtype: ResourceType, name: &str) -> Option<NodeIndex> {
        self.subs.node_indices().find(|idx| {
            let sub = &self.subs[*idx];
            sub.resource_type == rtype && sub.name == name
        })
    }

    fn find_or_create(&mut self, rtype: ResourceType, name: &str, wildcard: bool) -> NodeIndex {
        match self.find(rtype, name) {
            Some(idx) => idx,
            None => {
                let idx = self.subs.add_node(SubscriptionInfo {
                    name: name.to_string(),
                    resource_type: rtype,
                    explicit: false,
                    wildcard,
                });

                // track that this is a newly created sub
                if !wildcard {
                    self.changes[rtype].added.insert(name.to_string());
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
    fn add_ref(&mut self, from_sub: NodeIndex, rtype: ResourceType, name: &str) {
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

#[derive(Debug, Default)]
pub(super) struct Cache {
    subs: Subscriptions,
    data: Arc<CacheData>,
    dns: DnsUpdates,
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

    #[cfg(test)]
    pub(crate) fn set_wildcard(&mut self, rtype: ResourceType, wildcard: bool) {
        if !rtype.supports_wildcard() {
            return;
        }
        self.subs.wildcard[rtype] = wildcard;
    }

    pub(crate) fn is_wildcard(&self, rtype: ResourceType) -> bool {
        self.subs.wildcard[rtype]
    }

    pub(crate) fn subscribe(&mut self, rtype: ResourceType, name: &str) {
        self.subs.subscribe(rtype, name);
    }

    pub(crate) fn subscribe_dns(&mut self, hostname: Hostname, port: u16) {
        self.dns.add.insert((hostname, port));
    }

    pub(crate) fn unsubscribe(&mut self, rtype: ResourceType, name: &str) {
        self.subs.unsubscribe(rtype, name);
    }

    pub(crate) fn unsubscribe_dns(&mut self, hostname: Hostname, port: u16) {
        self.dns.remove.insert((hostname, port));
    }

    pub(crate) fn subscriptions(&self, rtype: ResourceType) -> Vec<String> {
        self.subs.explicit(rtype).map(|s| s.to_string()).collect()
    }

    pub(crate) fn initial_subscriptions(&self, rtype: ResourceType) -> Vec<String> {
        macro_rules! missing_from {
            ($m:expr) => {
                self.subs
                    .explicit(rtype)
                    .filter(|k| !$m.contains_key(k))
                    .map(|s| s.to_string())
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

    pub(crate) fn versions(&self, rtype: ResourceType) -> HashMap<String, String> {
        match rtype {
            ResourceType::Cluster => self.data.clusters.versions(),
            ResourceType::ClusterLoadAssignment => self.data.load_assignments.versions(),
            ResourceType::Listener => self.data.listeners.versions(),
            ResourceType::RouteConfiguration => self.data.route_configs.versions(),
        }
    }

    // FIXME: garbage collect the cache, and return the changes to it since the
    // last time we got changes.  this should be the set of added/removed items
    // to send messages about modulo the things we gc'd
    //
    // FIXME: should we collect on every insert?
    pub(crate) fn changes(&mut self) -> (EnumMap<ResourceType, Changes>, DnsUpdates) {
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
        let mut dns = std::mem::take(&mut self.dns);
        for cluster_name in &changes[ResourceType::Cluster].removed {
            if let Some(entry) = self.data.clusters.remove(&cluster_name) {
                let dns_name = entry.data().and_then(|c| {
                    let svc = c.id();
                    match &svc.service {
                        Service::Dns(dns) => Some((dns.hostname.clone(), svc.port)),
                        _ => None,
                    }
                });

                if let Some(dns_name) = dns_name {
                    dns.remove.insert(dns_name);
                }
            }
        }

        (changes, dns)
    }

    pub(crate) fn insert(&mut self, resources: ResourceVec) -> Vec<ResourceError> {
        match resources {
            ResourceVec::Cluster(clusters) => self.insert_clusters(clusters),
            ResourceVec::ClusterLoadAssignment(clas) => self.insert_load_assignments(clas),
            ResourceVec::Listener(listeners) => self.insert_listeners(listeners),
            ResourceVec::RouteConfiguration(rcs) => self.insert_route_configs(rcs),
        }
    }

    pub(crate) fn remove(&mut self, rtype: ResourceType, names: &[String]) {
        macro_rules! remove_all {
            ($data:ident, $rtype:expr, $names:expr) => {{
                for name in $names {
                    self.subs.remove($rtype, name);
                    self.data.$data.remove(name);
                }
            }};
        }

        match rtype {
            ResourceType::Listener => remove_all!(listeners, rtype, names),
            ResourceType::RouteConfiguration => remove_all!(route_configs, rtype, names),
            ResourceType::Cluster => remove_all!(clusters, rtype, names),
            ResourceType::ClusterLoadAssignment => remove_all!(load_assignments, rtype, names),
        }
    }

    fn insert_listeners(
        &mut self,
        listeners: Vec<(ResourceVersion, xds_listener::Listener)>,
    ) -> Vec<ResourceError> {
        let mut errors = Vec::new();

        for (version, listener) in listeners {
            if !self.data.listeners.is_changed(&listener.name, &listener) {
                continue;
            }
            let Some(sub) = self
                .subs
                .find_subcribed(ResourceType::Listener, &listener.name)
            else {
                continue;
            };

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

            // reset all outgoing references
            self.subs.reset_refs(sub);

            match &api_listener.route_config {
                // RDS: add a reference from this Listener to a RouteConfiguration
                ApiListenerData::Rds(rc_name) => {
                    self.subs
                        .add_ref(sub, ResourceType::RouteConfiguration, rc_name.as_str());
                }
                // with an inline RouteConfiguration, add a reference to all of
                // the clusters that this Listener points to.
                ApiListenerData::Inlined(RouteConfigData::Route { clusters, .. }) => {
                    for cluster in clusters {
                        self.subs
                            .add_ref(sub, ResourceType::Cluster, cluster.as_str());
                    }
                }
                // policy RouteConfigurations are the *targets* of references,
                // and shouldn't actually reference any resources themselves.
                // the only thing we should do here is update the clusters that
                // point at this policy.
                ApiListenerData::Inlined(RouteConfigData::LbPolicy { action, cluster }) => {
                    // inserting the cluster should have already subscribed to
                    // this route implicitly! don't recreate the edge in the
                    // other direction.
                    //
                    // rebuild the cluster with the new LbPolicy from this
                    // listener. we don't have to use the ref graph here, since
                    // we have the cluster name (effectively the parent-pointer)
                    // in the LbPolicy.
                    //
                    // TODO: remove this from Listener and only have Routes serve this purpose.
                    let version_and_xds = self.data.clusters.get(cluster.as_str()).and_then(|e| {
                        let version = e.version();
                        let data = e.data();
                        version.zip(data).map(|(v, d)| (v.clone(), d.xds.clone()))
                    });
                    let res = match version_and_xds {
                        Some((version, xds)) => {
                            self.insert_cluster(version, xds, Some(Arc::clone(action)))
                        }
                        None => Ok(()),
                    };
                    if let Err(e) = res {
                        self.data
                            .listeners
                            .insert_error(listener_name, version, e.clone());
                        errors.push(e);
                        continue;
                    }
                }
            }

            // do the update
            self.subs
                .clear_changes(ResourceType::Listener, &listener_name);
            self.data
                .listeners
                .insert_ok(listener_name, version, api_listener);
        }

        errors
    }

    fn insert_clusters(
        &mut self,
        clusters: Vec<(ResourceVersion, xds_cluster::Cluster)>,
    ) -> Vec<ResourceError> {
        let mut errors = Vec::new();

        for (version, cluster) in clusters {
            if !self.data.clusters.is_changed(&cluster.name, &cluster) {
                continue;
            }

            let lb_action = self.find_lb_action(&cluster.name);
            if let Err(e) = self.insert_cluster(version, cluster, lb_action) {
                errors.push(e);
            }
        }

        errors
    }

    fn insert_cluster(
        &mut self,
        version: ResourceVersion,
        cluster: xds_cluster::Cluster,
        lb_policy: Option<Arc<xds_route::RouteAction>>,
    ) -> Result<(), ResourceError> {
        let Some(sub) = self
            .subs
            .find_subcribed(ResourceType::Cluster, &cluster.name)
        else {
            return Ok(());
        };

        let cluster_name = cluster.name.clone();
        let cluster = match Cluster::from_xds(cluster, lb_policy.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.data
                    .clusters
                    .insert_error(cluster_name, version.clone(), e.clone());
                return Err(e);
            }
        };

        // reset all outgoing references since we know we're updating
        self.subs.reset_refs(sub);

        // point to the CLA for this cluster or start a DNS subscription for it.
        let cluster_id = cluster.id();
        match &cluster_id.service {
            Service::Dns(dns) => {
                self.dns.add.insert((dns.hostname.clone(), cluster_id.port));
            }
            _ => self
                .subs
                .add_ref(sub, ResourceType::ClusterLoadAssignment, &cluster_name),
        }

        // point to the LB config Listener for this cluster. pointing to the Listener
        // means that the control plane has the option of sending us either a Listener
        // or a RouteConfig for the Lb Config.
        //
        // TODO: make this a RouteConfig instead of a listener?
        let lb_config_name = cluster.backend_lb.config.id.lb_config_route_name();
        self.subs
            .add_ref(sub, ResourceType::Listener, &lb_config_name);

        // actually insert the data
        self.subs
            .clear_changes(ResourceType::Cluster, &cluster_name);
        self.data.clusters.insert_ok(cluster_name, version, cluster);

        Ok(())
    }

    fn find_lb_action(&self, cluster_name: &str) -> Option<Arc<xds_route::RouteAction>> {
        let target = BackendId::from_str(cluster_name).ok()?;
        let listener = self.data.listeners.get(&target.lb_config_route_name())?;

        match &listener.data()?.route_config {
            ApiListenerData::Rds(name) => {
                let route_config = self.data.route_configs.get(name.as_str())?;
                route_config.data().and_then(|rc| match &rc.data {
                    RouteConfigData::LbPolicy { action, .. } => Some(action.clone()),
                    _ => None,
                })
            }
            ApiListenerData::Inlined(data) => match &data {
                RouteConfigData::LbPolicy { action, .. } => Some(action.clone()),
                _ => None,
            },
        }
    }

    fn insert_route_configs(
        &mut self,
        route_configs: Vec<(ResourceVersion, xds_route::RouteConfiguration)>,
    ) -> Vec<ResourceError> {
        let mut errors = Vec::new();

        for (version, route_config) in route_configs {
            let Some(sub) = self
                .subs
                .find_subcribed(ResourceType::RouteConfiguration, &route_config.name)
            else {
                continue;
            };

            let route_name = route_config.name.clone();
            let route_config = match RouteConfig::from_xds(route_config) {
                Ok(route_config) => route_config,
                Err(e) => {
                    self.data
                        .route_configs
                        .insert_error(route_name, version, e.clone());
                    errors.push(e);
                    continue;
                }
            };

            match &route_config.data {
                // add a ref to all downstream clusters
                RouteConfigData::Route { clusters, .. } => {
                    for cluster in clusters {
                        self.subs
                            .add_ref(sub, ResourceType::Cluster, cluster.as_str());
                    }
                }
                // this is Lb policy, update the cluster it's attached to.
                RouteConfigData::LbPolicy { action, cluster } => {
                    // inserting the cluster should have already subscribed to
                    // this route implicitly! don't recreate the edge in the
                    // other direction.
                    //
                    // rebuild the cluster with the new LbPolicy from this
                    // route. we don't have to use the ref graph here, since
                    // we have the cluster name (effectively the parent-pointer)
                    // in the LbPolicy.
                    //
                    // TODO: remove this from Listener and only have Routes serve this purpose.
                    let version_and_xds = self.data.clusters.get(cluster.as_str()).and_then(|e| {
                        let version = e.version();
                        let data = e.data();
                        version.zip(data).map(|(v, d)| (v.clone(), d.xds.clone()))
                    });
                    let res = match version_and_xds {
                        Some((version, xds)) => {
                            self.insert_cluster(version, xds, Some(Arc::clone(action)))
                        }
                        None => Ok(()),
                    };
                    if let Err(e) = res {
                        self.data
                            .route_configs
                            .insert_error(route_name, version, e.clone());
                        errors.push(e);
                        continue;
                    }
                }
            }

            // complete the insert
            self.subs
                .clear_changes(ResourceType::RouteConfiguration, &route_name);
            self.data
                .route_configs
                .insert_ok(route_name, version, route_config);
        }

        errors
    }

    fn insert_load_assignments(
        &mut self,
        load_assignments: Vec<(ResourceVersion, xds_endpoint::ClusterLoadAssignment)>,
    ) -> Vec<ResourceError> {
        let mut errors = Vec::new();

        for (version, load_assignment) in load_assignments {
            let sub = self.subs.find_subcribed(
                ResourceType::ClusterLoadAssignment,
                &load_assignment.cluster_name,
            );
            if !sub.is_some() {
                continue;
            };

            let cla_name = load_assignment.cluster_name.clone();
            match LoadAssignment::from_xds(load_assignment) {
                Ok(cla) => {
                    self.subs
                        .clear_changes(ResourceType::ClusterLoadAssignment, &cla_name);
                    self.data.load_assignments.insert_ok(cla_name, version, cla);
                }
                Err(e) => {
                    self.data
                        .load_assignments
                        .insert_error(cla_name, version, e.clone());
                    errors.push(e);
                }
            };
        }

        errors
    }
}

/// A read-only handle to a [Cache]. `CacheReader`s are meant to passed around
/// and shared, and are cheap to clone.
#[derive(Default, Clone)]
pub(super) struct CacheReader {
    data: Arc<CacheData>,
}

impl ConfigCache for CacheReader {
    async fn get_route<S: AsRef<str>>(&self, host: S) -> Option<Arc<Route>> {
        let listener = self.data.listeners.get_await(host.as_ref()).await?;

        match &listener.data()?.route_config {
            ApiListenerData::Rds(name) => {
                let route_config = self.data.route_configs.get_await(name.as_str()).await?;

                route_config.data().and_then(|rc| match &rc.data {
                    RouteConfigData::Route { route, .. } => Some(route.clone()),
                    _ => None,
                })
            }
            ApiListenerData::Inlined(data) => match &data {
                RouteConfigData::Route { route, .. } => Some(route.clone()),
                _ => None,
            },
        }
    }

    async fn get_backend(&self, id: &BackendId) -> Option<Arc<BackendLb>> {
        let cluster = self.data.clusters.get_await(&id.name()).await?;
        let cluster_data = cluster.data()?;
        Some(cluster_data.backend_lb.clone())
    }

    async fn get_endpoints(&self, id: &BackendId) -> Option<Arc<EndpointGroup>> {
        let la = self.data.load_assignments.get_await(&id.name()).await?;
        let la_data = la.data()?;
        Some(la_data.endpoint_group.clone())
    }
}

impl CacheReader {
    pub(super) fn iter_routes(&self) -> impl Iterator<Item = Arc<Route>> + '_ {
        let listener_routes = self.data.listeners.iter().filter_map(|entry| {
            entry
                .data()
                .and_then(|api_listener| match &api_listener.route_config {
                    ApiListenerData::Inlined(RouteConfigData::Route { route, .. }) => {
                        Some(route.clone())
                    }
                    _ => None,
                })
        });

        let route_config_routes = self.data.route_configs.iter().filter_map(|entry| {
            entry.data().and_then(|rc| match &rc.data {
                RouteConfigData::Route { route, .. } => Some(route.clone()),
                _ => None,
            })
        });

        listener_routes.chain(route_config_routes)
    }

    pub(super) fn iter_backends(&self) -> impl Iterator<Item = Arc<BackendLb>> + '_ {
        self.data
            .clusters
            .iter()
            .filter_map(|entry| entry.data().map(|cluster| cluster.backend_lb.clone()))
    }

    pub(super) fn iter_xds(&self) -> impl Iterator<Item = XdsConfig> + '_ {
        use prost::Name;

        macro_rules! any_iter {
            ($field:ident, $xds_type:ty) => {
                self.data.$field.iter().map(|entry| {
                    let name = entry.name().to_string();
                    let type_url = <$xds_type>::type_url();
                    let version = entry.version().cloned();

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

#[cfg(test)]
mod test {
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

    macro_rules! collect_str {
        ($($arg:expr),* $(,)?) => {
            [$(
                    $arg.to_string(),
            )*].into_iter().collect()
        }
    }

    macro_rules! collect_kv_str {
        ($(($k:expr, $v:expr)),* $(,)?) => {
            [$(
                ($k.to_string(), $v.to_string()),
            )*].into_iter().collect()
        }
    }

    #[track_caller]
    fn assert_insert(errors: Vec<ResourceError>) {
        assert!(errors.is_empty(), "errors is not empty: {errors:?}");
    }

    #[test]
    fn test_empty_cache() {
        let mut cache = Cache::default();

        // changes should return nothing
        let (rs, dns) = cache.changes();
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
        let listeners = ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        );

        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Listener, false);

        // insert with no errors, and no effects
        assert_insert(cache.insert(listeners.clone()));
        let (resources, dns) = cache.changes();
        assert_eq!(resources, EnumMap::default());
        assert!(dns.is_noop());

        // subscribe and clear the resulting subs for the listener
        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        let _ = cache.changes();

        // insert with no errors and generate the subscription to the cluster
        assert_insert(cache.insert(listeners));
        let (resources, dns) = cache.changes();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    added: collect_str!["example-route"],
                    removed: BTreeSet::new(),
                 },
                _ => Changes::default(),
            }
        );

        // listener subscriptions should be the listener subscribed to versions
        // and should report the version of the listener we have.
        assert_eq!(
            cache.subscriptions(ResourceType::Listener),
            vec!["listener.example.svc.cluster.local"]
        );
        assert_eq!(
            cache.versions(ResourceType::Listener),
            collect_kv_str![("listener.example.svc.cluster.local", "123")]
        );
    }

    #[test]
    fn test_insert_listener_lds_wildcard() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        )));

        // check that we've added an explicit subscription to the new cluster
        // and that there are no DNS updates.
        let (resources, dns) = cache.changes();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::RouteConfiguration => Changes {
                    added: collect_str!["example-route"],
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
            collect_kv_str![("listener.example.svc.cluster.local", "123")]
        );
    }

    #[test]
    fn test_insert_listener_lds_inline_rds() {
        let mut cache = Cache::default();

        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local:80",
                "example-route" => [xds_test::vhost!(
                    "a-virtual-host",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "cluster.example:8008")],
                )],
            )],
        )));

        // check that we've added an explicit subscription to the new cluster
        // and that there are no DNS updates.
        let (resources, dns) = cache.changes();
        assert!(dns.is_noop());
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: collect_str!["cluster.example:8008"],
                    removed: BTreeSet::new(),
                 },
                _ => Changes::default(),
            }
        );
    }

    #[test]
    fn test_insert_listener_invalid() {
        let mut cache = Cache::default();
        cache.subscribe(ResourceType::Listener, "potato");
        // clear subscription changes
        let _ = cache.changes();

        // the invalid insert should return an error
        let errors = cache.insert(ResourceVec::from_listeners(
            "123".into(),
            [xds_listener::Listener {
                name: "potato".to_string(),
                ..Default::default()
            }],
        ));
        assert_eq!(errors.len(), 1);

        // should not have changed the cache
        assert_eq!(cache.subscriptions(ResourceType::Listener), vec!["potato"]);
        assert!(cache.versions(ResourceType::Listener).is_empty());
        let (resources, dns) = cache.changes();
        assert_eq!(resources, Default::default());
        assert!(dns.is_noop());
    }

    #[test]
    fn test_insert_cluster_cds_wildcard() {
        let mut cache = Cache::default();

        let kube_backend = BackendId {
            service: Service::kube("default", "whatever").unwrap(),
            port: 7890,
        };
        let dns_backend = BackendId {
            service: Service::dns("cluster.example").unwrap(),
            port: 4433,
        };

        assert_insert(cache.insert(ResourceVec::from_clusters(
            "123".into(),
            vec![
                xds_test::cluster!(dns_backend.name().leak()),
                xds_test::cluster!(kube_backend.name().leak()),
            ],
        )));

        let (resources, dns) = cache.changes();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    added: collect_str!(
                        kube_backend.lb_config_route_name(),
                        dns_backend.lb_config_route_name(),
                    ),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes {
                    added: collect_str![kube_backend.name()],
                    ..Default::default()
                 },
                _ => Changes::default(),
            }
        );
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::from_iter([(Hostname::from_static("cluster.example"), 4433)]),
                ..Default::default()
            },
        );

        // subscriptions should be empty but current versions should match
        assert!(cache.subscriptions(ResourceType::Cluster).is_empty());
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![(kube_backend.name(), "123"), (dns_backend.name(), "123"),]
        );
    }

    #[test]
    fn test_insert_cluster_cds_explicit() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, false);

        let kube_backend = BackendId {
            service: Service::kube("default", "whatever").unwrap(),
            port: 7890,
        };
        let dns_backend = BackendId {
            service: Service::dns("cluster.example").unwrap(),
            port: 4433,
        };

        // subscribe only to the kube backend, clear changes
        cache.subscribe(ResourceType::Cluster, &kube_backend.name());
        let _ = cache.changes();

        // insert both clusters at the same version
        assert_insert(cache.insert(ResourceVec::from_clusters(
            "123".into(),
            vec![
                xds_test::cluster!(dns_backend.name().leak()),
                xds_test::cluster!(kube_backend.name().leak()),
            ],
        )));

        // only the kbue cluster should have had an effect, no DNS updates
        let (resources, dns) = cache.changes();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    added: collect_str!(
                        kube_backend.lb_config_route_name(),
                    ),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes {
                    added: collect_str![kube_backend.name()],
                    ..Default::default()
                 },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());

        // should have the explicit subscription to one cluster and one version
        assert_eq!(
            cache.subscriptions(ResourceType::Cluster),
            vec![kube_backend.name()],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![(kube_backend.name(), "123")]
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
        assert_insert(cache.insert(ResourceVec::from_route_configs(
            "123".into(),
            vec![route_config.clone()],
        )));
        let (resources, dns) = cache.changes();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert!(cache.data.route_configs.is_empty());

        // insert listener, should now be able to insert the route config
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route"
            )],
        )));

        // should now have a new reference to a cluster
        assert_insert(cache.insert(ResourceVec::from_route_configs(
            "123".into(),
            vec![route_config],
        )));
        let (resources, dns) = cache.changes();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Cluster => Changes {
                    added: collect_str!["cluster.example:8008"],
                    ..Default::default()
                },
                _ => Changes::default(),
            }
        );
        assert!(dns.is_noop());

        assert_eq!(
            cache.subscriptions(ResourceType::RouteConfiguration),
            vec!["example-route"],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            collect_kv_str![("example-route", 123)],
        );
    }

    #[test]
    fn test_insert_load_assignment() {
        let kube_backend = BackendId {
            service: Service::kube("default", "whatever").unwrap(),
            port: 7890,
        };
        let mut cache = Cache::default();

        // try to insert before being referenced
        assert_insert(cache.insert(ResourceVec::from_load_assignments(
            "123".into(),
            vec![xds_test::cla!(
                "whatever.default.svc.cluster.local:7890" => {
                    "zone1" => ["1.1.1.1"]
                }
            )],
        )));
        let (resources, dns) = cache.changes();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert!(cache.data.load_assignments.is_empty());

        // insert a cluster
        assert_insert(cache.insert(ResourceVec::from_clusters(
            "123".into(),
            vec![xds_test::cluster!(kube_backend.name().leak())],
        )));
        let _ = cache.changes();

        // try again
        assert_insert(cache.insert(ResourceVec::from_load_assignments(
            "123".into(),
            vec![xds_test::cla!(
                "whatever.default.svc.cluster.local:7890" => {
                    "zone1" => ["1.1.1.1"]
                }
            )],
        )));

        // should be inserted, but won't cause changes
        let (resources, dns) = cache.changes();
        assert!(resources.values().all(|c| c.is_empty()));
        assert!(dns.is_noop());
        assert_eq!(
            cache.versions(ResourceType::ClusterLoadAssignment),
            collect_kv_str![("whatever.default.svc.cluster.local:7890", "123")]
        );
    }

    #[test]
    fn test_remove_listener_explicit() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, false);
        cache.set_wildcard(ResourceType::Listener, false);

        // subscribe to two listeners
        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        cache.subscribe(ResourceType::Listener, "listener.local");
        let _ = cache.changes();

        // insert a listener -> route -> cluster -> dns chain of configuration
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.example.svc.cluster.local",
                "example-route",
            )],
        )));
        assert_insert(cache.insert(ResourceVec::from_route_configs(
            "123".into(),
            vec![xds_test::route_config!(
                "example-route",
                vec![xds_test::vhost!(
                    "a-vhost",
                    ["listener.example.svc.cluster.local"],
                    [xds_test::route!(default "cluster.example:8008")]
                )]
            )],
        )));
        assert_insert(cache.insert(ResourceVec::from_clusters(
            "123".into(),
            vec![xds_test::cluster!("cluster.example:8008")],
        )));
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "cluster.example.lb.jct:8008",
                "lb-route" => [xds_test::vhost!(
                    "lb-vhost",
                    ["cluster.example.lb.jct:8080"],
                    [xds_test::route!(default "cluster.example:8008")],
                )],
            )],
        )));

        // check that the first set of resources makes sense
        let _ = cache.changes();
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![("cluster.example:8008", "123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Listener),
            collect_kv_str![
                ("listener.example.svc.cluster.local", "123"),
                ("cluster.example.lb.jct:8008", "123"),
            ],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            collect_kv_str![("example-route", "123")],
        );
        assert!(cache
            .versions(ResourceType::ClusterLoadAssignment)
            .is_empty());

        // add the second listener/route-config pointing to the same cluster
        // and remove the first one
        cache.remove(
            ResourceType::Listener,
            &vec!["listener.example.svc.cluster.local".to_string()],
        );
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "listener.local",
                "better-example-route",
            )],
        )));
        assert_insert(cache.insert(ResourceVec::from_route_configs(
            "123".into(),
            vec![xds_test::route_config!(
                "better-example-route",
                vec![xds_test::vhost!(
                    "a-vhost",
                    ["listener.local"],
                    [xds_test::route!(default "cluster.example:8008")]
                )]
            )],
        )));

        // should add a remove for the RouteConfig. the Listener was already gone.
        // nothing else should change.
        let (resources, dns) = cache.changes();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    removed: collect_str!("listener.example.svc.cluster.local"),
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: collect_str!("example-route"),
                    ..Default::default()
                },
                _ => Changes::default()
            }
        );
        assert!(dns.is_noop());
        assert_eq!(
            cache.versions(ResourceType::Listener),
            collect_kv_str![
                ("listener.local", "123"),
                ("cluster.example.lb.jct:8008", "123"),
            ],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            collect_kv_str![("better-example-route", "123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![("cluster.example:8008", "123")],
        );

        // removing the listener should remove everything
        cache.remove(ResourceType::Listener, &vec!["listener.local".to_string()]);
        let (resources, dns) = cache.changes();
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::new(),
                remove: [(Hostname::from_static("cluster.example"), 8008)]
                    .into_iter()
                    .collect(),
                sync: false,
            }
        );
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    removed: collect_str!("listener.local", "cluster.example.lb.jct:8008"),
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: collect_str!("better-example-route"),
                    ..Default::default()
                },
                ResourceType::Cluster => Changes {
                    removed: collect_str!("cluster.example:8008"),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes::default(),
            }
        );
    }

    #[test]
    fn test_remove_listener_wildcard() {
        let mut cache = Cache::default();
        cache.set_wildcard(ResourceType::Cluster, true);
        cache.set_wildcard(ResourceType::Listener, true);

        // subscribe to one listener
        cache.subscribe(ResourceType::Listener, "listener.example.svc.cluster.local");
        let _ = cache.changes();

        // insert two listener->route pairs pointing at the same cluster
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![
                xds_test::listener!("listener.example.svc.cluster.local", "example-route"),
                xds_test::listener!("listener.local", "better-example-route"),
            ],
        )));
        assert_insert(cache.insert(ResourceVec::from_route_configs(
            "123".into(),
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
        )));
        assert_insert(cache.insert(ResourceVec::from_clusters(
            "123".into(),
            vec![xds_test::cluster!("cluster.example:8008")],
        )));
        assert_insert(cache.insert(ResourceVec::from_listeners(
            "123".into(),
            vec![xds_test::listener!(
                "cluster.example.lb.jct:8008",
                "lb-route" => [xds_test::vhost!(
                    "lb-vhost",
                    ["cluster.example.lb.jct:8080"],
                    [xds_test::route!(default "cluster.example:8008")],
                )],
            )],
        )));

        // check that the first set of resources makes sense
        let _ = cache.changes();
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![("cluster.example:8008", "123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Listener),
            collect_kv_str![
                ("listener.local", "123"),
                ("listener.example.svc.cluster.local", "123"),
                ("cluster.example.lb.jct:8008", "123"),
            ],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            collect_kv_str![("example-route", "123"), ("better-example-route", "123")],
        );
        assert!(cache
            .versions(ResourceType::ClusterLoadAssignment)
            .is_empty());

        // remove the explicitly subscribed listener
        cache.remove(
            ResourceType::Listener,
            &vec!["listener.example.svc.cluster.local".to_string()],
        );

        let (resources, dns) = cache.changes();
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    removed: collect_str!("listener.example.svc.cluster.local"),
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: collect_str!("example-route"),
                    ..Default::default()
                },
                _ => Changes::default()
            }
        );
        assert!(dns.is_noop());
        assert_eq!(
            cache.versions(ResourceType::Listener),
            collect_kv_str![
                ("listener.local", "123"),
                ("cluster.example.lb.jct:8008", "123"),
            ],
        );
        assert_eq!(
            cache.versions(ResourceType::RouteConfiguration),
            collect_kv_str![("better-example-route", "123")],
        );
        assert_eq!(
            cache.versions(ResourceType::Cluster),
            collect_kv_str![("cluster.example:8008", "123")],
        );

        // explicitly removing the wildcard listener should remove everything
        cache.remove(ResourceType::Listener, &vec!["listener.local".to_string()]);
        let (resources, dns) = cache.changes();
        assert_eq!(
            dns,
            DnsUpdates {
                add: BTreeSet::new(),
                remove: [(Hostname::from_static("cluster.example"), 8008)]
                    .into_iter()
                    .collect(),
                sync: false,
            }
        );
        assert_eq!(
            resources,
            enum_map::enum_map! {
                ResourceType::Listener => Changes {
                    removed: collect_str!("listener.local", "cluster.example.lb.jct:8008"),
                    ..Default::default()
                },
                ResourceType::RouteConfiguration => Changes {
                    removed: collect_str!("better-example-route"),
                    ..Default::default()
                },
                ResourceType::Cluster => Changes {
                    removed: collect_str!("cluster.example:8008"),
                    ..Default::default()
                },
                ResourceType::ClusterLoadAssignment => Changes::default(),
            }
        );
    }
}
