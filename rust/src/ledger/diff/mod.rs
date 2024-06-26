pub mod account;

use self::account::{AccountDiff, AccountDiffType, FailedTransactionNonceDiff};
use super::{coinbase::Coinbase, LedgerHash, PublicKey};
use crate::{
    block::{precomputed::PrecomputedBlock, BlockHash},
    command::{Command, Payment, UserCommandWithStatusT},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct LedgerDiff {
    /// State hash of the block
    pub state_hash: BlockHash,

    /// Staged ledger hash of the resulting ledger
    pub staged_ledger_hash: LedgerHash,

    /// Some(pk) if the coinbase receiver account is new,
    /// else None
    pub new_coinbase_receiver: Option<PublicKey>,

    /// All pk's involved in the block
    pub public_keys_seen: Vec<PublicKey>,

    /// Map of new pk -> balance (after coinbase, before fee transfers)
    pub new_pk_balances: BTreeMap<PublicKey, u64>,

    /// Account updates
    pub account_diffs: Vec<AccountDiff>,
}

impl LedgerDiff {
    /// Compute a ledger diff from the given precomputed block
    pub fn from_precomputed(precomputed_block: &PrecomputedBlock) -> Self {
        let mut account_diff_fees = AccountDiff::from_block_fees(precomputed_block);
        // applied user commands
        let mut account_diff_txns: Vec<Command> = precomputed_block
            .commands()
            .into_iter()
            .filter(|txn| txn.is_applied())
            .map(|txn| txn.to_command())
            .filter(|cmd| match cmd {
                Command::Payment(Payment { amount, .. }) => amount.0 > 0,
                Command::Delegation(_) => true,
            })
            .collect();
        account_diff_txns.sort();

        let mut account_diff_txns: Vec<AccountDiff> = account_diff_txns
            .into_iter()
            .flat_map(AccountDiff::from_command)
            .collect();

        // failed user commands
        account_diff_txns.append(
            &mut precomputed_block
                .commands()
                .iter()
                .filter(|txn| !txn.is_applied())
                .map(|txn| {
                    AccountDiff::FailedTransactionNonce(FailedTransactionNonceDiff {
                        public_key: txn.sender(),
                        nonce: txn.nonce(),
                    })
                })
                .collect(),
        );

        // replace fee_transfer with fee_transfer_via_coinbase, if any
        let coinbase = Coinbase::from_precomputed(precomputed_block);
        if coinbase.has_fee_transfer() {
            let fee_transfer = coinbase.fee_transfer();
            let idx = account_diff_fees
                .iter()
                .enumerate()
                .position(|(n, diff)| match diff {
                    AccountDiff::FeeTransfer(fee) => {
                        *fee == fee_transfer[0]
                            && match &account_diff_fees[n + 1] {
                                AccountDiff::FeeTransfer(fee) => *fee == fee_transfer[1],
                                _ => false,
                            }
                    }
                    _ => false,
                });
            idx.iter().for_each(|i| {
                account_diff_fees[*i] =
                    AccountDiff::FeeTransferViaCoinbase(fee_transfer[0].clone());
                account_diff_fees[*i + 1] =
                    AccountDiff::FeeTransferViaCoinbase(fee_transfer[1].clone());
            });
        }

        let mut account_diffs = Vec::new();
        account_diffs.append(&mut account_diff_txns);

        if coinbase.is_coinbase_applied() {
            account_diffs.push(coinbase.as_account_diff()[0].clone());
        }
        account_diffs.append(&mut account_diff_fees);

        let accounts_created = precomputed_block.accounts_created();
        LedgerDiff {
            account_diffs,
            new_pk_balances: accounts_created.0,
            new_coinbase_receiver: accounts_created.1,
            state_hash: precomputed_block.state_hash(),
            staged_ledger_hash: precomputed_block.staged_ledger_hash(),
            public_keys_seen: precomputed_block.active_public_keys(),
        }
    }

    pub fn append(&mut self, other: Self) {
        // add public keys
        other.public_keys_seen.into_iter().for_each(|account| {
            if !self.public_keys_seen.contains(&account) {
                self.public_keys_seen.push(account);
            }
        });

        // add diff
        other.account_diffs.into_iter().for_each(|update| {
            self.account_diffs.push(update);
        });

        // update hash
        self.staged_ledger_hash = other.staged_ledger_hash;
    }

    pub fn append_vec(diffs: Vec<Self>) -> Self {
        let mut acc = Self::default();
        diffs.iter().for_each(|diff| acc.append(diff.clone()));
        acc
    }

    pub fn from(value: &[(&str, &str, AccountDiffType, u64)]) -> Vec<AccountDiff> {
        value
            .iter()
            .flat_map(|(s, r, t, a)| AccountDiff::from(s, r, t.clone(), *a))
            .collect()
    }
}

impl std::fmt::Debug for LedgerDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== LedgerDiff ===")?;
        for account_diff in &self.account_diffs {
            writeln!(f, "{account_diff:?}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        block::precomputed::{PcbVersion, PrecomputedBlock},
        ledger::diff::LedgerDiff,
    };
    use std::path::PathBuf;

    #[test]
    fn fees_from_precomputed_111() -> anyhow::Result<()> {
        use crate::ledger::diff::account::AccountDiffType::*;

        let path = PathBuf::from("./tests/data/non_sequential_blocks/mainnet-111-3NL33j16AWm3Jhjj1Ud25E54hu7HpUq4WBQcAiijEKMfXqwFJwzK.json");
        let block = PrecomputedBlock::parse_file(&path, PcbVersion::V1)?;
        let ledger_diff = LedgerDiff::from_precomputed(&block);
        let expected = LedgerDiff::from(&[
            (
                "B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy",
                "B62qjYanmV7y9njVeH5UHkz3GYBm7xKir1rAnoY4KsEYUGLMiU45FSM",
                Payment(164),
                1000,
            ),
            (
                "B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy",
                "B62qjYanmV7y9njVeH5UHkz3GYBm7xKir1rAnoY4KsEYUGLMiU45FSM",
                Payment(165),
                1000,
            ),
            (
                "B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy",
                "B62qjYanmV7y9njVeH5UHkz3GYBm7xKir1rAnoY4KsEYUGLMiU45FSM",
                Payment(166),
                1000,
            ),
            (
                "B62qoaMj7u1JzuqXaBByQBL5jzqLguK8e7LHVPdY9LcvvLXK7HPsusD",
                "",
                Coinbase,
                720000000000,
            ),
            (
                "B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy",
                "B62qoaMj7u1JzuqXaBByQBL5jzqLguK8e7LHVPdY9LcvvLXK7HPsusD",
                FeeTransfer,
                10000000,
            ),
            (
                "B62qre3erTHfzQckNuibViWQGyyKwZseztqrjPZBv6SQF384Rg6ESAy",
                "B62qoaMj7u1JzuqXaBByQBL5jzqLguK8e7LHVPdY9LcvvLXK7HPsusD",
                FeeTransfer,
                20000000,
            ),
        ]);

        assert_eq!(ledger_diff.account_diffs, expected);
        Ok(())
    }

    #[test]
    fn fees_from_precomputed_320081() -> anyhow::Result<()> {
        use crate::ledger::diff::account::AccountDiffType::*;

        let path = PathBuf::from("./tests/data/non_sequential_blocks/mainnet-320081-3NK3bLM3eMyCum34ovAGCUw2GWUqDxkNwiti8XtKBYrocinp8oZM.json");
        let block = PrecomputedBlock::parse_file(&path, PcbVersion::V1)?;
        let mut ledger_diff = LedgerDiff::from_precomputed(&block);
        let mut expected = LedgerDiff::from(&[
            (
                "B62qjBMMMbvj17vc5n6y7839mJr28QLLx8RC3QpKLDbsagtTgQA5sAW",
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                Payment(4),
                9900000000,
            ),
            (
                "B62qjSrS8AvFXHT98buTFFxysXfifxp8wfecZQVLdT4cmP8BWDyqvPU",
                "B62qkRodi7nj6W1geB12UuW2XAx2yidWZCcDthJvkf9G4A6G5GFasVQ",
                Payment(3),
                59950000000,
            ),
            (
                "B62qjUFeTbJpW4LkRrawvkbjSeA3iMmtX53tA6HxhgUHquAAEum9W5b",
                "B62qp9vk2jHCqKotH8j9kJeHL56tB2n2bfMMiyqn5RAqwLLugMU53jG",
                Payment(0),
                563303000000,
            ),
            (
                "B62qk7JnTyMBipxKGiM4juN5by7NXiVRnw28TiQHaG7ahJgN9qc9cr4",
                "B62qp9vk2jHCqKotH8j9kJeHL56tB2n2bfMMiyqn5RAqwLLugMU53jG",
                Payment(184),
                1438400000000,
            ),
            (
                "B62qkRodi7nj6W1geB12UuW2XAx2yidWZCcDthJvkf9G4A6G5GFasVQ",
                "B62qihdMVfrUCKRnSFLz7YunnsnfhLR5qjrhDpAftMDWK5uoS3XQz4w",
                Payment(27034),
                1155820070000,
            ),
            (
                "B62qkgmZE4WZWPAWvyJM6RfH3wF4unVP2jHNxneDufgUq7JouKgH5G3",
                "B62qqLjG8qFtbXWStm4tdWrcdqgQ7HYkcQEzPRXCoTziR7Gd4fjrMa2",
                Payment(16),
                1002000000000,
            ),
            (
                "B62qnEeb4KAp9WxdMxddHVtJ8gwfyJURG5BZZ6e4LsRjQKHNWqmgSWt",
                "B62qq6PqndihT5uoGAXzndoNgYSUMvUPmVqMQATusaoS1ZmCZRcM1ku",
                Payment(174178),
                70000000,
            ),
            (
                "B62qnP8WVALtU6kmazMcNgrnCMVroQkPGUHNvGGA6rVCMTRZFDLvshR",
                "B62qp9vk2jHCqKotH8j9kJeHL56tB2n2bfMMiyqn5RAqwLLugMU53jG",
                Payment(8),
                6999800000000,
            ),
            (
                "B62qnPGoYZdQcjjDhadZrM1SUL1EjCxoEXaby7hmkqkeNrpwpWsBo1E",
                "B62qmsHz2vjanLj3AUdBxwjRjNB5nFvPAAeBMwBU3ZNRGZeAKQvrB9n",
                Payment(1),
                10000000000,
            ),
            (
                "B62qnXy1f75qq8c6HS2Am88Gk6UyvTHK3iSYh4Hb3nD6DS2eS6wZ4or",
                "B62qqJ1AqK3YQmEEALdJeMw49438Sh6zuQ5cNWUYfCgRsPkduFE2uLU",
                Payment(190280),
                90486110,
            ),
            (
                "B62qntsJ1p1ECs3jLoBByBHkt74G8VM4Q5Uv82e1xa2NtUBbwdUpJR9",
                "B62qjt1rDfVjGX6opVnLpshRigH5U6UFjyMNYdWCjo99im2v7VrzqF6",
                Payment(264),
                13301123000000,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpH18BnG6GZ3BNgdd9oVwwVPrRjK9tn8kA3VfH8oxWp9Kn8waC9R",
                Payment(391242),
                4711164859,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqsF2yXzetcugSP1hJeWJxxLFpAtHDrPd4qoSxp3vh1BZMLy3rxr",
                Payment(391243),
                4703371436,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmK64Db88niy625DhF5v9K58eqZk5Gn9PxdVnCaM2iyre4rxUUdi",
                Payment(391244),
                4695198157,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qn1yW5z1zmbmXxYx7TyT7VswAa1FprGw6wy8CPrhc1q3NKq8mTJ4",
                Payment(391245),
                4664570969,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqkz8cqaYFJgTFBe29wUjai7jkxb2oM7qEngphj6joPZSxvb338x",
                Payment(391246),
                4659843231,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qr4ks2n5oVg7CTKxyu6mwaLGk2wKyeXxtDdfpd2R23DuSyKQWzy8",
                Payment(391247),
                4656134210,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmFjbjdVc5hrpGZRTamU7LFVq7kcoQqxzp51xUCAUXo84rahg9xW",
                Payment(391248),
                4643216054,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qr7EMRgMbcbTCDavX8eMWkP5huL1aQytVgSnjjWsenB8v2QhpG3B",
                Payment(391249),
                4639193740,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqVKw7K8fVExUavQB59W7VKkmsAz9Uf8tjgZZDobCkTFWxTDvCzn",
                Payment(391250),
                4635443101,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmMVRsryoNvqiGx2jMqfyaNNRjj8JnD3AchpfGnT3rA8Tb6JfQHv",
                Payment(391251),
                4630288603,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpwM5wCk71gn4e2R8RwDCqzggnmTPPfu42BHkPZayQFftQhGY7xb",
                Payment(391252),
                4628847695,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkW1BaZsUBGR6d2bGjiDutGFTD2u8KiBMDMiwGXTQvAgWrGmsEaQ",
                Payment(391253),
                4623449093,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrbBLCDDFvhUWfbvTjwKb8uuwEDeMgKK5f7uobHaJfRTeeQ3ihEZ",
                Payment(391254),
                4614989416,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrboa8YMSgdqZrQCNteBKUKvXNtpnwtZuXdH4etA5btAFD7DqpYu",
                Payment(391255),
                4609832969,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmw9CgT41jB9KJMKXspwqVVyE71dYE9xQpuDnGAE6sD9cc5b8vZj",
                Payment(391256),
                4591950732,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qncXxNL1Dkb63rduekAc9wvRnCa6FhTQiLbneSvgaPexox7ERG6M",
                Payment(391257),
                4585795783,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpiE6o8stMDFd4TiccziGs65MsczsnfyFzJpVM4XCGEkvfmJvADU",
                Payment(391258),
                4583294440,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkezQcfJ7YB8m8tvcMPaGYUGdMsw6Lrnx9FCou9Myjuv4ZB2Ng5Q",
                Payment(391259),
                4583278220,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qnp7P7r7EvH2kxJtKwu1Uk67k6v3VjgZrzto6equCkd5rW98CYLh",
                Payment(391260),
                4573996694,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpMiHoZLGBuQAYXaaUDds5DQpVbZNHsxm1QzZ9NkqDfvCtwnNKxt",
                Payment(391261),
                4562860142,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmTQZLWoeQq4EVfDecEwe22iXTU2gYUHfSNVoNuCfF7F8AbaBKmE",
                Payment(391262),
                4531242244,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrwgzwAN5hJ6aEnt3U72bM5LWvVd2o5XuT6KvTAXxUdc9dUd8bWG",
                Payment(391263),
                4527758305,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrs1u8v3Nvwi8gaPJc38xzXDnKCBcCZURQSSiegcvabYZUocSkvm",
                Payment(391264),
                4516695405,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qk427qNAYDMna7bPHtjoeBvG7EqAotULixqwNHKjNBTTpS6L8f4F",
                Payment(391265),
                4514639065,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpwDP4vhPBkyxFfm7Ro2JoD4idQ7RrFBYwSgDBzsKHjDSwkxpEu7",
                Payment(391266),
                4511768855,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qjWiqmTNbFny7t3RY5qW8WightKFxFKbofxiSQjPxtuCQKkYJu7J",
                Payment(391267),
                4507587516,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qnF9nzupzRDYyCgpC4NpMotn8e98yXajidaWeAEhr5LCZuccRNZN",
                Payment(391268),
                4499898791,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qoNXZ6oMjRm8FZQraKTYjDVTT6jgBDEnxGZMtupB2ZKxrTsUFzGw",
                Payment(391269),
                4498834297,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqYZ7iU7yYmnc2qAp8PJQ4oSA3MTspA5UJeTiDnWSH5tLdowhRJ1",
                Payment(391270),
                4486432607,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qnyQZjm4AFP3FMXbd3K5FnWFqWYDCkJxPAasrjVix79GisEf8ddC",
                Payment(391271),
                4474016012,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrqD542ZHazeF5k7iCfsVnXenjY39i2jvvepVDxx46NKBzANaMSB",
                Payment(391272),
                4448138789,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qm4xfV1gvYqbecBWWWQLE1HufiKyUMFd8u6YBie1Cqswh8yihfVU",
                Payment(391273),
                4437747358,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmUvzgG7ZSkUoWg7QZcXGJ6kRUNdWMBKuf4LMYupwkqzB8D1HywT",
                Payment(391274),
                4433958048,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpHkUNVnk32WLtEdgQk4DRMRtxdvUpccnMGjjyAWhfnov6SjoKAr",
                Payment(391275),
                4429673334,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qq2UpXD62qbTuihmoNV9N6nPHtCpC4crrWisraDRbBk996NPXM37",
                Payment(391276),
                4419299792,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpGfKha2nUjJ4wdQcj2esKdidbf2qbLjPEAcg6EoiCo4C3kG4DE3",
                Payment(391277),
                4418175211,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qjxSWcBEwQpyoqqgdRHTNk6K45RrMia8ibviXGJER28NJJ1q65Md",
                Payment(391278),
                4409329389,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmZt291j21iHYSU8gftqeJdN7hmrHkz3SJqhusxwZ4nRHnaGdFhb",
                Payment(391279),
                4397743946,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrSQ8T9Mri2Wd2Hbzf4RBcYQNrf7JK59TgiRYKMBZidDaN1KeQJj",
                Payment(391280),
                4393892622,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qik3mgrdsqpZkEaq3sDq6yayEV2omk2LKLQJxtVdyHf43Miudcei",
                Payment(391281),
                4393085065,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqgixHEp5CJe8XBgV7ByGcYdM8BPp3DGqY6o1z7H3QC6rW3SMjW6",
                Payment(391282),
                4392541499,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qp87txNgn2MdzxVWHA4nKccibcExgxf7DBPK51zCkMJ6WkMszK9d",
                Payment(391283),
                4392265401,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qjs17nfUPro651kzABbtLXN3wHromNvxiYSWMTbZcZfnXviAGJWp",
                Payment(391284),
                4389060537,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qk2VNkSziPszThGVbMhuV3KJ1pFujdyLPanvU2DA8k7VpqFqrf9e",
                Payment(391285),
                4384928224,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qiYTzDYwMZjgKe86eMi6QWdaTgcS3SetR6qh53vJtrcxYVNAsFqM",
                Payment(391286),
                4378428734,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qp556HEFqpHQj9GYPVXqfigXEJvNzaUxToEdHVsd8qszhBCK6ArT",
                Payment(391287),
                4376045448,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrpaDAWDAgZThmxEp2ZdHkTTRFQ2sYT9eyUMJwhok9x199AQFYbE",
                Payment(391288),
                4374266754,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qoj63kTfzAKQwSjADUmEUZahPrbmnxSdMHaBkvZy9HwrAKD1KVQc",
                Payment(391289),
                4367671289,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qiosCfJAgH36jytY9h9M8E3XmkHeYq7gQ4XCuhsYUHNtGWGFAbaH",
                Payment(391290),
                4363979657,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qk85o2X6KitLcvQQtjaDsgUtSjfY3u2FH47Pyjsn6cTtxXkVAMvp",
                Payment(391291),
                4363049592,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrosYDK6Lc25rG19MuvMpFQg6QNC9e4zxBUHLAoaEXjCdcbWrGbj",
                Payment(391292),
                4361542001,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpxbZ4itH5sruLsQmDvQ8ZxJBi11pBgTmKFN2ML5QmsjWTztHB4B",
                Payment(391293),
                4360436930,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpMnVFEDCfs5R6yKSyVzrKKRqJJeC2sxHe3F3pYpVgeZmi1xG6vp",
                Payment(391294),
                4358544759,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qpxsJrMom5zLWuVLb1kQbJZopdanSqgb95gZZnnk4xEeotEgcXom",
                Payment(391295),
                4355028243,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrbvhUwaU49SJ5nSB1t6d3kPuzPhXwYMbwE4KunZiXSATWc5fX1j",
                Payment(391296),
                4341027163,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qnB45GwcwaLfQc6vKRchGEdQLtTefrSRq2fRtt7t9E9Jj3v7M1m4",
                Payment(391297),
                4339014502,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qoabGf7Gf9N9kCVT13diUYJbQBQYJnmeD1wJ2fnhd3AMgUr1ea1z",
                Payment(391298),
                4338626567,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkLUEDftjpH5VKvewWoXnUUYex5EeJuZAhwQQzxaLPiMYBonsyWb",
                Payment(391299),
                4333486550,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qoKQ9TPjhjemqHK7knTD74HE5Ka65ukJdmZ7eGxnpbF5ozktb5tq",
                Payment(391300),
                4324586732,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qiYkNwECbdPB5rufamCoxa2Bb2Lbz9ipPqe4N2i3Lop5iyxYiGVG",
                Payment(391301),
                4314213462,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qp6xgKBFqYC5xqToo6W1fj7Tr1pedDmH4wD6ojQnRxRerkvR4ct9",
                Payment(391302),
                4289292603,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qp2WHtKAp2XUrV2qDUtpoWqSWXtcR5DLmpPpg5uFcGgnvswQqKUM",
                Payment(391303),
                4278767633,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqm9q54eGwpKpPaPHx29BVzTia8gs1ognBKg7SsW1SfkRMzm9exR",
                Payment(391304),
                4276172359,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qqEB7vSxuq4xGJfondD5QZrc4KYYjSNmRkvVcfQPj3vYouThXTRJ",
                Payment(391305),
                4262927844,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qq57815YGLQqGeJAN9vrqsC29sSPWBXx1L3QZ7jTkbY78eEBSKZi",
                Payment(391306),
                4253643608,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkLAMqaoW5g1aDWxMNkWmZRCNtWzSRTmpxYHZMi1iMqAHL4WyLXh",
                Payment(391307),
                4244131192,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkcfQ7URzcGNyNYPxGjtx8HK3BbuNKtyBRcgsVKwLfdzodc2G2Nw",
                Payment(391308),
                4240992225,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qmxTAnCJySfgSNPBou1c6KpfuGcyJ6jn6LXVHCzhTbND6zcPG7MT",
                Payment(391309),
                4232082942,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qrcizHRp9189kr88vUBaANPwbFw9bwf9jDaSeAgNU9HtiP1DGoMZ",
                Payment(391310),
                4225262058,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkseUSt8qLLYxJdKF4y9yoCsRmaMt1C7bMr9tDzE9Rahaj5jnCyx",
                Payment(391311),
                4223743080,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qnGawm5Jnn23KEbtqmSRGHfXCRg7qscE3YMYEYrK4EaGeX3AnNep",
                Payment(391312),
                4217007829,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qjAUJSQeY3ot9SDHQtmVPSdoz5EPAGQpTgNpoZzCVV9AK5WNdt5e",
                Payment(391313),
                4209807378,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qizEvrYJeK6v5iXCpkvViKAUVpdwwbQ3vx8jkYoD9taUNnFtCxnd",
                Payment(35904),
                251100000000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qoo9t8gRqZYP8dxjBVRtzZNZ5MMAwBLKxKj9Bfwo2HRutTkJebnR",
                Payment(35905),
                251100000000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qjG3yXAR2wqG73ANHsNyFhQLMQyvHqaYMKTuuFnUYa7aNTNQkTh5",
                Payment(35906),
                104100000000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qmde9CNS62zrfyiGXfyZjfig6QtRVpi2uVLR2Az7NVXnqX9S35os",
                Payment(35907),
                78834800000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qkAisarqupqnLi2KiboiWenxwtGPQ19uNWvq3bBXen6J5tJNhZH6",
                Payment(35908),
                499695400000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qqFKe5UH7VCUp6LPu6Y6kkGtgrgKsx5GHtYmbXdJdVfUwi3Nnik1",
                Payment(35909),
                243954900000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qmHMtPATE8gmDedhuG19chsB1bKy5GQUtTZFupBm6768mCcYHBB9",
                Payment(35910),
                24127100000000,
            ),
            (
                "B62qov9yv8TayLteD6SDXvxyYtmn3KkUoozAbs47fVo9JZSpcynbzTz",
                "B62qpV4EsWwwaoQo9PaVVxk7RNPopWDEd3u4hZZgd83gXCPcuoDBrEz",
                Payment(140184),
                70000000,
            ),
            (
                "B62qp69bsgUNySCY2wEYDCrRN3gdMB6cDSZGBucTzc9vUUH4jUoDSED",
                "B62qnJ3zFub6A17fbHzcixWZbV9a99qdeFfQnQwZABH37NtraiUR2gv",
                Payment(188264),
                90486110,
            ),
            (
                "B62qpGpM8mK1cSPn1NzKpkTLaUK2dpx27Jf2bsEsJ6hVKY6ThHhTZJV",
                "B62qp9vk2jHCqKotH8j9kJeHL56tB2n2bfMMiyqn5RAqwLLugMU53jG",
                Payment(30),
                8985070000000,
            ),
            (
                "B62qpLST3UC1rpVT6SHfB7wqW2iQgiopFAGfrcovPgLjgfpDUN2LLeg",
                "B62qkiF5CTjeiuV1HSx4SpEytjiCptApsvmjiHHqkb1xpAgVuZTtR14",
                Payment(205034),
                90000000,
            ),
            (
                "B62qpWaQoQoPL5AGta7Hz2DgJ9CJonpunjzCGTdw8KiCCD1hX8fNHuR",
                "B62qmJWjC9V7QxQ8NM9bfo6MeMgNKoUgV3ghkSmBXHF9AygsUeGsgXE",
                Payment(38570),
                300280000000,
            ),
            (
                "B62qrAWZFqvgJbfU95t1owLAMKtsDTAGgSZzsBJYUzeQZ7dQNMmG5vw",
                "B62qnFCUtCu4bHJZGroNZvmq8ya1E9kAJkQGYnETh9E3CMHV98UvrPZ",
                Payment(246882),
                70000000,
            ),
            (
                "B62qrDtZh2prv8NEUgmW376K6U2u7rtpWGar2MaQzroEcL9i69xLfbw",
                "B62qkfvEZEUSaQKGKgx6ZH8dn35rafvwBYM4D33NMkGCwgahS1JaoLs",
                Payment(5198),
                422871600000,
            ),
            (
                "B62qq3TQ8AP7MFYPVtMx5tZGF3kWLJukfwG1A1RGvaBW1jfTPTkDBW6",
                "B62qjbA7potJQDh7QP1x9TaBBgKZHVUWDyvNoqRt5FS1FvSLernEued",
                Delegation(0),
                0,
            ),
            (
                "B62qq3TQ8AP7MFYPVtMx5tZGF3kWLJukfwG1A1RGvaBW1jfTPTkDBW6",
                "B62qqRqD7TqHE6owbcwutqgeSMhuY7rWXoDaMTuyEabPDR3oZyCXria",
                Delegation(2),
                0,
            ),
            (
                "B62qrQiw9JhUumq457sMxicgQ94Z1WD9JChzJu19kBE8Szb5T8tcUAC",
                "B62qr7RA6AW891n9vKifWvyVTngprLLqFpogMTA4uB8iFGq9nR4dMUF",
                Delegation(0),
                0,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "",
                Coinbase,
                1440000000000,
            ),
            (
                "B62qnXy1f75qq8c6HS2Am88Gk6UyvTHK3iSYh4Hb3nD6DS2eS6wZ4or",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                5000001,
            ),
            (
                "B62qjbA7potJQDh7QP1x9TaBBgKZHVUWDyvNoqRt5FS1FvSLernEued",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                10100000,
            ),
            (
                "B62qp69bsgUNySCY2wEYDCrRN3gdMB6cDSZGBucTzc9vUUH4jUoDSED",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                5000001,
            ),
            (
                "B62qjUFeTbJpW4LkRrawvkbjSeA3iMmtX53tA6HxhgUHquAAEum9W5b",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                200000000,
            ),
            (
                "B62qntsJ1p1ECs3jLoBByBHkt74G8VM4Q5Uv82e1xa2NtUBbwdUpJR9",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                250000000,
            ),
            (
                "B62qkRodi7nj6W1geB12UuW2XAx2yidWZCcDthJvkf9G4A6G5GFasVQ",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                50000000,
            ),
            (
                "B62qov9yv8TayLteD6SDXvxyYtmn3KkUoozAbs47fVo9JZSpcynbzTz",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                25486100,
            ),
            (
                "B62qjSrS8AvFXHT98buTFFxysXfifxp8wfecZQVLdT4cmP8BWDyqvPU",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                50000000,
            ),
            (
                "B62qk7JnTyMBipxKGiM4juN5by7NXiVRnw28TiQHaG7ahJgN9qc9cr4",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                200000000,
            ),
            (
                "B62qrAWZFqvgJbfU95t1owLAMKtsDTAGgSZzsBJYUzeQZ7dQNMmG5vw",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                25486100,
            ),
            (
                "B62qnPGoYZdQcjjDhadZrM1SUL1EjCxoEXaby7hmkqkeNrpwpWsBo1E",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                10100000,
            ),
            (
                "B62qjBMMMbvj17vc5n6y7839mJr28QLLx8RC3QpKLDbsagtTgQA5sAW",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                100000000,
            ),
            (
                "B62qoXQhp63oNsLSN9Dy7wcF3PzLmdBnnin2rTnNWLbpgF7diABciU6",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                95486111,
            ),
            (
                "B62qnEeb4KAp9WxdMxddHVtJ8gwfyJURG5BZZ6e4LsRjQKHNWqmgSWt",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                25486100,
            ),
            (
                "B62qpWaQoQoPL5AGta7Hz2DgJ9CJonpunjzCGTdw8KiCCD1hX8fNHuR",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                500000000,
            ),
            (
                "B62qpGpM8mK1cSPn1NzKpkTLaUK2dpx27Jf2bsEsJ6hVKY6ThHhTZJV",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                200000000,
            ),
            (
                "B62qnP8WVALtU6kmazMcNgrnCMVroQkPGUHNvGGA6rVCMTRZFDLvshR",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                200000000,
            ),
            (
                "B62qkgmZE4WZWPAWvyJM6RfH3wF4unVP2jHNxneDufgUq7JouKgH5G3",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                10100000,
            ),
            (
                "B62qpLST3UC1rpVT6SHfB7wqW2iQgiopFAGfrcovPgLjgfpDUN2LLeg",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                5486111,
            ),
            (
                "B62qrDtZh2prv8NEUgmW376K6U2u7rtpWGar2MaQzroEcL9i69xLfbw",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                250000000,
            ),
            (
                "B62qr7RA6AW891n9vKifWvyVTngprLLqFpogMTA4uB8iFGq9nR4dMUF",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                10100000,
            ),
            (
                "B62qospDjUj43x2yMKiNehojWWRUsE1wpdUDVpfxH8V3n5Y1QgJKFfw",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                360000000,
            ),
            (
                "B62qqRqD7TqHE6owbcwutqgeSMhuY7rWXoDaMTuyEabPDR3oZyCXria",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                10100000,
            ),
            (
                "B62qouNvgzGaA3fe6G9mKtktCfsEinqj27eqTSvDu4jSKReDEx7A8Vx",
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                FeeTransfer,
                2100000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qr3qCQ5XeTCrhy1FCU8FgHnuNDdfvJhq9aaSVA5KBSns2Vb9xsZf",
                FeeTransfer,
                1999740,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qnrr3cKh7uDPNFxAsnJR6BGk2ufsG1KeY5cVyKuiHnPjaZ9uEpef",
                FeeTransfer,
                400000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qoiEyq2QHR8m3sw9eLdJxZzA5ttZ8C4EYfRs8uyE4Gc7Bi5rY1iA",
                FeeTransfer,
                1000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qmwvcwk2vFwAA4DUtRE5QtPDnhJgNQUwxiZmidiqm6QK63v82vKP",
                FeeTransfer,
                250000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qiwCoe7sqkp7Y2kLyw29LxXVbyyDh8rLar3EHmYbyfmgyoNiv8C6",
                FeeTransfer,
                21000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qju6zexNSobvnqjr2Z3xZHQGDicEunBNvTJNbWqmUbiqqLQEzrfB",
                FeeTransfer,
                16000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qpsyB3gCndt8sNz4GRwusBtg9U72TNiL4mxmcQfWKZ5noa9fFnWr",
                FeeTransfer,
                45187695,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qnM71LjMchDsRgWinBXyNrXR8smf9NXoJZnQrTXe74DrEQoaUStb",
                FeeTransfer,
                10000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qqv8p3QdZVTVjYsyc6sJfxBAGmhQ8PZfeup3CYgFTeNMgMHdDpYv",
                FeeTransfer,
                9500000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qrmjLNrAjq9S3pMgiu2x7auofmq3BSEvyyfAR1MwVChQc38EHgs2",
                FeeTransfer,
                15399930,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qm6DVpmVNaRHjc2tpfZJKtPELSz9v82q3E5DV5FqhdNxcsBrkWSc",
                FeeTransfer,
                9000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qkcHAv5hwUEdURLfr97qqHKnB5vpW1Fy4iSKHCsSQydHzkAAyEgR",
                FeeTransfer,
                7800000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qosqzHi58Czax2RXfqPhMDzLogBeDVzSpsRDTCN1xeYUfrVy2F8P",
                FeeTransferViaCoinbase,
                10000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qo9HFmbMYZyXoeVQm1fRe4R1enAQ4nrC32zEVcFNwwhjfWSKsixc",
                FeeTransfer,
                9150000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qp5dXkkj3TkkfPos35rNYxVTKTbm5CqigfgKppA5E7GQHK7H3nNd",
                FeeTransfer,
                8888888,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qjzLBwZgmoyfBtM89U953J76SYxFQh3nzGknfrfexYRfeDje2o2v",
                FeeTransfer,
                36000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qkoe8LtiRw7JEusUSA5P1tFZNfBu6mMWT87h4F3NswcMP5BfS6Vo",
                FeeTransfer,
                29988000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qjQ3k78nzaePyXhg298UEVnwbCeqQUcNwZRSR4VK1gVJ6mer6M8V",
                FeeTransfer,
                36000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qohnEDTKat5gVkDjUoRJHdiQPcrMxLDfQccCB5e6wC9daxuFzX27",
                FeeTransfer,
                7000000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qrB4hLHkwUz3UXwx6jLx6XrvbRae4d8t6pMVaGhjt2c1XoqJZTUb",
                FeeTransfer,
                18990000,
            ),
            (
                "B62qkMUJyt7LmPnfu8in6qshaQSvTgLgNjx6h7YySRJ28wJegJ82n6u",
                "B62qpUS44ENkEKgpjcx4jpckg989UJp7xCHkin6GDAY5Y9iNPD1Syic",
                FeeTransfer,
                33947755,
            ),
        ]);

        ledger_diff.account_diffs.sort();
        expected.sort();

        assert_eq!(ledger_diff.account_diffs, expected);
        Ok(())
    }
}
