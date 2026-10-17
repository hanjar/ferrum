//! Policy hash, approval MAC and verification (Automation design §8.3-§8.5, SE-3). This is the
//! core of the authorization path: an automation may run unattended only while
//! [`verify`] answers [`Verdict::Valid`] for it.
//!
//! ## Policy hash (`HASH_VERSION` 1)
//!
//! ```text
//! H = SHA-256( lp("ferrum.automation.policy.v1") ‖ lp(automation_id)
//!            ‖ lp(profile_id) ‖ u64le(profile_revision)
//!            ‖ lp(serde_json::to_vec(PolicyV1 { profile, automation })) )
//! ```
//!
//! `lp(x)` = `u64le(len(x)) ‖ x`. `PolicyV1` field order is the struct definition order; any change
//! to `ProfilePolicy`, `AutomationPolicy` or their serialization MUST bump `HASH_VERSION` (a
//! golden-hash test pins `H` for a fixture). The automation NAME is not part of `H`.
//!
//! ## Approval MAC
//!
//! ```text
//! mac = HMAC-SHA256(key, lp("ferrum.automation.approval.v1") ‖ u64le(hash_version) ‖ lp(H)
//!                      ‖ lp(automation_id) ‖ lp(profile_id) ‖ u64le(profile_revision)
//!                      ‖ u64le(config_rev) ‖ u64le(approved_ms))
//! ```
//!
//! compared in constant time ([`hmac::Mac::verify_slice`]). The key is the DPAPI-protected secret
//! of `automation::dpapi`; without it every approval reads as `keyUnavailable` (fail closed).
//!
//! Tauri-free: the runner links this module.

use hmac::{Hmac, Mac};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::automation::dpapi::{ApprovalKey, ApprovalKeys, KeyLoad};
use crate::automation::model::{AutomationPolicy, ProfilePolicy};
use crate::automation::store::{StoreError, approvals, automations, profiles};

type HmacSha256 = Hmac<Sha256>;

/// Version of the hash layout above. Bump on ANY change to the hashed serialization.
pub const HASH_VERSION: u32 = 1;
const HASH_DOMAIN: &str = "ferrum.automation.policy.v1";
const MAC_DOMAIN: &str = "ferrum.automation.approval.v1";

/// The hashed document: profile policy (with root identities) and automation policy.
#[derive(Serialize)]
struct PolicyV1<'a> {
    profile: &'a ProfilePolicy,
    automation: &'a AutomationPolicy,
}

fn put(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn revision_bytes(revision: i64) -> Option<[u8; 8]> {
    // Revisions and counters are positive; a negative value is a corrupted row.
    u64::try_from(revision).ok().map(u64::to_le_bytes)
}

/// The policy hash `H` as lowercase hex, or `None` when the inputs cannot be hashed (a negative
/// revision, a serialization failure) - which callers treat as "policy changed".
pub fn policy_hash(
    automation_id: &str,
    profile_id: &str,
    profile_revision: i64,
    profile: &ProfilePolicy,
    automation: &AutomationPolicy,
) -> Option<String> {
    let document = serde_json::to_vec(&PolicyV1 {
        profile,
        automation,
    })
    .ok()?;
    let mut buf: Vec<u8> = Vec::with_capacity(document.len() + 256);
    put(&mut buf, HASH_DOMAIN.as_bytes());
    put(&mut buf, automation_id.as_bytes());
    put(&mut buf, profile_id.as_bytes());
    buf.extend_from_slice(&revision_bytes(profile_revision)?);
    put(&mut buf, &document);
    Some(hex::encode(Sha256::digest(&buf)))
}

/// The inputs of a MAC. All of them are stored in the `approvals` row except the key.
pub struct MacInput<'a> {
    pub hash_version: i64,
    pub policy_hash: &'a str,
    pub automation_id: &'a str,
    pub profile_id: &'a str,
    pub profile_revision: i64,
    pub config_rev: i64,
    pub approved_ms: i64,
}

fn mac_for(key: &ApprovalKey, input: &MacInput<'_>) -> Option<HmacSha256> {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    put(&mut buf, MAC_DOMAIN.as_bytes());
    buf.extend_from_slice(&revision_bytes(input.hash_version)?);
    put(&mut buf, input.policy_hash.as_bytes());
    put(&mut buf, input.automation_id.as_bytes());
    put(&mut buf, input.profile_id.as_bytes());
    buf.extend_from_slice(&revision_bytes(input.profile_revision)?);
    buf.extend_from_slice(&revision_bytes(input.config_rev)?);
    buf.extend_from_slice(&revision_bytes(input.approved_ms)?);
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).ok()?;
    mac.update(&buf);
    Some(mac)
}

