//! Extract deployed contract artifacts directly from an Arena checkpoint.
//!
//! This deliberately opens only the state database. Unlike the full replay
//! controller it does not scan accepted block history, so it is suitable for
//! inspecting large migration checkpoints while another replay is stopped.

use std::{
    env,
    fs,
    io,
    path::PathBuf,
    str::FromStr,
};

use pulsevm_database::Database;
use pulsevm_name::Name;
use sha2::{
    Digest,
    Sha256,
};

fn usage(program: &str) {
    eprintln!("Usage: {program} <arena-dir> <output-dir> <account> [account ...]");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "xpr_contract_dump".into());
    let Some(arena_dir) = args.next() else {
        usage(&program);
        return Err("missing arena directory".into());
    };
    let Some(output_dir) = args.next() else {
        usage(&program);
        return Err("missing output directory".into());
    };
    let accounts: Vec<String> = args.collect();
    if accounts.is_empty() {
        usage(&program);
        return Err("at least one account is required".into());
    }

    let database = Database::new(&arena_dir, 0).map_err(io::Error::other)?;
    let output_dir = PathBuf::from(output_dir);
    fs::create_dir_all(&output_dir)?;
    println!(
        "revision={} state_root={}",
        database.revision(),
        database
            .arena_state_root()
            .map(hex::encode)
            .unwrap_or_else(|| "unavailable".into())
    );
    for (table, bytes) in database.arena_state_table_bytes() {
        println!(
            "table={table} bytes={} sha256={}",
            bytes.len(),
            hex::encode(Sha256::digest(&bytes))
        );
    }

    for account_text in accounts {
        let account = Name::from_str(&account_text)?;
        let (code_hash, vm_type, vm_version) = database.account_code_hash_vm(account.as_u64())?;
        let code = database.get_code_bytes_by_hash(&code_hash, vm_type, vm_version)?;
        let abi = database
            .arena_account_abi_bytes(account.as_u64())
            .ok_or_else(|| io::Error::other(format!("account {account} has no ABI row")))?;

        let wasm_path = output_dir.join(format!("{account}.wasm"));
        let abi_path = output_dir.join(format!("{account}.abi.bin"));
        fs::write(&wasm_path, &code)?;
        fs::write(&abi_path, &abi)?;
        println!(
            "account={account} code_hash={} vm_type={vm_type} vm_version={vm_version} wasm_bytes={} abi_bytes={} wasm={} abi={}",
            hex::encode(code_hash),
            code.len(),
            abi.len(),
            wasm_path.display(),
            abi_path.display()
        );
    }

    Ok(())
}
