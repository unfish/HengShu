#![no_main]

use hengshu::api::CompiledProgram;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = CompiledProgram::from_bytes(data);
});
