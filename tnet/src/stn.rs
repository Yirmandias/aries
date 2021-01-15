use crate::num::Time;
use crate::stn::Event::{EdgeActivated, EdgeAdded, NewPendingActivation, NodeInitialized, NodeReserved};

use std::collections::{HashMap, VecDeque};
use std::ops::IndexMut;

/// A timepoint in a Simple Temporal Network.
/// It simply represents a pointer/index into a particular STN object.
/// Creating a timepoint can only be done by invoking the `add_timepoint` method
/// on the STN.
/// It can be converted to an unsigned integer with `to_index` and can be used to
/// index into a Vec.  
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct Timepoint(u32);

impl Timepoint {
    fn from_index(index: u32) -> Timepoint {
        Timepoint(index)
    }

    pub fn to_index(self) -> u32 {
        self.0
    }
}
impl From<Timepoint> for usize {
    fn from(tp: Timepoint) -> Self {
        tp.to_index() as usize
    }
}

impl<X> Index<Timepoint> for Vec<X> {
    type Output = X;

    fn index(&self, index: Timepoint) -> &Self::Output {
        &self[index.to_index() as usize]
    }
}

impl<X> IndexMut<Timepoint> for Vec<X> {
    fn index_mut(&mut self, index: Timepoint) -> &mut Self::Output {
        &mut self[index.to_index() as usize]
    }
}

/// A unique identifier for an edge in the STN.
/// An edge and its negation share the same `base_id` but differ by the `is_negated` property.
///
/// For instance, valid edge ids:
///  -  a - b <= 10
///    - base_id: 3
///    - negated: false
///  - a - b > 10       # negation of the previous one
///    - base_id: 3     # same
///    - negated: true  # inverse
///  - a -b <= 20       # unrelated
///    - base_id: 4
///    - negated: false
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct EdgeID {
    base_id: u32,
    negated: bool,
}
impl EdgeID {
    fn new(base_id: u32, negated: bool) -> EdgeID {
        EdgeID { base_id, negated }
    }
    pub fn base_id(&self) -> u32 {
        self.base_id
    }
    pub fn is_negated(&self) -> bool {
        self.negated
    }
}

#[cfg(feature = "theories")]
impl From<EdgeID> for aries_smt::AtomID {
    fn from(edge: EdgeID) -> Self {
        aries_smt::AtomID::new(edge.base_id, edge.negated)
    }
}
#[cfg(feature = "theories")]
impl From<aries_smt::AtomID> for EdgeID {
    fn from(atom: aries_smt::AtomID) -> Self {
        EdgeID::new(atom.base_id(), atom.is_negated())
    }
}

/// An edge in the STN, representing the constraint `target - source <= weight`
/// An edge can be either in canonical form or in negated form.
/// Given to edges (tgt - src <= w) and (tgt -src > w) one will be in canonical form and
/// the other in negated form.
#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub struct Edge<W> {
    pub source: Timepoint,
    pub target: Timepoint,
    pub weight: W,
}

impl<W: Time> Edge<W> {
    pub fn new(source: Timepoint, target: Timepoint, weight: W) -> Edge<W> {
        Edge { source, target, weight }
    }

    fn is_negated(&self) -> bool {
        !self.is_canonical()
    }

    fn is_canonical(&self) -> bool {
        self.source < self.target || self.source == self.target && self.weight >= W::zero()
    }

    // not(b - a <= 6)
    //   = b - a > 6
    //   = a -b < -6
    //   = a - b <= -7
    //
    // not(a - b <= -7)
    //   = a - b > -7
    //   = b - a < 7
    //   = b - a <= 6
    fn negated(&self) -> Self {
        Edge {
            source: self.target,
            target: self.source,
            weight: -self.weight - W::step(),
        }
    }
}

#[derive(Copy, Clone)]
struct Constraint<W> {
    /// True if the constraint active (participates in propagation)
    active: bool,
    edge: Edge<W>,
}
impl<W> Constraint<W> {
    pub fn new(active: bool, edge: Edge<W>) -> Constraint<W> {
        Constraint { active, edge }
    }
}

/// A pair of constraints (a, b) where edgee(a) = !edge(b)
struct ConstraintPair<W> {
    /// constraint where the edge is in its canonical form
    base: Constraint<W>,
    /// constraint corresponding to the negation of base
    negated: Constraint<W>,
}

impl<W: Time> ConstraintPair<W> {
    pub fn new_inactives(edge: Edge<W>) -> ConstraintPair<W> {
        if edge.is_canonical() {
            ConstraintPair {
                base: Constraint::new(false, edge),
                negated: Constraint::new(false, edge.negated()),
            }
        } else {
            ConstraintPair {
                base: Constraint::new(false, edge.negated()),
                negated: Constraint::new(false, edge),
            }
        }
    }
}

/// Data structures that holds all active and inactive edges in the STN.
/// Note that some edges might be represented even though they were never inserted if they are the
/// negation of an inserted edge.
struct ConstraintDB<W> {
    /// All constraints pairs, the index of this vector is the base_id of the edges in the pair.
    constraints: Vec<ConstraintPair<W>>,
    /// Maps each canonical edge to its location
    lookup: HashMap<Edge<W>, u32>,
}
impl<W: Time> ConstraintDB<W> {
    pub fn new() -> ConstraintDB<W> {
        ConstraintDB {
            constraints: vec![],
            lookup: HashMap::new(),
        }
    }

