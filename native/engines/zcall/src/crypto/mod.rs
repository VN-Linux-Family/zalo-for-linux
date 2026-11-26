//! Cryptographic primitives for call-v2 transport and SRTP media.

pub mod callv2;
pub mod srtp;

pub use callv2::{decode_transport_key, decrypt, encrypt};
pub use srtp::{master_from_sess_id, protect_rtp, session_keys, unprotect_rtp, SrtpKeys};
