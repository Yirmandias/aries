use crate::expressions::ExprHandle;
use crate::lang::{BVar, IntCst, VarRef};
use crate::{Label, WriterId};
use aries_backtrack::Q;
use aries_backtrack::{Backtrack, BacktrackWith};
use aries_collections::ref_store::{RefMap, RefVec};
use aries_sat::all::BVar as SatVar;
use aries_sat::all::Lit;

#[derive(Clone)]
pub struct IntDomain {
    pub lb: IntCst,
    pub ub: IntCst,
}
impl IntDomain {
    pub fn new(lb: IntCst, ub: IntCst) -> IntDomain {
        IntDomain { lb, ub }
    }
}
#[derive(Copy, Clone)]
pub struct VarEvent {
    pub var: VarRef,
    pub ev: DomEvent,
}

#[derive(Copy, Clone)]
pub enum DomEvent {
    NewLB { prev: IntCst, new: IntCst },
    NewUB { prev: IntCst, new: IntCst },
}

#[derive(Copy, Clone)]
pub struct Cause {
    pub writer: WriterId,
    pub payload: u64,
}
impl Cause {
    pub fn new(writer: WriterId, payload: impl Into<u64>) -> Self {
        Cause {
            writer,
            payload: payload.into(),
        }
    }
}

#[derive(Default, Clone)]
pub struct DiscreteModel {
    labels: RefVec<VarRef, Label>,
    pub(crate) domains: RefVec<VarRef, (IntDomain, Option<Lit>)>,
    trail: Q<(VarEvent, Cause)>,
    pub(crate) binding: RefMap<BVar, Lit>,
    pub(crate) expr_binding: RefMap<ExprHandle, Lit>,
    pub(crate) values: RefMap<SatVar, bool>,
    pub(crate) sat_to_int: RefMap<SatVar, IntOfSatVar>,
    pub(crate) lit_trail: Q<(Lit, Cause)>,
}

/// Representation of a sat variable as a an integer variable.
/// The variable can be inverted (true <=> 0), in which case the `inverted`
/// boolean flag is true.
#[derive(Copy, Clone)]
pub(crate) struct IntOfSatVar {
    variable: VarRef,
    inverted: bool,
}

impl DiscreteModel {
    pub fn new() -> DiscreteModel {
        DiscreteModel {
            labels: Default::default(),
            domains: Default::default(),
            trail: Default::default(),
            binding: Default::default(),
            expr_binding: Default::default(),
            values: Default::default(),
            sat_to_int: Default::default(),
            lit_trail: Default::default(),
        }
    }

    pub fn new_discrete_var<L: Into<Label>>(&mut self, lb: IntCst, ub: IntCst, label: L) -> VarRef {
        let id1 = self.labels.push(label.into());
        let id2 = self.domains.push((IntDomain::new(lb, ub), None));
        debug_assert_eq!(id1, id2);
        id1
    }

    pub fn variables(&self) -> impl Iterator<Item = VarRef> {
        self.labels.keys()
    }

    pub fn label(&self, var: impl Into<VarRef>) -> Option<&str> {
        self.labels[var.into()].get()
    }

    pub fn domain_of(&self, var: impl Into<VarRef>) -> &IntDomain {
        &self.domains[var.into()].0
    }

    fn dom_mut(&mut self, var: impl Into<VarRef>) -> &mut IntDomain {
        &mut self.domains[var.into()].0
    }

    pub fn set_lb(&mut self, var: impl Into<VarRef>, lb: IntCst, cause: Cause) {
        let var = var.into();
        let dom = self.dom_mut(var);
        let prev = dom.lb;
        if prev < lb {
            dom.lb = lb;
            let event = VarEvent {
                var,
                ev: DomEvent::NewLB { prev, new: lb },
            };
            self.trail.push((event, cause));

            if let Some(lit) = self.domains[var].1 {
                // there is literal corresponding to this variable
                debug_assert!(lb == 1 && prev == 0);
                self.set(lit, cause); // TODO: this might recursivly (and uselessly call us)
            }
        }
    }

    pub fn set_ub(&mut self, var: impl Into<VarRef>, ub: IntCst, cause: Cause) {
        let var = var.into();
        let dom = self.dom_mut(var);
        let prev = dom.ub;
        if prev > ub {
            dom.ub = ub;
            let event = VarEvent {
                var,
                ev: DomEvent::NewUB { prev, new: ub },
            };
            self.trail.push((event, cause));

            if let Some(lit) = self.domains[var].1 {
                // there is literal corresponding to this variable
                debug_assert!(ub == 0 && prev == 1);
                self.set(!lit, cause); // TODO: this might recursivly (and uselessly call us)
            }
        }
    }

    // ============= UNDO ================