    fn find_existing(&self, edge: &Edge<W>) -> Option<EdgeID> {
        if edge.is_canonical() {
            self.lookup.get(edge).map(|&id| EdgeID::new(id, false))
        } else {
            self.lookup.get(&edge.negated()).map(|&id| EdgeID::new(id, true))
        }
    }

    /// Adds a new edge and return a pair (created, edge_id) where:
    ///  - created is false if NO new edge was inserted (it was merge with an identical edge already in the DB)
    ///  - edge_id is the id of the edge
    ///
    /// If the edge is marked as hidden, then it will not appead in the lookup table. This will prevent
    /// it from being unified with a future edge.
    pub fn push_edge(&mut self, source: Timepoint, target: Timepoint, weight: W, hidden: bool) -> (bool, EdgeID) {
        let edge = Edge::new(source, target, weight);
        match self.find_existing(&edge) {
            Some(id) => {
                // edge already exists in the DB, return its id and say it wasn't created
                debug_assert_eq!(self[id].edge, edge);
                (false, id)
            }
            None => {
                // edge does not exist, record the corresponding pair and return the new id.
                let pair = ConstraintPair::new_inactives(edge);
                let base_id = self.constraints.len() as u32;
                if !hidden {
                    // the edge is not hidden, add it to lookup so it can be unified with existing ones
                    self.lookup.insert(pair.base.edge, base_id);
                }
                self.constraints.push(pair);
                let edge_id = EdgeID::new(base_id, edge.is_negated());
                debug_assert_eq!(self[edge_id].edge, edge);
                (true, edge_id)
            }
        }
    }

    /// Removes the last created ConstraintPair in the DB. Note that this will remove the last edge that was
    /// push THAT WAS NOT UNIFIED with an existing edge (i.e. edge_push returned : (true, _)).
    pub fn pop_last(&mut self) {
        if let Some(pair) = self.constraints.pop() {
            self.lookup.remove(&pair.base.edge);
        }
    }

    pub fn has_edge(&self, id: EdgeID) -> bool {
        id.base_id <= self.constraints.len() as u32
    }
}
impl<W> Index<EdgeID> for ConstraintDB<W> {
    type Output = Constraint<W>;

    fn index(&self, index: EdgeID) -> &Self::Output {
        let pair = &self.constraints[index.base_id as usize];
        if index.negated {
            &pair.negated
        } else {
            &pair.base
        }
    }
}
impl<W> IndexMut<EdgeID> for ConstraintDB<W> {
    fn index_mut(&mut self, index: EdgeID) -> &mut Self::Output {
        let pair = &mut self.constraints[index.base_id as usize];
        if index.negated {
            &mut pair.negated
        } else {
            &mut pair.base
        }
    }
}

type BacktrackLevel = u32;

enum Event<W> {
    Level(BacktrackLevel),
    NodeReserved,
    NodeInitialized(Timepoint),
    EdgeAdded,
    NewPendingActivation,
    EdgeActivated(EdgeID),
    ForwardUpdate {
        node: Timepoint,
        previous_dist: W,
        previous_cause: Option<EdgeID>,
    },
    BackwardUpdate {
        node: Timepoint,
        previous_dist: W,
        previous_cause: Option<EdgeID>,
    },
}

pub enum NetworkStatus<'network, W> {
    /// Network is fully propagated and consistent
    Consistent(NetworkUpdates<'network, W>),
    /// Network is inconsistent, due to the presence of the given negative cycle.
    /// Note that internal edges (typically those inserted to represent lower/upper bounds) are
    /// omitted from the inconsistent set.
    Inconsistent(&'network [EdgeID]),
}

// TODO: document and impl IntoIterator for NetworkUpdates
pub struct VarEvent<W> {
    tp: Timepoint,
    event: DomainEvent<W>,
}
pub enum DomainEvent<W> {
    NewLB(W),
    NewUB(W),
}
pub struct NetworkUpdates<'network, W> {
    network: &'network IncSTN<W>,
    point_in_trail: usize,
}

impl<'network, W: Time> Iterator for NetworkUpdates<'network, W> {
    type Item = VarEvent<W>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.point_in_trail < self.network.trail.len() {
            self.point_in_trail += 1;
            match self.network.trail[self.point_in_trail - 1] {
                Event::ForwardUpdate { node, .. } => {
                    return Some(VarEvent {
                        tp: node,
                        event: DomainEvent::NewUB(self.network.ub(node)),
                    });
                }
                Event::BackwardUpdate { node, .. } => {
                    return Some(VarEvent {
                        tp: node,
                        event: DomainEvent::NewLB(self.network.lb(node)),
                    });
                }
                _ => (),
            }
        }
        None
    }
}

struct Distance<W> {
    initialized: bool,
    forward: W,
    forward_cause: Option<EdgeID>,
    forward_pending_update: bool,
    backward: W,
    backward_cause: Option<EdgeID>,
    backward_pending_update: bool,
}

#[derive(Default)]
struct Stats {
    num_propagations: u64,
    distance_updates: u64,
}

