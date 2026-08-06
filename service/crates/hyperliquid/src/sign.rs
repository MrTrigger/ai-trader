//! Signing an exchange action with an agent wallet.
//!
//! Hyperliquid authenticates a write by an EIP-712 signature over a small typed
//! struct. The payload it signs is not the action itself but a hash of it: the
//! action is msgpack-encoded, the nonce and an optional vault address are
//! appended, and the keccak of that becomes the `connectionId` field of an
//! `Agent(string source, bytes32 connectionId)` struct signed under a fixed
//! domain.
//!
//! # Why this is hand-rolled
//!
//! Signing one well-known typed structure needs a curve and a hash. A general
//! Ethereum client brings a provider, an ABI coder, an RPC transport and a
//! chain-state model, none of which this ever calls, and all of which would be
//! in the dependency tree of the one process that holds a trading key. Two
//! focused crates — `k256` and `sha3` — is the smaller attack surface and the
//! smaller thing to audit.
//!
//! # The key this holds
//!
//! An **agent** key, never the account's own. An agent can place and cancel and
//! cannot withdraw, which is the boundary design spec §3.3 asks for: the
//! process that trades must not be able to move funds off the venue. Nothing in
//! this module can produce a withdrawal action, and there is no code path that
//! signs one.

use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{RecoveryId, Signature, SigningKey};
use sha3::{Digest, Keccak256};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error(
        "the agent key is not a 32-byte hex private key. Hyperliquid gives you one when you \
         generate an API wallet; it is 64 hex characters, optionally 0x-prefixed."
    )]
    BadKey,
    #[error("cannot encode the action for signing: {0}")]
    Encode(String),
    #[error("signing failed: {0}")]
    Sign(String),
}

/// An agent wallet. Holds a private key and can sign exchange actions.
///
/// Deliberately not `Debug` or `Clone`: a key that can be printed ends up in a
/// log, and a key that can be cloned ends up somewhere nobody audited.
pub struct Agent {
    key: SigningKey,
    address: String,
}

