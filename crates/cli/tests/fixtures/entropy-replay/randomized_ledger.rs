use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const ENTROPY_BYTES: usize = 1_280;
const OPERATION_BYTES: usize = 5;

fn main() {
    let mut arguments = env::args().skip(1);
    let entropy = decode_hex(&arguments.next().expect("entropy hex argument"));
    let output = arguments.next().expect("output path argument");
    assert!(arguments.next().is_none(), "unexpected argument");
    assert_eq!(entropy.len(), ENTROPY_BYTES, "wrong entropy length");

    let mut balances = [10_000_u64; 3];
    for operation in entropy.chunks_exact(OPERATION_BYTES) {
        let mut source = usize::from(operation[1] % 3);
        let mut destination = usize::from(operation[2] % 3);
        if operation[0] & 1 == 1 {
            std::mem::swap(&mut source, &mut destination);
        }
        if source == destination {
            destination = (destination + 1) % balances.len();
        }
        let requested = u64::from(u16::from_be_bytes([operation[3], operation[4]]) % 257) + 1;
        let transferred = requested.min(balances[source]);
        balances[source] -= transferred;
        balances[destination] += transferred;
    }

    let result = format!(
        "{{\"alice\":{},\"bob\":{},\"carol\":{},\"operations\":{},\"entropy_batches\":8,\"entropy_bytes\":{ENTROPY_BYTES}}}\n",
        balances[0],
        balances[1],
        balances[2],
        entropy.len() / OPERATION_BYTES,
    );
    let output = Path::new(&output);
    fs::write(output, result).expect("write result");
    fs::set_permissions(output, fs::Permissions::from_mode(0o644)).expect("set result mode");
    println!("ledger-complete");
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "odd hex length");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hex is UTF-8");
            u8::from_str_radix(text, 16).expect("valid entropy hex")
        })
        .collect()
}
