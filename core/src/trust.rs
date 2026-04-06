//! Trust adapter for TSP signing and verification.

use crate::config::TrustConfig;
use crate::messages::InboundMessage;
use base64::{Engine, engine::general_purpose};
use tracing::{debug, warn};

/// Result of inbound trust verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundTrustDecision {
    Accepted,
    Rejected(String),
    Skipped(String),
}

/// Verify an inbound message trust packet against the configured trusted VID.
///
/// Behavior:
/// - trust disabled => skip
/// - no packet + allow_unsigned_inbound => skip
/// - no packet + strict => reject
/// - packet present => verify signature and payload integrity
pub async fn verify_inbound(
    msg: &InboundMessage,
    trust_config: &TrustConfig,
) -> InboundTrustDecision {
    if !trust_config.enabled {
        return InboundTrustDecision::Skipped("trust disabled".to_string());
    }

    let Some(packet) = msg.trust_packet.as_deref() else {
        if should_require_trust_packet(msg, trust_config) {
            return InboundTrustDecision::Rejected(format!(
                "missing trust packet for strict channel '{}'",
                msg.channel
            ));
        }

        if trust_config.allow_unsigned_inbound {
            return InboundTrustDecision::Skipped("unsigned inbound allowed".to_string());
        }
        return InboundTrustDecision::Rejected("missing trust packet".to_string());
    };

    let Some(verify_vid_path) = resolve_verify_vid_path(msg, trust_config) else {
        return InboundTrustDecision::Rejected(format!(
            "no verify VID path configured for sender '{}'",
            msg.sender_id
        ));
    };

    let verify_vid = match tsp_sdk::OwnedVid::from_file(&verify_vid_path).await {
        Ok(vid) => vid,
        Err(err) => {
            return InboundTrustDecision::Rejected(format!(
                "failed to load verify VID from '{}': {}",
                verify_vid_path, err
            ));
        }
    };

    let mut tsp_packet = match decode_packet(packet) {
        Ok(decoded) => decoded,
        Err(err) => {
            return InboundTrustDecision::Rejected(format!(
                "invalid trust packet encoding: {}",
                err
            ));
        }
    };

    match tsp_sdk::crypto::verify(&verify_vid, &mut tsp_packet) {
        Ok((payload, _message_type)) => {
            if payload == msg.content.as_bytes() {
                InboundTrustDecision::Accepted
            } else {
                InboundTrustDecision::Rejected(
                    "signature valid but payload does not match message content".to_string(),
                )
            }
        }
        Err(err) => {
            InboundTrustDecision::Rejected(format!("signature verification failed: {}", err))
        }
    }
}

/// Sign outbound content with configured private VID and return a serialized trust packet.
///
/// Returns None when signing is disabled or configuration is incomplete.
pub async fn sign_outbound(content: &str, trust_config: &TrustConfig) -> Option<String> {
    if !trust_config.enabled {
        return None;
    }

    let signing_vid_path = trust_config.signing_vid_path.trim();
    if signing_vid_path.is_empty() {
        debug!("TEA: signing skipped because signing_vid_path is not configured");
        return None;
    }

    let signing_vid = match tsp_sdk::OwnedVid::from_file(signing_vid_path).await {
        Ok(vid) => vid,
        Err(err) => {
            warn!(
                "TEA: failed to load signing VID from '{}': {}",
                signing_vid_path, err
            );
            return None;
        }
    };

    match tsp_sdk::crypto::sign(&signing_vid, None, content.as_bytes()) {
        Ok(packet) => Some(general_purpose::URL_SAFE_NO_PAD.encode(packet)),
        Err(err) => {
            warn!("TEA: outbound signing failed: {}", err);
            None
        }
    }
}

fn decode_packet(packet: &str) -> Result<Vec<u8>, base64::DecodeError> {
    // Prefer URL-safe packet encoding, but accept standard base64 for compatibility.
    general_purpose::URL_SAFE_NO_PAD
        .decode(packet)
        .or_else(|_| general_purpose::STANDARD.decode(packet))
}

fn resolve_verify_vid_path(msg: &InboundMessage, trust_config: &TrustConfig) -> Option<String> {
    // Internal subagent events are produced inside the same process and are signed with
    // the local signing VID when trust is enabled. Verify them against the signing VID first.
    if msg.channel.eq_ignore_ascii_case("system") && msg.sender_id.starts_with("subagent_") {
        let path = trust_config.signing_vid_path.trim();
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }

    if let Some(path) = trust_config.sender_verify_vid_paths.get(&msg.sender_id) {
        let path = path.trim();
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }

    let path = trust_config.verify_vid_path.trim();
    if !path.is_empty() {
        return Some(path.to_string());
    }

    None
}