/// Lowercase hex HMAC-SHA256 of an approval, or `None` for unhashable inputs.
pub fn compute_mac(key: &ApprovalKey, input: &MacInput<'_>) -> Option<String> {
    mac_for(key, input).map(|m| hex::encode(m.finalize().into_bytes()))
}

/// Constant-time check of a stored hex MAC.
pub fn mac_matches(key: &ApprovalKey, input: &MacInput<'_>, stored_hex: &str) -> bool {
    let Ok(stored) = hex::decode(stored_hex) else {
        return false;
    };
    mac_for(key, input).is_some_and(|m| m.verify_slice(&stored).is_ok())
}

/// Lowercase hex HMAC-SHA256 of `data` under a (sub)key.
pub fn hmac_hex(key: &ApprovalKey, data: &[u8]) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).ok()?;
    mac.update(data);
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Constant-time check of a hex HMAC-SHA256 of `data` under a (sub)key.
pub fn hmac_matches(key: &ApprovalKey, data: &[u8], stored_hex: &str) -> bool {
    let Ok(stored) = hex::decode(stored_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(key.as_bytes()) else {
        return false;
    };
    mac.update(data);
    mac.verify_slice(&stored).is_ok()
}

/// A subkey for another purpose, derived from the approval key so one secret serves both but a
/// MAC made for one purpose can never verify for the other (domain separation). `None` only if
/// the HMAC key setup fails (it cannot for a 32-byte key; callers fail closed anyway).
pub fn derive_subkey(key: &ApprovalKey, purpose: &str) -> Option<ApprovalKey> {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).ok()?;
    mac.update(purpose.as_bytes());
    let out = mac.finalize().into_bytes();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    Some(ApprovalKey(bytes))
}

// ── Verification (design §8.5) ──

/// Why an automation needs a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsReview {
    /// The profile was saved with a policy change since the approval.
    ProfileChanged { from: i64, to: i64 },
    /// The stored automation, profile or approval no longer matches what was approved.
    PolicyChanged,
    /// No approval, or one that does not authenticate on this machine for this user.
    NotApproved,
    /// The DPAPI key is missing or unusable: no approval can be verified.
    KeyUnavailable,
    /// The approval was made with a hash layout this version does not know.
    UnsupportedVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Valid,
    NeedsReview(NeedsReview),
}

/// `policy::verify`: approval row exists -> hash version supported -> profile revision equal ->
/// recomputed `H` equals the stored one -> configuration revision equal -> MAC verifies. Reads
/// only; run it inside the caller's transaction to see one consistent snapshot. A store failure is
/// an `Err` (callers treat it as not valid).
pub fn verify(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
) -> Result<Verdict, StoreError> {
    use NeedsReview::*;
    let review = |r| Ok(Verdict::NeedsReview(r));
    let Some(automation) = automations::get(conn, automation_id)? else {
        return review(NotApproved);
    };
    let Some(approval) = approvals::get(conn, automation_id)? else {
        return review(NotApproved);
    };
    if approval.hash_version != i64::from(HASH_VERSION) {
        return review(UnsupportedVersion);
    }
    // The approval must cover the profile the automation points at now.
    if approval.profile_id != automation.profile_id {
        return review(PolicyChanged);
    }
    let Some(profile) = profiles::get(conn, &automation.profile_id)? else {
        return review(PolicyChanged);
    };
    if profile.revision != approval.profile_revision {
        return review(ProfileChanged {
            from: approval.profile_revision,
            to: profile.revision,
        });
    }
    // An unparsable stored row is never repaired silently (design §4.4).
    let (Ok(profile_policy), Ok(automation_policy)) = (
        serde_json::from_str::<ProfilePolicy>(&profile.policy),
        serde_json::from_str::<AutomationPolicy>(&automation.policy),
    ) else {
        return review(PolicyChanged);
    };
    let Some(hash) = policy_hash(
        &automation.id,
        &profile.id,
        profile.revision,
        &profile_policy,
        &automation_policy,
    ) else {
        return review(PolicyChanged);
    };
    if hash != approval.policy_hash {
        return review(PolicyChanged);
    }
    if automation.config_rev != approval.config_rev {
        return review(PolicyChanged);
    }
    let KeyLoad::Key(key) = keys.load() else {
        return review(KeyUnavailable);
    };
    let input = MacInput {
        hash_version: approval.hash_version,
        policy_hash: &approval.policy_hash,
        automation_id: &approval.automation_id,
        profile_id: &approval.profile_id,
        profile_revision: approval.profile_revision,
        config_rev: approval.config_rev,
        approved_ms: approval.approved_ms,
    };
    if mac_matches(&key, &input, &approval.mac) {
        Ok(Verdict::Valid)
    } else {
        review(NotApproved)
    }
}

#[cfg(test)]
mod tests;
