// Cross-platform npm entry point. Cargo must resolve patched paths after preparation
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const result = spawnSync(
    process.env.PYTHON || (process.platform === 'win32' ? 'python' : 'python3'),
    [
        fileURLToPath(new URL('./prepare_patched_deps.py', import.meta.url)),
        ...process.argv.slice(2),
    ],
    { stdio: 'inherit' },
);
if (result.error) console.error(result.error.message);
process.exit(result.status ?? 1);
