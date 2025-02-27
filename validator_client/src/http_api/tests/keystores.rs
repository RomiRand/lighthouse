use super::*;
use account_utils::random_password_string;
use eth2::lighthouse_vc::{
    http_client::ValidatorClientHttpClient as HttpClient,
    std_types::{KeystoreJsonStr as Keystore, *},
    types::Web3SignerValidatorRequest,
};
// use eth2_keystore::Keystore;
use itertools::Itertools;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use slashing_protection::interchange::{Interchange, InterchangeMetadata};
use std::collections::HashMap;
use std::path::Path;

fn new_keystore(password: ZeroizeString) -> Keystore {
    let keypair = Keypair::random();
    Keystore(
        KeystoreBuilder::new(&keypair, password.as_ref(), String::new())
            .unwrap()
            .build()
            .unwrap(),
    )
}

fn web3_signer_url() -> String {
    "http://localhost:1/this-url-hopefully-doesnt-exist".into()
}

fn new_web3signer_validator() -> (Keypair, Web3SignerValidatorRequest) {
    let keypair = Keypair::random();
    let pk = keypair.pk.clone();
    (keypair, web3signer_validator_with_pubkey(pk))
}

fn web3signer_validator_with_pubkey(pubkey: PublicKey) -> Web3SignerValidatorRequest {
    Web3SignerValidatorRequest {
        enable: true,
        description: "".into(),
        graffiti: None,
        voting_public_key: pubkey,
        url: web3_signer_url(),
        root_certificate_path: None,
        request_timeout_ms: None,
    }
}

fn run_test<F, V>(f: F)
where
    F: FnOnce(ApiTester) -> V,
    V: Future<Output = ()>,
{
    let runtime = build_runtime();
    let weak_runtime = Arc::downgrade(&runtime);
    runtime.block_on(async {
        let tester = ApiTester::new(weak_runtime).await;
        f(tester).await
    });
}

fn run_dual_vc_test<F, V>(f: F)
where
    F: FnOnce(ApiTester, ApiTester) -> V,
    V: Future<Output = ()>,
{
    let runtime = build_runtime();
    let weak_runtime = Arc::downgrade(&runtime);
    runtime.block_on(async {
        let tester1 = ApiTester::new(weak_runtime.clone()).await;
        let tester2 = ApiTester::new(weak_runtime).await;
        f(tester1, tester2).await
    });
}

fn keystore_pubkey(keystore: &Keystore) -> PublicKeyBytes {
    keystore.0.public_key().unwrap().compress()
}

fn all_with_status<T: Clone>(count: usize, status: T) -> impl Iterator<Item = T> {
    std::iter::repeat(status).take(count)
}

fn all_imported(count: usize) -> impl Iterator<Item = ImportKeystoreStatus> {
    all_with_status(count, ImportKeystoreStatus::Imported)
}

fn all_duplicate(count: usize) -> impl Iterator<Item = ImportKeystoreStatus> {
    all_with_status(count, ImportKeystoreStatus::Duplicate)
}

fn all_import_error(count: usize) -> impl Iterator<Item = ImportKeystoreStatus> {
    all_with_status(count, ImportKeystoreStatus::Error)
}

fn all_deleted(count: usize) -> impl Iterator<Item = DeleteKeystoreStatus> {
    all_with_status(count, DeleteKeystoreStatus::Deleted)
}

fn all_not_active(count: usize) -> impl Iterator<Item = DeleteKeystoreStatus> {
    all_with_status(count, DeleteKeystoreStatus::NotActive)
}

fn all_not_found(count: usize) -> impl Iterator<Item = DeleteKeystoreStatus> {
    all_with_status(count, DeleteKeystoreStatus::NotFound)
}

fn all_delete_error(count: usize) -> impl Iterator<Item = DeleteKeystoreStatus> {
    all_with_status(count, DeleteKeystoreStatus::Error)
}

