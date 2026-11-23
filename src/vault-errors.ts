export function vaultFormatError(error: unknown): boolean {
    return (error instanceof Error ? error.message : String(error)) === 'vault_unsupported_format';
}

export const oldVaultMessage =
    'This Vault uses an earlier development format. To start fresh, open Vault and choose Reset Vault.';
