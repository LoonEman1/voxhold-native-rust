//! Small block patch for large binaries such as the application executable.
//!
//! The patch is a stream of copy/literal operations over 4 KiB blocks. It is
//! only used when it is smaller than storing the target file verbatim.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::error::{Result, UpdateError};

const MAGIC: &[u8; 4] = b"VXD1";
const OP_COPY: u8 = 0;
const OP_LITERAL: u8 = 1;
/// Block size used when matching base content.
pub const BLOCK_SIZE: usize = 4096;
const MAX_OPS: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Copy { offset: usize, len: usize },
    Literal { start: usize, len: usize },
}

/// Create a patch from `base` to `target`.
///
/// Returns `None` when the patch would not be smaller than the target.
pub fn create_patch(base: &[u8], target: &[u8]) -> Option<Vec<u8>> {
    if target.is_empty() {
        return None;
    }
    let mut index: HashMap<[u8; 32], Vec<(usize, usize)>> = HashMap::new();
    let mut offset = 0;
    while offset < base.len() {
        let len = BLOCK_SIZE.min(base.len() - offset);
        index
            .entry(digest(&base[offset..offset + len]))
            .or_default()
            .push((offset, len));
        offset += len;
    }

    let mut ops = Vec::new();
    let mut position = 0;
    while position < target.len() {
        let len = BLOCK_SIZE.min(target.len() - position);
        let hash = digest(&target[position..position + len]);
        let found = index.get(&hash).and_then(|candidates| {
            candidates
                .iter()
                .find(|(_, candidate_len)| *candidate_len == len)
        });
        if let Some((base_offset, _)) = found {
            match ops.last_mut() {
                Some(Op::Copy {
                    offset: previous,
                    len: previous_len,
                }) if *previous + *previous_len == *base_offset => {
                    *previous_len += len;
                }
                _ => ops.push(Op::Copy {
                    offset: *base_offset,
                    len,
                }),
            }
        } else {
            match ops.last_mut() {
                Some(Op::Literal {
                    start: previous,
                    len: previous_len,
                }) if *previous + *previous_len == position => {
                    *previous_len += len;
                }
                _ => ops.push(Op::Literal {
                    start: position,
                    len,
                }),
            }
        }
        position += len;
    }

    let mut patch = Vec::new();
    patch.extend_from_slice(MAGIC);
    for op in ops {
        match op {
            Op::Copy { offset, len } => {
                patch.push(OP_COPY);
                patch.extend_from_slice(&(len as u32).to_le_bytes());
                patch.extend_from_slice(&(offset as u64).to_le_bytes());
            }
            Op::Literal { start, len } => {
                patch.push(OP_LITERAL);
                patch.extend_from_slice(&(len as u32).to_le_bytes());
                patch.extend_from_slice(&target[start..start + len]);
            }
        }
    }
    if patch.len() >= target.len() {
        None
    } else {
        Some(patch)
    }
}

/// Apply a patch to `base`, producing exactly `expected_len` bytes.
pub fn apply_patch(base: &[u8], patch: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    if patch.len() < MAGIC.len() || &patch[..MAGIC.len()] != MAGIC {
        return Err(UpdateError::InvalidDelta("patch header is invalid".into()));
    }
    let mut output = Vec::with_capacity(expected_len);
    let mut cursor = MAGIC.len();
    let mut ops = 0usize;
    while cursor < patch.len() {
        ops += 1;
        if ops > MAX_OPS {
            return Err(UpdateError::InvalidDelta(
                "patch has too many operations".into(),
            ));
        }
        let kind = *patch
            .get(cursor)
            .ok_or_else(|| UpdateError::InvalidDelta("patch is truncated".into()))?;
        cursor += 1;
        let len = read_u32(patch, &mut cursor)? as usize;
        match kind {
            OP_COPY => {
                let offset = read_u64(patch, &mut cursor)? as usize;
                let end = offset
                    .checked_add(len)
                    .ok_or_else(|| UpdateError::InvalidDelta("patch copy overflows".into()))?;
                if end > base.len() {
                    return Err(UpdateError::InvalidDelta(
                        "patch copy leaves the base file".into(),
                    ));
                }
                if output.len() + len > expected_len {
                    return Err(UpdateError::InvalidDelta(
                        "patch produces more than the expected size".into(),
                    ));
                }
                output.extend_from_slice(&base[offset..end]);
            }
            OP_LITERAL => {
                let end = cursor
                    .checked_add(len)
                    .ok_or_else(|| UpdateError::InvalidDelta("patch literal overflows".into()))?;
                if end > patch.len() {
                    return Err(UpdateError::InvalidDelta(
                        "patch literal is truncated".into(),
                    ));
                }
                if output.len() + len > expected_len {
                    return Err(UpdateError::InvalidDelta(
                        "patch produces more than the expected size".into(),
                    ));
                }
                output.extend_from_slice(&patch[cursor..end]);
                cursor = end;
            }
            _ => {
                return Err(UpdateError::InvalidDelta(format!(
                    "unknown patch operation {kind}"
                )))
            }
        }
    }
    if output.len() != expected_len {
        return Err(UpdateError::SizeMismatch {
            path: "patch".into(),
            expected: expected_len as u64,
            actual: output.len() as u64,
        });
    }
    Ok(output)
}

fn read_u32(patch: &[u8], cursor: &mut usize) -> Result<u32> {
    let end = cursor
        .checked_add(4)
        .ok_or_else(|| UpdateError::InvalidDelta("patch offset overflows".into()))?;
    let slice = patch
        .get(*cursor..end)
        .ok_or_else(|| UpdateError::InvalidDelta("patch is truncated".into()))?;
    *cursor = end;
    Ok(u32::from_le_bytes(slice.try_into().expect("four bytes")))
}

fn read_u64(patch: &[u8], cursor: &mut usize) -> Result<u64> {
    let end = cursor
        .checked_add(8)
        .ok_or_else(|| UpdateError::InvalidDelta("patch offset overflows".into()))?;
    let slice = patch
        .get(*cursor..end)
        .ok_or_else(|| UpdateError::InvalidDelta("patch is truncated".into()))?;
    *cursor = end;
    Ok(u64::from_le_bytes(slice.try_into().expect("eight bytes")))
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_is_smaller_and_round_trips() {
        let mut base = vec![0u8; 256 * 1024];
        for (index, byte) in base.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let mut target = base.clone();
        target[100 * 1024..100 * 1024 + 32].copy_from_slice(&[0xABu8; 32]);

        let patch = create_patch(&base, &target).expect("patch should win");
        assert!(patch.len() < target.len() / 10);
        let restored = apply_patch(&base, &patch, target.len()).unwrap();
        assert_eq!(restored, target);
    }

    #[test]
    fn unrelated_content_uses_literals() {
        let base = vec![7u8; 8192];
        let target = vec![9u8; 8192];
        assert!(create_patch(&base, &target).is_none());
    }

    #[test]
    fn rejects_broken_patches() {
        let base = vec![1u8; 100];
        assert!(apply_patch(&base, b"nope", 100).is_err());
        let mut patch = Vec::new();
        patch.extend_from_slice(MAGIC);
        patch.push(OP_COPY);
        patch.extend_from_slice(&1000u32.to_le_bytes());
        patch.extend_from_slice(&0u64.to_le_bytes());
        assert!(apply_patch(&base, &patch, 1000).is_err());
    }
}
