use crate::backtrack::{Backtrack, Trail};
use crate::model::assignments::{Assignment, SavedAssignment};
use crate::solver::stats::Stats;
use aries_collections::heap::IdxHeap;

use aries_sat::all::{BVar, Lit};

pub struct BranchingParams {
    pub prefered_bool_value: bool,
    pub allowed_conflicts: u64,
    pub increase_ratio_for_allowed_conflicts: f32,
}

impl Default for BranchingParams {
    fn default() -> Self {
        BranchingParams {
            prefered_bool_value: false,
            allowed_conflicts: 100,
            increase_ratio_for_allowed_conflicts: 1.5_f32,
        }
    }
}

pub struct Brancher {
    pub params: BranchingParams,
    bool_sel: BoolVarSelect,
    default_assignment: Option<SavedAssignment>,
    trail: Trail<UndoChange>,
    conflicts_at_last_restart: u64,
}

enum UndoChange {
    Insertion(BVar),
    Removal(BVar),
}

pub enum Decision {
    SetLiteral(Lit),
    Restart,
}

impl Brancher {
    pub fn new() -> Self {
        Brancher {
            params: Default::default(),
            bool_sel: BoolVarSelect::new(Default::default()),
            default_assignment: None,
            trail: Default::default(),
            conflicts_at_last_restart: 0,
        }
    }

    pub fn is_declared(&self, var: BVar) -> bool {
        self.bool_sel.is_declared(var)
    }

    pub fn declare(&mut self, bvar: BVar) {
        self.bool_sel.declare_variable(bvar);
    }

    pub fn enqueue(&mut self, bvar: BVar) {
        self.bool_sel.enqueue_variable(bvar);
        self.trail.push(UndoChange::Insertion(bvar));
    }

    /// Select the next decision to make.
    /// Returns `None` if no decision is left to be made.
    pub fn next_decision(&mut self, stats: &Stats, current_assignment: &impl Assignment) -> Option<Decision> {
        // extract the highest priority variable that is not set yet.
        let next_unset = loop {
            match self.bool_sel.peek_next_var() {
                Some(v) => {
                    if current_assignment.value_of_sat_variable(v).is_some() {
                        // already bound, drop the peeked variable before proceeding to next
                        let v = self.bool_sel.pop_next_var().unwrap();
                        self.trail.push(UndoChange::Removal(v));
                    } else {
                        // not set, select for decision
                        break Some(v);
                    }
                }
                None => {
                    // no variables left in queue
                    break None;
                }
            }
        };
        if let Some(v) = next_unset {
            if stats.num_conflicts - self.conflicts_at_last_restart >= self.params.allowed_conflicts {
                // we have exceeded the number of allowed conflict, time for a restart
                self.conflicts_at_last_restart = stats.num_conflicts;
                // increase the number of allowed conflicts
                self.params.allowed_conflicts =
                    (self.params.allowed_conflicts as f32 * self.params.increase_ratio_for_allowed_conflicts) as u64;

                Some(Decision::Restart)
            } else {
                // determine value for literal:
                // - first from per-variable preferred assignments
                // - otherwise from the preferred value for boolean variables
                let value = self
                    .default_assignment
                    .as_ref()
                    .and_then(|ass| ass.value_of_sat_variable(v))
                    .unwrap_or(self.params.prefered_bool_value);

                let literal = v.lit(value);
                Some(Decision::SetLiteral(literal))
            }
        } else {
            // all variables are set, no decision left
            None
        }
    }

    /// Increase the activity of the variable and perform an reordering in the queue.
    /// The activity is then used to select the next variable.
    pub fn bump_activity(&mut self, bvar: BVar) {
        self.bool_sel.var_bump_activity(bvar);
    }
}

impl Default for Brancher {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BoolHeuristicParams {
    pub var_inc: f64,
    pub var_decay: f64,
}
impl Default for BoolHeuristicParams {
    fn default() -> Self {
        BoolHeuristicParams {
            var_inc: 1_f64,
            var_decay: 0.95_f64,
        }
    }
}

/// Heuristic value associated to a variable.
#[derive(Copy, Clone, PartialEq, PartialOrd)]
struct BoolVarHeuristicValue {
    activity: f64,
}

pub struct BoolVarSelect {
    params: BoolHeuristicParams,
    heap: IdxHeap<BVar, BoolVarHeuristicValue>,
}

impl BoolVarSelect {
    pub fn new(params: BoolHeuristicParams) -> Self {
        BoolVarSelect {
            params,
            heap: IdxHeap::new(),
        }
    }

    pub fn is_declared(&self, v: BVar) -> bool {
        self.heap.is_declared(v)
    }

    /// Declares a new variable. The variable is NOT added to the queue.
    pub fn declare_variable(&mut self, v: BVar) {
        let hvalue = BoolVarHeuristicValue {
            activity: self.params.var_inc,
        };
        self.heap.declare_element(v, hvalue);
    }

    /// Add the value to the queue, the variable must have been previously declared.
    pub fn enqueue_variable(&mut self, var: BVar) {
        self.heap.enqueue(var)
    }

    pub fn pop_next_var(&mut self) -> Option<BVar> {
        self.heap.pop()
    }

    pub fn peek_next_var(&mut self) -> Option<BVar> {
        self.heap.peek().copied()
    }

    pub fn var_bump_activity(&mut self, var: BVar) {
        let var_inc = self.params.var_inc;
        self.heap.change_priority(var, |p| p.activity += var_inc);
        if self.heap.priority(var).activity > 1e100_f64 {
            self.var_rescale_activity()
        }
    }

    pub fn decay_activities(&mut self) {
        self.params.var_inc /= self.params.var_decay;
    }

    fn var_rescale_activity(&mut self) {
        // here we scale the activity of all variables, to avoid overflowing
        // this can not change the relative order in the heap, since activities are scaled by the same amount.
        self.heap.change_all_priorities_in_place(|p| p.activity *= 1e-100_f64);
        self.params.var_inc *= 1e-100_f64;
    }
}

impl Backtrack for Brancher {
    fn save_state(&mut self) -> u32 {
        self.trail.save_state()
    }

    fn num_saved(&self) -> u32 {
        self.trail.num_saved()
    }

    fn restore_last(&mut self) {
        let bools = &mut self.bool_sel;
        self.trail.restore_last_with(|event| match event {
            UndoChange::Insertion(_) => {
                // variables can be left in the queue, they are checked to be unset before return them
                // to the caller of next_decision.
            }
            UndoChange::Removal(x) => {
                bools.enqueue_variable(x);
            }
        })
    }
}