fn check_get_response<'a>(
    response: &ListKeystoresResponse,
    expected_keystores: impl IntoIterator<Item = &'a Keystore>,
) {
    for (ks1, ks2) in response.data.iter().zip_eq(expected_keystores) {
        assert_eq!(ks1.validating_pubkey, keystore_pubkey(ks2));
        assert_eq!(ks1.derivation_path, ks2.path());
        assert!(ks1.readonly == None || ks1.readonly == Some(false));
    }
}

fn check_import_response(
    response: &ImportKeystoresResponse,
    expected_statuses: impl IntoIterator<Item = ImportKeystoreStatus>,
) {
    for (status, expected_status) in response.data.iter().zip_eq(expected_statuses) {
        assert_eq!(
            expected_status, status.status,
            "message: {:?}",
            status.message
        );
    }
}

fn check_delete_response<'a>(
    response: &DeleteKeystoresResponse,
    expected_statuses: impl IntoIterator<Item = DeleteKeystoreStatus>,
) {
    for (status, expected_status) in response.data.iter().zip_eq(expected_statuses) {
        assert_eq!(
            status.status, expected_status,
            "message: {:?}",
            status.message
        );
    }
}

#[test]
fn get_auth_no_token() {
    run_test(|mut tester| async move {
        tester.client.send_authorization_header(false);
        let auth_response = tester.client.get_auth().await.unwrap();

        // Load the file from the returned path.
        let token_path = Path::new(&auth_response.token_path);
        let token = HttpClient::load_api_token_from_file(token_path).unwrap();

        // The token should match the one that the client was originally initialised with.
        assert!(tester.client.api_token() == Some(&token));
    })
}

#[test]
fn get_empty_keystores() {
    run_test(|tester| async move {
        let res = tester.client.get_keystores().await.unwrap();
        assert_eq!(res, ListKeystoresResponse { data: vec![] });
    })
}

#[test]
fn import_new_keystores() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..3)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        let import_res = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: vec![password.clone(); keystores.len()],
                slashing_protection: None,
            })
            .await
            .unwrap();

        // All keystores should be imported.
        check_import_response(&import_res, all_imported(keystores.len()));

        // Check that GET lists all the imported keystores.
        let get_res = tester.client.get_keystores().await.unwrap();
        check_get_response(&get_res, &keystores);
    })
}

#[test]
fn import_only_duplicate_keystores() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..3)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        let req = ImportKeystoresRequest {
            keystores: keystores.clone(),
            passwords: vec![password.clone(); keystores.len()],
            slashing_protection: None,
        };

        // All keystores should be imported on first import.
        let import_res = tester.client.post_keystores(&req).await.unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));

        // No keystores should be imported on repeat import.
        let import_res = tester.client.post_keystores(&req).await.unwrap();
        check_import_response(&import_res, all_duplicate(keystores.len()));

        // Check that GET lists all the imported keystores.
        let get_res = tester.client.get_keystores().await.unwrap();
        check_get_response(&get_res, &keystores);
    })
}

#[test]
fn import_some_duplicate_keystores() {
    run_test(|tester| async move {
        let password = random_password_string();
        let num_keystores = 5;
        let keystores_all = (0..num_keystores)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        // Import even numbered keystores first.
        let keystores1 = keystores_all
            .iter()
            .enumerate()
            .filter_map(|(i, keystore)| {
                if i % 2 == 0 {
                    Some(keystore.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let req1 = ImportKeystoresRequest {
            keystores: keystores1.clone(),
            passwords: vec![password.clone(); keystores1.len()],
            slashing_protection: None,
        };

        let req2 = ImportKeystoresRequest {
            keystores: keystores_all.clone(),
            passwords: vec![password.clone(); keystores_all.len()],
            slashing_protection: None,
        };

        let import_res = tester.client.post_keystores(&req1).await.unwrap();
        check_import_response(&import_res, all_imported(keystores1.len()));

        // Check partial import.
        let expected = (0..num_keystores).map(|i| {
            if i % 2 == 0 {
                ImportKeystoreStatus::Duplicate
            } else {
                ImportKeystoreStatus::Imported
            }
        });
        let import_res = tester.client.post_keystores(&req2).await.unwrap();
        check_import_response(&import_res, expected);
    })
}

#[test]
fn import_wrong_number_of_passwords() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..3)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        let err = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: vec![password.clone()],
                slashing_protection: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.status().unwrap(), 400);
    })
}