/// STN that supports:
///  - incremental edge addition and consistency checking with [Cesta96]
///  - undoing the latest changes
///  - providing explanation on inconsistency in the form of a culprit
///         set of constraints
///  - unifies new edges with previously inserted ones
///
/// Once the network reaches an inconsistent state, the only valid operation
/// is to undo the latest change go back to a consistent network. All other
/// operations have an undefined behavior.
///
/// Requirement for `W` : `W` is used internally to represent both delays
/// (weight on edges) and absolute times (bound on nodes). It is the responsibility
/// of the caller to ensure that no overflow occurs when adding an absolute and relative time,
/// either by the choice of an appropriate type (e.g. saturating add) or by the choice of
/// appropriate initial bounds.
pub struct IncSTN<W> {
    constraints: ConstraintDB<W>,
    /// Forward/Backward adjacency list containing active edges.
    active_forward_edges: Vec<Vec<FwdActive<W>>>,
    active_backward_edges: Vec<Vec<BwdActive<W>>>,
    distances: Vec<Distance<W>>,
    /// History of changes and made to the STN with all information necessary to undo them.
    trail: Vec<Event<W>>,
    pending_activations: VecDeque<ActivationEvent>,
    level: BacktrackLevel,
    stats: Stats,
    /// Internal data structure to construct explanations as negative cycles.
    /// When encountering an inconsistency, this vector will be cleared and
    /// a negative cycle will be constructed in it. The explanation returned
    /// will be a slice of this vector to avoid any allocation.
    explanation: Vec<EdgeID>,
    /// Internal data structure used by the `propagate` method to keep track of pending work.
    internal_propagate_queue: VecDeque<Timepoint>,
    internal_visited_by_cycle_extraction: RefSet<Timepoint>,
}

/// Stores the target and weight of an edge in the active forward queue.
/// This structure serves as a cache to avoid touching the `constraints` array
/// in the propagate loop.
struct FwdActive<W> {
    target: Timepoint,
    weight: W,
    id: EdgeID,
}

/// Stores the source and weight of an edge in the active backward queue.
/// This structure serves as a cache to avoid touching the `constraints` array
/// in the propagate loop.
struct BwdActive<W> {
    source: Timepoint,
    weight: W,
    id: EdgeID,
}

enum ActivationEvent {
    ToActivate(EdgeID),
    BacktrackPoint(BacktrackLevel),
}

impl<W: Time> IncSTN<W> {
    /// Creates a new STN. Initially, the STN contains a single timepoint
    /// representing the origin whose domain is [0,0]. The id of this timepoint can
    /// be retrieved with the `origin()` method.
    pub fn new() -> Self {
        let mut stn = IncSTN {
            constraints: ConstraintDB::new(),
            active_forward_edges: vec![],
            active_backward_edges: vec![],
            distances: vec![],
            trail: vec![],
            pending_activations: VecDeque::new(),
            level: 0,
            stats: Default::default(),
            explanation: vec![],
            internal_propagate_queue: Default::default(),
            internal_visited_by_cycle_extraction: Default::default(),
        };
        let origin = stn.add_timepoint(W::zero(), W::zero());
        assert_eq!(origin, stn.origin());
        // make sure that initialization of the STN can not be undone
        stn.trail.clear();
        stn
    }
    pub fn num_nodes(&self) -> u32 {
        debug_assert_eq!(self.active_forward_edges.len(), self.active_backward_edges.len());
        self.active_forward_edges.len() as u32
    }

    pub fn origin(&self) -> Timepoint {
        Timepoint::from_index(0)
    }

    pub fn lb(&self, node: Timepoint) -> W {
        -self.distances[node].backward
    }
    pub fn ub(&self, node: Timepoint) -> W {
        self.distances[node].forward
    }

    /// Adds a new node (timepoint) to the STN with a domain of `[lb, ub]`.
    /// Returns the identifier of the newly added node.
    /// Lower and upper bounds have corresponding edges in the STN distance
    /// graph that will participate in propagation:
    ///  - `ORIGIN --(ub)--> node`
    ///  - `node --(-lb)--> ORIGIN`
    /// However, those edges are purely internal and since their IDs are not
    /// communicated, they will be omitted when appearing in the explanation of
    /// inconsistencies.  
    /// If you want for those to appear in explanation, consider setting bounds
    /// to -/+ infinity adding those edges manually.
    ///
    /// Panics if `lb > ub`. This guarantees that the network remains consistent
    /// when adding a node.
    ///
    /// It is the responsibility of the caller to ensure that the bound provided
    /// will not overflow when added to arbitrary weight of the network.
    pub fn add_timepoint(&mut self, lb: W, ub: W) -> Timepoint {
        let tp = self.reserve_timepoint();
        self.init_timepoint(tp, lb, ub);
        tp
    }

    pub fn add_timepoint_with_id(&mut self, id: NonZeroU32, lb: W, ub: W) -> Timepoint {
        let id = id.get();
        while id >= self.num_nodes() {
            self.reserve_timepoint();
        }
        let tp = Timepoint::from_index(id);
        self.init_timepoint(tp, lb, ub);
        tp
    }

    pub fn reserve_timepoint(&mut self) -> Timepoint {
        let id = Timepoint::from_index(self.num_nodes());
        self.active_forward_edges.push(Vec::new());
        self.active_backward_edges.push(Vec::new());
        debug_assert_eq!(id.to_index(), self.num_nodes() - 1);
        self.trail.push(NodeReserved);
        self.distances.push(Distance {
            initialized: false,
            forward: W::zero(),
            forward_cause: None,
            forward_pending_update: false,
            backward: W::zero(),
            backward_cause: None,
            backward_pending_update: false,
        });
        id
    }

