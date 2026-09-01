//! Code-hash-pinned native accelerators for historical XPR migration replay.
//!
//! These handlers are deliberately unavailable unless the replay operator opts
//! in. An account or action name is never sufficient: each handler is also
//! pinned to the SHA-256 of the deployed WASM whose semantics it implements.

use pulsevm_crypto::Digest;
use pulsevm_error::ChainError;
use pulsevm_proc_macros::{
    NumBytes,
    Read,
    Write,
};
use pulsevm_serialization::{
    CanonicalMap,
    Read as SerializationRead,
    Write as SerializationWrite,
};

use crate::{
    chain::{
        ACTIVE_NAME,
        apply_context::ApplyContext,
        authority::PermissionLevel,
        transaction::Action,
    },
    name::Name,
};

const BOT: u64 = 4_409_586_985_149_136_896;
const BOTS: u64 = 4_410_009_197_614_202_880;
const ORACLES: u64 = 11_947_074_179_527_868_416;
const DATA: u64 = 5_310_412_463_739_502_592;
const PROCESS: u64 = 12_531_412_623_406_661_632;
const FEED: u64 = 6_527_000_089_641_091_072;
const FEEDS: u64 = 6_527_013_283_780_624_384;

// XPRNetwork/proton-bots commit 44457b697c9c7dd91abc610332bc20e9ecfa4866.
const XPR_BOT_CODE_HASH: [u8; 32] = [
    0x8e, 0x7d, 0x40, 0xff, 0x68, 0x07, 0xab, 0x49, 0x07, 0xdd, 0x30, 0x05, 0x33, 0x18, 0xea, 0x3b,
    0x38, 0xef, 0x71, 0xea, 0x56, 0xb0, 0x52, 0xa0, 0xe6, 0x0d, 0x3c, 0xbe, 0x34, 0x17, 0xa1, 0xbb,
];

