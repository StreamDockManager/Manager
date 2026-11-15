"""Neutral file mechanics; callers retain their own file/trust policy checks."""
import hashlib
import os
from pathlib import Path


# Stream file bytes into a SHA-256 integrity digest
def sha256_file(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


# Lexical containment and symlink traversal only, not a regular-file proof
def contained_without_symlinks(path, root, *, outside_message, symlink_message):
    """Lexical containment and symlink traversal only, not a regular-file proof.

    The caller chooses the root normalization, existence/size policy and whether
    junction checks or validation of ancestors outside that root are required.
    """
    path = Path(os.path.abspath(path))
    if not path.is_relative_to(root) or path == root:
        raise ValueError(outside_message)
    for entry in (path, *path.parents):
        if entry == root:
            break
        if entry.is_symlink():
            raise ValueError(symlink_message)
    return path
