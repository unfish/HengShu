#![no_main]

use hengshu::api::{check_source, format_source};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let formatted = format_source(source);
        if let Ok(formatted) = formatted {
            let _ = format_source(&formatted);
            let _ = check_source(&formatted);
        }
        let _ = check_source(source);
    }
});
