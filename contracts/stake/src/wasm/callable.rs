// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) DUSK NETWORK. All rights reserved.

use crate::stake::{Counter, Key, Stake, StakeContract};
use alloc::vec::Vec;
use canonical::Store;
use dusk_bls12_381::BlsScalar;
use dusk_bls12_381_sign::{Signature, APK};
use dusk_bytes::Serializable;
use phoenix_core::Note;
use transfer_contract::Call as TransferCall;

/// TODO: Still waiting for values from the research side.
/// t_m in the specs
const MATURITY_PERIOD: u64 = 0;
/// t_b in the specs
const EXPIRATION_PERIOD: u64 = 250_000;
/// t_c in the specs
const COOLDOWN_PERIOD: u64 = 0;
/// Minimum amount you're allowed to stake
/// 10,000 DUSK (taking into account 10 decimals)
const MINIMUM_STAKE: u64 = 100_000_000_000_000;
/// Maximum amount you're allowed to stake
/// 1,000,000 DUSK (taking into account 10 decimals)
const MAXIMUM_STAKE: u64 = 10_000_000_000_000_000;

extern "C" {
    fn verify_bls_sig(pk: &u8, sig: &u8, msg: &u8) -> i32;
}

impl<S: Store> StakeContract<S> {
    pub fn stake(
        &mut self,
        value: u64,
        public_key: APK,
        spending_proof: Vec<u8>,
    ) -> (Counter, bool) {
        if value > MAXIMUM_STAKE || value < MINIMUM_STAKE {
            return (Counter::default(), false);
        }

        // Compute maturity & expiration periods
        let eligibility = dusk_abi::block_height() + MATURITY_PERIOD;
        let expiration =
            dusk_abi::block_height() + MATURITY_PERIOD + EXPIRATION_PERIOD;
        // Generate the Stake instance
        let stake = Stake {
            value,
            pk: public_key,
            eligibility,
            expiration,
        };

        let w_i = self.counter.clone();
        let k = Key {
            pk: public_key,
            w_i: w_i.clone(),
        };

        // We should never encounter a duplicate key, so we return false if
        // there is one.
        match self
            .stake_identifier_set
            .insert(self.counter.clone(), k.clone())
        {
            Ok(None) => {}
            _ => {
                return (w_i, false);
            }
        }

        // We should also never encounter a duplicate key in this mapping, so
        // again, we return false if there is one.
        match self.stake_mapping.insert(k, stake) {
            Ok(None) => {}
            _ => {
                return (w_i, false);
            }
        }

        self.counter.increment();

        let transaction = match TransferCall::send_to_contract_transparent(
            dusk_abi::caller(),
            value,
            spending_proof,
        ) {
            Ok(t) => t,
            _ => return (w_i, false),
        };

        (
            w_i,
            dusk_abi::transact_raw(&self.transfer_contract, &transaction)?,
        )
    }

    pub fn extend_stake(
        &mut self,
        w_i: Counter,
        pk: APK,
        sig: Signature,
    ) -> bool {
        // Verify the signature by getting `t_e` from the Stake and calling the
        // VERIFY_SIG host fn.
        let k = Key { pk, w_i };
        let mut stake: Stake;

        match self.stake_mapping.get(&k) {
            Ok(Some(s)) => stake = *s,
            _ => {
                return false;
            }
        }

        let t_e = stake.expiration.clone();
        let msg_bytes = BlsScalar::from(t_e).to_bytes();
        let pk_bytes = pk.to_bytes();
        let sig_bytes = sig.to_bytes();

        // Verify BLS sig.
        let res = unsafe {
            verify_bls_sig(&pk_bytes[0], &sig_bytes[0], &msg_bytes[0])
        };

        if res == 0i32 {
            return false;
        }

        // Assuming now that the result of the verification is true, we now
        // should update the expiration of the Bid by
        // `EXPIRATION_PERIOD`
        stake.expiration += EXPIRATION_PERIOD;
        match self.stake_mapping.insert(k, stake) {
            Ok(Some(_)) => true,
            _ => false,
        }
    }

    pub fn withdraw_stake(
        &mut self,
        w_i: Counter,
        pk: APK,
        sig: Signature,
        note: Note,
    ) -> bool {
        let k = Key { pk, w_i };
        let stake: Stake;

        match self.stake_mapping.get(&k) {
            Ok(Some(s)) => stake = *s,
            _ => {
                return false;
            }
        }

        let t_e = stake.expiration.clone();

        // Make sure that the stake has expired.
        if t_e >= dusk_abi::block_height() + COOLDOWN_PERIOD as u64 {
            return false;
        }

        let msg_bytes = BlsScalar::from(t_e).to_bytes();
        let pk_bytes = pk.to_bytes();
        let sig_bytes = sig.to_bytes();

        // Verify BLS sig.
        let res = unsafe {
            verify_bls_sig(&pk_bytes[0], &sig_bytes[0], &msg_bytes[0])
        };

        if res == 0i32 {
            return false;
        }

        match self.stake_mapping.remove(&k) {
            Ok(Some(_)) => true,
            _ => false,
        }

        // Withdraw note
        let transaction = TransferCall::withdraw_from_contract_transparent(
            dusk_abi::caller(),
            note,
        )?;
        dusk_abi::transact_raw(&self.transfer_contract, &transaction)?
    }

    pub fn slash(
        &mut self,
        pk: APK,
        _round: u64,
        _step: u8,
        message_1: BlsScalar,
        message_2: BlsScalar,
        signature_1: Signature,
        signature_2: Signature,
        note: Note,
    ) -> bool {
        if message_1 == message_2 {
            return false;
        }

        let pk_bytes = pk.to_bytes();
        let sig_bytes = signature_1.to_bytes();

        // Verify first BLS sig.
        let res = unsafe {
            verify_bls_sig(
                &pk_bytes[0],
                &sig_bytes[0],
                &message_1.to_bytes()[0],
            )
        };

        if res == 0i32 {
            return false;
        }

        let pk_bytes = pk.to_bytes();
        let sig_bytes = signature_2.to_bytes();

        // Verify second BLS sig.
        let res = unsafe {
            verify_bls_sig(
                &pk_bytes[0],
                &sig_bytes[0],
                &message_2.to_bytes()[0],
            )
        };

        if res == 0i32 {
            return false;
        }

        if let Ok(v) = note.value(None) {
            if v != 5000e10 {
                return false;
            }

            let transaction = TransferCall::withdraw_from_contract_transparent(
                dusk_abi::caller(),
                note,
            )?;

            if !dusk_abi::transact_raw(&self.transfer_contract, &transaction)? {
                return false;
            }

            let stake = self.find_stake(pk);

            let dest = match dusk_abi::block_height() {
                v if v < 6311520 => self.arbitration_contract,
                _ => ContractId::default(),
            };

            let transaction =
                TransferCall::withdraw_from_transparent_to_contract(
                    stake.value - 5000e10,
                    dest,
                    stake.value - 5000e10,
                )?;

            if !dusk_abi::transact_raw(&self.transfer_contract, &transaction)? {
                return false;
            }
        }

        true
    }
}