#[test]
fn get_web3_signer_keystores() {
    run_test(|tester| async move {
        let num_local = 3;
        let num_remote = 2;

        // Add some local validators.
        let password = random_password_string();
        let keystores = (0..num_local)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        let import_res = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: vec![password.clone(); keystores.len()],
                slashing_protection: None,
            })
            .await
            .unwrap();

        // All keystores should be imported.
        check_import_response(&import_res, all_imported(keystores.len()));

        // Add some web3signer validators.
        let remote_vals = (0..num_remote)
            .map(|_| new_web3signer_validator().1)
            .collect::<Vec<_>>();

        tester
            .client
            .post_lighthouse_validators_web3signer(&remote_vals)
            .await
            .unwrap();

        // Check that both local and remote validators are returned.
        let get_res = tester.client.get_keystores().await.unwrap();

        let expected_responses = keystores
            .iter()
            .map(|local_keystore| SingleKeystoreResponse {
                validating_pubkey: keystore_pubkey(local_keystore),
                derivation_path: local_keystore.path(),
                readonly: None,
            })
            .chain(remote_vals.iter().map(|remote_val| SingleKeystoreResponse {
                validating_pubkey: remote_val.voting_public_key.compress(),
                derivation_path: None,
                readonly: Some(true),
            }))
            .collect::<Vec<_>>();

        for response in expected_responses {
            assert!(get_res.data.contains(&response), "{:?}", response);
        }
    })
}

#[test]
fn import_and_delete_conflicting_web3_signer_keystores() {
    run_test(|tester| async move {
        let num_keystores = 3;

        // Create some keystores to be used as both web3signer keystores and local keystores.
        let password = random_password_string();
        let keystores = (0..num_keystores)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();
        let pubkeys = keystores.iter().map(keystore_pubkey).collect::<Vec<_>>();

        // Add the validators as web3signer validators.
        let remote_vals = pubkeys
            .iter()
            .map(|pubkey| web3signer_validator_with_pubkey(pubkey.decompress().unwrap()))
            .collect::<Vec<_>>();

        tester
            .client
            .post_lighthouse_validators_web3signer(&remote_vals)
            .await
            .unwrap();

        // Attempt to import the same validators as local validators, which should error.
        let import_req = ImportKeystoresRequest {
            keystores: keystores.clone(),
            passwords: vec![password.clone(); keystores.len()],
            slashing_protection: None,
        };
        let import_res = tester.client.post_keystores(&import_req).await.unwrap();
        check_import_response(&import_res, all_import_error(keystores.len()));

        // Attempt to delete the web3signer validators, which should fail.
        let delete_req = DeleteKeystoresRequest {
            pubkeys: pubkeys.clone(),
        };
        let delete_res = tester.client.delete_keystores(&delete_req).await.unwrap();
        check_delete_response(&delete_res, all_delete_error(keystores.len()));

        // Get should still list all the validators as `readonly`.
        let get_res = tester.client.get_keystores().await.unwrap();
        for (ks, pubkey) in get_res.data.iter().zip_eq(&pubkeys) {
            assert_eq!(ks.validating_pubkey, *pubkey);
            assert_eq!(ks.derivation_path, None);
            assert_eq!(ks.readonly, Some(true));
        }

        // Disabling the web3signer validators should *still* prevent them from being
        // overwritten.
        for pubkey in &pubkeys {
            tester
                .client
                .patch_lighthouse_validators(pubkey, false)
                .await
                .unwrap();
        }
        let import_res = tester.client.post_keystores(&import_req).await.unwrap();
        check_import_response(&import_res, all_import_error(keystores.len()));
        let delete_res = tester.client.delete_keystores(&delete_req).await.unwrap();
        check_delete_response(&delete_res, all_delete_error(keystores.len()));
    })
}

