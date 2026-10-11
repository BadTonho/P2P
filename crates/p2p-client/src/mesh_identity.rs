//! Identidade criptográfica efêmera usada para autenticar a malha de controle.
//!
//! Cada execução do aplicativo gera um par Ed25519. A chave pública viaja no
//! `ParticipantInfo` (via servidor de sinalização, que a vincula ao ID do membro)
//! e a chave privada nunca sai da memória nem é registrada em log.

use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

const AUTH_CONTEXT: &[u8] = b"p2p-control-mesh-v1";

pub struct MeshIdentity {
    key_pair: Ed25519KeyPair,
    public_key: String,
}

impl MeshIdentity {
    pub fn generate() -> Result<Self, String> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| "Não foi possível gerar a identidade da malha de controle.".to_owned())?;
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| {
            "Não foi possível carregar a identidade da malha de controle.".to_owned()
        })?;
        let public_key =
            base64::engine::general_purpose::STANDARD.encode(key_pair.public_key().as_ref());
        Ok(Self {
            key_pair,
            public_key,
        })
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Assina o desafio recebido do par e devolve a assinatura em base64.
    pub fn sign_challenge(
        &self,
        room_code: &str,
        challenge_nonce: &str,
        signer_id: &str,
        verifier_id: &str,
    ) -> String {
        let message = auth_message(room_code, challenge_nonce, signer_id, verifier_id);
        base64::engine::general_purpose::STANDARD.encode(self.key_pair.sign(&message).as_ref())
    }
}

/// Confere a assinatura do par sobre o desafio que este lado enviou.
pub fn verify_challenge(
    public_key: &str,
    signature: &str,
    room_code: &str,
    challenge_nonce: &str,
    signer_id: &str,
    verifier_id: &str,
) -> bool {
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(key), Ok(signature)) = (engine.decode(public_key), engine.decode(signature)) else {
        return false;
    };
    let message = auth_message(room_code, challenge_nonce, signer_id, verifier_id);
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&message, &signature)
        .is_ok()
}

/// Desafio aleatório de 128 bits em hexadecimal, único por conexão.
pub fn new_nonce() -> String {
    let mut bytes = [0u8; 16];
    // A falha do gerador do sistema é praticamente impossível; sem entropia o
    // desafio ficaria previsível, então devolvemos vazio e o chamador recusa.
    if SystemRandom::new().fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A mensagem inclui sala, desafio e os dois IDs em ordem fixa, impedindo que uma
/// assinatura seja reutilizada em outra conexão, sala ou direção (reflexão).
fn auth_message(
    room_code: &str,
    challenge_nonce: &str,
    signer_id: &str,
    verifier_id: &str,
) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(AUTH_CONTEXT);
    for part in [room_code, challenge_nonce, signer_id, verifier_id] {
        message.push(0);
        message.extend_from_slice(part.as_bytes());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::{MeshIdentity, new_nonce, verify_challenge};

    #[test]
    fn signature_verifies_only_for_the_exact_challenge_context() {
        let identity = MeshIdentity::generate().unwrap();
        let nonce = new_nonce();
        assert_eq!(nonce.len(), 32);
        let signature = identity.sign_challenge("ROOM", &nonce, "alice", "bob");
        let key = identity.public_key();

        assert!(verify_challenge(
            key, &signature, "ROOM", &nonce, "alice", "bob"
        ));
        assert!(!verify_challenge(
            key, &signature, "OTHER", &nonce, "alice", "bob"
        ));
        assert!(!verify_challenge(
            key,
            &signature,
            "ROOM",
            &new_nonce(),
            "alice",
            "bob"
        ));
        // Reflexão: a mesma assinatura não vale com os papéis invertidos.
        assert!(!verify_challenge(
            key, &signature, "ROOM", &nonce, "bob", "alice"
        ));
        assert!(!verify_challenge(
            key, &signature, "ROOM", &nonce, "mallory", "bob"
        ));
    }

    #[test]
    fn signature_from_another_key_or_malformed_input_is_rejected() {
        let alice = MeshIdentity::generate().unwrap();
        let mallory = MeshIdentity::generate().unwrap();
        let nonce = new_nonce();
        let forged = mallory.sign_challenge("ROOM", &nonce, "alice", "bob");

        assert!(!verify_challenge(
            alice.public_key(),
            &forged,
            "ROOM",
            &nonce,
            "alice",
            "bob"
        ));
        assert!(!verify_challenge(
            alice.public_key(),
            "!!!",
            "ROOM",
            &nonce,
            "alice",
            "bob"
        ));
        assert!(!verify_challenge(
            "curta", &forged, "ROOM", &nonce, "alice", "bob"
        ));
    }
}
