#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_mouse_case, MouseCase};

fuzz_target!(|case: MouseCase| {
    run_mouse_case(&case);
});