    pub fn init_timepoint(&mut self, tp: Timepoint, lb: W, ub: W) {
        assert!(tp.to_index() < self.num_nodes());
        assert!(!self.distances[tp].initialized, "Timepoint is already initialized");
        assert!(lb <= ub);
        let (fwd_edge, _) = self.add_inactive_constraint(self.origin(), tp, ub, true);
        let (bwd_edge, _) = self.add_inactive_constraint(tp, self.origin(), -lb, true);
        // todo: these should not require propagation because they will properly set the
        //       node's domain. However mark_active will add them to the propagation queue
        self.mark_active(fwd_edge);
        self.mark_active(bwd_edge);
        self.distances[tp] = Distance {
            initialized: true,
            forward: ub,
            forward_cause: Some(fwd_edge),
            forward_pending_update: false,
            backward: -lb,
            backward_cause: Some(bwd_edge),
            backward_pending_update: false,
        };
        self.trail.push(NodeInitialized(tp));
    }

    pub fn add_edge(&mut self, source: Timepoint, target: Timepoint, weight: W) -> EdgeID {
        let id = self.add_inactive_edge(source, target, weight);
        self.mark_active(id);
        id
    }

    /// Records an INACTIVE new edge and returns its identifier.
    /// If an identical is already present in the network, then the identifier will the one of
    /// the existing edge.
    ///
    /// After calling this method, the edge is inactive and will not participate in
    /// propagation. The edge can be activated with the `mark_active()` method.
    ///
    /// Since the edge is inactive, the STN remains consistent after calling this method.
    pub fn add_inactive_edge(&mut self, source: Timepoint, target: Timepoint, weight: W) -> EdgeID {
        self.add_inactive_constraint(source, target, weight, false).0
    }

    /// Marks an edge as active and enqueue it for propagation.
    /// No changes are committed to the network by this function until a call to `propagate_all()`
    pub fn mark_active(&mut self, edge: EdgeID) {
        debug_assert!(self.constraints.has_edge(edge));
        self.pending_activations.push_back(ActivationEvent::ToActivate(edge));
        self.trail.push(Event::NewPendingActivation);
    }

    /// Propagates all edges that have been marked as active since the last propagation.
    pub fn propagate_all(&mut self) -> NetworkStatus<W> {
        let trail_end_before_propagation = self.trail.len();
        while let Some(event) = self.pending_activations.pop_front() {
            // if let ActivationEvent::ToActivate(edge) = event
            let edge = match event {
                ActivationEvent::ToActivate(edge) => edge,
                ActivationEvent::BacktrackPoint(_) => continue, // we are not concerned with this backtrack point
            };
            let c = &mut self.constraints[edge];
            let Edge { source, target, weight } = c.edge;
            if source == target {
                if weight < W::zero() {
                    // negative self loop: inconsistency
                    self.explanation.clear();
                    self.explanation.push(edge);
                    return NetworkStatus::Inconsistent(&self.explanation);
                } else {
                    // positive self loop : useless edge that we can ignore
                }
            } else if !c.active {
                c.active = true;
                self.active_forward_edges[source].push(FwdActive {
                    target,
                    weight,
                    id: edge,
                });
                self.active_backward_edges[target].push(BwdActive {
                    source,
                    weight,
                    id: edge,
                });
                self.trail.push(EdgeActivated(edge));
                // if self.propagate(edge) != NetworkStatus::Consistent;
                if let NetworkStatus::Inconsistent(explanation) = self.propagate(edge) {
                    // work around borrow checker, transmutation should be a no-op that just resets lifetimes
                    let x = unsafe { std::mem::transmute(explanation) };
                    return NetworkStatus::Inconsistent(x);
                }
            }
        }
        NetworkStatus::Consistent(NetworkUpdates {
            network: self,
            point_in_trail: trail_end_before_propagation,
        })
    }

    /// Creates a new backtrack point that represents the STN at the point of the method call,
    /// just before the insertion of the backtrack point.
    pub fn set_backtrack_point(&mut self) -> BacktrackLevel {
        assert!(
            self.pending_activations.is_empty(),
            "Cannot set a backtrack point if a propagation is pending. \
            The code introduced in this commit should enable this but has not been thoroughly tested yet."
        );
        self.level += 1;
        self.pending_activations
            .push_back(ActivationEvent::BacktrackPoint(self.level));
        self.trail.push(Event::Level(self.level));
        self.level
    }

    /// Revert all changes and put the STN to the state it was in immediately before the insertion of
    /// the given backtrack point.
    /// Panics if the backtrack point is not recorded.
    pub fn backtrack_to(&mut self, point: BacktrackLevel) {
        assert!(self.level >= point, "Invalid backtrack point: already backtracked upon");
        while self.level >= point {
            self.undo_to_last_backtrack_point();
        }
    }