fn should_require_trust_packet(msg: &InboundMessage, trust_config: &TrustConfig) -> bool {
    trust_config
        .strict_inbound_channels
        .iter()
        .any(|channel| channel.eq_ignore_ascii_case(&msg.channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::InboundMessage;
    use std::collections::HashMap;
    use tempfile::tempdir;

    async fn create_test_vid_file() -> String {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("agent_vid.json");

        let vid = tsp_sdk::OwnedVid::bind(
            "did:test:agent",
            "tcp://127.0.0.1:13371"
                .parse()
                .expect("parse transport URL"),
        );

        let serialized = serde_json::to_string(&vid).expect("serialize vid");
        tokio::fs::write(&path, serialized)
            .await
            .expect("write vid file");

        // Keep the temp directory alive by leaking it for test lifetime.
        let leaked = Box::leak(Box::new(dir));
        leaked.path().join("agent_vid.json").display().to_string()
    }

    #[tokio::test]
    async fn verify_skips_when_disabled() {
        let cfg = TrustConfig::default();
        let msg = InboundMessage::new("cli", "user", "chat", "hello");
        let result = verify_inbound(&msg, &cfg).await;
        assert!(matches!(result, InboundTrustDecision::Skipped(_)));
    }

    #[tokio::test]
    async fn verify_rejects_missing_packet_when_strict() {
        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: false,
            signing_vid_path: String::new(),
            verify_vid_path: String::new(),
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let msg = InboundMessage::new("cli", "user", "chat", "hello");
        let result = verify_inbound(&msg, &cfg).await;
        assert!(matches!(result, InboundTrustDecision::Rejected(_)));
    }

    #[tokio::test]
    async fn sign_and_verify_roundtrip() {
        let vid_path = create_test_vid_file().await;

        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: false,
            signing_vid_path: vid_path.clone(),
            verify_vid_path: vid_path,
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let packet = sign_outbound("hello", &cfg)
            .await
            .expect("outbound packet should be generated");

        let msg =
            InboundMessage::new("system", "agent-A", "hub:1", "hello").with_trust_packet(packet);

        let result = verify_inbound(&msg, &cfg).await;
        assert_eq!(result, InboundTrustDecision::Accepted);
    }

    #[tokio::test]
    async fn verify_rejects_tampered_content() {
        let vid_path = create_test_vid_file().await;

        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: false,
            signing_vid_path: vid_path.clone(),
            verify_vid_path: vid_path,
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let packet = sign_outbound("hello", &cfg)
            .await
            .expect("outbound packet should be generated");

        let msg =
            InboundMessage::new("system", "agent-A", "hub:1", "tampered").with_trust_packet(packet);

        let result = verify_inbound(&msg, &cfg).await;
        assert!(matches!(result, InboundTrustDecision::Rejected(_)));
    }

    #[tokio::test]
    async fn verify_prefers_sender_specific_vid_path() {
        let sender_vid_path = create_test_vid_file().await;
        let fallback_vid_path = create_test_vid_file().await;

        let mut sender_verify_vid_paths = HashMap::new();
        sender_verify_vid_paths.insert("agent-A".to_string(), sender_vid_path.clone());

        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: false,
            signing_vid_path: sender_vid_path,
            // Keep fallback intentionally different; sender-specific path should win.
            verify_vid_path: fallback_vid_path,
            sender_verify_vid_paths,
            strict_inbound_channels: vec!["system".to_string()],
        };

        let packet = sign_outbound("hello", &cfg)
            .await
            .expect("outbound packet should be generated");

        let msg =
            InboundMessage::new("system", "agent-A", "hub:1", "hello").with_trust_packet(packet);

        let result = verify_inbound(&msg, &cfg).await;
        assert_eq!(result, InboundTrustDecision::Accepted);
    }

    #[test]
    fn resolve_verify_vid_path_prefers_signing_vid_for_internal_subagent_message() {
        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: false,
            signing_vid_path: "local_signing_vid.json".to_string(),
            verify_vid_path: "peer_verify_vid.json".to_string(),
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let msg = InboundMessage::new("system", "subagent_123", "hub:1", "hello");

        assert_eq!(
            resolve_verify_vid_path(&msg, &cfg),
            Some("local_signing_vid.json".to_string())
        );
    }

    #[tokio::test]
    async fn verify_rejects_missing_packet_for_strict_channel() {
        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: true,
            signing_vid_path: String::new(),
            verify_vid_path: String::new(),
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let msg = InboundMessage::new("system", "agent-A", "hub:1", "hello");
        let result = verify_inbound(&msg, &cfg).await;

        assert!(matches!(result, InboundTrustDecision::Rejected(_)));
    }

    #[tokio::test]
    async fn verify_skips_missing_packet_for_non_strict_channel() {
        let cfg = TrustConfig {
            enabled: true,
            allow_unsigned_inbound: true,
            signing_vid_path: String::new(),
            verify_vid_path: String::new(),
            sender_verify_vid_paths: HashMap::new(),
            strict_inbound_channels: vec!["system".to_string()],
        };

        let msg = InboundMessage::new("cli", "user", "chat", "hello");
        let result = verify_inbound(&msg, &cfg).await;

        assert!(matches!(result, InboundTrustDecision::Skipped(_)));
    }
}