    fn undo_int_event(domains: &mut RefVec<VarRef, (IntDomain, Option<Lit>)>, ev: VarEvent) {
        let dom = &mut domains[ev.var].0;
        match ev.ev {
            DomEvent::NewLB { prev, new } => {
                debug_assert_eq!(dom.lb, new);
                dom.lb = prev;
            }
            DomEvent::NewUB { prev, new } => {
                debug_assert_eq!(dom.ub, new);
                dom.ub = prev;
            }
        }
    }

    // =============== BOOL ===============

    pub fn bind(&mut self, k: BVar, lit: Lit) {
        assert!(!self.binding.contains(k));

        self.binding.insert(k, lit);

        let dvar = VarRef::from(k);
        // make sure updates to the integer variable are repercuted to the literal
        assert!(
            self.domains[dvar].1.is_none(),
            "The same variable is bound to more than one literal"
        );
        self.domains[dvar].1 = Some(lit);

        // make sure updates to the literal are repercuted to the int variable
        let inverted = !lit.value();
        let rep = IntOfSatVar {
            variable: dvar,
            inverted,
        };
        self.sat_to_int.insert(lit.variable(), rep)
    }

    pub fn literal_of(&self, bvar: BVar) -> Option<Lit> {
        self.binding.get(bvar).copied()
    }

    /// Returns the literal associated with this `BVar`. If the variable is not already
    /// bound to a literal, a new one will be created through the `make_lit` closure.
    pub fn intern_variable_with(&mut self, bvar: BVar, make_lit: impl FnOnce() -> Lit) -> Lit {
        match self.literal_of(bvar) {
            Some(lit) => lit,
            None => {
                let lit = make_lit();
                self.bind(bvar, lit);
                lit
            }
        }
    }

    pub fn boolean_variables(&self) -> impl Iterator<Item = BVar> + '_ {
        self.binding.keys()
    }

    /// Returns an iterator on all internal bool variables that have been given a value.
    pub fn bound_sat_variables(&self) -> impl Iterator<Item = (SatVar, bool)> + '_ {
        self.values.entries().map(|(k, v)| (k, *v))
    }

    pub fn value(&self, lit: Lit) -> Option<bool> {
        self.values
            .get(lit.variable())
            .copied()
            .map(|value| if lit.value() { value } else { !value })
    }

    pub fn value_of(&self, v: BVar) -> Option<bool> {
        self.binding.get(v).and_then(|lit| self.value(*lit))
    }

    pub fn set(&mut self, lit: Lit, cause: Cause) {
        let var = lit.variable();
        let val = lit.value();
        let prev = self.values.get(var).copied();
        assert_ne!(prev, Some(!val), "Incompatible values");
        if prev.is_none() {
            self.values.insert(var, val);
            self.lit_trail.push((lit, cause));
            if let Some(int_var) = self.sat_to_int.get(lit.variable()) {
                let variable = int_var.variable;
                // this literal is bound to an integer variable, set its domain accordingly
                if val && !int_var.inverted {
                    // note: in the current implementation, the set_lb/set_ub will call us again.
                    // This is ok, because it will be a no-op, but wan be wasteful.
                    self.set_lb(variable, 1, cause);
                } else {
                    self.set_ub(variable, 0, cause)
                }
            }
        } else {
            // no-op
            debug_assert_eq!(prev, Some(val));
        }
    }

    // ================ EXPR ===========

    pub fn interned_expr(&self, handle: ExprHandle) -> Option<Lit> {
        self.expr_binding.get(handle).copied()
    }

    pub fn intern_expr_with(&mut self, handle: ExprHandle, make_lit: impl FnOnce() -> Lit) -> Lit {
        match self.interned_expr(handle) {
            Some(lit) => lit,
            None => {
                let lit = make_lit();
                self.bind_expr(handle, lit);
                lit
            }
        }
    }

    fn bind_expr(&mut self, handle: ExprHandle, literal: Lit) {
        self.expr_binding.insert(handle, literal);
    }
}

impl Backtrack for DiscreteModel {
    fn save_state(&mut self) -> u32 {
        let a = self.trail.save_state();
        let b = self.lit_trail.save_state();
        debug_assert_eq!(a, b);
        a
    }

    fn num_saved(&self) -> u32 {
        let a = self.trail.num_saved();
        debug_assert_eq!(a, self.lit_trail.num_saved());
        a
    }

    fn restore_last(&mut self) {
        let int_domains = &mut self.domains;
        self.trail
            .restore_last_with(|(ev, _)| Self::undo_int_event(int_domains, ev));

        let bool_domains = &mut self.values;
        self.lit_trail
            .restore_last_with(|(lit, _)| bool_domains.remove(lit.variable()));
    }

    fn restore(&mut self, saved_id: u32) {
        let int_domains = &mut self.domains;
        self.trail
            .restore_with(saved_id, |(ev, _)| Self::undo_int_event(int_domains, ev));
        let bool_domains = &mut self.values;
        self.lit_trail
            .restore_with(saved_id, |(lit, _)| bool_domains.remove(lit.variable()));
    }
}
