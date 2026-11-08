//! Modulation matrix: routes from sources to destinations, additive or multiplicative.

use super::util::sanitize;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RouteKind {
    /// Adds `source × amount` to the destination.
    #[default]
    Add,
    /// Multiplies the destination by `1 - amount + amount × source`, floored at zero, so a full
    /// amount with a unipolar source goes from silence to the unmodulated value.
    Mul,
}

/// One connection in a [`ModMatrix`]. Indices are positions in the source and destination lists.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Route {
    pub source: usize,
    pub dest: usize,
    pub amount: f32,
    pub kind: RouteKind,
    /// A second source that scales `amount`, such as a mod wheel.
    pub via: Option<usize>,
}

/// The routes of a device, shared by all voices. Per-voice results go in a [`ModBus`].
#[derive(Clone, Debug)]
pub struct ModMatrix {
    sources: usize,
    dests: usize,
    routes: Box<[Option<Route>]>,
}

impl ModMatrix {
    /// A matrix with `max_routes` empty slots.
    pub fn new(sources: usize, dests: usize, max_routes: usize) -> Self {
        Self {
            sources,
            dests,
            routes: vec![None; max_routes].into_boxed_slice(),
        }
    }

    pub fn bus(&self) -> ModBus {
        ModBus {
            add: vec![0.0; self.dests].into_boxed_slice(),
            mul: vec![1.0; self.dests].into_boxed_slice(),
        }
    }

    /// Replace slot `slot`. A route naming an out-of-range source or destination is ignored,
    /// and so is a slot past the end.
    pub fn set_route(&mut self, slot: usize, route: Option<Route>) {
        let valid = route.is_none_or(|r| {
            r.source < self.sources
                && r.dest < self.dests
                && r.via.is_none_or(|via| via < self.sources)
        });
        if let (true, Some(s)) = (valid, self.routes.get_mut(slot)) {
            *s = route;
        }
    }

    pub fn routes(&self) -> &[Option<Route>] {
        &self.routes
    }

    /// Work out every destination's modulation for one voice. Missing sources read as zero.
    pub fn evaluate(&self, sources: &[f32], bus: &mut ModBus) {
        bus.add.fill(0.0);
        bus.mul.fill(1.0);
        for route in self.routes.iter().flatten() {
            let source = sources.get(route.source).copied().unwrap_or(0.0);
            let mut amount = route.amount;
            if let Some(via) = route.via {
                amount *= sanitize(sources.get(via).copied().unwrap_or(0.0));
            }
            let Some((add, mul)) = bus.add.get_mut(route.dest).zip(bus.mul.get_mut(route.dest))
            else {
                continue;
            };
            let source = sanitize(source);
            match route.kind {
                RouteKind::Add => *add += source * amount,
                RouteKind::Mul => *mul *= (1.0 - amount + amount * source).max(0.0),
            }
        }
    }
}

/// One voice's modulation for every destination, recomputed by [`ModMatrix::evaluate`].
#[derive(Clone, Debug)]
pub struct ModBus {
    add: Box<[f32]>,
    mul: Box<[f32]>,
}

impl ModBus {
    /// `(base + additive) × multiplicative` for `dest`.
    #[inline]
    pub fn apply(&self, dest: usize, base: f32) -> f32 {
        let (add, mul) = match (self.add.get(dest), self.mul.get(dest)) {
            (Some(a), Some(m)) => (*a, *m),
            _ => return base,
        };
        sanitize((base + add) * mul)
    }

    pub fn offset(&self, dest: usize) -> f32 {
        self.add.get(dest).copied().unwrap_or(0.0)
    }

    pub fn gain(&self, dest: usize) -> f32 {
        self.mul.get(dest).copied().unwrap_or(1.0)
    }
}