#[test]
fn import_keystores_wrong_password() {
    run_test(|tester| async move {
        let num_keystores = 4;
        let (keystores, correct_passwords): (Vec<_>, Vec<_>) = (0..num_keystores)
            .map(|_| {
                let password = random_password_string();
                (new_keystore(password.clone()), password)
            })
            .unzip();

        // First import with some incorrect passwords.
        let incorrect_passwords = (0..num_keystores)
            .map(|i| {
                if i % 2 == 0 {
                    random_password_string()
                } else {
                    correct_passwords[i].clone()
                }
            })
            .collect::<Vec<_>>();

        let import_res = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: incorrect_passwords.clone(),
                slashing_protection: None,
            })
            .await
            .unwrap();

        let expected_statuses = (0..num_keystores).map(|i| {
            if i % 2 == 0 {
                ImportKeystoreStatus::Error
            } else {
                ImportKeystoreStatus::Imported
            }
        });
        check_import_response(&import_res, expected_statuses);

        // Import again with the correct passwords and check that the statuses are as expected.
        let correct_import_req = ImportKeystoresRequest {
            keystores: keystores.clone(),
            passwords: correct_passwords.clone(),
            slashing_protection: None,
        };
        let import_res = tester
            .client
            .post_keystores(&correct_import_req)
            .await
            .unwrap();
        let expected_statuses = (0..num_keystores).map(|i| {
            if i % 2 == 0 {
                ImportKeystoreStatus::Imported
            } else {
                ImportKeystoreStatus::Duplicate
            }
        });
        check_import_response(&import_res, expected_statuses);

        // Import one final time, at which point all keys should be duplicates.
        let import_res = tester
            .client
            .post_keystores(&correct_import_req)
            .await
            .unwrap();
        check_import_response(
            &import_res,
            (0..num_keystores).map(|_| ImportKeystoreStatus::Duplicate),
        );
    });
}

#[test]
fn import_invalid_slashing_protection() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..3)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        // Invalid slashing protection data with mismatched version and mismatched GVR.
        let slashing_protection = Interchange {
            metadata: InterchangeMetadata {
                interchange_format_version: 0,
                genesis_validators_root: Hash256::zero(),
            },
            data: vec![],
        };

        let import_res = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: vec![password.clone(); keystores.len()],
                slashing_protection: Some(InterchangeJsonStr(slashing_protection)),
            })
            .await
            .unwrap();

        // All keystores should be imported.
        check_import_response(&import_res, all_import_error(keystores.len()));

        // Check that GET lists none of the failed keystores.
        let get_res = tester.client.get_keystores().await.unwrap();
        check_get_response(&get_res, &[]);
    })
}

fn all_indices(count: usize) -> Vec<usize> {
    (0..count).collect()
}

#[test]
fn migrate_all_with_slashing_protection() {
    let n = 3;
    generic_migration_test(
        n,
        vec![
            (0, make_attestation(1, 2)),
            (1, make_attestation(2, 3)),
            (2, make_attestation(1, 2)),
        ],
        all_indices(n),
        all_indices(n),
        all_indices(n),
        vec![
            (0, make_attestation(1, 2), false),
            (1, make_attestation(2, 3), false),
            (2, make_attestation(1, 2), false),
        ],
    );
}

#[test]
fn migrate_some_with_slashing_protection() {
    let n = 3;
    generic_migration_test(
        n,
        vec![
            (0, make_attestation(1, 2)),
            (1, make_attestation(2, 3)),
            (2, make_attestation(1, 2)),
        ],
        vec![0, 1],
        vec![0, 1],
        vec![0, 1],
        vec![
            (0, make_attestation(1, 2), false),
            (1, make_attestation(2, 3), false),
            (0, make_attestation(2, 3), true),
            (1, make_attestation(3, 4), true),
        ],
    );
}

