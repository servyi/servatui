#![no_main]

use libfuzzer_sys::fuzz_target;
use servatui_fuzz::{run_render_case, RenderCase};

fuzz_target!(|case: RenderCase| {
    run_render_case(&case);
});
