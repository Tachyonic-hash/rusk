// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) DUSK NETWORK. All rights reserved.

#![allow(non_snake_case)]

use bid_circuits::BidCorrectnessCircuit;
use blindbid_circuits::BlindBidCircuit;
use dusk_blindbid::{Bid, Score};
use dusk_bls12_381::BlsScalar;
use dusk_jubjub::{JubJubAffine, GENERATOR_EXTENDED, GENERATOR_NUMS_EXTENDED};
use dusk_pki::{PublicSpendKey, SecretSpendKey};
use dusk_plonk::prelude::*;
use dusk_poseidon::tree::PoseidonBranch;
use lazy_static::lazy_static;
use profile_tooling::CircuitLoader;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

lazy_static! {
    static ref PUB_PARAMS: PublicParameters = {
        match rusk_profile::get_common_reference_string() {
            Ok(buff) if rusk_profile::verify_common_reference_string(&buff) => unsafe {
                info!("Got the CRS from cache");

                PublicParameters::from_slice_unchecked(&buff[..])
            },

            _ => {
                info!("New CRS needs to be generated and cached");

                use rand::rngs::StdRng;
                use rand::SeedableRng;

                let mut rng = StdRng::seed_from_u64(0xbeef);

                let pp = PublicParameters::setup(1 << 17, &mut rng)
                    .expect("Cannot initialize Public Parameters");

                info!("Public Parameters initialized");

                rusk_profile::set_common_reference_string(
                    pp.to_raw_var_bytes(),
                )
                .expect("Unable to write the CRS");

                pp
            }
        }
    };
}

/// Buildfile for the rusk crate.
///
/// Main goals of the file at the moment are:
/// 1. Compile the `.proto` files for tonic.
/// 2. Get the version of the crate and some extra info to
/// support the `-v` argument properly.
/// 3. Compile the contract-related circuits.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure we run the build script again even if we change just the build.rs
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=path/to/Cargo.lock");

    // Get crate version + commit + toolchain for `-v` arg support.
    println!(
        "cargo:rustc-env=GIT_HASH={}",
        rustc_tools_util::get_commit_hash().unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=COMMIT_DATE={}",
        rustc_tools_util::get_commit_date().unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=RUSTC_RELEASE_CHANNEL={}",
        rustc_tools_util::get_channel().unwrap_or_default()
    );

    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info,
        // warn, etc.) will be written to stdout.
        .with_max_level(Level::TRACE)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    // This will enforce the usage and therefore the cache / generation
    // of the CRS even if it's not used to compiles circuits inside the
    // build script.
    lazy_static::initialize(&PUB_PARAMS);

    // Compile protos for tonic
    tonic_build::compile_protos("../schema/rusk.proto")?;

    // Run the rusk-profile Circuit-keys checks
    use bid::BidCircuitLoader;
    use blindbid::BlindBidCircuitLoader;

    // Wipe the `.rusk/keys` folder entirely if DELETE_RUSK_KEYS env variable is
    // set.
    if option_env!("RUSK_BUILD_BID_KEYS").unwrap_or("0") != "0" {
        info!("DELETE_RUSK_KEYS env set!");
        info!("Starting `keys/` folder wipe process..");
        rusk_profile::clear_all_keys()?;
        info!("Keys folder contents were removed successfully!");
    };

    profile_tooling::run_circuit_keys_checks(vec![
        &BidCircuitLoader {},
        &BlindBidCircuitLoader {},
    ])?;

    Ok(())
}

mod bid {
    use super::*;

    pub struct BidCircuitLoader;

    impl CircuitLoader for BidCircuitLoader {
        fn circuit_id(&self) -> &[u8; 32] {
            &BidCorrectnessCircuit::CIRCUIT_ID
        }

        fn circuit_name(&self) -> &'static str {
            "BidCorrectness"
        }

        fn compile_circuit(
            &self,
        ) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
            let pub_params = &PUB_PARAMS;
            let value = JubJubScalar::from(100000_u64);
            let blinder = JubJubScalar::from(50000_u64);