#[test]
fn migrate_some_missing_slashing_protection() {
    let n = 3;
    generic_migration_test(
        n,
        vec![
            (0, make_attestation(1, 2)),
            (1, make_attestation(2, 3)),
            (2, make_attestation(1, 2)),
        ],
        vec![0, 1],
        vec![0],
        vec![0, 1],
        vec![
            (0, make_attestation(1, 2), false),
            (1, make_attestation(2, 3), true),
            (0, make_attestation(2, 3), true),
        ],
    );
}

#[test]
fn migrate_some_extra_slashing_protection() {
    let n = 3;
    generic_migration_test(
        n,
        vec![
            (0, make_attestation(1, 2)),
            (1, make_attestation(2, 3)),
            (2, make_attestation(1, 2)),
        ],
        all_indices(n),
        all_indices(n),
        vec![0, 1],
        vec![
            (0, make_attestation(1, 2), false),
            (1, make_attestation(2, 3), false),
            (0, make_attestation(2, 3), true),
            (1, make_attestation(3, 4), true),
            (2, make_attestation(2, 3), false),
        ],
    );
}

/// Run a test that creates some validators on one VC, and then migrates them to a second VC.
///
/// All indices given are in the range 0..`num_validators`. They are *not* validator indices in the
/// ordinary sense.
///
/// Parameters:
///
/// - `num_validators`: the total number of validators to create
/// - `first_vc_attestations`: attestations to sign on the first VC as `(validator_idx, att)`
/// - `delete_indices`: validators to delete from the first VC
/// - `slashing_protection_indices`: validators to transfer slashing protection data for. It should
///    be a subset of `delete_indices` or the test will panic.
/// - `import_indices`: validators to transfer. It needn't be a subset of `delete_indices`.
/// - `second_vc_attestations`: attestations to sign on the second VC after the transfer. The bool
///   indicates whether the signing should be successful.
fn generic_migration_test(
    num_validators: usize,
    first_vc_attestations: Vec<(usize, Attestation<E>)>,
    delete_indices: Vec<usize>,
    slashing_protection_indices: Vec<usize>,
    import_indices: Vec<usize>,
    second_vc_attestations: Vec<(usize, Attestation<E>, bool)>,
) {
    run_dual_vc_test(move |tester1, tester2| async move {
        // Create the validators on VC1.
        let (keystores, passwords): (Vec<_>, Vec<_>) = (0..num_validators)
            .map(|_| {
                let password = random_password_string();
                (new_keystore(password.clone()), password)
            })
            .unzip();

        let import_res = tester1
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: passwords.clone(),
                slashing_protection: None,
            })
            .await
            .unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));

        // Sign attestations on VC1.
        for (validator_index, mut attestation) in first_vc_attestations {
            let public_key = keystore_pubkey(&keystores[validator_index]);
            let current_epoch = attestation.data.target.epoch;
            tester1
                .validator_store
                .sign_attestation(public_key, 0, &mut attestation, current_epoch)
                .await
                .unwrap();
        }

        // Delete the selected keys from VC1.
        let delete_res = tester1
            .client
            .delete_keystores(&DeleteKeystoresRequest {
                pubkeys: delete_indices
                    .iter()
                    .copied()
                    .map(|i| keystore_pubkey(&keystores[i]))
                    .collect(),
            })
            .await
            .unwrap();
        check_delete_response(&delete_res, all_deleted(delete_indices.len()));

        // Check that slashing protection data was returned for all selected validators.
        assert_eq!(
            delete_res.slashing_protection.data.len(),
            delete_indices.len()
        );
        for &i in &delete_indices {
            assert!(delete_res
                .slashing_protection
                .data
                .iter()
                .any(|interchange_data| interchange_data.pubkey == keystore_pubkey(&keystores[i])));
        }

        // Filter slashing protection according to `slashing_protection_indices`.
        let mut slashing_protection = delete_res.slashing_protection;
        let data = std::mem::take(&mut slashing_protection.data);

        for &i in &slashing_protection_indices {
            let pubkey = keystore_pubkey(&keystores[i]);
            slashing_protection.data.push(
                data.iter()
                    .find(|interchange_data| interchange_data.pubkey == pubkey)
                    .expect("slashing protection indices should be subset of deleted")
                    .clone(),
            );
        }
        assert_eq!(
            slashing_protection.data.len(),
            slashing_protection_indices.len()
        );

        // Import into the 2nd VC using the slashing protection data.
        let import_res = tester2
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: import_indices
                    .iter()
                    .copied()
                    .map(|i| keystores[i].clone())
                    .collect(),
                passwords: import_indices
                    .iter()
                    .copied()
                    .map(|i| passwords[i].clone())
                    .collect(),
                slashing_protection: Some(InterchangeJsonStr(slashing_protection)),
            })
            .await
            .unwrap();
        check_import_response(&import_res, all_imported(import_indices.len()));

        // Sign attestations on the second VC.
        for (validator_index, mut attestation, should_succeed) in second_vc_attestations {
            let public_key = keystore_pubkey(&keystores[validator_index]);
            let current_epoch = attestation.data.target.epoch;
            match tester2
                .validator_store
                .sign_attestation(public_key, 0, &mut attestation, current_epoch)
                .await
            {
                Ok(()) => assert!(should_succeed),
                Err(e) => assert!(!should_succeed, "{:?}", e),
            }
        }
    });
}

