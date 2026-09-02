//! Print one account's resource limits and RAM usage from an Arena checkpoint.

use std::{
    env,
    io,
    str::FromStr,
};

use pulsevm_database::Database;
use pulsevm_name::Name;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let program = args
        .next()
        .unwrap_or_else(|| "xpr_account_resource_dump".into());
    let values: Vec<String> = args.collect();
    if values.len() != 2 {
        return Err(format!("Usage: {program} <arena-dir> <account>").into());
    }

    let database = Database::new(&values[0], 0).map_err(io::Error::other)?;
    let account = Name::from_str(&values[1])?;
    let mut ram_bytes = 0;
    let mut net_weight = 0;
    let mut cpu_weight = 0;
    database.get_account_limits(
        account.as_u64(),
        &mut ram_bytes,
        &mut net_weight,
        &mut cpu_weight,
    )?;
    let ram_usage = database.get_account_ram_usage(account.as_u64())?;

    println!(
        "revision={} account={} ram_usage={} ram_bytes={} ram_available={} net_weight={} cpu_weight={}",
        database.revision(),
        account,
        ram_usage,
        ram_bytes,
        ram_bytes.saturating_sub(ram_usage),
        net_weight,
        cpu_weight,
    );
    let deferred: Vec<_> = database
        .arena_deferred_transactions()
        .into_iter()
        .filter(|transaction| transaction.payer == account.as_u64())
        .collect();
    println!("deferred_paid={}", deferred.len());
    for transaction in deferred {
        println!(
            "deferred trx_id={} sender={} sender_id={} packed_bytes={} delay_until={} expiration={} published={}",
            hex::encode(transaction.trx_id),
            Name::new(transaction.sender),
            transaction.sender_id,
            transaction.packed_trx.len(),
            transaction.delay_until,
            transaction.expiration,
            transaction.published,
        );
    }
    Ok(())
}