            let c = JubJubAffine::from(
                (GENERATOR_EXTENDED * value)
                    + (GENERATOR_NUMS_EXTENDED * blinder),
            );

            let mut circuit = BidCorrectnessCircuit {
                commitment: c,
                value,
                blinder,
            };

            let (pk, vd) = circuit.compile(&pub_params)?;
            Ok((pk.to_var_bytes(), vd.to_var_bytes()))
        }
    }
}

mod blindbid {
    use super::*;
    pub struct BlindBidCircuitLoader;
    impl CircuitLoader for BlindBidCircuitLoader {
        fn circuit_id(&self) -> &[u8; 32] {
            &BlindBidCircuit::CIRCUIT_ID
        }

        fn circuit_name(&self) -> &'static str {
            "BlindBid"
        }

        fn compile_circuit(
            &self,
        ) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
            let pub_params = &PUB_PARAMS;

            // Generate a correct Bid
            let secret = JubJubScalar::random(&mut rand::thread_rng());
            let secret_k = BlsScalar::random(&mut rand::thread_rng());
            let bid = random_bid(&secret, secret_k);
            let secret: JubJubAffine = (GENERATOR_EXTENDED * secret).into();

            // Generate fields for the Bid & required by the compute_score
            let consensus_round_seed = 50u64;
            let latest_consensus_round = 50u64;
            let latest_consensus_step = 50u64;

            // Extract the branch
            let branch = PoseidonBranch::<17>::default();

            // Generate a `Score` for our Bid with the consensus parameters
            let score = Score::compute(
                &bid,
                &secret,
                secret_k,
                *branch.root(),
                BlsScalar::from(consensus_round_seed),
                latest_consensus_round,
                latest_consensus_step,
            )
            .expect("Score gen error");

            let mut circuit = BlindBidCircuit {
                bid,
                score,
                secret_k,
                secret,
                seed: BlsScalar::from(consensus_round_seed),
                latest_consensus_round: BlsScalar::from(latest_consensus_round),
                latest_consensus_step: BlsScalar::from(latest_consensus_step),
                branch: &branch,
            };

            let (pk, vd) = circuit.compile(&pub_params)?;
            Ok((pk.to_var_bytes(), vd.to_var_bytes()))
        }
    }

    fn random_bid(secret: &JubJubScalar, secret_k: BlsScalar) -> Bid {
        let mut rng = rand::thread_rng();
        let pk_r = PublicSpendKey::from(SecretSpendKey::random(&mut rng));
        let stealth_addr = pk_r.gen_stealth_address(&secret);
        let secret = GENERATOR_EXTENDED * secret;
        let value = 60_000u64;
        let value = JubJubScalar::from(value);
        // Set the timestamps as the max values so the proofs do not fail for
        // them (never expired or non-elegible).
        let elegibility_ts = u64::MAX;
        let expiration_ts = u64::MAX;

        Bid::new(
            &mut rng,
            &stealth_addr,
            &value,
            &secret.into(),
            secret_k,
            elegibility_ts,
            expiration_ts,
        )
        .expect("Error generating a Bid")
    }
}