impl Agent {
    /// Parse an agent private key, with or without the `0x`.
    pub fn from_hex(key: &str) -> Result<Self, SignError> {
        let clean = key.trim().trim_start_matches("0x");
        if clean.len() != 64 {
            return Err(SignError::BadKey);
        }
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] =
                u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|_| SignError::BadKey)?;
        }
        let key = SigningKey::from_bytes((&bytes).into()).map_err(|_| SignError::BadKey)?;
        let address = address_of(&key);
        Ok(Self { key, address })
    }

    /// The agent's own address, which is what the venue checks the signature
    /// against. Public — this is not the secret.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Sign one exchange action.
    ///
    /// `is_mainnet` selects the source string the venue expects; getting it
    /// wrong produces a signature that verifies against nothing and an order
    /// rejected for a reason that does not mention the network.
    pub fn sign_action(
        &self,
        action: &serde_json::Value,
        nonce: u64,
        vault: Option<&str>,
        is_mainnet: bool,
    ) -> Result<SignatureJson, SignError> {
        let connection_id = action_hash(action, nonce, vault)?;
        let source = if is_mainnet { "a" } else { "b" };
        let digest = agent_digest(source, &connection_id);
        let (sig, recid): (Signature, RecoveryId) = self
            .key
            .sign_prehash(&digest)
            .map_err(|e| SignError::Sign(e.to_string()))?;
        let b = sig.to_bytes();
        Ok(SignatureJson {
            r: format!("0x{}", hex(&b[..32])),
            s: format!("0x{}", hex(&b[32..])),
            // Ethereum's v is the recovery id plus 27.
            v: recid.to_byte() as u64 + 27,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureJson {
    pub r: String,
    pub s: String,
    pub v: u64,
}

/// keccak(msgpack(action) ++ nonce_be ++ vault_flag[++ vault_bytes]).
///
/// The byte layout is the venue's, not ours: msgpack rather than JSON, a
/// big-endian u64 nonce, then a single byte that is 0 when there is no vault
/// and 1 followed by the 20 address bytes when there is.
pub fn action_hash(
    action: &serde_json::Value,
    nonce: u64,
    vault: Option<&str>,
) -> Result<[u8; 32], SignError> {
    let mut buf = rmp_serde::to_vec_named(action).map_err(|e| SignError::Encode(e.to_string()))?;
    buf.extend_from_slice(&nonce.to_be_bytes());
    match vault {
        None => buf.push(0),
        Some(v) => {
            buf.push(1);
            buf.extend_from_slice(&addr_bytes(v).ok_or_else(|| {
                SignError::Encode(format!("vault address is not 20 hex bytes: {v}"))
            })?);
        }
    }
    Ok(keccak(&buf))
}

/// The EIP-712 digest for `Agent(string source, bytes32 connectionId)` under
/// Hyperliquid's `Exchange` domain.
///
/// The domain is fixed by the venue, including `chainId: 1337`, which is not
/// the chain anything settles on — it is simply the constant Hyperliquid picked
/// for this domain separator.
fn agent_digest(source: &str, connection_id: &[u8; 32]) -> [u8; 32] {
    let domain_type = keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut dom = Vec::with_capacity(160);
    dom.extend_from_slice(&domain_type);
    dom.extend_from_slice(&keccak(b"Exchange"));
    dom.extend_from_slice(&keccak(b"1"));
    dom.extend_from_slice(&u256_be(1337));
    dom.extend_from_slice(&[0u8; 32]); // verifyingContract: the zero address
    let domain_separator = keccak(&dom);

    let struct_type = keccak(b"Agent(string source,bytes32 connectionId)");
    let mut st = Vec::with_capacity(96);
    st.extend_from_slice(&struct_type);
    st.extend_from_slice(&keccak(source.as_bytes()));
    st.extend_from_slice(connection_id);
    let struct_hash = keccak(&st);

    let mut pre = Vec::with_capacity(66);
    pre.extend_from_slice(b"\x19\x01");
    pre.extend_from_slice(&domain_separator);
    pre.extend_from_slice(&struct_hash);
    keccak(&pre)
}

/// The Ethereum address for a key: last 20 bytes of the keccak of the
/// uncompressed public key, minus its leading 0x04 tag.
fn address_of(key: &SigningKey) -> String {
    let pubkey = key.verifying_key().to_encoded_point(false);
    let h = keccak(&pubkey.as_bytes()[1..]);
    format!("0x{}", hex(&h[12..]))
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(bytes);
    h.finalize().into()
}

fn u256_be(v: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&v.to_be_bytes());
    out
}

fn addr_bytes(addr: &str) -> Option<[u8; 20]> {
    let clean = addr.trim().trim_start_matches("0x");
    if clean.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A published test vector: this key is in every Ethereum tutorial ever
    /// written and controls nothing.
    const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

    #[test]
    fn a_key_derives_the_address_ethereum_would_give_it() {
        // If this drifts, every signature is being attributed to the wrong
        // agent and the venue rejects them all with an unhelpful message.
        let a = Agent::from_hex(KEY).unwrap();
        assert_eq!(a.address(), "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23");
    }

    #[test]
    fn the_0x_prefix_is_optional_and_nothing_else_is_accepted() {
        assert!(Agent::from_hex(KEY).is_ok());
        assert!(Agent::from_hex(KEY.trim_start_matches("0x")).is_ok());
        assert!(matches!(Agent::from_hex("nope"), Err(SignError::BadKey)));
        assert!(matches!(Agent::from_hex(""), Err(SignError::BadKey)));
        // One hex character short: a truncated paste, which must not be treated
        // as a different valid key.
        assert!(matches!(
            Agent::from_hex(&KEY[..KEY.len() - 1]),
            Err(SignError::BadKey)
        ));
    }

    #[test]
    fn the_action_hash_depends_on_every_part_of_the_action() {
        // The hash is what authorises a specific order. If any field could
        // change without changing it, a signature would authorise more than the
        // order it was produced for.
        let a = serde_json::json!({"type": "order", "orders": [{"a": 0, "b": true, "s": "1"}]});
        let b = serde_json::json!({"type": "order", "orders": [{"a": 0, "b": true, "s": "2"}]});
        assert_ne!(
            action_hash(&a, 1, None).unwrap(),
            action_hash(&b, 1, None).unwrap()
        );
        assert_ne!(
            action_hash(&a, 1, None).unwrap(),
            action_hash(&a, 2, None).unwrap()
        );
        let v = "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303";
        assert_ne!(
            action_hash(&a, 1, None).unwrap(),
            action_hash(&a, 1, Some(v)).unwrap()
        );
    }

    #[test]
    fn the_same_action_always_hashes_the_same_way() {
        let a = serde_json::json!({"type": "cancel", "cancels": [{"a": 0, "o": 12345}]});
        assert_eq!(
            action_hash(&a, 7, None).unwrap(),
            action_hash(&a, 7, None).unwrap()
        );
    }

    #[test]
    fn mainnet_and_testnet_produce_different_signatures() {
        // Same action, different network: a testnet signature must not be
        // replayable against mainnet.
        let agent = Agent::from_hex(KEY).unwrap();
        let action = serde_json::json!({"type": "cancel", "cancels": []});
        let m = agent.sign_action(&action, 1, None, true).unwrap();
        let t = agent.sign_action(&action, 1, None, false).unwrap();
        assert_ne!((m.r, m.s), (t.r, t.s));
    }

    #[test]
    fn a_signature_is_deterministic_for_the_same_action() {
        // RFC 6979 deterministic ECDSA: the same input signs identically, which
        // is what makes a retry of a submission safe to reason about.
        let agent = Agent::from_hex(KEY).unwrap();
        let action = serde_json::json!({"type": "order", "orders": []});
        let a = agent.sign_action(&action, 42, None, true).unwrap();
        let b = agent.sign_action(&action, 42, None, true).unwrap();
        assert_eq!((a.r, a.s, a.v), (b.r, b.s, b.v));
    }

    #[test]
    fn nothing_here_can_sign_a_withdrawal() {
        // The agent-key boundary is the point: an agent cannot move funds off
        // the venue, and this module must not grow a path that assumes
        // otherwise. Scanned over the production half only - the prose above
        // and this test both have to be able to name the thing they forbid.
        let src = include_str!("sign.rs");
        let code: String = src[..src.find("#[cfg(test)]").unwrap()]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            concat!("with", "draw"),
            concat!("spot", "Send"),
            concat!("usd", "Send"),
            concat!("vault", "Transfer"),
        ] {
            assert!(
                !code.contains(needle),
                "{needle} appears in signing code: this key must only ever trade"
            );
        }
    }
}