    pub fn undo_to_last_backtrack_point(&mut self) -> Option<BacktrackLevel> {
        // remove pending activations since the last backtrack point.
        while let Some(event) = self.pending_activations.pop_back() {
            match event {
                ActivationEvent::ToActivate(_) => {}
                ActivationEvent::BacktrackPoint(level) => {
                    // found the next backtrack point, stop here
                    assert_eq!(level, self.level);
                    break;
                }
            }
        }
        // undo changes since the last backtrack point
        while let Some(ev) = self.trail.pop() {
            match ev {
                Event::Level(lvl) => {
                    self.level -= 1;
                    return Some(lvl);
                }
                NodeReserved => {
                    self.active_forward_edges.pop();
                    self.active_backward_edges.pop();
                    self.distances.pop();
                }
                NodeInitialized(tp) => {
                    self.distances[tp].initialized = false;
                }
                EdgeAdded => {
                    self.constraints.pop_last();
                }
                NewPendingActivation => {
                    self.pending_activations.pop_back();
                }
                EdgeActivated(e) => {
                    let c = &mut self.constraints[e];
                    self.active_forward_edges[c.edge.source].pop();
                    self.active_backward_edges[c.edge.target].pop();
                    c.active = false;
                }
                Event::ForwardUpdate {
                    node,
                    previous_dist,
                    previous_cause,
                } => {
                    let x = &mut self.distances[node];
                    x.forward = previous_dist;
                    x.forward_cause = previous_cause;
                }
                Event::BackwardUpdate {
                    node,
                    previous_dist,
                    previous_cause,
                } => {
                    let x = &mut self.distances[node];
                    x.backward = previous_dist;
                    x.backward_cause = previous_cause;
                }
            }
        }
        None
    }

    /// Return a tuple `(id, created)` where id is the id of the edge and created is a boolean value that is true if the
    /// edge was created and false if it was unified with a previous instance
    fn add_inactive_constraint(
        &mut self,
        source: Timepoint,
        target: Timepoint,
        weight: W,
        hidden: bool,
    ) -> (EdgeID, bool) {
        assert!(
            source.to_index() < self.num_nodes() && target.to_index() < self.num_nodes(),
            "Unrecorded node"
        );
        let (created, id) = self.constraints.push_edge(source, target, weight, hidden);
        if created {
            self.trail.push(EdgeAdded);
        }
        (id, created)
    }

    fn fdist(&self, n: Timepoint) -> W {
        self.distances[n].forward
    }
    fn bdist(&self, n: Timepoint) -> W {
        self.distances[n].backward
    }
    fn active(&self, e: EdgeID) -> bool {
        self.constraints[e].active
    }

    /// Implementation of [Cesta96]
    /// It propagates a **newly_inserted** edge in a **consistent** STN.
    fn propagate(&mut self, new_edge: EdgeID) -> NetworkStatus<W> {
        self.stats.num_propagations += 1;
        let trail_end_before_propagation = self.trail.len();
        self.internal_propagate_queue.clear(); // reset to make sure we are not in a dirty state
        let c = &self.constraints[new_edge];
        debug_assert_ne!(
            c.edge.source, c.edge.target,
            "This algorithm does not support self loops."
        );
        let new_edge_source = c.edge.source;
        let new_edge_target = c.edge.target;
        self.internal_propagate_queue.push_back(new_edge_source);
        self.internal_propagate_queue.push_back(new_edge_target);

        self.distances[new_edge_source].forward_pending_update = true;
        self.distances[new_edge_source].backward_pending_update = true;
        self.distances[new_edge_target].forward_pending_update = true;
        self.distances[new_edge_target].backward_pending_update = true;
        let mut target_updated_ub = false;
        let mut source_updated_lb = false;

        while let Some(u) = self.internal_propagate_queue.pop_front() {
            if self.distances[u].forward_pending_update {
                for &FwdActive {
                    target,
                    weight,
                    id: out_edge,
                } in &self.active_forward_edges[u]
                {
                    let source = u;

                    debug_assert_eq!(&Edge { source, target, weight }, &self.constraints[out_edge].edge);
                    debug_assert!(self.active(out_edge));

                    let previous = self.fdist(target);
                    let candidate = self.fdist(source).saturating_add(&weight);
                    if candidate < previous {
                        self.trail.push(Event::ForwardUpdate {
                            node: target,
                            previous_dist: previous,
                            previous_cause: self.distances[target].forward_cause,
                        });
                        self.distances[target].forward = candidate;
                        self.distances[target].forward_cause = Some(out_edge);
                        self.distances[target].forward_pending_update = true;
                        self.stats.distance_updates += 1;

                        // now that we have updated the causes (necessary for cycle extraction)
                        // detect whether we have a negative cycle
                        if candidate.saturating_add(&self.bdist(target)) < W::zero() {
                            // negative cycle
                            return NetworkStatus::Inconsistent(self.extract_cycle(target));
                        }

                        if target == new_edge_target {
                            if target_updated_ub {
                                // updated twice, there is a cycle. See cycle detection in [Cesta96]
                                return NetworkStatus::Inconsistent(self.extract_cycle(target));
                            } else {
                                target_updated_ub = true;
                            }
                        }

                        // note: might result in having this more than once in the queue.
                        // this is ok though since any further work is guarded by the forward/backward pending updates
                        self.internal_propagate_queue.push_back(target);
                    }
                }
            }

            if self.distances[u].backward_pending_update {
                for &BwdActive {
                    source,
                    weight,
                    id: in_edge,
                } in &self.active_backward_edges[u]
                {
                    let target = u;

                    debug_assert_eq!(&Edge { source, target, weight }, &self.constraints[in_edge].edge);
                    debug_assert!(self.active(in_edge));

                    let previous = self.bdist(source);
                    let candidate = self.bdist(target).saturating_add(&weight);
                    if candidate < previous {
                        self.trail.push(Event::BackwardUpdate {
                            node: source,
                            previous_dist: previous,
                            previous_cause: self.distances[source].backward_cause,
                        });
                        self.distances[source].backward = candidate;
                        self.distances[source].backward_cause = Some(in_edge);
                        self.distances[source].backward_pending_update = true;
                        self.stats.distance_updates += 1;

                        // now that we have updated the causes (necessary for cycle extraction)
                        // detect whether we have a negative cycle
                        if candidate.saturating_add(&self.fdist(source)) < W::zero() {
                            // negative cycle
                            return NetworkStatus::Inconsistent(self.extract_cycle(source));
                        }

                        if source == new_edge_source {
                            if source_updated_lb {
                                // updated twice, there is a cycle. See cycle detection in [Cesta96]
                                return NetworkStatus::Inconsistent(self.extract_cycle(source));
                            } else {
                                source_updated_lb = true;
                            }
                        }

                        self.internal_propagate_queue.push_back(source); // idem, might result in more than once in the queue
                    }
                }
            }
            // problematic in the case of self cycles...
            self.distances[u].forward_pending_update = false;
            self.distances[u].backward_pending_update = false;
        }
        NetworkStatus::Consistent(NetworkUpdates {
            network: self,
            point_in_trail: trail_end_before_propagation,
        })
    }

