//! A lazy grammar walker.
//!
//! A grammar is a tree built by composition ([`Lit`], [`Seq`], [`Alt`],
//! [`Opt`], [`Rep`], [`Dyn`]) and is **never materialized**: a draw is
//! a random [`Node::walk`] from the root that appends to the output
//! string as it goes. Because the tree exists only as the path being
//! walked, grammars may be **infinite** — in depth, and in edge set:
//! [`Dyn`] computes its value at walk time, so its edges can come
//! from live external state (e.g. what a server currently reports)
//! that did not exist before the walk and changes between walks.
//!
//! The walk contract (also the differential-testing contract, e.g.
//! for refactor checking): a draw is a pure function of the tree and
//! the sequence of [`Rng::next_u64`] values; and **every edge out of
//! a node has nonzero probability** — an [`Alt`] draws uniformly, an
//! [`Opt`] takes both branches with nonzero chance, a [`Rep`] may
//! always stop or continue. Terminating walks are guaranteed by the
//! budget: every append consumes budget, so no walk can loop forever.
//!
//! The crate has no dependencies and knows nothing of servatui's
//! protocol types; any project can define its grammar over its own
//! vocabulary.

use std::fmt;

/// The closure [`Dyn`] computes its walk-time value with.
pub type DynFn = Box<dyn Fn(&mut dyn Rng) -> String>;

/// The randomness a walk consumes. Implement over any RNG (splitmix64,
/// pcg, ...); determinism of a draw follows from determinism of the
/// value sequence.
pub trait Rng {
    fn next_u64(&mut self) -> u64;
}

/// Splitmix64 — the default RNG for tests and for callers that just
/// want seeded determinism (`Seedable` below is intentionally absent:
/// callers often carry their own).
#[derive(Clone)]
pub struct SplitMix64(pub u64);

impl Rng for SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Everything a node may consume or produce during one walk: the rng,
/// the output string, and the remaining budget.
///
/// The budget counts **append calls** (not bytes): any repetition that
/// appends must eventually exhaust it, which is what makes infinite
/// grammars safe to walk. Construct via [`Walk::new`].
pub struct Walk<'a> {
    rng: &'a mut dyn Rng,
    out: String,
    budget: usize,
}

impl<'a> Walk<'a> {
    /// A walk producing at most `budget` appends.
    pub fn new(rng: &'a mut dyn Rng, budget: usize) -> Self {
        Self { rng, out: String::new(), budget }
    }

    /// Append `s` if budget remains; `false` means the walk is full
    /// and the caller should wind down (repetition nodes stop).
    pub fn append(&mut self, s: &str) -> bool {
        if self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        self.out.push_str(s);
        true
    }

    /// Whether any budget remains.
    pub fn has_budget(&self) -> bool {
        self.budget > 0
    }

    /// Finish the walk, yielding the generated string.
    pub fn finish(self) -> String {
        self.out
    }
}

/// One node of a lazily-defined grammar tree.
pub trait Node {
    /// Consume randomness and append output; every edge out of this
    /// node must have nonzero probability.
    fn walk(&self, w: &mut Walk);
}

/// A literal string.
pub struct Lit(pub &'static str);

impl Node for Lit {
    fn walk(&self, w: &mut Walk) {
        let _ = w.append(self.0);
    }
}

/// A sequence: children in order.
pub struct Seq<'a>(pub &'a [&'a dyn Node]);

impl Node for Seq<'_> {
    fn walk(&self, w: &mut Walk) {
        for child in self.0 {
            child.walk(w);
        }
    }
}

/// An alternative: one child drawn uniformly — every child has
/// nonzero probability.
pub struct Alt<'a>(pub &'a [&'a dyn Node]);

impl Node for Alt<'_> {
    fn walk(&self, w: &mut Walk) {
        if self.0.is_empty() {
            return;
        }
        let idx = (w.rng.next_u64() % self.0.len() as u64) as usize;
        self.0[idx].walk(w);
    }
}

/// An optional child: taken or skipped, both with nonzero
/// probability. `p_take_num`/`p_take_den` is the chance of taking
/// (must be strictly between 0 and the denominator, keeping both
/// edges nonzero).
pub struct Opt<'a> {
    pub node: &'a dyn Node,
    pub p_take_num: u64,
    pub p_take_den: u64,
}

impl Node for Opt<'_> {
    fn walk(&self, w: &mut Walk) {
        assert!(
            self.p_take_den > 1 && self.p_take_num > 0 && self.p_take_num < self.p_take_den,
            "Opt probabilities must keep both edges nonzero"
        );
        if w.rng.next_u64() % self.p_take_den < self.p_take_num {
            self.node.walk(w);
        }
    }
}

/// Repetition: the child zero or more times. Each iteration rolls the
/// continue/stop edge (both nonzero); the budget also stops the loop,
/// which is the guard that makes infinite grammars walkable.
pub struct Rep<'a> {
    pub node: &'a dyn Node,
    /// Chance of ONE MORE iteration, as num/den (both edges nonzero:
    /// 0 < num < den, den > 1).
    pub p_more_num: u64,
    pub p_more_den: u64,
}

