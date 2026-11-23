// Normal development shares the macOS bundle workflow; other platforms retain Tauri dev.
import { signedDev } from './dev-signed.mjs';
import { runTauri } from './tauri.mjs';
export async function development(
    args,
    { platform = process.platform, local = signedDev, tauri = runTauri } = {},
) {
    return platform === 'darwin' ? local(args, { signed: false }) : tauri(['dev', ...args]);
}
import { pathToFileURL } from 'node:url';
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    try {
        const result = await development(process.argv.slice(2));
        if (typeof result === 'number') process.exitCode = result;
        else console.log(JSON.stringify(result, null, 2));
    } catch (error) {
        console.error(error.message);
        process.exitCode = 1;
    }
}
