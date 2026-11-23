import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, readFile, rm, writeFile, access } from 'node:fs/promises';
import { setImmediate } from 'node:timers/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runDevCycles } from './dev-native-cycle.mjs';

const watchDir = 'STREAM_DOCK_MANAGER_NATIVE_WATCH_DIR';

async function fixture(t) {
    const directory = await mkdtemp(join(tmpdir(), 'manager-watch-cycle-'));
    t.after(() => rm(directory, { recursive: true, force: true }));
    return directory;
}

test('waits for the old CLI and relaunches only after its orderly restart handoff', async (t) => {
    const directory = await fixture(t);
    let changed;
    let closed = false;
    let active = 0;
    const generations = [];
    const code = await runDevCycles(
        ['dev', '--no-watch'],
        {},
        {
            directory,
            watch: (onChange) => {
                changed = onChange;
                return () => {
                    closed = true;
                };
            },
            runCLI: async (args, env) => {
                assert.deepEqual(args, ['dev', '--no-watch']);
                assert.equal(++active, 1);
                try {
                    const generation = env[watchDir];
                    generations.push(generation);
                    await writeFile(join(generation, 'ready'), '');
                    if (generations.length === 1) {
                        changed();
                        changed();
                        assert.equal(await readFile(join(generation, 'reload'), 'utf8'), '');
                        await writeFile(env.STREAM_DOCK_MANAGER_DEV_RESTART_FILE, 'restart');
                        await setImmediate();
                        assert.equal(generations.length, 1);
                    } else {
                        await assert.rejects(access(join(generation, 'reload')), {
                            code: 'ENOENT',
                        });
                    }
                    return 0;
                } finally {
                    active--;
                }
            },
        },
    );
    assert.equal(code, 0);
    assert.equal(generations.length, 2);
    assert.notEqual(generations[0], generations[1]);
    assert.ok(closed);
    await assert.rejects(access(generations[0]), { code: 'ENOENT' });
});

test('a canceled reload followed by a manual quit does not relaunch', async (t) => {
    const directory = await fixture(t);
    let changed;
    let launches = 0;
    assert.equal(
        await runDevCycles(
            ['dev'],
            {},
            {
                directory,
                watch: (onChange) => {
                    changed = onChange;
                },
                runCLI: async (_args, env) => {
                    launches++;
                    await writeFile(join(env[watchDir], 'ready'), '');
                    changed();
                    await rm(join(env[watchDir], 'reload'));
                    return 0;
                },
            },
        ),
        0,
    );
    assert.equal(launches, 1);
});

test('failed compile waits for another edit and recovers without restarting the watcher', async (t) => {
    const directory = await fixture(t);
    let changed;
    let builds = 0;
    const reports = [];
    assert.equal(
        await runDevCycles(
            ['dev'],
            {},
            {
                directory,
                watch: (onChange) => {
                    changed = onChange;
                },
                report: (message) => {
                    reports.push(message);
                    void setImmediate().then(() => changed());
                },
                runCLI: async (_args, env) => {
                    if (++builds === 1) return 101;
                    await writeFile(join(env[watchDir], 'ready'), '');
                    return 0;
                },
            },
        ),
        0,
    );
    assert.equal(builds, 2);
    assert.match(reports[0], /Waiting for another Rust change/);
});

test('interrupt while waiting after a failed build closes the watcher without relaunching', async (t) => {
    const directory = await fixture(t);
    const signals = new EventEmitter();
    let builds = 0;
    let closed = false;
    assert.equal(
        await runDevCycles(
            ['dev'],
            {},
            {
                directory,
                signals,
                watch: () => () => {
                    closed = true;
                },
                report: () => {
                    void setImmediate().then(() => signals.emit('SIGINT'));
                },
                runCLI: async () => {
                    builds++;
                    return 101;
                },
            },
        ),
        130,
    );
    assert.equal(builds, 1);
    assert.equal(signals.listenerCount('SIGINT'), 0);
    assert.ok(closed);
});

test('no-watch strips inherited watch state and preserves manual application restarts', async (t) => {
    const directory = await fixture(t);
    let runs = 0;
    const code = await runDevCycles(
        ['dev', '--no-watch'],
        { [watchDir]: '/inherited' },
        {
            directory,
            runCLI: async (_args, env) => {
                assert.equal(env[watchDir], undefined);
                if (++runs === 1) {
                    await writeFile(env.STREAM_DOCK_MANAGER_DEV_RESTART_FILE, 'restart');
                }
                return 0;
            },
        },
    );
    assert.equal(code, 0);
    assert.equal(runs, 2);
});