const XPR_ORACLES_CODE_HASH: [u8; 32] = [
    0xcc, 0x1e, 0x51, 0x38, 0x47, 0xcc, 0x95, 0x92, 0xed, 0x2e, 0x4b, 0xb1, 0x85, 0xdb, 0x5e, 0x65,
    0x32, 0x8d, 0x7a, 0x18, 0x0b, 0x1c, 0x34, 0x87, 0x71, 0x8d, 0x97, 0x51, 0x29, 0x3b, 0xca, 0xf2,
];

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct DataVariant {
    d_string: Option<String>,
    d_uint64: Option<u64>,
    d_double: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct BotEntry {
    bot_index: u64,
    data: DataVariant,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct Process {
    account: u64,
    entries: Vec<BotEntry>,
    nonce: u64,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct Tx {
    id: Digest,
    time: i64,
    data: DataVariant,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct BotRow {
    index: u64,
    account: u64,
    description: String,
    oracle_contract: u64,
    feed_index: u64,
    tx_count_by_utc_hour: CanonicalMap<u8, u64>,
    max_history: u8,
    history: Vec<Tx>,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct ProviderPoint {
    provider: u64,
    time: i64,
    data: DataVariant,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct FeedRow {
    index: u64,
    name: String,
    description: String,
    aggregate_function: String,
    data_type: String,
    config: CanonicalMap<String, u64>,
    providers: CanonicalMap<u64, i64>,
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct DataRow {
    feed_index: u64,
    aggregate: DataVariant,
    points: Vec<ProviderPoint>,
}

fn read_exact<T: SerializationRead>(bytes: &[u8], label: &str) -> Result<T, ChainError> {
    let mut pos = 0;
    let value = T::read(bytes, &mut pos)?;
    if pos != bytes.len() {
        return Err(ChainError::SerializationError(format!(
            "XPR native {label} has {} trailing bytes",
            bytes.len() - pos
        )));
    }
    Ok(value)
}

/// Apply an exact historical contract transition without entering Wasmer.
/// `None` means the canonical WASM path must run.
pub(super) fn try_apply(
    context: &mut ApplyContext,
    action: &Action,
    code_hash: &[u8; 32],
) -> Result<Option<u64>, ChainError> {
    if !context.xpr_native_replay_enabled() || !context.is_explicitly_billed()? {
        return Ok(None);
    }

    let applied = if *code_hash == XPR_BOT_CODE_HASH
        && context.receiver().as_u64() == BOT
        && action.account().as_u64() == BOT
        && action.name().as_u64() == PROCESS
    {
        apply_bot_process(context, action)?;
        true
    } else if *code_hash == XPR_ORACLES_CODE_HASH
        && context.receiver().as_u64() == ORACLES
        && action.account().as_u64() == ORACLES
        && action.name().as_u64() == FEED
    {
        apply_oracles_feed(context, action)?
    } else {
        false
    };
    if !applied {
        return Ok(None);
    }

    // Accepted-block migration replay uses the producer-recorded CPU bill.
    // Native work therefore contributes no synthetic WASM points here.
    Ok(Some(0))
}

fn apply_oracles_feed(context: &mut ApplyContext, action: &Action) -> Result<bool, ChainError> {
    let feed_action: FeedActionData =
        match read_exact(action.data().as_ref(), "oracles::feed action") {
            Ok(action) => action,
            Err(_) => return Ok(false),
        };
    let feed_iterator = context.db_find_i64(ORACLES, ORACLES, FEEDS, feed_action.feed_index)?;
    let data_iterator = context.db_find_i64(ORACLES, ORACLES, DATA, feed_action.feed_index)?;
    if feed_iterator < 0 || data_iterator < 0 {
        return Err(ChainError::ApplyError(if feed_iterator < 0 {
            "feed not found".into()
        } else {
            "data not found".into()
        }));
    }

    let mut packed_feed = Vec::new();
    let feed_size = context.db_get_i64(feed_iterator, &mut packed_feed, 0)?;
    context.db_get_i64(feed_iterator, &mut packed_feed, feed_size as usize)?;
    let mut feed: FeedRow = read_exact(&packed_feed, "oracles feed row")?;

    // The 40M workload uses this stable, numerically straightforward feed.
    // Any configuration or data-type change falls back to the deployed WASM.
    let supported = feed.aggregate_function == "mean"
        && feed.data_type == "double"
        && feed.config.get("data_window_size") == Some(&20)
        && feed.config.get("data_same_provider_limit") == Some(&0)
        && feed.config.get("data_freshness_sec") == Some(&0)
        && feed.config.get("min_provider_wait_sec") == Some(&0)
        && feed_action.data.d_string.is_none()
        && feed_action.data.d_uint64.is_none()
        && feed_action.data.d_double.is_some();
    if !supported {
        return Ok(false);
    }
    if !feed.providers.contains_key(&feed_action.account) {
        return Err(ChainError::ApplyError("not a registered provider".into()));
    }
    context.require_authorization(&Name::new(feed_action.account), None)?;

    let mut packed_data = Vec::new();
    let data_size = context.db_get_i64(data_iterator, &mut packed_data, 0)?;
    context.db_get_i64(data_iterator, &mut packed_data, data_size as usize)?;
    let mut data: DataRow = read_exact(&packed_data, "oracles data row")?;
    let now = context
        .pending_block_timestamp()
        .to_time_point()
        .time_since_epoch()
        .count();
    data.points.insert(
        0,
        ProviderPoint {
            provider: feed_action.account,
            time: now,
            data: feed_action.data,
        },
    );
    data.points.truncate(20);

    let mut total = 0.0;
    for point in &data.points {
        let Some(value) = point.data.d_double else {
            return Ok(false);
        };
        total += value;
    }
    data.aggregate = DataVariant {
        d_string: None,
        d_uint64: None,
        d_double: Some(total / data.points.len() as f64),
    };
    feed.providers.insert(feed_action.account, now);

    context.db_update_i64(data_iterator, &Name::new(ORACLES), data.pack()?)?;
    context.db_update_i64(feed_iterator, &Name::new(ORACLES), feed.pack()?)?;
    Ok(true)
}

fn apply_bot_process(context: &mut ApplyContext, action: &Action) -> Result<(), ChainError> {
    let process: Process = read_exact(action.data().as_ref(), "bot::process action")?;
    let transaction_id = context.get_packed_transaction().id().0.0;
    let now = context
        .pending_block_timestamp()
        .to_time_point()
        .time_since_epoch()
        .count();
    let utc_hour = ((now / 1_000_000) % 86_400 / 3_600) as u8;
    let utc_hour_to_erase = (utc_hour + 1) % 24;

    for entry in process.entries {
        let iterator = context.db_find_i64(BOT, BOT, BOTS, entry.bot_index)?;
        if iterator < 0 {
            return Err(ChainError::ApplyError("bot not found".into()));
        }
        let mut packed_row = Vec::new();
        let row_size = context.db_get_i64(iterator, &mut packed_row, 0)?;
        context.db_get_i64(iterator, &mut packed_row, row_size as usize)?;
        let mut bot: BotRow = read_exact(&packed_row, "bot row")?;
        if process.account != bot.account {
            return Err(ChainError::ApplyError("account mismatch".into()));
        }

        let feed_data = FeedActionData {
            account: process.account,
            feed_index: bot.feed_index,
            data: entry.data.clone(),
        }
        .pack()?;
        context.execute_inline(&Action::new(
            Name::new(bot.oracle_contract),
            Name::new(FEED),
            feed_data,
            vec![PermissionLevel::new(process.account, ACTIVE_NAME.as_u64())],
        ))?;

        let count = bot.tx_count_by_utc_hour.entry(utc_hour).or_insert(0);
        *count = count.wrapping_add(1);
        bot.tx_count_by_utc_hour.insert(utc_hour_to_erase, 0);
        bot.history.insert(
            0,
            Tx {
                id: Digest(transaction_id),
                time: now,
                data: entry.data,
            },
        );
        if bot.history.len() > usize::from(bot.max_history) {
            bot.history.pop();
        }
        context.db_update_i64(iterator, &Name::new(BOT), bot.pack()?)?;
    }

    Ok(())
}

#[derive(Clone, Debug, PartialEq, Read, Write, NumBytes)]
struct FeedActionData {
    account: u64,
    feed_index: u64,
    data: DataVariant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_process_payload_round_trips_byte_exactly() {
        // block 40,006,000: one bot entry carrying a double oracle value.
        let bytes =
            hex::decode("000000000040323d0103000000000000000000018fc2f5280cf3d1405427000000000000")
                .unwrap();
        let process: Process = read_exact(&bytes, "fixture").unwrap();
        assert_eq!(process.entries.len(), 1);
        assert_eq!(process.entries[0].bot_index, 3);
        assert_eq!(process.pack().unwrap(), bytes);
    }

    #[test]
    fn bot_row_codec_preserves_canonical_map_order() {
        let mut counts = CanonicalMap::new();
        counts.insert(23, 4);
        counts.insert(1, 7);
        let row = BotRow {
            index: 3,
            account: 42,
            description: "oracle".into(),
            oracle_contract: 99,
            feed_index: 8,
            tx_count_by_utc_hour: counts,
            max_history: 2,
            history: Vec::new(),
        };
        let packed = row.pack().unwrap();
        let decoded: BotRow = read_exact(&packed, "fixture").unwrap();
        assert_eq!(decoded, row);
        assert_eq!(
            decoded
                .tx_count_by_utc_hour
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 23]
        );
    }
}
