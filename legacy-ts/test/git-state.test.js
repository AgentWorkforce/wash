import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { runGitState } from '../src/tools/git-state.js';

function newRepo() {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-git-'));
  const run = (args) => spawnSync('git', args, { cwd: dir });
  run(['init', '-b', 'main']);
  run(['config', 'user.email', 'test@example.com']);
  run(['config', 'user.name', 'tester']);
  run(['config', 'commit.gpgsign', 'false']);
  run(['config', 'tag.gpgsign', 'false']);
  writeFileSync(join(dir, 'README.md'), '# hi\n');
  run(['add', '.']);
  spawnSync('git', ['-c', 'commit.gpgsign=false', 'commit', '-m', 'init'], {
    cwd: dir,
    env: { ...process.env, GIT_CONFIG_COUNT: '1', GIT_CONFIG_KEY_0: 'commit.gpgsign', GIT_CONFIG_VALUE_0: 'false' },
  });
  return dir;
}

test('GitState: status returns branch and file list', () => {
  const dir = newRepo();
  writeFileSync(join(dir, 'new.txt'), 'hello');
  const r = runGitState({ op: 'status', cwd: dir });
  assert.equal(r.branch, 'main');
  assert(Array.isArray(r.files));
  assert.equal(r._meta.replaces[0], 'Bash:git-status');
});

test('GitState: log returns structured commits', () => {
  const dir = newRepo();
  const r = runGitState({ op: 'log', cwd: dir });
  assert(r.commits.length >= 1);
  assert(r.commits[0].sha);
  assert.equal(r.commits[0].subject, 'init');
});
