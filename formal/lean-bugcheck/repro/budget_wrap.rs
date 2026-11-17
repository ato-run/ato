// rustc -O budget_wrap.rs && ./budget_wrap   (debug build panics on `+ bytes` instead)
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
include!(concat!(env!("OUT"), "/transfer_budget.rs"));
fn main() {
    let b = TransferBudget::new(10);
    assert!(b.consume(5));
    let ok = b.consume(u64::MAX - 2); // real total = 2^64+2 > limit
    println!("consume(u64::MAX-2) -> {ok}, used={} exhausted={}", b.used(), b.exhausted());
    assert!(ok, "bug not reproduced");
}
