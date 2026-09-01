#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_server_case, ServerCase};

fuzz_target!(|case: ServerCase| {
    run_server_case(&case);
});