    /// Extracts a cycle from `culprit` following backward causes first.
    /// The method will write the cycle to `self.explanation`.
    /// If there is no cycle visible from the causes reachable from `culprit`,
    /// the method might loop indefinitely or panic.
    fn extract_cycle_impl(&mut self, culprit: Timepoint) {
        let mut current = culprit;
        self.explanation.clear();
        let origin = self.origin();
        let visited = &mut self.internal_visited_by_cycle_extraction;
        visited.clear();
        // follow backward causes until finding a cycle, or reaching the origin
        // all visited edges are added to the explanation.
        loop {
            visited.insert(current);
            let next_constraint_id = self.distances[current]
                .forward_cause
                .expect("No cause on member of cycle");
            let next = self.constraints[next_constraint_id].edge.source;
            self.explanation.push(next_constraint_id);
            if next == current {
                // the edge is self loop which is only allowed on the origin. This mean that we have reached the origin.
                // we don't want to add this edge to the cycle, so we exit early.
                debug_assert!(current == origin, "Self loop only present on origin");
                break;
            }
            current = next;
            if current == culprit {
                // we have found the cycle. Return immediately, the cycle is written to self.explanation
                return;
            } else if current == origin {
                // reached the origin, nothing more we can do by following backward causes.
                break;
            } else if visited.contains(current) {
                // cycle, that does not goes through culprit
                let cycle_start = self
                    .explanation
                    .iter()
                    .position(|x| current == self.constraints[*x].edge.target)
                    .unwrap();
                self.explanation.drain(0..cycle_start).count();
                return;
            }
        }
        // remember how many edges were added to the explanation in case we need to remove them
        let added_by_backward_pass = self.explanation.len();

        // complete the cycle by following forward causes
        let mut current = culprit;
        visited.clear();
        loop {
            visited.insert(current);
            let next_constraint_id = self.distances[current]
                .backward_cause
                .expect("No cause on member of cycle");

            self.explanation.push(next_constraint_id);
            current = self.constraints[next_constraint_id].edge.target;

            if current == origin {
                // we have completed the previous cycle involving the origin.
                // return immediately, the cycle is already written to self.explanation
                return;
            } else if current == culprit {
                // we have reached a cycle through forward edges only.
                // before returning, we need to remove the edges that were added by the backward causes.
                self.explanation.drain(0..added_by_backward_pass).count();
                return;
            } else if visited.contains(current) {
                // cycle, that does not goes through culprit
                // find the start of the cycle. we start looking in the edges added by the current pass.
                let cycle_start = self.explanation[added_by_backward_pass..]
                    .iter()
                    .position(|x| current == self.constraints[*x].edge.source)
                    .unwrap();
                // prefix to remove consists of the edges added in the previous pass + the ones
                // added by this pass before the beginning of the cycle
                let cycle_start = added_by_backward_pass + cycle_start;
                // remove the prefix from the explanation
                self.explanation.drain(0..cycle_start).count();
                return;
            }
        }
    }

    /// Builds a negative cycle involving `culprit` by following forward/backward causes until a cycle is found.
    /// If no such cycle exists, the method might panic or loop indefinitely
    fn extract_cycle(&mut self, culprit: Timepoint) -> &[EdgeID] {
        self.extract_cycle_impl(culprit);

        let cycle = &self.explanation;

        debug_assert!(
            cycle
                .iter()
                .fold(W::zero(), |acc, eid| acc + self.constraints[*eid].edge.weight)
                < W::zero(),
            "Cycle extraction returned a cycle with a non-negative length."
        );
        cycle
    }

    pub fn print_stats(&self) {
        println!("# nodes: {}", self.num_nodes());
        println!("# constraints: {}", self.constraints.constraints.len());
        println!("# propagations: {}", self.stats.num_propagations);
        println!("# domain updates: {}", self.stats.distance_updates);
    }
}

impl<W: Time> Default for IncSTN<W> {
    fn default() -> Self {
        Self::new()
    }
}

use aries_backtrack::Q;
use aries_model::lang::{Fun, IAtom, IVar};
use aries_smt::solver::{Binding, BindingResult};
#[cfg(feature = "theories")]
use aries_smt::Theory;
use aries_smt::{AtomID, AtomRecording, TheoryResult};
use std::hash::Hash;
use std::ops::Index;

