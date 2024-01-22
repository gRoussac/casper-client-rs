use std::path::Path;

use bytes::{BufMut, Bytes, BytesMut};
use casper_types::Key;
use flate2::{write::GzEncoder, Compression};
use reqwest::{
    header::{HeaderMap, HeaderValue, CONTENT_TYPE},
    Client, ClientBuilder, StatusCode,
};
use tar::Builder as TarBuilder;
use tokio::time::{sleep, Duration};

use crate::{
    verification_types::{VerificationDetails, VerificationRequest, VerificationStatus},
    Error, Verbosity,
};

static GIT_DIR_NAME: &str = ".git";
static TARGET_DIR_NAME: &str = "target";

/// Builds an archive from the specified path.
///
/// This function creates a compressed tar archive from the files and directories located at the
/// specified path. It excludes the `.git` and `target` directories from the archive.
///
/// # Arguments
///
/// * `path` - The path to the directory containing the files and directories to be archived.
///
/// # Returns
///
/// The compressed tar archive as a `Bytes` object, or an `std::io::Error` if an error occurs during
/// the archiving process.
pub fn build_archive(path: &Path) -> Result<Bytes, std::io::Error> {
    let buffer = BytesMut::new().writer();
    let encoder = GzEncoder::new(buffer, Compression::best());
    let mut archive = TarBuilder::new(encoder);

    for entry in (path.read_dir()?).flatten() {
        let file_name = entry.file_name();
        // Skip `.git` and `target`.
        if file_name == TARGET_DIR_NAME || file_name == GIT_DIR_NAME {
            continue;
        }
        let full_path = entry.path();
        if full_path.is_dir() {
            archive.append_dir_all(&file_name, &full_path)?;
        } else {
            archive.append_path_with_name(&full_path, &file_name)?;
        }
    }

    let encoder = archive.into_inner()?;
    let buffer = encoder.finish()?;
    Ok(buffer.into_inner().freeze())
}

/// Verifies the smart contract code against the one deployed at deploy hash.
///
/// Sends a verification request to the specified verification URL base path, including the deploy hash,
/// public key, and code archive.
///
/// # Arguments
///
/// * `deploy_hash` - The hash of the deployed contract.
/// * `public_key` - The public key associated with the contract.
/// * `base_url` - The base path of the verification URL.
/// * `verbosity` - The verbosity level of the verification process.
///
/// # Returns
///
/// The verification details of the contract.
pub async fn send_verification_request(
    key: Key,
    base_url: &str,
    code_archive: String,
    verbosity: Verbosity,
) -> Result<VerificationDetails, Error> {
    let verification_request = VerificationRequest {
        deploy_hash: key,
        code_archive,
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let Ok(http_client) = ClientBuilder::new()
        .default_headers(headers)
        .user_agent("casper-client-rs")
        .build()
    else {
        eprintln!("Failed to build HTTP client");
        return Err(Error::ContractVerificationFailed); // FIXME: different error
    };

    if verbosity == Verbosity::Medium || verbosity == Verbosity::High {
        println!("Sending verification request");
    }

    let url = base_url.to_string() + "/verification";
    let response = match http_client
        .post(url)
        .json(&verification_request)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            eprintln!("Cannot send verification request: {error:?}");
            return Err(Error::ContractVerificationFailed);
        }
    };

    match response.status() {
        StatusCode::OK => {
            if verbosity == Verbosity::Medium || verbosity == Verbosity::High {
                println!("Sent verification request",);
            }
        }
        status => {
            eprintln!("Verification faile with status {status}");
            return Err(Error::ContractVerificationFailed);
        }
    }

    wait_for_verification_finished(base_url, &http_client, key, verbosity).await;

    if verbosity == Verbosity::Medium || verbosity == Verbosity::High {
        println!("Getting verification details...");
    }

    let url = base_url.to_string() + "/verification" + &key.to_formatted_string() + "/details";
    match http_client.get(url).send().await {
        Ok(response) => response.json().await.map_err(|err| {
            eprintln!("Failed to parse JSON {err}");
            Error::ContractVerificationFailed
        }),
        Err(error) => {
            eprintln!("Cannot get verification details: {error:?}");
            Err(Error::ContractVerificationFailed)
        }
    }
}

/// Waits for the verification process to finish.
async fn wait_for_verification_finished(
    base_url: &str,
    http_client: &Client,
    key: Key,
    verbosity: Verbosity,
) {
    let mut verification_status = match get_verification_status(base_url, http_client, key).await {
        Ok(verification_status) => verification_status,
        Err(error) => {
            eprintln!("Cannot get verification status: {error:?}");
            return;
        }
    };

    while verification_status != VerificationStatus::Verified
        && verification_status != VerificationStatus::Failed
    {
        verification_status = match get_verification_status(base_url, http_client, key).await {
            Ok(verification_status) => verification_status,
            Err(error) => {
                eprintln!("Cannot get verification status: {error:?}");
                return;
            }
        };

        sleep(Duration::from_millis(100)).await;
        // TODO: Add backoff with limited retries.
    }

    if verbosity == Verbosity::Medium || verbosity == Verbosity::High {
        println!("Verification finished - status {verification_status:?}");
    }
}

/// Gets the verification status of the contract.
async fn get_verification_status(
    base_url: &str,
    http_client: &Client,
    key: Key,
) -> Result<VerificationStatus, Error> {
    let url = base_url.to_string() + "/verification" + &key.to_formatted_string() + "/status";
    let response = match http_client.get(url).send().await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("Failed to fetch verification status: {error:?}");
            return Err(Error::ContractVerificationFailed);
        }
    };

    match response.status() {
        StatusCode::OK => response.json().await.map_err(|err| {
            eprintln!("Failed to parse JSON for verification status, {err}");
            Error::ContractVerificationFailed
        }),
        status => {
            eprintln!("Verification status not found, {status}");
            Err(Error::ContractVerificationFailed)
        }
    }
}