#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_client_case, ClientCase};

fuzz_target!(|case: ClientCase| {
    run_client_case(&case);
});