#[cfg(feature = "theories")]
pub struct DiffLogicTheory<W> {
    stn: IncSTN<W>,
    timepoints: HashMap<IVar, Timepoint>,
    ivars: HashMap<Timepoint, IVar>,
    mapping: Mapping,
    num_saved_states: u32,
}

impl<T: Time> DiffLogicTheory<T> {
    pub fn new() -> DiffLogicTheory<T> {
        DiffLogicTheory {
            stn: IncSTN::default(),
            timepoints: Default::default(),
            ivars: Default::default(),
            mapping: Default::default(),
            num_saved_states: 0,
        }
    }
}

impl DiffLogicTheory<i32> {
    fn timepoint(&mut self, ivar: IVar, model: &Model) -> Timepoint {
        match self.timepoints.entry(ivar) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(free_spot) => {
                let (lb, ub) = model.bounds(ivar);
                let tp = self.stn.add_timepoint(lb, ub);
                free_spot.insert(tp);
                self.ivars.insert(tp, ivar);
                tp
            }
        }
    }
}

impl Default for DiffLogicTheory<i32> {
    fn default() -> Self {
        DiffLogicTheory::new()
    }
}

impl<T: Time> Backtrack for DiffLogicTheory<T> {
    fn save_state(&mut self) -> u32 {
        self.num_saved_states += 1;
        self.stn.set_backtrack_point();
        self.num_saved_states - 1
    }

    fn num_saved(&self) -> u32 {
        self.num_saved_states
    }

    fn restore_last(&mut self) {
        self.num_saved_states -= 1;
        self.stn
            .undo_to_last_backtrack_point()
            .expect("No backtrack point left");
    }
}

use aries_sat::all::Lit;

use aries_backtrack::Backtrack;
use aries_model::{Model, ModelEvents, WModel};

use aries_collections::set::RefSet;
use aries_model::expressions::ExprHandle;
use std::collections::hash_map::Entry;
#[cfg(feature = "theories")]
use std::convert::*;
use std::num::NonZeroU32;

impl Theory for DiffLogicTheory<i32> {
    fn bind(&mut self, literal: Lit, expr: ExprHandle, model: &mut Model, queue: &mut Q<Binding>) -> BindingResult {
        let expr = model.expressions.get(expr);
        match expr.fun {
            Fun::Leq => {
                let a = IAtom::try_from(expr.args[0]).expect("type error");
                let b = IAtom::try_from(expr.args[1]).expect("type error");
                let va = match a.var {
                    Some(v) => self.timepoint(v, model),
                    None => self.stn.origin(),
                };
                let vb = match b.var {
                    Some(v) => self.timepoint(v, model),
                    None => self.stn.origin(),
                };

                // va + da <= vb + db    <=>   va - vb <= db + da
                let edge = crate::max_delay(vb, va, b.shift - a.shift);

                match record_atom(&mut self.stn, edge) {
                    AtomRecording::Created(id) => self.mapping.bind(literal, id),
                    AtomRecording::Unified(id) => self.mapping.bind(literal, id),
                    AtomRecording::Tautology => unimplemented!(),
                    AtomRecording::Contradiction => unimplemented!(),
                }
                BindingResult::Enforced
            }
            Fun::Eq => {
                let a = IAtom::try_from(expr.args[0]).expect("type error");
                let b = IAtom::try_from(expr.args[1]).expect("type error");
                let x = model.leq(a, b);
                let y = model.leq(b, a);
                queue.push(Binding::new(literal, model.and2(x, y)));
                BindingResult::Refined
            }

            _ => BindingResult::Unsupported,
        }
    }

    fn propagate(&mut self, events: &mut ModelEvents, model: &mut WModel) -> TheoryResult {
        while let Some((lit, _)) = events.bool_events.pop() {
            for &atom in self.mapping.atoms_of(lit) {
                self.stn.mark_active(atom.into());
            }
        }

        match self.stn.propagate_all() {
            NetworkStatus::Consistent(updates) => {
                for update in updates {
                    if let Some(ivar) = self.ivars.get(&update.tp) {
                        match update.event {
                            DomainEvent::NewLB(lb) => model.set_lower_bound(*ivar, lb, 0u64),
                            DomainEvent::NewUB(ub) => model.set_upper_bound(*ivar, ub, 0u64),
                        }
                    }
                }
                TheoryResult::Consistent
            }
            NetworkStatus::Inconsistent(x) => {
                let mapping = &mut self.mapping; // alias to please the borrow checker
                let clause = x
                    .iter()
                    .map(|e| aries_smt::AtomID::from(*e))
                    .filter_map(|atom| mapping.literal_of(atom))
                    .map(|lit| !lit)
                    .collect();
                TheoryResult::Contradiction(clause)
            }
        }
    }

    fn print_stats(&self) {
        self.stn.print_stats();
    }
}

fn record_atom<W: Time>(stn: &mut IncSTN<W>, atom: Edge<W>) -> aries_smt::AtomRecording {
    let (id, created) = stn.add_inactive_constraint(atom.source, atom.target, atom.weight, false);
    if created {
        aries_smt::AtomRecording::Created(id.into())
    } else {
        aries_smt::AtomRecording::Unified(id.into())
    }
}

