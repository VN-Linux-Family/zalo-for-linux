//! `updateReferenceMessageId(rootPath, updates)` — mark files in the global tree with
//! the message id that references them. The renderer feeds this the `{filePath, id}`
//! pairs it discovers while scanning conversations; every marked file is thereby kept
//! alive (no longer "homeless") for the subsequent delete/stat passes.

use crate::model::STATE;
use napi_derive::napi;

/// One `{ filePath, id }` pair from JS. napi maps camelCase JS keys onto these fields.
#[napi(object)]
pub struct ReferenceUpdate {
    pub file_path: String,
    pub id: String,
}

/// Apply the batch to the process-global tree. Returns how many stored files were
/// actually (re)marked — the renderer surfaces this as `updateCount`.
pub fn update_reference_message_id(updates: &[ReferenceUpdate]) -> u32 {
    let mut state = STATE.lock();
    let mut updated = 0u32;
    for u in updates {
        if let Some(&i) = state.index.get(&u.file_path) {
            // Count every applied mark (matching the mac `update_count`), including
            // re-marks, so a batch that touches a file twice reports two updates.
            state.files[i].reference_message_id = u.id.clone();
            updated += 1;
        }
    }
    updated
}