#[test]
fn delete_keystores_twice() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..2)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        // 1. Import all keystores.
        let import_req = ImportKeystoresRequest {
            keystores: keystores.clone(),
            passwords: vec![password.clone(); keystores.len()],
            slashing_protection: None,
        };
        let import_res = tester.client.post_keystores(&import_req).await.unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));

        // 2. Delete all.
        let delete_req = DeleteKeystoresRequest {
            pubkeys: keystores.iter().map(keystore_pubkey).collect(),
        };
        let delete_res = tester.client.delete_keystores(&delete_req).await.unwrap();
        check_delete_response(&delete_res, all_deleted(keystores.len()));

        // 3. Delete again.
        let delete_res = tester.client.delete_keystores(&delete_req).await.unwrap();
        check_delete_response(&delete_res, all_not_active(keystores.len()));
    })
}

#[test]
fn delete_nonexistent_keystores() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..2)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        // Delete all.
        let delete_req = DeleteKeystoresRequest {
            pubkeys: keystores.iter().map(keystore_pubkey).collect(),
        };
        let delete_res = tester.client.delete_keystores(&delete_req).await.unwrap();
        check_delete_response(&delete_res, all_not_found(keystores.len()));
    })
}

fn make_attestation(source_epoch: u64, target_epoch: u64) -> Attestation<E> {
    Attestation {
        aggregation_bits: BitList::with_capacity(
            <E as EthSpec>::MaxValidatorsPerCommittee::to_usize(),
        )
        .unwrap(),
        data: AttestationData {
            source: Checkpoint {
                epoch: Epoch::new(source_epoch),
                root: Hash256::from_low_u64_le(source_epoch),
            },
            target: Checkpoint {
                epoch: Epoch::new(target_epoch),
                root: Hash256::from_low_u64_le(target_epoch),
            },
            ..AttestationData::default()
        },
        signature: AggregateSignature::empty(),
    }
}

