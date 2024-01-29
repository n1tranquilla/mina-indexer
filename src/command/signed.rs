use crate::{
    block::{precomputed::PrecomputedBlock, BlockHash},
    command::*,
    ledger::public_key::PublicKey,
};
use blake2::digest::VariableOutput;
use mina_serialization_types::staged_ledger_diff as mina_rs;
use serde_derive::{Deserialize, Serialize};
use std::io::Write;
use versioned::Versioned;

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignedCommand(pub mina_serialization_types::staged_ledger_diff::SignedCommandV1);

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignedCommandWithStateHash {
    pub command: SignedCommand,
    pub state_hash: BlockHash,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignedCommandWithData {
    pub command: SignedCommand,
    pub state_hash: BlockHash,
    pub status: CommandStatusData,
}

impl SignedCommand {
    pub fn fee_payer_pk(&self) -> PublicKey {
        self.payload_common().fee_payer_pk.into()
    }

    pub fn receiver_pk(&self) -> PublicKey {
        match self.payload_body() {
            mina_rs::SignedCommandPayloadBody::PaymentPayload(payment_payload) => {
                payment_payload.t.t.receiver_pk.into()
            }
            mina_rs::SignedCommandPayloadBody::StakeDelegation(delegation_payload) => {
                match delegation_payload.t {
                    mina_rs::StakeDelegation::SetDelegate {
                        delegator: _,
                        new_delegate,
                    } => new_delegate.into(),
                }
            }
        }
    }

    pub fn source_pk(&self) -> PublicKey {
        match self.payload_body() {
            mina_rs::SignedCommandPayloadBody::PaymentPayload(payment_payload) => {
                payment_payload.t.t.source_pk.into()
            }
            mina_rs::SignedCommandPayloadBody::StakeDelegation(delegation_payload) => {
                match delegation_payload.t {
                    mina_rs::StakeDelegation::SetDelegate {
                        delegator,
                        new_delegate: _,
                    } => delegator.into(),
                }
            }
        }
    }

    pub fn signer(&self) -> PublicKey {
        self.0.clone().inner().inner().signer.0.inner().into()
    }

    pub fn all_public_keys(&self) -> Vec<PublicKey> {
        vec![
            self.receiver_pk(),
            self.source_pk(),
            self.fee_payer_pk(),
            self.signer(),
        ]
    }

    pub fn contains_public_key(&self, pk: &PublicKey) -> bool {
        self.all_public_keys().contains(pk)
    }

    pub fn is_delegation(&self) -> bool {
        matches!(
            self.payload_body(),
            mina_rs::SignedCommandPayloadBody::StakeDelegation(_)
        )
    }

    pub fn payload(&self) -> &mina_rs::SignedCommandPayload {
        &self.0.t.t.payload.t.t
    }

    pub fn from_user_command(uc: UserCommandWithStatus) -> Self {
        match uc.0.inner().data.inner().inner() {
            mina_rs::UserCommand::SignedCommand(signed_command) => signed_command.into(),
        }
    }

    pub fn source_nonce(&self) -> u32 {
        self.payload_common().nonce.t.t as u32
    }

    pub fn payload_body(&self) -> mina_rs::SignedCommandPayloadBody {
        self.payload().body.clone().inner().inner()
    }

    pub fn payload_common(&self) -> mina_rs::SignedCommandPayloadCommon {
        self.payload().common.clone().inner().inner().inner()
    }

    pub fn hash_signed_command(&self) -> anyhow::Result<String> {
        let mut binprot_bytes = Vec::new();
        bin_prot::to_writer(&mut binprot_bytes, &self.0).map_err(anyhow::Error::from)?;

        let binprot_bytes_bs58 = bs58::encode(&binprot_bytes[..])
            .with_check_version(0x13)
            .into_string();
        let mut hasher = blake2::Blake2bVar::new(32).unwrap();

        hasher.write_all(binprot_bytes_bs58.as_bytes()).unwrap();

        let mut hash = hasher.finalize_boxed().to_vec();
        hash.insert(0, hash.len() as u8);
        hash.insert(0, 1);

        Ok(bs58::encode(hash).with_check_version(0x12).into_string())
    }

    pub fn from_precomputed(block: &PrecomputedBlock) -> Vec<Self> {
        block.commands().into_iter().map(Self::from).collect()
    }
}

impl SignedCommandWithStateHash {
    pub fn from(signed_cmd: &SignedCommand, state_hash: &str) -> Self {
        Self {
            command: signed_cmd.clone(),
            state_hash: state_hash.into(),
        }
    }
}

impl From<mina_rs::UserCommand> for SignedCommand {
    fn from(value: mina_rs::UserCommand) -> Self {
        let mina_rs::UserCommand::SignedCommand(v1) = value;
        Self(v1)
    }
}

impl From<mina_rs::UserCommandWithStatus> for SignedCommand {
    fn from(value: mina_rs::UserCommandWithStatus) -> Self {
        Self::from_user_command(value.into())
    }
}

impl From<UserCommandWithStatus> for SignedCommand {
    fn from(value: UserCommandWithStatus) -> Self {
        let value: mina_rs::UserCommandWithStatus = value.into();
        value.into()
    }
}

impl From<SignedCommand> for Command {
    fn from(value: SignedCommand) -> Command {
        match value.payload_body() {
            mina_rs::SignedCommandPayloadBody::PaymentPayload(payment_payload_v1) => {
                let mina_rs::PaymentPayload {
                    source_pk,
                    receiver_pk,
                    amount,
                    ..
                } = payment_payload_v1.inner().inner();
                Command::Payment(Payment {
                    source: source_pk.into(),
                    receiver: receiver_pk.into(),
                    amount: amount.inner().inner().into(),
                })
            }
            mina_rs::SignedCommandPayloadBody::StakeDelegation(stake_delegation_v1) => {
                let mina_rs::StakeDelegation::SetDelegate {
                    delegator,
                    new_delegate,
                } = stake_delegation_v1.inner();
                Command::Delegation(Delegation {
                    delegate: new_delegate.into(),
                    delegator: delegator.into(),
                })
            }
        }
    }
}

impl From<SignedCommandWithStateHash> for SignedCommand {
    fn from(value: SignedCommandWithStateHash) -> Self {
        value.command
    }
}

impl From<SignedCommandWithStateHash> for Command {
    fn from(value: SignedCommandWithStateHash) -> Self {
        value.command.into()
    }
}

impl From<SignedCommandWithStateHash> for CommandWithStateHash {
    fn from(value: SignedCommandWithStateHash) -> Self {
        Self {
            command: value.command.into(),
            state_hash: value.state_hash,
        }
    }
}

impl From<Versioned<Versioned<mina_rs::SignedCommand, 1>, 1>> for SignedCommand {
    fn from(value: Versioned<Versioned<mina_rs::SignedCommand, 1>, 1>) -> Self {
        SignedCommand(value)
    }
}

impl std::fmt::Debug for SignedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use serde_json::*;

        let mut json = Map::new();
        let mina_rs::SignedCommand { payload, .. } = self.0.clone().inner().inner();

        let mut common = Map::new();
        let mina_rs::SignedCommandPayloadCommon {
            fee,
            fee_token,
            fee_payer_pk,
            nonce,
            valid_until,
            memo,
        } = payload
            .clone()
            .inner()
            .inner()
            .common
            .inner()
            .inner()
            .inner();
        common.insert(
            "fee".into(),
            Value::Number(Number::from(fee.inner().inner())),
        );
        common.insert(
            "fee_token".into(),
            Value::Number(Number::from(fee_token.inner().inner().inner())),
        );
        common.insert(
            "fee_payer_pk".into(),
            Value::String(PublicKey::from(fee_payer_pk).to_address()),
        );
        common.insert(
            "nonce".into(),
            Value::Number(Number::from(nonce.inner().inner())),
        );
        common.insert(
            "valid_until".into(),
            Value::Number(Number::from(valid_until.inner().inner())),
        );
        common.insert(
            "memo".into(),
            Value::String(String::from_utf8_lossy(&memo.inner().0).to_string()),
        );

        let mut body = Map::new();
        match payload.inner().inner().body.inner().inner() {
            mina_rs::SignedCommandPayloadBody::PaymentPayload(payment_payload) => {
                let mina_rs::PaymentPayload {
                    source_pk,
                    receiver_pk,
                    token_id,
                    amount,
                } = payment_payload.inner().inner();

                let mut payment = Map::new();
                payment.insert(
                    "source_pk".into(),
                    Value::String(PublicKey::from(source_pk).to_address()),
                );
                payment.insert(
                    "receiver_pk".into(),
                    Value::String(PublicKey::from(receiver_pk).to_address()),
                );
                payment.insert(
                    "token_id".into(),
                    Value::Number(Number::from(token_id.inner().inner().inner())),
                );
                payment.insert(
                    "amount".into(),
                    Value::Number(Number::from(amount.inner().inner())),
                );
                body.insert("Payment".into(), Value::Object(payment));
            }
            mina_rs::SignedCommandPayloadBody::StakeDelegation(stake_delegation) => {
                let mina_rs::StakeDelegation::SetDelegate {
                    delegator,
                    new_delegate,
                } = stake_delegation.inner();

                let mut stake_delegation = Map::new();
                stake_delegation.insert(
                    "delegator".into(),
                    Value::String(PublicKey::from(delegator).to_address()),
                );
                stake_delegation.insert(
                    "new_delegate".into(),
                    Value::String(PublicKey::from(new_delegate).to_address()),
                );
                body.insert("StakeDelegation".into(), Value::Object(stake_delegation));
            }
        };

        json.insert("common".into(), Value::Object(common));
        json.insert("body".into(), Value::Object(body));
        write!(f, "{}", to_string(&json).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use crate::block::precomputed::PrecomputedBlock;
    use std::path::PathBuf;

    #[tokio::test]
    async fn transaction_hash() {
        // refer to the hashes on Minascan
        // https://minascan.io/mainnet/tx/CkpZDcqGWQVpckXjcg99hh4EzmCrnPzMM8VzHaLAYxPU5tMubuLaj
        // https://minascan.io/mainnet/tx/CkpZZsSm9hQpGkGzMi8rcsQEWPZwGJXktiqGYADNwLoBeeamhzqnX

        let block_file = PathBuf::from("./tests/data/sequential_blocks/mainnet-105489-3NK4huLvUDiL4XuCUcyrWCKynmvhqfKsx5h2MfBXVVUq2Qwzi5uT.json");
        let precomputed_block = PrecomputedBlock::parse_file(&block_file).unwrap();
        let hashes = precomputed_block.command_hashes();
        let expect = vec![
            "CkpZZsSm9hQpGkGzMi8rcsQEWPZwGJXktiqGYADNwLoBeeamhzqnX".to_string(),
            "CkpZDcqGWQVpckXjcg99hh4EzmCrnPzMM8VzHaLAYxPU5tMubuLaj".to_string(),
        ];

        assert_eq!(hashes, expect);
    }
}