impl Node for Rep<'_> {
    fn walk(&self, w: &mut Walk) {
        assert!(
            self.p_more_den > 1 && self.p_more_num > 0 && self.p_more_num < self.p_more_den,
            "Rep probabilities must keep both edges nonzero"
        );
        while w.has_budget() && w.rng.next_u64() % self.p_more_den < self.p_more_num {
            self.node.walk(w);
        }
    }
}

/// A leaf whose value is computed AT WALK TIME — the node whose edges
/// may not exist before the walk (live server state, fresh counters,
/// anything mutable). The closure draws its own randomness and
/// returns the value; the nonzero-edges contract is the closure's:
/// every value it can return must be reachable.
pub struct Dyn(pub DynFn);

impl Node for Dyn {
    fn walk(&self, w: &mut Walk) {
        let v = (self.0)(w.rng);
        let _ = w.append(&v);
    }
}

impl fmt::Debug for Dyn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Dyn(..)")
    }
}

/// Draw one string from `node`: fresh walk, given rng, given budget.
pub fn draw(node: &dyn Node, rng: &mut dyn Rng, budget: usize) -> String {
    let mut w = Walk::new(rng, budget);
    node.walk(&mut w);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Determinism: identical seed → byte-identical draws.
    #[test]
    fn draws_are_a_pure_function_of_the_seed() {
        let tree = Seq(&[
            &Lit("grant "),
            &Dyn(Box::new(|rng| format!("id{}", rng.next_u64() % 3))),
            &Rep { node: &Lit(" x"), p_more_num: 1, p_more_den: 2 },
        ]);
        for seed in 0..50u64 {
            let a = draw(&tree, &mut SplitMix64(seed), 32);
            let b = draw(&tree, &mut SplitMix64(seed), 32);
            assert_eq!(a, b, "seed {seed} must reproduce");
        }
    }

    /// Every Alt edge is drawn across seeds (nonzero probability).
    #[test]
    fn every_alt_edge_is_reachable() {
        let words: [&dyn Node; 4] =
            [&Lit("reset"), &Lit("remove"), &Lit("rotate"), &Lit("grant")];
        let tree = Alt(&words);
        let mut seen = [false; 4];
        for seed in 0..400u64 {
            let s = draw(&tree, &mut SplitMix64(seed), 4);
            match s.as_str() {
                "reset" => seen[0] = true,
                "remove" => seen[1] = true,
                "rotate" => seen[2] = true,
                "grant" => seen[3] = true,
                other => panic!("not an edge: {other}"),
            }
        }
        assert!(seen.iter().all(|s| *s), "some Alt edge never drawn: {seen:?}");
    }

    /// An infinite grammar — unbounded Rep of a growing Seq — must
    /// still terminate, bounded only by the budget, and never exceed
    /// it in appends.
    #[test]
    fn infinite_grammar_terminates_at_the_budget() {
        // (nested repetition: Rep of Seq(Lit("a"), inner Rep of ...))
        let inner = Rep { node: &Lit("b"), p_more_num: 9, p_more_den: 10 };
        let outer = Rep { node: &Seq(&[&Lit("a"), &inner]), p_more_num: 9, p_more_den: 10 };
        for budget in [1usize, 5, 50] {
            for seed in 0..100u64 {
                let s = draw(&outer, &mut SplitMix64(seed), budget);
                assert!(
                    s.len() <= 2 * budget,
                    "appends bounded by budget: {s}"
                );
            }
        }
    }

    /// Opt takes both branches across seeds (nonzero both ways).
    #[test]
    fn opt_takes_and_skips() {
        let tree = Opt { node: &Lit(" x"), p_take_num: 1, p_take_den: 2 };
        let mut taken = false;
        let mut skipped = false;
        for seed in 0..100u64 {
            match draw(&tree, &mut SplitMix64(seed), 4).as_str() {
                " x" => taken = true,
                "" => skipped = true,
                other => panic!("not an Opt outcome: {other:?}"),
            }
        }
        assert!(taken && skipped);
    }

    /// Dyn edges are computed at walk time: the same tree over a
    /// changing source yields the source's CURRENT values.
    #[test]
    fn dyn_edges_come_from_walk_time_state() {
        let live: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(vec!["old".to_string()]));
        let src = std::sync::Arc::clone(&live);
        let tree = Dyn(Box::new(move |rng| {
            // uniform nonzero draw over the live set
            let names = src.lock().expect("live set").clone();
            names[(rng.next_u64() % names.len() as u64) as usize].clone()
        }));
        let mut saw_old = false;
        for seed in 0..50u64 {
            if draw(&tree, &mut SplitMix64(seed), 1) == "old" {
                saw_old = true;
            }
        }
        assert!(saw_old, "live values are drawn");
        live.lock().expect("live set").clear();
        live.lock().expect("live set").push("new".to_string());
        for seed in 0..50u64 {
            assert_eq!(draw(&tree, &mut SplitMix64(seed), 1), "new");
        }
    }

    /// Zero-children Alt and budget exhaustion are calm: no panic,
    /// empty/halted output.
    #[test]
    fn degenerate_cases_do_not_panic() {
        let empty = Alt(&[] as &[&dyn Node]);
        assert_eq!(draw(&empty, &mut SplitMix64(1), 4), "");
        let tree = Seq(&[&Lit("ab"), &Rep { node: &Lit("c"), p_more_num: 1, p_more_den: 2 }]);
        assert_eq!(draw(&tree, &mut SplitMix64(7), 0), "");
    }
}
