//! A grammar walker that IS the autocomplete algorithm.
//!
//! One abstraction — a *suggestion function* `current → candidate
//! lines` (in servatui this is the completer verbatim: it returns
//! FULL candidate lines, prefix included) — and one loop:
//!
//! 1. ask for suggestions given the string constructed so far;
//! 2. if there are none, stop;
//! 3. otherwise either stop (nonzero chance, whenever suggestions
//!    exist) or adopt one suggestion verbatim as the new string so
//!    far (uniform draw — every suggestion has nonzero probability);
//! 4. repeat.
//!
//! That is a user typing with Tab-completion (the TUI applies a
//! suggestion by replacing the input line with it, verbatim), and it
//! is a random walk through a potentially infinite grammar: nothing
//! is materialized, the edges of each iteration are computed by the
//! suggestion function at iteration time, and the walk's state is
//! exactly the string so far. Determinism contract: a draw is a pure
//! function of the suggestion function's behavior and the
//! `rand_core::RngCore` value sequence.
//!
//! The crate has no dependencies and knows nothing of servatui's
//! types; any project whose surface can answer "what may the line
//! become next" can be walked (fuzzed, differentially tested).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

use rand_core::RngCore;

/// Simulate the autocomplete typist over a suggestion function.
///
/// * `suggest` — given the string so far, the candidate lines it may
///   become (servatui completer shape: full lines, prefix included;
///   called once per iteration, so its answers may change between
///   iterations — live state).
/// * `rng` — any `rand_core::RngCore` implementor (`SmallRng`,
///   `StdRng`, a consumer's hand-rolled splitmix64, ...); the only
///   randomness the walk consumes is `next_u64`.
/// * `p_continue_num/den` — the chance of ONE MORE step whenever
///   suggestions exist; strictly between 0 and `den` (`den > 1`), so
///   stopping and continuing are both always possible.
/// * `max_iterations` — safety bound only; a walk of a suggester that
///   always answers would still stop by the roll alone (almost
///   surely). Choose it generously.
///
/// Returns the string the typist stopped on: no suggestions, chose to
/// stop, or (degenerate) the bound.
///
/// # Panics
///
/// Panics unless `p_continue_den > 1` and
/// `0 < p_continue_num < p_continue_den` — the both-edges-nonzero
/// contract.
pub fn type_out(
    start: &str,
    suggest: &dyn Fn(&str) -> Vec<String>,
    rng: &mut dyn RngCore,
    p_continue_num: u64,
    p_continue_den: u64,
    max_iterations: usize,
) -> String {
    assert!(
        p_continue_den > 1 && p_continue_num > 0 && p_continue_num < p_continue_den,
        "continue probability must keep both edges nonzero"
    );
    let mut line = start.to_string();
    for _ in 0..max_iterations {
        let mut suggestions = suggest(&line);
        if suggestions.is_empty() {
            return line;
        }
        if rng.next_u64() % p_continue_den >= p_continue_num {
            return line;
        }
        let pick = (rng.next_u64() % suggestions.len() as u64) as usize;
        line = suggestions.swap_remove(pick);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand_core::SeedableRng;

    fn seeded(seed: u64) -> SmallRng {
        SmallRng::seed_from_u64(seed)
    }

    /// The verified real-world shape: full-line suggestions, prefix
    /// included (mirrors fuse-protocol's complete_first_secret —
    /// `format!("{cmd} {n}")`), adopted verbatim.
    #[test]
    fn suggestions_are_adopted_verbatim() {
        let suggest = |line: &str| {
            if line == "reset exi" {
                vec!["reset existing.yaml".to_string()]
            } else {
                Vec::new()
            }
        };
        // two steps: "reset exi" -> adopt -> no further suggestions
        let out = type_out("reset exi", &suggest, &mut seeded(0), 9, 10, 10);
        assert_eq!(out, "reset existing.yaml");
    }

    /// Determinism: identical seed → byte-identical draws.
    #[test]
    fn draws_are_a_pure_function_of_the_seed() {
        let suggest = |line: &str| {
            if line.is_empty() {
                vec!["cmd ".to_string()]
            } else if line == "cmd " {
                vec!["cmd a".to_string(), "cmd b".to_string()]
            } else if let Some(rest) = line.strip_prefix("cmd ") {
                if rest.is_empty() {
                    vec!["cmd a".to_string(), "cmd b".to_string()]
                } else {
                    vec![format!("cmd {rest} c")]
                }
            } else {
                Vec::new()
            }
        };
        for seed in 0..100u64 {
            let a = type_out("", &suggest, &mut seeded(seed), 8, 10, 1000);
            let b = type_out("", &suggest, &mut seeded(seed), 8, 10, 1000);
            assert_eq!(a, b, "seed {seed} must reproduce");
        }
    }

    /// Every suggestion is drawn across seeds (nonzero probability),
    /// and stopping mid-grammar also happens (nonzero) — including
    /// the empty stop before the first step.
    #[test]
    fn every_suggestion_and_stopping_are_reachable() {
        let suggest = |line: &str| {
            if line.is_empty() {
                vec!["one".to_string(), "two".to_string()]
            } else {
                Vec::new()
            }
        };
        let mut saw_one = false;
        let mut saw_two = false;
        let mut saw_stop = false;
        for seed in 0..200u64 {
            match type_out("", &suggest, &mut seeded(seed), 7, 10, 10).as_str() {
                "one" => saw_one = true,
                "two" => saw_two = true,
                "" => saw_stop = true,
                other => panic!("not an outcome of the grammar: {other:?}"),
            }
        }
        assert!(saw_one && saw_two && saw_stop);
    }

    /// Suggestions computed at iteration time: each iteration's answer
    /// comes from a fresh call (word index = call index), so a
    /// multi-step walk proves successive lines were fetched live, not
    /// cached up front.
    #[test]
    fn suggestions_are_live() {
        let mut three_live_rounds = false;
        let mut total_calls = 0usize;
        for seed in 0..200u64 {
            // fresh suggester per walk: the call index restarts, so
            // the walk's line shows exactly which iteration produced
            // which word.
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let c = std::sync::Arc::clone(&calls);
            let suggest = move |_line: &str| {
                let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 5 {
                    vec![format!("w{n}")]
                } else {
                    Vec::new()
                }
            };
            let out = type_out("", &suggest, &mut seeded(seed), 9, 10, 10);
            // call order is observable: w0 adopted, then w1, then w2
            if out == "w2" && calls.load(std::sync::atomic::Ordering::SeqCst) >= 3 {
                three_live_rounds = true;
            }
            assert!(
                out == "w0" || out == "w1" || out == "w2" || out == "w3" || out == "w4"
                    || out.is_empty(),
                "walk must adopt some call's line or stop: {out:?}"
            );
            total_calls += calls.load(std::sync::atomic::Ordering::SeqCst);
        }
        assert!(three_live_rounds, "some walk reached three live iterations");
        assert!(
            total_calls > 200,
            "the suggester is called once per iteration across walks"
        );
    }

    /// A suggester that always answers (each step growing the line)
    /// terminates anyway — the stop roll ends the walk, not the bound.
    #[test]
    fn always_answering_terminates_by_the_roll() {
        let suggest = |line: &str| vec![format!("{line} x")];
        let mut hit_bound = 0;
        for seed in 0..200u64 {
            let out = type_out("", &suggest, &mut seeded(seed), 1, 2, 1000);
            if out.matches(" x").count() >= 1000 {
                hit_bound += 1;
            }
        }
        assert_eq!(hit_bound, 0, "stop roll must terminate long before the bound");
    }

    /// No suggestions at the start: the start string comes back
    /// unchanged, zero rng draws consumed.
    #[test]
    fn no_suggestions_returns_start() {
        let suggest = |_line: &str| Vec::new();
        let mut rng = seeded(42);
        assert_eq!(type_out("already done", &suggest, &mut rng, 1, 2, 10), "already done");
        // The walk consumed NO randomness: replaying the same seed
        // draws the identical first values the untouched rng holds.
        let mut probe = [0u8; 8];
        rng.fill_bytes(&mut probe);
        let mut fresh = [0u8; 8];
        let mut rng2 = seeded(42);
        rng2.fill_bytes(&mut fresh);
        assert_eq!(probe, fresh, "no randomness consumed by the walk");
    }
}