/*
mod transfer {
    use super::PUB_PARAMS;
    use std::convert::TryInto;

    use anyhow::{anyhow, Result};
    use dusk_bytes::Serializable;
    use dusk_pki::SecretSpendKey;
    use dusk_plonk::circuit;
    use dusk_plonk::circuit::VerifierData;
    use phoenix_core::{Message, Note};
    use sha2::{Digest, Sha256};
    use tracing::info;
    use transfer_circuits::{
        ExecuteCircuit, SendToContractObfuscatedCircuit,
        SendToContractTransparentCircuit, WithdrawFromObfuscatedCircuit,
    };

    use dusk_plonk::prelude::*;

    pub fn compile_stco_circuit() -> Result<(&'static str, Vec<u8>, Vec<u8>)> {
        let mut rng = rand::thread_rng();

        let ssk = SecretSpendKey::random(&mut rng);
        let vk = ssk.view_key();
        let psk = ssk.public_spend_key();

        let c_value = 100;
        let c_blinding_factor = JubJubScalar::random(&mut rng);
        let c_note =
            Note::obfuscated(&mut rng, &psk, c_value, c_blinding_factor);
        let (fee, crossover) = c_note.try_into().map_err(|e| {
            anyhow!("Failed to convert phoenix note into crossover: {:?}", e)
        })?;

        let address = BlsScalar::random(&mut rng);
        let message_r = JubJubScalar::random(&mut rng);
        let message_value = 100;
        let message = Message::new(&mut rng, &message_r, &psk, message_value);

        let c_signature = SendToContractObfuscatedCircuit::sign(
            &mut rng, &ssk, &fee, &crossover, &message, &address,
        );

        let mut circuit = SendToContractObfuscatedCircuit::new(
            fee,
            crossover,
            &vk,
            c_signature,
            true,
            message,
            &psk,
            message_r,
            address,
        )
        .map_err(|e| anyhow!("Error generating circuit: {:?}", e))?;

        let (pk, vd) = circuit.compile(&PUB_PARAMS)?;

        let id = SendToContractObfuscatedCircuit::rusk_keys_id();
        let pk = pk.to_var_bytes();
        let vd = vd.to_var_bytes();

        Ok((id, pk, vd))
    }

    pub fn compile_stct_circuit() -> Result<(&'static str, Vec<u8>, Vec<u8>)> {
        let mut rng = rand::thread_rng();

        let c_ssk = SecretSpendKey::random(&mut rng);
        let c_vk = c_ssk.view_key();
        let c_psk = c_ssk.public_spend_key();

        let c_value = 100;
        let c_blinding_factor = JubJubScalar::random(&mut rng);

        let c_note =
            Note::obfuscated(&mut rng, &c_psk, c_value, c_blinding_factor);
        let (fee, crossover) = c_note.try_into().map_err(|e| {
            anyhow!("Failed to convert phoenix note into crossover: {:?}", e)
        })?;

        let address = BlsScalar::random(&mut rng);
        let c_signature = SendToContractTransparentCircuit::sign(
            &mut rng, &c_ssk, &fee, &crossover, c_value, &address,
        );

        let mut circuit = SendToContractTransparentCircuit::new(
            fee,
            crossover,
            &c_vk,
            address,
            c_signature,
        )
        .map_err(|e| anyhow!("Error generating circuit: {:?}", e))?;

        let (pk, vd) = circuit.compile(&PUB_PARAMS)?;

        let id = SendToContractTransparentCircuit::rusk_keys_id();
        let pk = pk.to_var_bytes();
        let vd = vd.to_var_bytes();

        Ok((id, pk, vd))
    }

    pub fn compile_wfo_circuit() -> Result<(&'static str, Vec<u8>, Vec<u8>)> {
        let mut rng = rand::thread_rng();

        let i_ssk = SecretSpendKey::random(&mut rng);
        let i_vk = i_ssk.view_key();
        let i_psk = i_ssk.public_spend_key();
        let i_value = 100;
        let i_blinding_factor = JubJubScalar::random(&mut rng);
        let i_note =
            Note::obfuscated(&mut rng, &i_psk, i_value, i_blinding_factor);

        let c_ssk = SecretSpendKey::random(&mut rng);
        let c_psk = c_ssk.public_spend_key();
        let c_r = JubJubScalar::random(&mut rng);
        let c_value = 25;
        let c = Message::new(&mut rng, &c_r, &c_psk, c_value);

        let o_ssk = SecretSpendKey::random(&mut rng);
        let o_vk = o_ssk.view_key();
        let o_psk = o_ssk.public_spend_key();
        let o_value = 75;
        let o_blinding_factor = JubJubScalar::random(&mut rng);
        let o_note =
            Note::obfuscated(&mut rng, &o_psk, o_value, o_blinding_factor);

        let mut circuit = WithdrawFromObfuscatedCircuit::new(
            &i_note,
            Some(&i_vk),
            &c,
            c_r,
            &c_psk,
            &o_note,
            Some(&o_vk),
        )
        .map_err(|e| anyhow!("Error generating circuit: {:?}", e))?;

        let (pk, vd) = circuit.compile(&PUB_PARAMS)?;

        let id = WithdrawFromObfuscatedCircuit::rusk_keys_id();
        let pk = pk.to_var_bytes();
        let vd = vd.to_var_bytes();

        Ok((id, pk, vd))
    }

    pub fn compile_execute_circuit(
        inputs: usize,
        outputs: usize,
    ) -> Result<(&'static str, Vec<u8>, Vec<u8>)> {
        info!(
            "Starting the compilation of the circuit for {}/{}",
            inputs, outputs
        );

        let (ci, _, pk, vd, proof, pi) = ExecuteCircuit::create_dummy_proof(
            &mut rand::thread_rng(),
            Some(<&PublicParameters>::from(&PUB_PARAMS).clone()),
            inputs,
            outputs,
            true,
            false,
        )?;

        info!(
            "Circuit generated with {}/{}",
            ci.inputs().len(),
            ci.outputs().len()
        );

        let id = ci.rusk_keys_id();

        // Sanity check
        circuit::verify_proof(
            &*PUB_PARAMS,
            vd.key(),
            &proof,
            pi.as_slice(),
            vd.pi_pos(),
            b"dusk-network",
        )
        .map_err(|_| anyhow!("Proof verification failed for {}", id))?;

        let pk = pk.to_var_bytes();
        let vd = vd.to_var_bytes();

        let mut hasher = Sha256::new();
        hasher.update(PUB_PARAMS.to_raw_var_bytes().as_slice());
        let contents = hasher.finalize();
        info!("Using PP {:x}", contents);

        let mut hasher = Sha256::new();
        hasher.update(vd.as_slice());
        let contents = hasher.finalize();

        let mut hasher = Sha256::new();
        let vk_p = VerifierData::from_slice(vd.as_slice()).expect("Data");
        hasher.update(&vk_p.key().to_bytes());
        let contents_key = hasher.finalize();

        info!(
            "Execute circuit data generated for {} with verifier data {:x} and key {:x}",
            id, contents, contents_key
        );

        Ok((id, pk, vd))
    }
}
*/