// TODO: we need to clean up and improve performance of this mess
#[derive(Default)]
pub struct Mapping {
    atoms: HashMap<Lit, Vec<AtomID>>,
    literal: HashMap<AtomID, Lit>,
    empty_vec: Vec<AtomID>,
}
impl Mapping {
    #[allow(clippy::map_entry)]
    pub fn bind(&mut self, lit: Lit, atom: impl Into<AtomID>) {
        let atom: AtomID = atom.into();

        if self.literal.contains_key(&atom) {
            assert_eq!(
                *self.literal.get(&atom).unwrap(),
                lit,
                "A binding with a different literal already exists"
            );
        } else {
            assert!(!self.literal.contains_key(&atom));
            self.literal.insert(atom, lit);
            self.literal.insert(!atom, !lit);
            self.atoms
                .entry(lit)
                .or_insert_with(|| Vec::with_capacity(1))
                .push(atom);
            self.atoms
                .entry(!lit)
                .or_insert_with(|| Vec::with_capacity(1))
                .push(!atom);
        }
    }

    pub fn atoms_of(&self, lit: Lit) -> &[AtomID] {
        self.atoms.get(&lit).unwrap_or(&self.empty_vec)
    }

    pub fn literal_of(&self, atom: AtomID) -> Option<Lit> {
        self.literal.get(&atom).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stn::NetworkStatus::{Consistent, Inconsistent};

    fn assert_consistent<W: Time>(stn: &mut IncSTN<W>) {
        assert!(matches!(stn.propagate_all(), Consistent(_)));
    }
    fn assert_inconsistent<W: Time>(stn: &mut IncSTN<W>, mut cycle: Vec<EdgeID>) {
        cycle.sort();
        match stn.propagate_all() {
            Consistent(_) => panic!("Expected inconsistent network"),
            Inconsistent(exp) => {
                let mut vec: Vec<EdgeID> = exp.iter().copied().collect();
                vec.sort();
                assert_eq!(vec, cycle);
            }
        }
    }

    #[test]
    fn test_backtracking() {
        let mut stn = IncSTN::new();
        let a = stn.add_timepoint(0, 10);
        let b = stn.add_timepoint(0, 10);
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 10);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 10);

        stn.add_edge(stn.origin(), a, 1);
        assert_consistent(&mut stn);
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 1);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 10);
        stn.set_backtrack_point();

        let ab = stn.add_edge(a, b, 5i32);
        assert_consistent(&mut stn);
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 1);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 6);

        stn.set_backtrack_point();

        let ba = stn.add_edge(b, a, -6i32);
        assert_inconsistent(&mut stn, vec![ab, ba]);

        stn.undo_to_last_backtrack_point();
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 1);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 6);

        stn.undo_to_last_backtrack_point();
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 1);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 10);

        let x = stn.add_inactive_edge(a, b, 5i32);
        stn.mark_active(x);
        assert_consistent(&mut stn);
        assert_eq!(stn.lb(a), 0);
        assert_eq!(stn.ub(a), 1);
        assert_eq!(stn.lb(b), 0);
        assert_eq!(stn.ub(b), 6);
    }

    #[test]
    fn test_unification() {
        // build base stn
        let mut stn = IncSTN::new();
        let a = stn.add_timepoint(0, 10);
        let b = stn.add_timepoint(0, 10);

        // two identical edges should be unified
        let id1 = stn.add_edge(a, b, 1);
        let id2 = stn.add_edge(a, b, 1);
        assert_eq!(id1, id2);

        // edge negations
        let edge = Edge::new(a, b, 3); // b - a <= 3
        let not_edge = edge.negated(); // b - a > 3   <=>  a - b < -3  <=>  a - b <= -4
        assert_eq!(not_edge, Edge::new(b, a, -4));

        let id = stn.add_edge(edge.source, edge.target, edge.weight);
        let nid = stn.add_edge(not_edge.source, not_edge.target, not_edge.weight);
        assert_eq!(id.base_id(), nid.base_id());
        assert_ne!(id.is_negated(), nid.is_negated());
    }

    #[test]
    fn test_explanation() {
        let mut stn = IncSTN::new();
        let a = stn.add_timepoint(0, 10);
        let b = stn.add_timepoint(0, 10);
        let c = stn.add_timepoint(0, 10);
        stn.propagate_all();

        stn.set_backtrack_point();
        let aa = stn.add_inactive_edge(a, a, -1);
        stn.mark_active(aa);
        assert_inconsistent(&mut stn, vec![aa]);

        stn.undo_to_last_backtrack_point();
        stn.set_backtrack_point();
        let ab = stn.add_edge(a, b, 2);
        let ba = stn.add_edge(b, a, -3);
        assert_inconsistent(&mut stn, vec![ab, ba]);

        stn.undo_to_last_backtrack_point();
        stn.set_backtrack_point();
        let ab = stn.add_edge(a, b, 2);
        let _ = stn.add_edge(b, a, -2);
        assert_consistent(&mut stn);
        let ba = stn.add_edge(b, a, -3);
        assert_inconsistent(&mut stn, vec![ab, ba]);

        stn.undo_to_last_backtrack_point();
        stn.set_backtrack_point();
        let ab = stn.add_edge(a, b, 2);
        let bc = stn.add_edge(b, c, 2);
        let _ = stn.add_edge(c, a, -4);
        assert_consistent(&mut stn);
        let ca = stn.add_edge(c, a, -5);
        assert_inconsistent(&mut stn, vec![ab, bc, ca]);
    }
}
