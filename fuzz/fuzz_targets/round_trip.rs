#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_round_trip_case, RoundTripCase};

fuzz_target!(|case: RoundTripCase| {
    run_round_trip_case(&case);
});
