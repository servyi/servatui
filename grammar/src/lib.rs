//! A grammar walker that IS the autocomplete algorithm.
//!
//! One abstraction — a *suggestion function* `prefix → what may come
//! next` (in servatui this is the protocol completer; in a fuzzer it
//! is fed by live server state) — and one loop:
//!
//! 1. ask for suggestions given the string constructed so far;
//! 2. if there are none, stop;
//! 3. otherwise either stop (nonzero chance, whenever suggestions
//!    exist) or apply one suggestion as the next component of the
//!    string (uniform draw — every suggestion has nonzero
//!    probability);
//! 4. repeat.
//!
//! That is a user typing with Tab-completion, and it is a random walk
//! through a potentially infinite grammar: nothing is materialized,
//! the edges of each iteration are computed by the suggestion
//! function at iteration time, and the walk's state is exactly the
//! string so far. Determinism contract: a draw is a pure function of
//! the suggestion function's behavior and the [`Rng`] value sequence.
//!
//! The crate has no dependencies and knows nothing of servatui's
//! types; any project whose surface can answer "what may come next
//! given this prefix" can be walked (fuzzed, differentially tested).

/// The randomness a walk consumes. Implement over any RNG (splitmix64,
/// pcg, ...); determinism of a draw follows from determinism of the
/// value sequence.
pub trait Rng {
    fn next_u64(&mut self) -> u64;
}

/// Splitmix64 — the default RNG for tests and for callers that just
/// want seeded determinism.
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

/// Simulate the autocomplete typist over a suggestion function.
///
/// * `suggest` — given the string so far, the candidates for the next
///   component (they may contain leading separators; they are opaque
///   to the walker). Called once per iteration, so its answers may
///   change between iterations (live state).
/// * `p_continue_num/den` — the chance of ONE MORE component whenever
///   suggestions exist; strictly between 0 and `den` (`den > 1`), so
///   stopping and continuing are both always possible.
/// * `max_iterations` — safety bound only; a walk of a suggester that
///   always answers would still stop by the roll alone (almost
///   surely). Choose it generously.
///
/// Returns the string constructed when the typist stopped: no
/// suggestions, chose to stop, or (degenerate) the bound.
pub fn type_out(
    start: &str,
    suggest: &dyn Fn(&str) -> Vec<String>,
    rng: &mut dyn Rng,
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
        let suggestions = suggest(&line);
        if suggestions.is_empty() {
            return line;
        }
        if rng.next_u64() % p_continue_den >= p_continue_num {
            return line;
        }
        let pick = (rng.next_u64() % suggestions.len() as u64) as usize;
        line.push_str(&suggestions[pick]);
    }
    line
}

/// Adapt a FULL-LINE completer (servatui's shape: it returns whole
/// candidate lines for a prefix) into the component suggester
/// [`type_out`] walks: each candidate line becomes the suffix it
/// would append. Candidates that do not extend the prefix are
/// dropped — they are not reachable components.
pub fn full_line_suggester<'a>(
    completer: &'a dyn Fn(&str) -> Vec<String>,
) -> impl Fn(&str) -> Vec<String> + 'a {
    move |prefix: &str| {
        completer(prefix)
            .into_iter()
            .filter_map(|line| {
                line.strip_prefix(prefix)
                    .map(|suffix| suffix.to_string())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Determinism: identical seed → byte-identical draws.
    #[test]
    fn draws_are_a_pure_function_of_the_seed() {
        let suggest = full_line_suggester(&|prefix: &str| {
            // "cmd " then words "a"/"b"/"c" — a tiny command grammar.
            if prefix.is_empty() {
                vec!["cmd ".to_string()]
            } else if let Some(rest) = prefix.strip_prefix("cmd ") {
                if rest.is_empty() {
                    vec!["cmd a".to_string(), "cmd b".to_string()]
                } else {
                    vec![format!("cmd {rest} c")]
                }
            } else {
                Vec::new()
            }
        });
        for seed in 0..100u64 {
            let a = type_out("", &suggest, &mut SplitMix64(seed), 8, 10, 1000);
            let b = type_out("", &suggest, &mut SplitMix64(seed), 8, 10, 1000);
            assert_eq!(a, b, "seed {seed} must reproduce");
        }
    }

    /// Every suggestion is drawn across seeds (nonzero probability),
    /// and stopping mid-grammar also happens (nonzero) — including
    /// the empty stop before the first component.
    #[test]
    fn every_suggestion_and_stopping_are_reachable() {
        let suggest = |prefix: &str| {
            if prefix.is_empty() {
                vec![" one".to_string(), " two".to_string()]
            } else {
                Vec::new()
            }
        };
        let mut saw_one = false;
        let mut saw_two = false;
        let mut saw_stop = false;
        for seed in 0..200u64 {
            match type_out("", &suggest, &mut SplitMix64(seed), 7, 10, 10).as_str() {
                " one" => saw_one = true,
                " two" => saw_two = true,
                "" => saw_stop = true,
                other => panic!("not an outcome of the grammar: {other:?}"),
            }
        }
        assert!(saw_one && saw_two && saw_stop);
    }

    /// Suggestions computed at iteration time: each iteration's
    /// answer comes from a fresh call (word index = call index), so a
    /// multi-component walk proves successive components were fetched
    /// live, not cached up front.
    #[test]
    fn suggestions_are_live() {
        let mut three_live_rounds = false;
        let mut total_calls = 0usize;
        for seed in 0..200u64 {
            // fresh suggester per walk: the call index restarts, so
            // the walk's string shows exactly which iteration
            // produced which component.
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let c = std::sync::Arc::clone(&calls);
            let suggest = move |_prefix: &str| {
                let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 5 {
                    vec![format!(" w{n}")]
                } else {
                    Vec::new()
                }
            };
            let out = type_out("", &suggest, &mut SplitMix64(seed), 9, 10, 10);
            // call order is observable in the string: w0 then w1 then w2
            if out.starts_with(" w0 w1 w2") {
                three_live_rounds = true;
            }
            assert!(
                out.starts_with(" w0") || out.is_empty(),
                "walk must start with the FIRST call's word or stop: {out:?}"
            );
            total_calls += calls.load(std::sync::atomic::Ordering::SeqCst);
        }
        assert!(three_live_rounds, "some walk reached three live iterations");
        assert!(
            total_calls > 200,
            "the suggester is called once per iteration across walks"
        );
    }

    /// A suggester that always answers terminates anyway — the stop
    /// roll ends the walk, not the bound.
    #[test]
    fn always_answering_terminates_by_the_roll() {
        let suggest = |_prefix: &str| vec![" x".to_string()];
        let mut hit_bound = 0;
        for seed in 0..200u64 {
            let out = type_out("", &suggest, &mut SplitMix64(seed), 1, 2, 1000);
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
        let suggest = |_prefix: &str| Vec::new();
        let mut rng = SplitMix64(42);
        assert_eq!(type_out("already done", &suggest, &mut rng, 1, 2, 10), "already done");
        assert_eq!(rng.0, SplitMix64(42).0, "no randomness consumed");
    }

    /// The full-line adapter: candidate lines become their suffixes;
    /// non-extending candidates are dropped.
    #[test]
    fn full_line_adapter_appends_suffixes_only() {
        let completer = |prefix: &str| {
            if prefix == "mod" {
                vec!["mode a".to_string(), "mode b".to_string(), "wrong".to_string()]
            } else {
                Vec::new()
            }
        };
        let suggest = full_line_suggester(&completer);
        assert_eq!(suggest("mod"), vec!["e a".to_string(), "e b".to_string()]);
        assert!(suggest("nothing").is_empty());
    }
}