mod profile_tooling {
    use super::*;

    pub trait CircuitLoader {
        fn circuit_id(&self) -> &[u8; 32];

        fn circuit_name(&self) -> &'static str;

        fn compile_circuit(
            &self,
        ) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>>;
    }

    fn clear_outdated_keys(
        loader_list: &[&dyn CircuitLoader],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let id_list: Vec<_> = loader_list
            .iter()
            .map(|loader| loader.circuit_id())
            .cloned()
            .collect();

        Ok(rusk_profile::clean_outdated_keys(&id_list)?)
    }

    fn check_keys_cache(
        loader_list: &[&dyn CircuitLoader],
    ) -> Result<Vec<()>, Box<dyn std::error::Error>> {
        loader_list
            .iter()
            .map(|loader| {
                info!("{} Keys cache checking stage", loader.circuit_name());
                match rusk_profile::keys_for(loader.circuit_id()) {
                    Ok(_) => {
                        info!(
                            "{} already loaded correctly!",
                            loader.circuit_name()
                        );
                        Ok(())
                    }
                    _ => {
                        warn!("{} not cached!", loader.circuit_name());
                        info!(
                            "Compiling {} and adding to the cache",
                            loader.circuit_name()
                        );
                        let (pk, vd) = loader.compile_circuit()?;
                        rusk_profile::add_keys_for(
                            loader.circuit_id(),
                            pk,
                            vd,
                        )?;
                        info!(
                            "{} Keys cache checking stage finished",
                            loader.circuit_name()
                        );
                        Ok(())
                    }
                }
            })
            .collect::<Result<Vec<()>, Box<dyn std::error::Error>>>()
    }

    pub fn run_circuit_keys_checks(
        loader_list: Vec<&dyn CircuitLoader>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        clear_outdated_keys(&loader_list)?;
        check_keys_cache(&loader_list).map(|_| ())
    }
}