#[test]
fn delete_concurrent_with_signing() {
    let runtime = build_runtime();
    let num_keys = 8;
    let num_signing_threads = 8;
    let num_attestations = 100;
    let num_delete_threads = 8;
    let num_delete_attempts = 100;
    let delete_prob = 0.01;

    assert!(
        num_keys % num_signing_threads == 0,
        "num_keys should be divisible by num threads for simplicity"
    );

    let weak_runtime = Arc::downgrade(&runtime);
    runtime.block_on(async {
        let tester = ApiTester::new(weak_runtime).await;

        // Generate a lot of keys and import them.
        let password = random_password_string();
        let keystores = (0..num_keys)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();
        let all_pubkeys = keystores.iter().map(keystore_pubkey).collect::<Vec<_>>();

        let import_res = tester
            .client
            .post_keystores(&ImportKeystoresRequest {
                keystores: keystores.clone(),
                passwords: vec![password.clone(); keystores.len()],
                slashing_protection: None,
            })
            .await
            .unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));

        // Start several threads signing attestations at sequential epochs.
        let mut join_handles = vec![];

        for thread_index in 0..num_signing_threads {
            let keys_per_thread = num_keys / num_signing_threads;
            let validator_store = tester.validator_store.clone();
            let thread_pubkeys = all_pubkeys
                [thread_index * keys_per_thread..(thread_index + 1) * keys_per_thread]
                .to_vec();

            let handle = runtime.spawn(async move {
                for j in 0..num_attestations {
                    let mut att = make_attestation(j, j + 1);
                    for (_validator_id, public_key) in thread_pubkeys.iter().enumerate() {
                        let _ = validator_store
                            .sign_attestation(*public_key, 0, &mut att, Epoch::new(j + 1))
                            .await;
                    }
                }
            });
            join_handles.push(handle);
        }

        // Concurrently, delete each validator one at a time. Store the slashing protection
        // data so we can ensure it doesn't change after a key is exported.
        let mut delete_handles = vec![];
        for _ in 0..num_delete_threads {
            let client = tester.client.clone();
            let all_pubkeys = all_pubkeys.clone();

            let handle = runtime.spawn(async move {
                let mut rng = SmallRng::from_entropy();

                let mut slashing_protection = vec![];
                for _ in 0..num_delete_attempts {
                    let to_delete = all_pubkeys
                        .iter()
                        .filter(|_| rng.gen_bool(delete_prob))
                        .copied()
                        .collect::<Vec<_>>();

                    if !to_delete.is_empty() {
                        let delete_res = client
                            .delete_keystores(&DeleteKeystoresRequest { pubkeys: to_delete })
                            .await
                            .unwrap();

                        for status in delete_res.data.iter() {
                            assert_ne!(status.status, DeleteKeystoreStatus::Error);
                        }

                        slashing_protection.push(delete_res.slashing_protection);
                    }
                }
                slashing_protection
            });

            delete_handles.push(handle);
        }

        // Collect slashing protection.
        let mut slashing_protection_map = HashMap::new();
        let collected_slashing_protection = futures::future::join_all(delete_handles).await;

        for interchange in collected_slashing_protection
            .into_iter()
            .map(Result::unwrap)
            .flatten()
        {
            for validator_data in interchange.data {
                slashing_protection_map
                    .entry(validator_data.pubkey)
                    .and_modify(|existing| {
                        assert_eq!(
                            *existing, validator_data,
                            "slashing protection data changed after first export"
                        )
                    })
                    .or_insert(validator_data);
            }
        }

        futures::future::join_all(join_handles).await
    });
}

#[test]
fn delete_then_reimport() {
    run_test(|tester| async move {
        let password = random_password_string();
        let keystores = (0..2)
            .map(|_| new_keystore(password.clone()))
            .collect::<Vec<_>>();

        // 1. Import all keystores.
        let import_req = ImportKeystoresRequest {
            keystores: keystores.clone(),
            passwords: vec![password.clone(); keystores.len()],
            slashing_protection: None,
        };
        let import_res = tester.client.post_keystores(&import_req).await.unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));

        // 2. Delete all.
        let delete_res = tester
            .client
            .delete_keystores(&DeleteKeystoresRequest {
                pubkeys: keystores.iter().map(keystore_pubkey).collect(),
            })
            .await
            .unwrap();
        check_delete_response(&delete_res, all_deleted(keystores.len()));

        // 3. Re-import
        let import_res = tester.client.post_keystores(&import_req).await.unwrap();
        check_import_response(&import_res, all_imported(keystores.len()));
    })
}
