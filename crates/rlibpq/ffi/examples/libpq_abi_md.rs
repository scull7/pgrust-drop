//! Prints `docs/libpq-abi.md`, the coverage matrix of libpq's C ABI:
//!
//! ```text
//! cargo run -p rlibpq-ffi --example libpq_abi_md > docs/libpq-abi.md
//! ```

use pq::abi::{EXPORTS_TXT, parse_exports, render_matrix};

fn main() {
    let exports = parse_exports(EXPORTS_TXT).expect("the vendored exports.txt parses");
    print!("{}", render_matrix(&exports));
}
