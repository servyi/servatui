#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_display_case, DisplayCase};

fuzz_target!(|case: DisplayCase| {
    run_display_case(&case);
});
